//! Small ESE databases built with the Windows ESE engine, shaped like SRUDB.dat.

use std::ffi::c_void;
use std::path::Path;
use std::ptr::{null, null_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use windows_sys::Win32::Storage::Jet::{
    JET_COLUMNDEF, JET_INSTANCE, JET_SESID, JetAddColumnW, JetBeginSessionW, JetBeginTransaction, JetCloseDatabase,
    JetCloseTable, JetCommitTransaction, JetCreateDatabaseW, JetCreateInstanceW, JetCreateTableW, JetDetachDatabaseW,
    JetEndSession, JetInit, JetPrepareUpdate, JetSetColumn, JetSetSystemParameterW, JetTerm, JetTerm2, JetUpdate,
};
use windows_sys::Win32::Storage::StructuredStorage::JET_TABLEID;

pub const LONG: u32 = 4;
pub const CURRENCY: u32 = 5;
pub const DATE_TIME: u32 = 8;
pub const LONG_BINARY: u32 = 11;

const PARAM_SYSTEM_PATH: u32 = 0;
const PARAM_TEMP_PATH: u32 = 1;
const PARAM_LOG_FILE_PATH: u32 = 2;
const PARAM_RECOVERY: u32 = 34;
const PARAM_NO_INFORMATION_EVENT: u32 = 50;
const PARAM_CREATE_PATH_IF_NOT_EXIST: u32 = 100;
const PREP_INSERT: u32 = 0;
const TERM_DIRTY: u32 = 8;
const DIRTY_SHUTDOWN: i32 = -1116;

static LOCK: Mutex<()> = Mutex::new(());
static COUNTER: AtomicU32 = AtomicU32::new(0);

pub enum Cell {
    Long(i32),
    Money(i64),
    Date(f64),
    Blob(Vec<u8>),
}

impl Cell {
    fn bytes(&self) -> Vec<u8> {
        match self {
            Cell::Long(value) => value.to_le_bytes().to_vec(),
            Cell::Money(value) => value.to_le_bytes().to_vec(),
            Cell::Date(value) => value.to_le_bytes().to_vec(),
            Cell::Blob(value) => value.clone(),
        }
    }
}

pub struct Table {
    pub name: &'static str,
    pub columns: Vec<(&'static str, u32)>,
    pub rows: Vec<Vec<Cell>>,
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn check(code: i32, step: &str) {
    assert!(code >= 0, "{step}: ESE error {code}");
}

/// Create a database at `path` holding `tables`, detached cleanly.
pub fn build(path: &Path, tables: &[Table]) {
    create(path, tables, false);
}

/// Create a database at `path` holding `tables` and stop the engine without a clean
/// shutdown, as when a copy is taken while the service writes to the database.
pub fn build_dirty(path: &Path, tables: &[Table]) {
    create(path, tables, true);
}

fn create(path: &Path, tables: &[Table], dirty: bool) {
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let number = COUNTER.fetch_add(1, Ordering::SeqCst);
    let work = path.parent().unwrap().join(format!("ese-work-{}-{number}", std::process::id()));
    std::fs::create_dir_all(&work).unwrap();
    let work_text = wide(&format!("{}\\", work.display()));
    let file = wide(&path.to_string_lossy());
    let name = wide(&format!("zero-surplus-test-{}-{number}", std::process::id()));
    let off = wide("Off");
    let mut instance: JET_INSTANCE = 0;
    let mut session: JET_SESID = 0;
    let mut database = 0u32;
    // SAFETY: every pointer passed is valid for the duration of its call.
    unsafe {
        check(JetCreateInstanceW(&mut instance, name.as_ptr()), "create instance");
        let handle = &mut instance as *mut JET_INSTANCE;
        let on = wide("On");
        let recovery = if dirty { on.as_ptr() } else { off.as_ptr() };
        check(JetSetSystemParameterW(handle, 0, PARAM_RECOVERY, 0, recovery), "recovery");
        check(JetSetSystemParameterW(handle, 0, PARAM_NO_INFORMATION_EVENT, 1, null()), "events");
        check(JetSetSystemParameterW(handle, 0, PARAM_CREATE_PATH_IF_NOT_EXIST, 1, null()), "create path");
        for parameter in [PARAM_SYSTEM_PATH, PARAM_TEMP_PATH, PARAM_LOG_FILE_PATH] {
            check(JetSetSystemParameterW(handle, 0, parameter, 0, work_text.as_ptr()), "paths");
        }
        check(JetInit(handle), "init");
        check(JetBeginSessionW(instance, &mut session, null(), null()), "session");
        check(JetCreateDatabaseW(session, file.as_ptr(), null(), &mut database, 0), "create database");
        for table in tables {
            let table_name = wide(table.name);
            let mut id: JET_TABLEID = 0;
            check(JetCreateTableW(session, database, table_name.as_ptr(), 0, 0, &mut id), "create table");
            let mut column_ids = Vec::new();
            for (column, kind) in &table.columns {
                let column_name = wide(column);
                let mut definition: JET_COLUMNDEF = std::mem::zeroed();
                definition.cbStruct = size_of::<JET_COLUMNDEF>() as u32;
                definition.coltyp = *kind;
                let mut column_id = 0u32;
                check(
                    JetAddColumnW(session, id, column_name.as_ptr(), &definition, null(), 0, &mut column_id),
                    "add column",
                );
                column_ids.push(column_id);
            }
            check(JetBeginTransaction(session), "begin transaction");
            for row in &table.rows {
                check(JetPrepareUpdate(session, id, PREP_INSERT), "prepare insert");
                for (cell, column_id) in row.iter().zip(&column_ids) {
                    let bytes = cell.bytes();
                    check(
                        JetSetColumn(session, id, *column_id, bytes.as_ptr().cast::<c_void>(), bytes.len() as u32, 0, null()),
                        "set column",
                    );
                }
                check(JetUpdate(session, id, null_mut(), 0, null_mut()), "update");
            }
            check(JetCommitTransaction(session, 0), "commit");
            check(JetCloseTable(session, id), "close table");
        }
        if dirty {
            // Sessions end but the database stays attached, so the header is never marked clean.
            check(JetCloseDatabase(session, database, 0), "close database");
            check(JetEndSession(session, 0), "end session");
            // JET_errDirtyShutdown reports that the databases were left dirty as requested.
            let code = JetTerm2(instance, TERM_DIRTY);
            assert!(code >= 0 || code == DIRTY_SHUTDOWN, "dirty term: ESE error {code}");
            let _ = std::fs::remove_dir_all(&work);
            return;
        }
        check(JetCloseDatabase(session, database, 0), "close database");
        check(JetDetachDatabaseW(session, file.as_ptr()), "detach");
        check(JetEndSession(session, 0), "end session");
        check(JetTerm(instance), "term");
    }
    let _ = std::fs::remove_dir_all(&work);
}
