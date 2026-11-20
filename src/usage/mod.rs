//! `usage`: Windows execution records for an app, and Explorer traces for a folder.
//!
//! Every source reports its own status, so a source that cannot be read never hides the others:
//! `ok`, `no_record`, `access_denied`, `source_absent`, `unsupported_format`.

pub mod prefetch;
pub mod shellbags;
pub mod srum;
pub mod userassist;

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::case::{Case, Kind};
use crate::cli::UsageArgs;
use crate::output::{Fail, Res};
use crate::win::{self, DriveMapping};

const COLLECT_HINT: &str = "run `zero-surplus collect --case <folder>` once and approve the UAC prompt to copy the records";

// ---------------------------------------------------------------------------------------------
// Query and path translation
// ---------------------------------------------------------------------------------------------

/// A program given by full path or by file name.
pub struct AppQuery {
    path_key: Option<String>,
    name: String,
    stem: String,
}

impl AppQuery {
    pub fn new(input: &str) -> Self {
        let is_path = input.contains(['\\', '/']);
        let name = input.rsplit(['\\', '/']).next().unwrap_or(input).to_lowercase();
        let stem = name.rsplit_once('.').map_or(name.clone(), |(stem, _)| stem.to_owned());
        Self {
            path_key: is_path.then(|| win::path_key(Path::new(input))),
            name,
            stem,
        }
    }

    pub fn kind(&self) -> &'static str {
        if self.path_key.is_some() { "path" } else { "name" }
    }

    /// `candidate` is a full path or a bare file name.
    pub fn matches(&self, candidate: &str) -> bool {
        let name = candidate.rsplit(['\\', '/']).next().unwrap_or(candidate);
        let path = candidate.contains(['\\', '/']).then_some(candidate);
        self.matches_parts(path, name)
    }

    pub fn matches_parts(&self, path: Option<&str>, name: &str) -> bool {
        match &self.path_key {
            Some(key) => path.is_some_and(|path| &win::path_key(Path::new(path)) == key),
            None => name.to_lowercase() == self.name,
        }
    }

    /// Shortcuts are named after the program, not its file: compare without the extension.
    pub fn matches_shortcut(&self, candidate: &str) -> bool {
        let name = candidate.rsplit(['\\', '/']).next().unwrap_or(candidate).to_lowercase();
        let stem = name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem);
        self.path_key.is_none() && stem == self.stem
    }
}

/// Translates the volume notations used inside the records into drive letters.
pub struct PathMap {
    drives: Vec<DriveMapping>,
}

impl PathMap {
    pub fn live() -> Self {
        Self { drives: win::drive_mappings() }
    }

    pub fn from_drives(drives: Vec<DriveMapping>) -> Self {
        Self { drives }
    }

    /// `\Device\HarddiskVolume3\Users\x` to `C:\Users\x`.
    pub fn from_device(&self, path: &str) -> Option<String> {
        let lower = path.to_lowercase();
        self.drives.iter().find_map(|drive| {
            let device = drive.device.to_lowercase();
            (lower.starts_with(&device) && lower.as_bytes().get(device.len()) == Some(&b'\\'))
                .then(|| format!("{}{}", drive.drive, &path[device.len()..]))
        })
    }

    /// `C:\Users\x` to `\Device\HarddiskVolume3\Users\x`.
    pub fn to_device(&self, path: &str) -> Option<String> {
        let drive = path.get(..2)?;
        let rest = path.get(2..)?;
        self.drives
            .iter()
            .find(|mapping| mapping.drive.eq_ignore_ascii_case(drive))
            .map(|mapping| format!("{}{rest}", mapping.device))
    }

    /// `\VOLUME{01d962d37536cd21-a2691d2c}\WINDOWS\X` to `C:\WINDOWS\X`; unknown volumes are kept.
    pub fn from_prefetch(&self, path: &str) -> String {
        let translated = path.strip_prefix("\\VOLUME{").and_then(|rest| {
            let (volume, tail) = rest.split_once('}')?;
            let serial = volume.rsplit('-').next()?;
            let serial = format!("{:08X}", u32::from_str_radix(serial, 16).ok()?);
            let drive = self.drives.iter().find(|drive| drive.serial.as_deref() == Some(serial.as_str()))?;
            Some(format!("{}{tail}", drive.drive))
        });
        translated.unwrap_or_else(|| path.to_owned())
    }
}

// ---------------------------------------------------------------------------------------------
// Source results
// ---------------------------------------------------------------------------------------------

/// `{ "status": ..., "origin": ..., <body fields> }`
pub fn source_result(status: &str, origin: impl Into<Value>, body: Value) -> Value {
    let mut result = Map::new();
    result.insert("status".to_owned(), json!(status));
    result.insert("origin".to_owned(), origin.into());
    if let Value::Object(fields) = body {
        result.extend(fields);
    }
    Value::Object(result)
}

pub fn status_of_io(err: &io::Error) -> &'static str {
    const ERROR_SHARING_VIOLATION: i32 = 32;
    match err.kind() {
        io::ErrorKind::PermissionDenied => "access_denied",
        _ if err.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => "access_denied",
        _ => "source_absent",
    }
}

fn system_root() -> PathBuf {
    std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
}

pub fn live_prefetch_dir() -> PathBuf {
    system_root().join("Prefetch")
}

pub fn live_srum_db() -> PathBuf {
    system_root().join(r"System32\sru\SRUDB.dat")
}

/// Collected sources in the case that contain `part` (`prefetch` or `srum`), newest first.
fn sources_with(case: &Case, part: &str) -> Vec<(String, PathBuf, Value)> {
    case.ids(Kind::Source)
        .into_iter()
        .filter_map(|id| {
            let info = case.read_json(&id).ok()?;
            let relative = info.get(part)?.get("path")?.as_str()?;
            let path = case.existing(&id)?.parent()?.join(relative);
            path.exists().then_some((id, path, info))
        })
        .collect()
}

fn latest_source(case: &Case, part: &str) -> Option<(String, PathBuf, Value)> {
    sources_with(case, part).into_iter().next()
}

fn copy_origin(id: &str, info: &Value) -> Value {
    json!({ "kind": "copy", "source": id, "collected_at": info["collected_at"] })
}

fn copy_map(info: &Value) -> PathMap {
    serde_json::from_value::<Vec<DriveMapping>>(info["drives"].clone())
        .map(PathMap::from_drives)
        .unwrap_or_else(|_| PathMap::live())
}

pub struct PrefetchSource {
    pub load: prefetch::PfLoad,
    pub origin: Value,
}

/// Explicit folder, then the running system, then the newest copy in the case.
pub fn open_prefetch(case: Option<&Case>, explicit: Option<&Path>) -> Result<PrefetchSource, Value> {
    if let Some(dir) = explicit {
        let origin = json!({ "kind": "explicit", "path": dir.to_string_lossy() });
        return match prefetch::load(dir, &PathMap::live()) {
            Ok(load) => Ok(PrefetchSource { load, origin }),
            Err(err) => Err(source_result(status_of_io(&err), origin, json!({ "message": err.to_string() }))),
        };
    }
    let live_error = match prefetch::load(&live_prefetch_dir(), &PathMap::live()) {
        Ok(load) => return Ok(PrefetchSource { load, origin: json!({ "kind": "live" }) }),
        Err(err) => err,
    };
    if let Some((id, dir, info)) = case.and_then(|case| latest_source(case, "prefetch"))
        && let Ok(load) = prefetch::load(&dir, &copy_map(&info))
    {
        return Ok(PrefetchSource { load, origin: copy_origin(&id, &info) });
    }
    Err(source_result(
        status_of_io(&live_error),
        json!({ "kind": "live" }),
        json!({ "message": live_error.to_string(), "hint": COLLECT_HINT }),
    ))
}

/// A SRUM database that was read, where it came from, and the newer copies passed over.
pub struct SrumRead {
    pub result: srum::SrumResult,
    pub origin: Value,
    /// Newer collected copies that could not be read, newest first, each with the reason.
    pub skipped: Vec<Value>,
}

fn skipped_copy(id: &str, info: &Value, path: &Path, reason: String) -> Value {
    json!({
        "source": id,
        "collected_at": info["collected_at"],
        "database_state": srum::database_info(path).ok().map(|(state, _)| state),
        "reason": reason,
    })
}

/// Read SRUM for the apps accepted by `wanted(path, name)`.
///
/// An explicit file is read as given. Otherwise the collected copies in the case are tried from
/// newest to oldest and the first one that can be read is used; the newer copies passed over
/// are returned with their reasons. Without any collected copy the running system is tried.
pub fn read_srum(
    case: Option<&Case>,
    explicit: Option<&Path>,
    wanted: &dyn Fn(Option<&str>, &str) -> bool,
) -> Result<SrumRead, Value> {
    if let Some(path) = explicit {
        let origin = json!({ "kind": "explicit", "path": path.to_string_lossy() });
        if let Err(err) = std::fs::metadata(path) {
            return Err(source_result(status_of_io(&err), origin, json!({ "message": err.to_string() })));
        }
        return match srum::query(path, &PathMap::live(), wanted) {
            Ok(result) => Ok(SrumRead { result, origin, skipped: Vec::new() }),
            Err(message) => Err(source_result("unsupported_format", origin, json!({ "message": message }))),
        };
    }

    let copies = case.map(|case| sources_with(case, "srum")).unwrap_or_default();
    if !copies.is_empty() {
        let mut skipped = Vec::new();
        for (id, path, info) in &copies {
            match srum::query(path, &copy_map(info), wanted) {
                Ok(result) => {
                    let mut origin = copy_origin(id, info);
                    origin["selection"] = json!("newest_readable_copy");
                    return Ok(SrumRead { result, origin, skipped });
                }
                Err(reason) => skipped.push(skipped_copy(id, info, path, reason)),
            }
        }
        return Err(source_result(
            "unsupported_format",
            json!({ "kind": "copy" }),
            json!({
                "message": "none of the collected SRUM copies could be read",
                "skipped_copies": skipped,
                "hint": COLLECT_HINT,
            }),
        ));
    }

    let live = live_srum_db();
    let origin = json!({ "kind": "live" });
    if let Err(err) = std::fs::File::open(&live) {
        return Err(source_result(status_of_io(&err), origin, json!({ "message": err.to_string(), "hint": COLLECT_HINT })));
    }
    match srum::query(&live, &PathMap::live(), wanted) {
        Ok(result) => Ok(SrumRead { result, origin, skipped: Vec::new() }),
        Err(message) => {
            Err(source_result("unsupported_format", origin, json!({ "message": message, "hint": COLLECT_HINT })))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------------------------

pub fn prefetch_matches<'a>(source: &'a PrefetchSource, query: &AppQuery) -> Vec<&'a prefetch::PfRecord> {
    source
        .load
        .records
        .iter()
        .filter(|record| {
            query.matches_parts(record.path.as_deref(), &record.executable)
                || record.path_candidates.iter().any(|candidate| query.matches_parts(Some(candidate), &record.executable))
        })
        .collect()
}

pub fn prefetch_record_json(record: &prefetch::PfRecord) -> Value {
    json!({
        "prefetch_file": record.file,
        "executable": record.executable,
        "path": record.path,
        "path_basis": record.path_basis,
        "path_candidates": record.path_candidates,
        "run_count": record.run_count,
        "last_run": record.last_runs.first().copied().and_then(win::filetime_rfc3339),
        "last_runs": record.last_runs.iter().filter_map(|&time| win::filetime_rfc3339(time)).collect::<Vec<_>>(),
        "format_version": record.version,
        "referenced_count": record.referenced.len(),
        "referenced": record.referenced,
    })
}

pub fn prefetch_lookup(source: &Result<PrefetchSource, Value>, query: &AppQuery) -> Value {
    let source = match source {
        Ok(source) => source,
        Err(result) => return result.clone(),
    };
    let records: Vec<Value> = prefetch_matches(source, query).into_iter().map(prefetch_record_json).collect();
    let status = if !records.is_empty() {
        "ok"
    } else if source.load.records.is_empty() && !source.load.unsupported.is_empty() {
        "unsupported_format"
    } else {
        "no_record"
    };
    source_result(
        status,
        source.origin.clone(),
        json!({
            "files_total": source.load.files_total,
            "files_unsupported": source.load.unsupported.len(),
            "unsupported_sample": source
                .load
                .unsupported
                .iter()
                .take(3)
                .map(|(file, reason)| json!({ "file": file, "reason": reason }))
                .collect::<Vec<_>>(),
            "records": records,
        }),
    )
}

pub fn srum_lookup(case: Option<&Case>, explicit: Option<&Path>, query: &AppQuery, max_days: usize) -> Value {
    let read = match read_srum(case, explicit, &|path, name| query.matches_parts(path, name)) {
        Ok(read) => read,
        Err(result) => return result,
    };
    let records: Vec<Value> = read.result.apps.iter().map(|app| app.to_json(max_days)).collect();
    source_result(
        if records.is_empty() { "no_record" } else { "ok" },
        read.origin,
        json!({
            "database_state": read.result.db_state,
            "skipped_copies": read.skipped,
            // Windows writes SRUM records about once an hour: use since the last write before
            // the copy was taken is not in the database yet.
            "recording_interval": "about one hour",
            "units": srum::units(),
            "records": records,
        }),
    )
}

/// Remove the long referenced-file lists from what is printed; the stored result keeps them.
fn strip_referenced(result: &mut Value) {
    if let Some(records) = result.pointer_mut("/sources/prefetch/records").and_then(Value::as_array_mut) {
        for record in records {
            if let Some(fields) = record.as_object_mut() {
                fields.remove("referenced");
            }
        }
    }
}

pub fn run(args: UsageArgs) -> Res<Option<Value>> {
    if args.app.is_none() && args.folder.is_none() {
        return Err(Fail::input("give --app <name or path> or --folder <path>"));
    }
    let case = args.case.as_deref().map(Case::create).transpose()?;
    let mut sources = Map::new();
    let mut target = Map::new();

    if let Some(app) = &args.app {
        let query = AppQuery::new(app);
        target.insert("app".to_owned(), json!(app));
        target.insert("matched_by".to_owned(), json!(query.kind()));
        sources.insert("userassist".to_owned(), userassist::lookup(&query));
        let prefetch = open_prefetch(case.as_ref(), args.prefetch_dir.as_deref());
        sources.insert("prefetch".to_owned(), prefetch_lookup(&prefetch, &query));
        sources.insert("srum".to_owned(), srum_lookup(case.as_ref(), args.srum_db.as_deref(), &query, args.top));
    }
    if let Some(folder) = &args.folder {
        let absolute = std::path::absolute(folder).map_err(|err| Fail::io("resolve folder", &err))?;
        target.insert("folder".to_owned(), json!(absolute.to_string_lossy()));
        sources.insert("shellbags".to_owned(), shellbags::lookup(&absolute, args.top));
    }

    let mut result = json!({
        "command": "usage",
        "created_at": win::now_rfc3339(),
        "target": target,
        "sources": sources,
    });
    if let Some(case) = &case {
        let id = case.new_id(Kind::Usage);
        result["id"] = json!(id);
        let path = case.write_json(&id, &result)?;
        result["stored"] = json!(case.relative(&path));
    }
    strip_referenced(&mut result);
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> PathMap {
        PathMap::from_drives(vec![DriveMapping {
            drive: "C:".to_owned(),
            device: r"\Device\HarddiskVolume3".to_owned(),
            serial: Some("A2691D2C".to_owned()),
        }])
    }

    #[test]
    fn device_paths_become_drive_paths() {
        assert_eq!(map().from_device(r"\Device\HarddiskVolume3\Users\me\a.exe").as_deref(), Some(r"C:\Users\me\a.exe"));
        // A longer volume number must not match by prefix.
        assert_eq!(map().from_device(r"\Device\HarddiskVolume30\a.exe"), None);
    }

    #[test]
    fn drive_paths_become_device_paths() {
        assert_eq!(map().to_device(r"c:\Windows\a.exe").as_deref(), Some(r"\Device\HarddiskVolume3\Windows\a.exe"));
        assert_eq!(map().to_device(r"D:\a.exe"), None);
    }

    #[test]
    fn prefetch_volume_paths_use_the_serial_number() {
        assert_eq!(map().from_prefetch(r"\VOLUME{01d962d37536cd21-a2691d2c}\WINDOWS\X.DLL"), r"C:\WINDOWS\X.DLL");
        let unknown = r"\VOLUME{01d962d37536cd21-00000001}\X.DLL";
        assert_eq!(map().from_prefetch(unknown), unknown);
    }

    #[test]
    fn name_queries_match_every_location_and_path_queries_only_one() {
        let by_name = AppQuery::new("Python.exe");
        assert!(by_name.matches(r"C:\envs\a\python.exe"));
        assert!(by_name.matches(r"D:\envs\b\PYTHON.EXE"));
        assert!(!by_name.matches(r"C:\envs\a\pythonw.exe"));

        let by_path = AppQuery::new(r"C:\envs\a\python.exe");
        assert!(by_path.matches(r"c:\ENVS\a\python.exe"));
        assert!(!by_path.matches(r"D:\envs\b\python.exe"));
        assert!(!by_path.matches_parts(None, "python.exe"));
    }

    #[test]
    fn shortcuts_match_by_name_without_extension() {
        assert!(AppQuery::new("viewer-app.exe").matches_shortcut(r"C:\Menu\Viewer-App.lnk"));
        assert!(!AppQuery::new("viewer-app.exe").matches_shortcut(r"C:\Menu\Other.lnk"));
    }
}
