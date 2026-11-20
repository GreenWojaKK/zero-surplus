mod common;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use common::*;
use serde_json::Value;

#[test]
fn watch_reports_delete_and_recreate_in_order() {
    let dir = TempDir::new("watch");
    let target = dir.join("cache");
    std::fs::create_dir_all(&target).unwrap();
    let case = dir.join("case");

    let mut child = Command::new(env!("CARGO_BIN_EXE_zero-surplus"))
        .args(["watch", "--path", text(&target), "--duration", "3s", "--case", text(&case)])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();

    // Nothing is changed before the watch says it is registered.
    let ready: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(ready["event"], "ready");
    assert_eq!(ready["target_exists"], true);
    assert!(ready["time"].is_string());

    std::fs::remove_dir(&target).unwrap();
    std::fs::create_dir(&target).unwrap();
    write_file(&dir.join("unrelated.txt"), 1);

    let events: Vec<Value> = lines.map(|line| serde_json::from_str(&line.unwrap()).unwrap()).collect();
    assert!(child.wait().unwrap().success());

    let kinds: Vec<&str> = events.iter().map(|event| event["event"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["deleted", "created", "end"]);
    for event in &events[..2] {
        assert_eq!(event["path"], text(&target));
        assert!(event["process"].is_null());
    }
    let end = &events[2];
    assert_eq!(end["gaps"], 0);
    assert_eq!(end["target_exists"], true);
    assert_eq!(end["reason"], "duration_elapsed");

    // The stored log has the same lines and is listed.
    let stored = std::fs::read_to_string(case.join(end["stored"].as_str().unwrap())).unwrap();
    assert_eq!(stored.lines().count(), 4);
    let listing = cli(&["list", "--case", text(&case), "--kind", "watch"]);
    assert_eq!(listing.json["items"][0]["target"], text(&target));
}

#[test]
fn watch_registers_on_the_parent_when_the_target_does_not_exist() {
    let dir = TempDir::new("watch-missing");
    let target = dir.join("later").join("deep");
    let mut child = Command::new(env!("CARGO_BIN_EXE_zero-surplus"))
        .args(["watch", "--path", text(&target), "--duration", "2s"])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(ready["target_exists"], false);
    assert_eq!(ready["watching"], text(dir.path()));

    write_file(&target.join("f.bin"), 3);
    let events: Vec<Value> = lines.map(|line| serde_json::from_str(&line.unwrap()).unwrap()).collect();
    child.wait().unwrap();
    let paths: Vec<&str> = events.iter().filter(|event| event["event"] == "created").map(|event| event["path"].as_str().unwrap()).collect();
    // `later` is outside the target; the target and the file below it are reported.
    assert_eq!(paths, [text(&target), text(&target.join("f.bin"))]);
}

fn scan_id(case: &std::path::Path, root: &std::path::Path) -> String {
    let outcome = cli(&["scan", "--case", text(case), "--root", text(root)]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    outcome.json["snapshots"][0]["id"].as_str().unwrap().to_owned()
}

#[test]
fn export_bundle_can_be_moved_and_carries_cited_evidence() {
    let dir = TempDir::new("export");
    let case = dir.join("case");
    write_file(&dir.join("data/sub/a.bin"), 10);
    let scan = scan_id(&case, &dir.join("data"));

    let note = dir.join("memo.txt");
    std::fs::write(&note, "캐시가 매일 커진다\r\n원문 그대로").unwrap();
    let diagnosis = dir.join("diagnosis.md");
    std::fs::write(&diagnosis, "# 진단\nsub 폴더가 원인").unwrap();

    let first = cli(&[
        "export",
        "--case",
        text(&case),
        "--out",
        text(&dir.join("out")),
        "--name",
        "first",
        "--note-file",
        text(&note),
        "--add-diagnosis",
        text(&diagnosis),
        "--cites",
        &scan,
    ]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let diagnosis_id = first.json["diagnosis"].as_str().unwrap().to_owned();
    let note_id = first.json["notes"][0].as_str().unwrap().to_owned();

    // The note is stored byte for byte.
    let stored_note = std::fs::read(case.join(format!("notes/{note_id}.txt"))).unwrap();
    assert_eq!(stored_note, std::fs::read(&note).unwrap());

    // A later export names only the diagnosis; the evidence it cites comes along.
    let second = cli(&[
        "export",
        "--case",
        text(&case),
        "--out",
        text(&dir.join("out")),
        "--name",
        "second",
        "--diagnosis",
        &diagnosis_id,
    ]);
    assert_eq!(second.code, 0, "{}", second.stderr);
    let included: Vec<&str> = second.json["included"].as_array().unwrap().iter().map(|id| id.as_str().unwrap()).collect();
    assert!(included.contains(&scan.as_str()));
    assert!(included.contains(&diagnosis_id.as_str()));

    // Move the bundle: every path in the manifest still opens, and none is absolute.
    let moved = dir.join("elsewhere/handed-over");
    std::fs::create_dir_all(moved.parent().unwrap()).unwrap();
    std::fs::rename(dir.join("out/second"), &moved).unwrap();
    let manifest: Value = serde_json::from_slice(&std::fs::read(moved.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["format"], 1);
    let items = manifest["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    for item in items {
        for key in ["path", "cites_path"] {
            if let Some(relative) = item[key].as_str() {
                assert!(!relative.contains(':') && !relative.starts_with('/'), "absolute path in manifest: {relative}");
                assert!(moved.join(relative).is_file(), "missing {relative}");
            }
        }
    }
    let entry = items.iter().find(|item| item["kind"] == "diagnosis").unwrap();
    assert_eq!(entry["cites"][0], scan.as_str());
}

#[test]
fn export_returns_missing_ids_and_keeps_existing_bundles() {
    let dir = TempDir::new("export-missing");
    let case = dir.join("case");
    write_file(&dir.join("data/a.bin"), 10);
    let scan = scan_id(&case, &dir.join("data"));
    let out = dir.join("out");

    let missing = cli(&[
        "export", "--case", text(&case), "--out", text(&out), "--name", "broken", "--include", &scan, "--include",
        "scan-20200101T000000Z-0000",
    ]);
    assert_eq!(missing.code, 3);
    assert_eq!(missing.json["error"]["detail"]["missing"][0], "scan-20200101T000000Z-0000");
    assert!(!out.join("broken").exists());

    let ok = cli(&["export", "--case", text(&case), "--out", text(&out), "--name", "kept", "--include", &scan]);
    assert_eq!(ok.code, 0, "{}", ok.stderr);
    let before = std::fs::read(out.join("kept/manifest.json")).unwrap();
    let again = cli(&["export", "--case", text(&case), "--out", text(&out), "--name", "kept", "--include", &scan]);
    assert_eq!(again.code, 3);
    assert_eq!(again.json["error"]["kind"], "bundle_exists");
    assert_eq!(std::fs::read(out.join("kept/manifest.json")).unwrap(), before);
}

#[test]
fn export_rejects_a_note_that_is_not_utf8() {
    let dir = TempDir::new("export-utf8");
    let note = dir.join("bad.txt");
    std::fs::write(&note, [0xff, 0xfe, 0x00, 0xd8]).unwrap();
    let outcome = cli(&["export", "--case", text(&dir.join("case")), "--out", text(&dir.join("out")), "--note-file", text(&note)]);
    assert_eq!(outcome.code, 2);
    assert!(!dir.join("out").exists() || std::fs::read_dir(dir.join("out")).unwrap().next().is_none());
}

fn diagnosis_with_citation(dir: &TempDir) -> (std::path::PathBuf, String) {
    let case = dir.join("case");
    write_file(&dir.join("data/a.bin"), 10);
    let scan = scan_id(&case, &dir.join("data"));
    let diagnosis = dir.join("diagnosis.md");
    std::fs::write(&diagnosis, "a.bin이 원인").unwrap();
    let created = cli(&[
        "export", "--case", text(&case), "--out", text(&dir.join("out")), "--name", "first", "--add-diagnosis",
        text(&diagnosis), "--cites", &scan,
    ]);
    assert_eq!(created.code, 0, "{}", created.stderr);
    (case, created.json["diagnosis"].as_str().unwrap().to_owned())
}

fn export_diagnosis(dir: &TempDir, case: &std::path::Path, id: &str) -> Outcome {
    cli(&["export", "--case", text(case), "--out", text(&dir.join("out")), "--name", "second", "--diagnosis", id])
}

fn out_entries(dir: &TempDir) -> Vec<String> {
    std::fs::read_dir(dir.join("out"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn export_stops_when_a_citation_list_cannot_be_parsed() {
    let dir = TempDir::new("export-damaged-cites");
    let (case, id) = diagnosis_with_citation(&dir);
    std::fs::write(case.join(format!("diagnoses/{id}.json")), "{\"cites\":").unwrap();

    let outcome = export_diagnosis(&dir, &case, &id);
    assert_eq!(outcome.code, 3);
    assert_eq!(outcome.json["error"]["kind"], "unreadable_citations");
    let unreadable = &outcome.json["error"]["detail"]["unreadable"][0];
    assert_eq!(unreadable["diagnosis"], id.as_str());
    assert!(unreadable["message"].as_str().unwrap().starts_with("parse"));
    // Nothing is published, not even a partial bundle.
    assert_eq!(out_entries(&dir), ["first"]);
}

#[test]
fn export_stops_when_a_citation_list_is_missing() {
    let dir = TempDir::new("export-missing-cites");
    let (case, id) = diagnosis_with_citation(&dir);
    std::fs::remove_file(case.join(format!("diagnoses/{id}.json"))).unwrap();

    let outcome = export_diagnosis(&dir, &case, &id);
    assert_eq!(outcome.code, 3);
    assert_eq!(outcome.json["error"]["kind"], "unreadable_citations");
    assert!(outcome.json["error"]["detail"]["unreadable"][0]["message"].as_str().unwrap().starts_with("read"));
    assert_eq!(out_entries(&dir), ["first"]);
}

struct Watch {
    child: std::process::Child,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
    id: String,
}

fn start_watch(target: &std::path::Path, case: &std::path::Path) -> Watch {
    let mut child = Command::new(env!("CARGO_BIN_EXE_zero-surplus"))
        .args(["watch", "--path", text(target), "--duration", "3s", "--case", text(case)])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready: Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
    assert_eq!(ready["event"], "ready");
    let id = ready["id"].as_str().unwrap().to_owned();
    Watch { child, lines, id }
}

impl Watch {
    /// Remaining output lines and the exit code.
    fn finish(mut self) -> (Vec<Value>, i32) {
        let lines = self.lines.map(|line| serde_json::from_str(&line.unwrap()).unwrap()).collect();
        (lines, self.child.wait().unwrap().code().unwrap_or(-1))
    }
}

fn assert_not_stored(lines: &[Value], code: i32, cause: &str) {
    assert_eq!(code, 1);
    let end = lines.iter().find(|line| line["event"] == "end").expect("end line");
    assert!(end["stored"].is_null());
    assert!(end["store_error"].as_str().unwrap().starts_with(cause), "{end}");
    assert_eq!(lines.last().unwrap()["error"]["kind"], "log_not_stored");
}

#[test]
fn watch_reports_a_log_that_could_not_be_written() {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx};

    let dir = TempDir::new("watch-write-fails");
    let target = dir.join("cache");
    std::fs::create_dir_all(&target).unwrap();
    let case = dir.join("case");
    let watch = start_watch(&target, &case);
    let log = case.join(format!("watches/{}.jsonl", watch.id));
    let tmp = case.join(format!("watches/{}.jsonl.tmp", watch.id));

    // Another handle locks the whole log, so the watch's next write fails.
    let holder = std::fs::OpenOptions::new().read(true).write(true).share_mode(7).open(&tmp).unwrap();
    let mut overlapped = unsafe { std::mem::zeroed() };
    // SAFETY: the handle stays open while locked; `overlapped` describes offset 0.
    let locked = unsafe {
        LockFileEx(holder.as_raw_handle(), LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY, 0, u32::MAX, u32::MAX, &mut overlapped)
    };
    assert_ne!(locked, 0, "lock the log");
    write_file(&target.join("new.bin"), 1);

    let (lines, code) = watch.finish();
    assert!(lines.iter().any(|line| line["event"] == "created"), "events still reach standard output");
    assert_not_stored(&lines, code, "write");
    drop(holder);
    assert!(!tmp.exists() && !log.exists());
}

#[test]
fn watch_reports_a_log_that_could_not_be_named() {
    let dir = TempDir::new("watch-rename-fails");
    let target = dir.join("cache");
    std::fs::create_dir_all(&target).unwrap();
    let case = dir.join("case");
    let watch = start_watch(&target, &case);
    // Something else already holds the final name.
    let log = case.join(format!("watches/{}.jsonl", watch.id));
    let tmp = case.join(format!("watches/{}.jsonl.tmp", watch.id));
    std::fs::create_dir_all(&log).unwrap();

    let (lines, code) = watch.finish();
    assert_not_stored(&lines, code, "rename");
    assert!(log.is_dir());
    assert!(!tmp.exists());
}
