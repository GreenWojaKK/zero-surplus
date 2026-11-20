//! `scan`: volume statistics and per-root snapshots.

pub mod snapshot;
mod walk;

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};
use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_LocalAppDataLow, FOLDERID_RoamingAppData};

use crate::case::{Case, Kind};
use crate::cli::ScanArgs;
use crate::output::{Fail, Res};
use crate::win;
use snapshot::{FORMAT, Failure, RootIdent, Row, Rules, Snapshot, Stats, TopFile, VolumeSnapshot};

pub fn run(args: ScanArgs) -> Res<Option<Value>> {
    let mut roots = args.root.clone();
    if args.appdata {
        for id in [&FOLDERID_LocalAppData, &FOLDERID_LocalAppDataLow, &FOLDERID_RoamingAppData] {
            roots.push(win::known_folder(id).ok_or_else(|| Fail::run("known_folder", "AppData folder not found"))?);
        }
    }
    if args.volume.is_empty() && roots.is_empty() {
        return Err(Fail::input("give --volume, --root or --appdata"));
    }
    let case = Case::create(&args.case)?;
    win::install_interrupt_flag();

    let mut volumes = Vec::new();
    for drive in &args.volume {
        volumes.push(scan_volume(&case, drive)?);
    }
    let mut snapshots = Vec::new();
    for root in &roots {
        snapshots.push(scan_root(&case, root, &args)?);
    }
    let mut result = json!({ "command": "scan" });
    if !volumes.is_empty() {
        result["volumes"] = Value::Array(volumes);
    }
    if !snapshots.is_empty() {
        result["snapshots"] = Value::Array(snapshots);
    }
    Ok(Some(result))
}

fn scan_volume(case: &Case, drive: &str) -> Res<Value> {
    let letter = drive.trim_end_matches(['\\', '/']);
    if letter.len() != 2 || !letter.ends_with(':') {
        return Err(Fail::input(format!("--volume takes a drive such as C:, got {drive}")));
    }
    let mount = PathBuf::from(format!("{letter}\\"));
    let space = win::volume_space(&mount).map_err(|err| Fail::io(&format!("volume statistics of {letter}"), &err))?;
    let volume = win::volume_ident(&mount).map_err(|err| Fail::io(&format!("volume identity of {letter}"), &err))?;
    let id = case.new_id(Kind::Volume);
    let snapshot = VolumeSnapshot {
        format: FORMAT,
        id: id.clone(),
        kind: "volume".to_owned(),
        volume,
        measured_at: win::now_rfc3339(),
        space,
        used_bytes: space.total_bytes.saturating_sub(space.free_bytes),
    };
    let path = case.write_json(&id, &snapshot)?;
    let mut value = serde_json::to_value(&snapshot).map_err(|err| Fail::run("serialize", err.to_string()))?;
    value["stored"] = json!(case.relative(&path));
    Ok(value)
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned()
}

fn scan_root(case: &Case, input: &Path, args: &ScanArgs) -> Res<Value> {
    let absolute = std::path::absolute(input).map_err(|err| Fail::io(&format!("resolve {}", input.display()), &err))?;
    let is_link =
        win::is_link(&absolute).map_err(|err| Fail::io(&format!("inspect root {}", absolute.display()), &err))?;
    if is_link {
        return Err(Fail::blocked(
            "root_is_link",
            format!("{} is a junction or symbolic link; links are not followed", absolute.display()),
            json!({ "root": absolute.to_string_lossy() }),
        ));
    }
    if !absolute.is_dir() {
        return Err(Fail::input(format!("root is not a directory: {}", absolute.display())));
    }
    let final_path = win::final_path(&absolute).map_err(|err| Fail::io("resolve final root path", &err))?;
    let volume = win::volume_ident(&final_path).map_err(|err| Fail::io("volume identity of root", &err))?;

    // The case folder is skipped only when it lies below this root.
    let case_final = win::final_path(case.root()).unwrap_or_else(|_| case.root().to_path_buf());
    let exclude =
        (win::path_key(&case_final) != win::path_key(&final_path) && is_below(&case_final, &final_path)).then_some(case_final);

    let threads = args.threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, usize::from));
    let started_at = win::now_rfc3339();
    let free_start = win::volume_space(&final_path).map(|space| space.free_bytes).unwrap_or(0);
    let clock = Instant::now();
    eprintln!("zero-surplus: scanning {}", final_path.display());

    let output = match walk::walk(&final_path, exclude.as_deref(), threads, args.top_files, &win::interrupted) {
        Ok(output) => output,
        Err(walk::WalkError::Cancelled) => {
            return Err(Fail::run("cancelled", "measurement was cancelled; nothing was stored"));
        }
        Err(walk::WalkError::Open(failure)) => {
            return Err(Fail::run(
                "root_unreadable",
                format!("cannot list {}: {}", final_path.display(), failure.message),
            ));
        }
    };
    if win::interrupted() {
        return Err(Fail::run("cancelled", "measurement was cancelled; nothing was stored"));
    }

    let elapsed_ms = clock.elapsed().as_millis() as u64;
    let free_end = win::volume_space(&final_path).map(|space| space.free_bytes).unwrap_or(0);

    let mut rows = Vec::new();
    let mut failures = Vec::new();
    let mut stats = Stats {
        files: output.root_files.files,
        dirs: output.dirs.len() as u64,
        links: output.links.len() as u64,
        elapsed_ms,
        peak_working_set_bytes: 0,
        threads,
    };
    for dir in output.dirs {
        stats.files += dir.tally.files;
        stats.dirs += dir.tally.dirs;
        stats.links += dir.tally.links;
        let complete = dir.tally.failures.is_empty() && dir.tally.finished;
        for raw in &dir.tally.failures {
            failures.push(Failure {
                row: dir.name.clone(),
                path: relative(&final_path, &raw.path),
                code: raw.code,
                message: raw.message.clone(),
            });
        }
        rows.push(Row {
            kind: "dir".to_owned(),
            name: dir.name,
            bytes: dir.tally.bytes,
            allocated_bytes: dir.tally.allocated,
            files: dir.tally.files,
            dirs: dir.tally.dirs,
            failures: dir.tally.failures.len() as u64,
            status: if complete { "complete" } else { "partial" }.to_owned(),
            top: None,
        });
    }
    rows.sort_by(|left, right| right.bytes.cmp(&left.bytes).then_with(|| left.name.cmp(&right.name)));

    for raw in output.root_failures.iter().chain(&output.root_files.failures) {
        failures.push(Failure {
            row: String::new(),
            path: relative(&final_path, &raw.path),
            code: raw.code,
            message: raw.message.clone(),
        });
    }
    let root_files_complete = output.root_files.failures.is_empty() && output.names_complete;
    rows.push(Row {
        kind: "root_files".to_owned(),
        name: String::new(),
        bytes: output.root_files.bytes,
        allocated_bytes: output.root_files.allocated,
        files: output.root_files.files,
        dirs: 0,
        failures: output.root_files.failures.len() as u64,
        status: if root_files_complete { "complete" } else { "partial" }.to_owned(),
        top: Some(output.top_files.into_iter().map(|(name, bytes)| TopFile { name, bytes }).collect()),
    });
    let mut links = output.links;
    links.sort();
    for name in links {
        rows.push(Row {
            kind: "link".to_owned(),
            name,
            bytes: 0,
            allocated_bytes: 0,
            files: 0,
            dirs: 0,
            failures: 0,
            status: "not_followed".to_owned(),
            top: None,
        });
    }
    stats.peak_working_set_bytes = win::peak_working_set();

    let id = case.new_id(Kind::Scan);
    let mut excluded: Vec<String> = output.excluded.iter().map(|path| relative(&final_path, path)).collect();
    excluded.sort();
    let snapshot = Snapshot {
        format: FORMAT,
        id: id.clone(),
        kind: "root".to_owned(),
        rules: Rules::current(),
        volume,
        root: RootIdent {
            input: input.to_string_lossy().into_owned(),
            absolute: absolute.to_string_lossy().into_owned(),
            final_path: final_path.to_string_lossy().into_owned(),
        },
        started_at,
        ended_at: win::now_rfc3339(),
        free_bytes_start: free_start,
        free_bytes_end: free_end,
        excluded,
        names_complete: output.names_complete,
        rows,
        failures,
        stats,
    };
    let path = case.write_json(&id, &snapshot)?;
    Ok(summary(&snapshot, &case.relative(&path), args.top))
}

fn is_below(path: &Path, ancestor: &Path) -> bool {
    let (path, ancestor) = (win::path_key(path), win::path_key(ancestor));
    path.len() > ancestor.len()
        && path.starts_with(&ancestor)
        && (ancestor.ends_with('\\') || path.as_bytes()[ancestor.len()] == b'\\')
}

/// What `scan` prints: every identifying field, the largest rows, and counts for the rest.
fn summary(snapshot: &Snapshot, stored: &str, top: usize) -> Value {
    let dirs: Vec<&Row> = snapshot.rows.iter().filter(|row| row.kind == "dir").collect();
    let shown: Vec<&Row> = dirs.iter().take(top).copied().collect();
    let rest = &dirs[shown.len()..];
    let root_files = snapshot.rows.iter().find(|row| row.kind == "root_files");
    let links: Vec<&str> = snapshot.rows.iter().filter(|row| row.kind == "link").map(|row| row.name.as_str()).collect();
    json!({
        "id": snapshot.id,
        "stored": stored,
        "root": snapshot.root.final_path,
        "volume": snapshot.volume,
        "rules": snapshot.rules,
        "started_at": snapshot.started_at,
        "ended_at": snapshot.ended_at,
        "free_bytes_start": snapshot.free_bytes_start,
        "free_bytes_end": snapshot.free_bytes_end,
        "names_complete": snapshot.names_complete,
        "excluded": snapshot.excluded,
        "dir_rows_total": dirs.len(),
        "dir_rows": shown,
        "dir_rows_not_shown": {
            "rows": rest.len(),
            "bytes": rest.iter().map(|row| row.bytes).sum::<u64>(),
            "partial": rest.iter().filter(|row| row.status != "complete").count(),
        },
        "root_files": root_files,
        "links_not_followed": links,
        "failures_total": snapshot.failures.len(),
        "failures_sample": snapshot.failures.iter().take(10).collect::<Vec<_>>(),
        "stats": snapshot.stats,
    })
}
