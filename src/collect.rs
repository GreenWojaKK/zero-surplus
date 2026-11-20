//! `collect`: copy the records that need administrator rights into the case folder.
//!
//! Without elevation the command starts itself again through a UAC prompt, waits for that
//! process and relays its result. The elevated process only copies files and writes
//! `source.json`; parsing happens later without elevation.

use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

use crate::case::{Case, Kind};
use crate::cli::CollectArgs;
use crate::output::{Fail, Res};
use crate::usage::{live_prefetch_dir, live_srum_db};
use crate::win;

pub fn run(args: CollectArgs) -> Res<Option<Value>> {
    let case = Case::create(&args.case)?;
    if !win::is_elevated() {
        if args.result_file.is_some() {
            return Err(Fail::run("not_elevated", "the elevated collection process did not get administrator rights"));
        }
        return relaunch_elevated(&case).map(Some);
    }
    let result = collect(&case);
    if let Some(path) = &args.result_file {
        // The non-elevated parent reads this file; the outcome is written even on failure.
        let value = match &result {
            Ok(value) => value.clone(),
            Err(fail) => fail.to_json(),
        };
        let _ = fs::write(path, serde_json::to_vec(&value).unwrap_or_default());
    }
    result.map(Some)
}

fn quote(path: &Path) -> String {
    format!("\"{}\"", path.to_string_lossy().trim_end_matches('\\'))
}

fn relaunch_elevated(case: &Case) -> Res<Value> {
    let exe = std::env::current_exe().map_err(|err| Fail::io("locate zero-surplus", &err))?;
    let result_file = case.root().join(format!(".collect-result-{}.json", std::process::id()));
    let _ = fs::remove_file(&result_file);
    let parameters = format!("collect --case {} --result-file {}", quote(case.root()), quote(&result_file));
    eprintln!("zero-surplus: waiting for the UAC prompt to be approved");
    let outcome = win::run_elevated(&exe, &parameters).map_err(|err| Fail::io("start elevated collection", &err))?;
    match outcome {
        win::Elevation::Declined => Err(Fail::blocked(
            "elevation_declined",
            "the UAC prompt was declined; nothing was collected",
            json!({ "needs": "administrator approval" }),
        )),
        win::Elevation::Exited(code) => {
            let bytes = fs::read(&result_file);
            let _ = fs::remove_file(&result_file);
            let value: Value = bytes
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                .ok_or_else(|| Fail::run("collect_failed", format!("elevated collection exited with code {code} without a result")))?;
            if let Some(error) = value.get("error") {
                return Err(Fail::run(
                    "collect_failed",
                    error["message"].as_str().unwrap_or("elevated collection failed").to_owned(),
                ));
            }
            Ok(value)
        }
    }
}

fn collect(case: &Case) -> Res<Value> {
    let id = case.new_id(Kind::Source);
    let dir = case.dir(Kind::Source).join(&id);
    fs::create_dir_all(&dir).map_err(|err| Fail::io(&format!("create {}", dir.display()), &err))?;

    let prefetch = copy_prefetch(&dir.join("prefetch"));
    let srum = copy_srum(&dir.join("srum"));
    let info = json!({
        "id": id,
        "kind": "source",
        "collected_at": win::now_rfc3339(),
        "prefetch": prefetch,
        "srum": srum,
        // Needed later to turn device and volume notations into drive letters.
        "drives": win::drive_mappings(),
    });
    let path = case.write_json(&id, &info)?;
    let mut result = json!({ "command": "collect" });
    if let (Value::Object(result), Value::Object(info)) = (&mut result, info) {
        result.extend(info);
    }
    result["stored"] = json!(case.relative(&path));
    Ok(result)
}

fn copy_prefetch(target: &Path) -> Value {
    let source = live_prefetch_dir();
    let entries = match fs::read_dir(&source) {
        Ok(entries) => entries,
        Err(err) => return json!({ "error": err.to_string(), "source": source.to_string_lossy() }),
    };
    if let Err(err) = fs::create_dir_all(target) {
        return json!({ "error": err.to_string() });
    }
    let (mut files, mut bytes, mut failed) = (0u64, 0u64, 0u64);
    for entry in entries.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().to_ascii_lowercase().ends_with(".pf") {
            continue;
        }
        match fs::copy(entry.path(), target.join(&name)) {
            Ok(copied) => {
                files += 1;
                bytes += copied;
            }
            Err(_) => failed += 1,
        }
    }
    json!({ "path": "prefetch", "files": files, "bytes": bytes, "failed": failed, "source": source.to_string_lossy() })
}

fn copy_srum(target_dir: &Path) -> Value {
    const SHARE_ALL: u32 = 7; // read | write | delete
    let source = live_srum_db();
    if let Err(err) = fs::create_dir_all(target_dir) {
        return json!({ "error": err.to_string() });
    }
    let target = target_dir.join("SRUDB.dat");
    // A copy taken while the service writes to the database is in the dirty-shutdown state and
    // cannot be read later. Each copy is checked with the reader and retried after a pause.
    const ATTEMPTS: u32 = 6;
    let mut outcome = (String::new(), None, Err(String::new()), 0u32);
    for attempt in 1..=ATTEMPTS {
        let _ = fs::remove_file(&target);
        let shared = fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_ALL)
            .open(&source)
            .and_then(|mut input| fs::File::create(&target).and_then(|mut output| std::io::copy(&mut input, &mut output)));
        let method = match shared {
            Ok(_) => "shared_read",
            Err(shared_error) => {
                // The service holds the file: let esentutl copy it from a volume shadow copy.
                let _ = fs::remove_file(&target);
                if let Err(message) = esentutl_copy(&source, &target) {
                    return json!({
                        "error": format!("shared read: {shared_error}; esentutl: {message}"),
                        "source": source.to_string_lossy(),
                    });
                }
                "esentutl_vss"
            }
        };
        let state = crate::usage::srum::database_info(&target).map(|(state, _)| state).ok();
        let readable = crate::usage::srum::probe(&target);
        let done = readable.is_ok();
        outcome = (method.to_owned(), state, readable, attempt);
        if done {
            break;
        }
        if attempt < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    }
    let (method, state, readable, attempts) = outcome;
    let mut result = json!({
        "path": "srum/SRUDB.dat",
        "bytes": fs::metadata(&target).map(|metadata| metadata.len()).unwrap_or(0),
        "method": method,
        "database_state": state,
        // Whether the reader can open this copy; lookups pass over copies that it cannot.
        "readable": readable.is_ok(),
        "attempts": attempts,
        "source": source.to_string_lossy(),
    });
    if let Err(reason) = readable {
        result["reason"] = json!(reason);
    }
    result
}

fn esentutl_copy(source: &Path, target: &Path) -> Result<(), String> {
    let esentutl = std::env::var_os("SystemRoot")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join(r"System32\esentutl.exe");
    let output = Command::new(esentutl)
        .arg("/y")
        .arg(source)
        .arg("/vss")
        .arg("/d")
        .arg(target)
        .output()
        .map_err(|err| err.to_string())?;
    if output.status.success() && target.is_file() {
        Ok(())
    } else {
        let text = String::from_utf8_lossy(&output.stdout);
        Err(text.lines().rev().find(|line| !line.trim().is_empty()).unwrap_or("esentutl failed").trim().to_owned())
    }
}
