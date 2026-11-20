//! SRUM: per-application CPU, disk and network counters.
//!
//! The database copy is opened read-only with the ESE engine that ships with Windows
//! (`esent.dll`), with recovery switched off: nothing is written to the copy. A copy that was
//! taken while the service had the database open is in the dirty-shutdown state and cannot be
//! attached this way. [`probe`] tells whether a copy can be read; `collect` retries the copy
//! until it can, and lookups pass over copies that cannot be read.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::Mutex;

use serde_json::{Value, json};
use windows_sys::Win32::Storage::Jet::{
    JET_COLUMNDEF, JET_DBINFOMISC, JET_INSTANCE, JET_SESID, JetAttachDatabaseW, JetBeginSessionW, JetCloseDatabase,
    JetCloseTable, JetCreateInstanceW, JetDetachDatabaseW, JetEndSession, JetGetDatabaseFileInfoW,
    JetGetTableColumnInfoW, JetInit, JetMove, JetOpenDatabaseW, JetOpenTableW, JetRetrieveColumn,
    JetSetSystemParameterW, JetTerm,
};
use windows_sys::Win32::Storage::StructuredStorage::JET_TABLEID;

use super::PathMap;
use crate::win;

const ID_MAP_TABLE: &str = "SruDbIdMapTable";
const APP_RESOURCE_TABLE: &str = "{D10CA2FE-6FCF-4F6D-848E-B2E99266FA89}";
const NETWORK_TABLE: &str = "{973F5D5C-1D90-4944-BE8E-24B94231A174}";

// Values from esent.h.
const JET_PARAM_SYSTEM_PATH: u32 = 0;
const JET_PARAM_TEMP_PATH: u32 = 1;
const JET_PARAM_LOG_FILE_PATH: u32 = 2;
const JET_PARAM_MAX_TEMPORARY_TABLES: u32 = 10;
const JET_PARAM_RECOVERY: u32 = 34;
const JET_PARAM_ENABLE_INDEX_CHECKING: u32 = 45;
const JET_PARAM_NO_INFORMATION_EVENT: u32 = 50;
const JET_PARAM_DATABASE_PAGE_SIZE: u32 = 64;
const JET_PARAM_CREATE_PATH_IF_NOT_EXIST: u32 = 100;
const JET_DB_INFO_MISC: u32 = 14;
const JET_BIT_DB_READ_ONLY: u32 = 0x1;
const JET_BIT_TABLE_READ_ONLY: u32 = 0x4;
const JET_BIT_TABLE_SEQUENTIAL: u32 = 0x8000;
const JET_MOVE_FIRST: i32 = i32::MIN;
const JET_MOVE_NEXT: i32 = 1;
const JET_WRN_COLUMN_NULL: i32 = 1004;
const JET_WRN_BUFFER_TRUNCATED: i32 = 1006;
const JET_ERR_DATABASE_DIRTY_SHUTDOWN: i32 = -550;

fn check(code: i32, step: &str) -> Result<(), String> {
    if code >= 0 {
        return Ok(());
    }
    let hint = match code {
        JET_ERR_DATABASE_DIRTY_SHUTDOWN => " (the copy was taken while the database was open; run collect again)",
        _ => "",
    };
    Err(format!("{step}: ESE error {code}{hint}"))
}

/// `(state name, page size)` read from the database header without opening the database.
pub fn database_info(path: &Path) -> Result<(&'static str, u32), String> {
    let name = win::wide(path);
    let mut info: JET_DBINFOMISC = unsafe { std::mem::zeroed() };
    // SAFETY: the output buffer is exactly one JET_DBINFOMISC.
    let code = unsafe {
        JetGetDatabaseFileInfoW(
            name.as_ptr(),
            (&raw mut info).cast::<c_void>(),
            size_of::<JET_DBINFOMISC>() as u32,
            JET_DB_INFO_MISC,
        )
    };
    check(code, "read database header")?;
    let state = match info.dbstate {
        1 => "JustCreated",
        2 => "DirtyShutdown",
        3 => "CleanShutdown",
        4 => "BeingConverted",
        5 => "ForceDetach",
        _ => "Unknown",
    };
    Ok((state, info.cbPageSize))
}

/// Page size the ESE engine of this process was configured with. The setting is process-wide
/// and is fixed once the first instance has started.
static PAGE_SIZE: Mutex<Option<u32>> = Mutex::new(None);

fn configure_page_size(page_size: u32) -> Result<(), String> {
    let mut configured = PAGE_SIZE.lock().map_err(|_| "page size lock poisoned".to_owned())?;
    match *configured {
        Some(size) if size == page_size => Ok(()),
        Some(size) => Err(format!(
            "database page size {page_size} differs from the page size {size} already used in this process"
        )),
        None => {
            // SAFETY: a null instance sets the process-wide value; no pointer is passed.
            let code = unsafe {
                JetSetSystemParameterW(null_mut(), 0, JET_PARAM_DATABASE_PAGE_SIZE, page_size as usize, null())
            };
            check(code, "set page size")?;
            *configured = Some(page_size);
            Ok(())
        }
    }
}

struct Engine {
    instance: JET_INSTANCE,
    session: JET_SESID,
    database: u32,
    path: Vec<u16>,
    work_dir: PathBuf,
    initialised: bool,
    attached: bool,
    opened: bool,
}

impl Engine {
    fn open(path: &Path, page_size: u32) -> Result<Self, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let work_dir = std::env::temp_dir().join(format!("zero-surplus-ese-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&work_dir).map_err(|err| format!("create engine folder: {err}"))?;
        let mut engine = Self {
            instance: 0,
            session: 0,
            database: 0,
            path: win::wide(path),
            work_dir,
            initialised: false,
            attached: false,
            opened: false,
        };
        let work = win::wide(format!("{}\\", engine.work_dir.display()));
        let off = win::wide("Off");
        configure_page_size(page_size)?;
        // SAFETY: every pointer passed below is valid for the duration of its call.
        unsafe {
            let name = win::wide("zero-surplus");
            check(JetCreateInstanceW(&mut engine.instance, name.as_ptr()), "create engine instance")?;
            let instance = &mut engine.instance as *mut JET_INSTANCE;
            // No recovery, no logs, no temporary database: the engine only reads.
            check(JetSetSystemParameterW(instance, 0, JET_PARAM_RECOVERY, 0, off.as_ptr()), "disable recovery")?;
            check(JetSetSystemParameterW(instance, 0, JET_PARAM_MAX_TEMPORARY_TABLES, 0, null()), "set temp tables")?;
            check(JetSetSystemParameterW(instance, 0, JET_PARAM_NO_INFORMATION_EVENT, 1, null()), "silence events")?;
            check(JetSetSystemParameterW(instance, 0, JET_PARAM_ENABLE_INDEX_CHECKING, 0, null()), "index checking")?;
            check(JetSetSystemParameterW(instance, 0, JET_PARAM_CREATE_PATH_IF_NOT_EXIST, 1, null()), "create path")?;
            for parameter in [JET_PARAM_SYSTEM_PATH, JET_PARAM_TEMP_PATH, JET_PARAM_LOG_FILE_PATH] {
                check(JetSetSystemParameterW(instance, 0, parameter, 0, work.as_ptr()), "set engine folder")?;
            }
            check(JetInit(instance), "start engine")?;
            engine.initialised = true;
            check(JetBeginSessionW(engine.instance, &mut engine.session, null(), null()), "begin session")?;
            check(JetAttachDatabaseW(engine.session, engine.path.as_ptr(), JET_BIT_DB_READ_ONLY), "attach database")?;
            engine.attached = true;
            check(
                JetOpenDatabaseW(engine.session, engine.path.as_ptr(), null(), &mut engine.database, JET_BIT_DB_READ_ONLY),
                "open database",
            )?;
            engine.opened = true;
        }
        Ok(engine)
    }

    fn table(&self, name: &str) -> Result<Table<'_>, String> {
        let wide = win::wide(name);
        let mut id: JET_TABLEID = 0;
        // SAFETY: `wide` is null-terminated and `id` receives the table handle.
        let code = unsafe {
            JetOpenTableW(
                self.session,
                self.database,
                wide.as_ptr(),
                null(),
                0,
                JET_BIT_TABLE_READ_ONLY | JET_BIT_TABLE_SEQUENTIAL,
                &mut id,
            )
        };
        check(code, &format!("open table {name}"))?;
        Ok(Table { engine: self, id })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: handles are released in reverse order of acquisition, each at most once.
        unsafe {
            if self.opened {
                JetCloseDatabase(self.session, self.database, 0);
            }
            if self.attached {
                JetDetachDatabaseW(self.session, self.path.as_ptr());
            }
            if self.session != 0 {
                JetEndSession(self.session, 0);
            }
            if self.instance != 0 {
                JetTerm(self.instance);
            }
        }
        let _ = std::fs::remove_dir_all(&self.work_dir);
    }
}

struct Table<'a> {
    engine: &'a Engine,
    id: JET_TABLEID,
}

impl Table<'_> {
    fn column(&self, name: &str) -> Option<u32> {
        let wide = win::wide(name);
        let mut definition: JET_COLUMNDEF = unsafe { std::mem::zeroed() };
        definition.cbStruct = size_of::<JET_COLUMNDEF>() as u32;
        // SAFETY: the output buffer is one JET_COLUMNDEF; info level 0 fills exactly that.
        let code = unsafe {
            JetGetTableColumnInfoW(
                self.engine.session,
                self.id,
                wide.as_ptr(),
                (&raw mut definition).cast::<c_void>(),
                size_of::<JET_COLUMNDEF>() as u32,
                0,
            )
        };
        (code >= 0).then_some(definition.columnid)
    }

    fn first(&self) -> bool {
        // SAFETY: session and table handles are valid while `self` exists.
        unsafe { JetMove(self.engine.session, self.id, JET_MOVE_FIRST, 0) >= 0 }
    }

    fn next(&self) -> bool {
        // SAFETY: as in `first`.
        unsafe { JetMove(self.engine.session, self.id, JET_MOVE_NEXT, 0) >= 0 }
    }

    /// Raw bytes of a column in the current row; `None` for NULL.
    fn bytes(&self, column: u32, buffer: &mut Vec<u8>) -> Option<usize> {
        let mut actual = 0u32;
        let retrieve = |buffer: &mut Vec<u8>, actual: &mut u32| {
            // SAFETY: `buffer` is valid for its length and `actual` receives the column size.
            unsafe {
                JetRetrieveColumn(
                    self.engine.session,
                    self.id,
                    column,
                    buffer.as_mut_ptr().cast::<c_void>(),
                    buffer.len() as u32,
                    actual,
                    0,
                    null_mut(),
                )
            }
        };
        let mut code = retrieve(buffer, &mut actual);
        if code == JET_WRN_BUFFER_TRUNCATED {
            buffer.resize(actual as usize, 0);
            code = retrieve(buffer, &mut actual);
        }
        (code >= 0 && code != JET_WRN_COLUMN_NULL).then_some((actual as usize).min(buffer.len()))
    }

    fn integer(&self, column: Option<u32>, buffer: &mut Vec<u8>) -> Option<i64> {
        let len = self.bytes(column?, buffer)?;
        Some(match len {
            1 => i64::from(buffer[0]),
            2 => i64::from(i16::from_le_bytes(buffer[..2].try_into().ok()?)),
            4 => i64::from(i32::from_le_bytes(buffer[..4].try_into().ok()?)),
            8 => i64::from_le_bytes(buffer[..8].try_into().ok()?),
            _ => return None,
        })
    }

    /// A DateTime column (OLE automation date, UTC) as Unix seconds.
    fn unix_seconds(&self, column: Option<u32>, buffer: &mut Vec<u8>) -> Option<i64> {
        let len = self.bytes(column?, buffer)?;
        (len == 8).then(|| ole_date_to_unix(f64::from_le_bytes(buffer[..8].try_into().unwrap())))
    }
}

impl Drop for Table<'_> {
    fn drop(&mut self) {
        // SAFETY: the table handle is closed once, before its engine.
        unsafe { JetCloseTable(self.engine.session, self.id) };
    }
}

/// Days since 1899-12-30 to seconds since 1970-01-01.
pub fn ole_date_to_unix(days: f64) -> i64 {
    ((days - 25_569.0) * 86_400.0).round() as i64
}

/// Binary SID to `S-1-5-...`.
pub fn sid_to_string(bytes: &[u8]) -> Option<String> {
    let count = usize::from(*bytes.get(1)?);
    let authority = bytes.get(2..8)?.iter().fold(0u64, |value, &byte| (value << 8) | u64::from(byte));
    let mut text = format!("S-{}-{authority}", bytes[0]);
    for index in 0..count {
        let start = 8 + index * 4;
        text.push_str(&format!("-{}", u32::from_le_bytes(bytes.get(start..start + 4)?.try_into().ok()?)));
    }
    Some(text)
}

fn utf16_blob(bytes: &[u8]) -> String {
    let units: Vec<u16> =
        bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).take_while(|&unit| unit != 0).collect();
    String::from_utf16_lossy(&units)
}

/// File name an app identifier stands for: the last path component, or the name inside a
/// short-lived app entry (`!!name.exe!2024/05/01:22:21:50!3f2312!`).
pub fn identifier_name(identifier: &str) -> &str {
    if let Some(rest) = identifier.strip_prefix("!!") {
        return rest.split('!').next().unwrap_or(rest);
    }
    identifier.rsplit('\\').next().unwrap_or(identifier)
}

#[derive(Default)]
struct Counters {
    records: u64,
    foreground_cycles: i128,
    background_cycles: i128,
    foreground_bytes_read: i128,
    foreground_bytes_written: i128,
    background_bytes_read: i128,
    background_bytes_written: i128,
    network_records: u64,
    bytes_sent: i128,
    bytes_received: i128,
}

impl Counters {
    fn to_json(&self) -> Value {
        json!({
            "records": self.records,
            "foreground_cycles": self.foreground_cycles,
            "background_cycles": self.background_cycles,
            "foreground_bytes_read": self.foreground_bytes_read,
            "foreground_bytes_written": self.foreground_bytes_written,
            "background_bytes_read": self.background_bytes_read,
            "background_bytes_written": self.background_bytes_written,
            "network_records": self.network_records,
            "bytes_sent": self.bytes_sent,
            "bytes_received": self.bytes_received,
        })
    }
}

pub struct AppUsage {
    /// Identifiers exactly as SRUM stores them (device path, package name, ...). SRUM keeps
    /// separate entries for spellings that differ only in letter case; they are merged here.
    pub identifiers: Vec<String>,
    /// Drive-letter path when the identifier is a device path on a known volume.
    pub path: Option<String>,
    users: BTreeSet<String>,
    first: i64,
    last: i64,
    total: Counters,
    days: BTreeMap<String, Counters>,
}

impl AppUsage {
    pub fn last_seen(&self) -> Option<String> {
        rfc3339(self.last)
    }

    pub fn active_days(&self) -> usize {
        self.days.len()
    }

    /// First and last recording time, as seconds since 1970.
    pub fn first_unix(&self) -> i64 {
        self.first
    }

    pub fn last_unix(&self) -> i64 {
        self.last
    }

    pub fn to_json(&self, max_days: usize) -> Value {
        let days: Vec<Value> = self
            .days
            .iter()
            .rev()
            .take(max_days)
            .map(|(day, counters)| {
                let mut value = json!({ "day": day });
                if let (Value::Object(value), Value::Object(counters)) = (&mut value, counters.to_json()) {
                    value.extend(counters);
                }
                value
            })
            .collect();
        json!({
            "identifiers": self.identifiers,
            "path": self.path,
            "users": self.users,
            // Records exist for the intervals in which the app ran between these two times.
            "observed_from": rfc3339(self.first),
            "observed_to": rfc3339(self.last),
            "active_days": self.days.len(),
            "totals": self.total.to_json(),
            "days_shown": days.len(),
            "days": days,
        })
    }
}

fn rfc3339(unix_seconds: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|utc| utc.with_timezone(&chrono::Local).to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

fn local_day(unix_seconds: i64) -> String {
    chrono::DateTime::from_timestamp(unix_seconds, 0)
        .map(|utc| utc.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

pub struct SrumResult {
    /// ESE database state of the copy that was read.
    pub db_state: String,
    pub apps: Vec<AppUsage>,
}

/// Units of every counter, stored next to the values.
pub fn units() -> Value {
    json!({
        "cycles": "CPU cycles",
        "bytes_read": "bytes",
        "bytes_written": "bytes",
        "bytes_sent": "bytes",
        "bytes_received": "bytes",
        "records": "one per app, user and recording interval (about an hour)",
    })
}

/// Whether the database at `path` can be read: it opens and has the tables [`query`] reads.
pub fn probe(path: &Path) -> Result<(), String> {
    let (_, page_size) = database_info(path)?;
    let engine = Engine::open(path, page_size)?;
    for table in [ID_MAP_TABLE, APP_RESOURCE_TABLE, NETWORK_TABLE] {
        engine.table(table)?;
    }
    Ok(())
}

/// Aggregate the app resource and network tables for apps accepted by `wanted(path, name)`.
pub fn query(db_path: &Path, map: &PathMap, wanted: &dyn Fn(Option<&str>, &str) -> bool) -> Result<SrumResult, String> {
    let (db_state, page_size) = database_info(db_path)?;
    let engine = Engine::open(db_path, page_size)?;
    let mut buffer = vec![0u8; 1024];

    // ID map: app identifiers (types 0-2, UTF-16 text) and user SIDs (type 3).
    let mut apps: Vec<AppUsage> = Vec::new();
    let mut app_slots: HashMap<i64, usize> = HashMap::new();
    let mut slot_by_key: HashMap<String, usize> = HashMap::new();
    let mut users: HashMap<i64, String> = HashMap::new();
    {
        let table = engine.table(ID_MAP_TABLE)?;
        let (id_type, id_index, id_blob) = (table.column("IdType"), table.column("IdIndex"), table.column("IdBlob"));
        let mut more = table.first();
        while more {
            if let (Some(kind), Some(index)) = (table.integer(id_type, &mut buffer), table.integer(id_index, &mut buffer))
                && let Some(len) = id_blob.and_then(|column| table.bytes(column, &mut buffer))
            {
                let blob = &buffer[..len];
                if kind == 3 {
                    if let Some(sid) = sid_to_string(blob) {
                        users.insert(index, sid);
                    }
                } else {
                    let identifier = utf16_blob(blob);
                    let path = identifier.starts_with('\\').then(|| map.from_device(&identifier)).flatten();
                    if wanted(path.as_deref(), identifier_name(&identifier)) {
                        let key = match &path {
                            Some(path) => win::path_key(Path::new(path)),
                            None => identifier.to_lowercase(),
                        };
                        let slot = *slot_by_key.entry(key).or_insert_with(|| {
                            apps.push(AppUsage {
                                identifiers: Vec::new(),
                                path: None,
                                users: BTreeSet::new(),
                                first: i64::MAX,
                                last: i64::MIN,
                                total: Counters::default(),
                                days: BTreeMap::new(),
                            });
                            apps.len() - 1
                        });
                        app_slots.insert(index, slot);
                        let app = &mut apps[slot];
                        // Keep the spelling that preserves letter case when there is one.
                        let lower_only = |text: &String| *text == text.to_lowercase();
                        if path.is_some() && app.path.as_ref().is_none_or(lower_only) {
                            app.path = path;
                        }
                        app.identifiers.push(identifier);
                    }
                }
            }
            more = table.next();
        }
    }

    if !apps.is_empty() {
        let add = |total: &mut i128, value: Option<i64>| *total += i128::from(value.unwrap_or(0));

        let table = engine.table(APP_RESOURCE_TABLE)?;
        let app_id = table.column("AppId");
        let user_id = table.column("UserId");
        let time = table.column("TimeStamp");
        let columns = [
            table.column("ForegroundCycleTime"),
            table.column("BackgroundCycleTime"),
            table.column("ForegroundBytesRead"),
            table.column("ForegroundBytesWritten"),
            table.column("BackgroundBytesRead"),
            table.column("BackgroundBytesWritten"),
        ];
        let mut more = table.first();
        while more {
            // Only the app id is read for rows of other apps.
            if let Some(slot) = table.integer(app_id, &mut buffer).and_then(|id| app_slots.get(&id).copied())
                && let Some(seconds) = table.unix_seconds(time, &mut buffer)
            {
                let values = columns.map(|column| table.integer(column, &mut buffer));
                let user = table.integer(user_id, &mut buffer).and_then(|id| users.get(&id));
                let app = &mut apps[slot];
                app.first = app.first.min(seconds);
                app.last = app.last.max(seconds);
                if let Some(user) = user {
                    app.users.insert(user.clone());
                }
                let day = app.days.entry(local_day(seconds)).or_default();
                for counters in [&mut app.total, day] {
                    counters.records += 1;
                    add(&mut counters.foreground_cycles, values[0]);
                    add(&mut counters.background_cycles, values[1]);
                    add(&mut counters.foreground_bytes_read, values[2]);
                    add(&mut counters.foreground_bytes_written, values[3]);
                    add(&mut counters.background_bytes_read, values[4]);
                    add(&mut counters.background_bytes_written, values[5]);
                }
            }
            more = table.next();
        }
        drop(table);

        let table = engine.table(NETWORK_TABLE)?;
        let app_id = table.column("AppId");
        let time = table.column("TimeStamp");
        let (sent, received) = (table.column("BytesSent"), table.column("BytesRecvd"));
        let mut more = table.first();
        while more {
            if let Some(slot) = table.integer(app_id, &mut buffer).and_then(|id| app_slots.get(&id).copied())
                && let Some(seconds) = table.unix_seconds(time, &mut buffer)
            {
                let (sent, received) = (table.integer(sent, &mut buffer), table.integer(received, &mut buffer));
                let app = &mut apps[slot];
                app.first = app.first.min(seconds);
                app.last = app.last.max(seconds);
                let day = app.days.entry(local_day(seconds)).or_default();
                for counters in [&mut app.total, day] {
                    counters.network_records += 1;
                    add(&mut counters.bytes_sent, sent);
                    add(&mut counters.bytes_received, received);
                }
            }
            more = table.next();
        }
    }

    // Apps listed in the ID map without any usage row are not reported.
    apps.retain(|app| app.first <= app.last);
    Ok(SrumResult { db_state: db_state.to_owned(), apps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ole_dates_convert_to_unix_seconds() {
        assert_eq!(ole_date_to_unix(25_569.0), 0);
        // 2026-11-19 12:00:00 UTC
        assert_eq!(ole_date_to_unix(46_345.5), 1_795_089_600);
    }

    #[test]
    fn binary_sids_are_formatted() {
        // S-1-5-21-1-2-3-1001
        let mut sid = vec![1, 5, 0, 0, 0, 0, 0, 5];
        for part in [21u32, 1, 2, 3, 1001] {
            sid.extend_from_slice(&part.to_le_bytes());
        }
        assert_eq!(sid_to_string(&sid).as_deref(), Some("S-1-5-21-1-2-3-1001"));
        assert_eq!(sid_to_string(&[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0]).as_deref(), Some("S-1-5-18"));
        assert_eq!(sid_to_string(&[1, 4, 0, 0]), None);
    }

    #[test]
    fn identifier_names_cover_paths_and_short_lived_entries() {
        assert_eq!(identifier_name(r"\Device\HarddiskVolume3\envs\a\python.exe"), "python.exe");
        assert_eq!(identifier_name("!!msedge.exe!2024/05/01:22:21:50!3f2312!"), "msedge.exe");
        assert_eq!(identifier_name("Microsoft.WindowsStore_8wekyb3d8bbwe"), "Microsoft.WindowsStore_8wekyb3d8bbwe");
    }
}
