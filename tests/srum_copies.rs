mod common;

use std::path::{Path, PathBuf};

use common::ese::{self, CURRENCY, Cell, DATE_TIME, LONG, LONG_BINARY, Table};
use common::*;
use serde_json::{Value, json};

const NOV1_NOON_UTC: i64 = 1_793_534_400;
const NOV2_NOON_UTC: i64 = 1_793_620_800;
const PYTHON: &str = r"\Device\HarddiskVolume3\envs\a\python.exe";

fn ole(unix: i64) -> Cell {
    Cell::Date(unix as f64 / 86_400.0 + 25_569.0)
}

fn utf16(text: &str) -> Cell {
    Cell::Blob(text.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect())
}

/// S-1-5-21-1-2-3-1001
fn sid() -> Cell {
    let mut bytes = vec![1, 5, 0, 0, 0, 0, 0, 5];
    for part in [21u32, 1, 2, 3, 1001] {
        bytes.extend_from_slice(&part.to_le_bytes());
    }
    Cell::Blob(bytes)
}

fn usage_row(app: i32, time: i64, values: [i64; 6]) -> Vec<Cell> {
    let mut row = vec![Cell::Long(app), Cell::Long(20), ole(time)];
    row.extend(values.map(Cell::Money));
    row
}

/// Two usage intervals and one network interval for python.exe, plus one interval of another app.
fn srum_tables() -> Vec<Table> {
    let counters = [
        "ForegroundCycleTime",
        "BackgroundCycleTime",
        "ForegroundBytesRead",
        "ForegroundBytesWritten",
        "BackgroundBytesRead",
        "BackgroundBytesWritten",
    ];
    let mut app_columns = vec![("AppId", LONG), ("UserId", LONG), ("TimeStamp", DATE_TIME)];
    app_columns.extend(counters.map(|name| (name, CURRENCY)));
    vec![
            Table {
                name: "SruDbIdMapTable",
                columns: vec![("IdType", LONG), ("IdIndex", LONG), ("IdBlob", LONG_BINARY)],
                rows: vec![
                    vec![Cell::Long(0), Cell::Long(10), utf16(PYTHON)],
                    vec![Cell::Long(0), Cell::Long(11), utf16(r"\Device\HarddiskVolume3\tools\other.exe")],
                    vec![Cell::Long(3), Cell::Long(20), sid()],
                ],
            },
            Table {
                name: "{D10CA2FE-6FCF-4F6D-848E-B2E99266FA89}",
                columns: app_columns,
                rows: vec![
                    usage_row(10, NOV1_NOON_UTC, [1000, 10, 500, 50, 5, 1]),
                    usage_row(10, NOV2_NOON_UTC, [2000, 20, 700, 70, 7, 2]),
                    usage_row(11, NOV2_NOON_UTC, [9, 9, 9, 9, 9, 9]),
                ],
            },
            Table {
                name: "{973F5D5C-1D90-4944-BE8E-24B94231A174}",
                columns: vec![
                    ("AppId", LONG),
                    ("UserId", LONG),
                    ("TimeStamp", DATE_TIME),
                    ("BytesSent", CURRENCY),
                    ("BytesRecvd", CURRENCY),
                ],
                rows: vec![vec![Cell::Long(10), Cell::Long(20), ole(NOV2_NOON_UTC + 600), Cell::Money(300), Cell::Money(700)]],
            },
    ]
}

fn readable_srum(path: &Path) {
    ese::build(path, &srum_tables());
}

/// The same content left in the dirty-shutdown state.
fn dirty_srum(path: &Path) {
    ese::build_dirty(path, &srum_tables());
}

/// A database the engine opens but without the SRUM tables.
fn database_without_tables(path: &Path) {
    ese::build(path, &[]);
}

fn not_a_database(path: &Path) {
    std::fs::write(path, b"not an ESE database").unwrap();
}

/// Write a collected source the way `collect` lays it out.
fn add_source(case: &Path, id: &str, collected_at: &str, write: fn(&Path)) -> PathBuf {
    let dir = case.join("sources").join(id);
    std::fs::create_dir_all(dir.join("srum")).unwrap();
    let database = dir.join("srum").join("SRUDB.dat");
    write(&database);
    let info = json!({
        "id": id,
        "kind": "source",
        "collected_at": collected_at,
        "srum": { "path": "srum/SRUDB.dat" },
        "drives": [{ "drive": "C:", "device": r"\Device\HarddiskVolume3", "serial": null }],
    });
    std::fs::write(dir.join("source.json"), serde_json::to_vec(&info).unwrap()).unwrap();
    database
}

fn srum_result(dir: &TempDir, extra: &[&str]) -> Value {
    let empty = dir.join("no-prefetch");
    std::fs::create_dir_all(&empty).unwrap();
    let case = dir.join("case");
    let mut args = vec!["usage", "--case", text(&case), "--app", "python.exe", "--prefetch-dir", text(&empty)];
    args.extend_from_slice(extra);
    let outcome = cli(&args);
    assert_eq!(outcome.code, 0, "usage failed: {}", outcome.stderr);
    outcome.json["sources"]["srum"].clone()
}

#[test]
fn a_newer_unreadable_copy_is_passed_over_for_an_older_readable_one() {
    let dir = TempDir::new("srum-fallback");
    let case = dir.join("case");
    add_source(&case, "src-20261101T000000Z-0001", "2026-11-01T09:00:00+09:00", readable_srum);
    add_source(&case, "src-20261102T000000Z-0002", "2026-11-02T09:00:00+09:00", dirty_srum);

    let srum = srum_result(&dir, &[]);
    assert_eq!(srum["status"], "ok");
    assert_eq!(srum["origin"]["kind"], "copy");
    assert_eq!(srum["origin"]["source"], "src-20261101T000000Z-0001");
    assert_eq!(srum["origin"]["collected_at"], "2026-11-01T09:00:00+09:00");
    assert_eq!(srum["origin"]["selection"], "newest_readable_copy");

    let skipped = srum["skipped_copies"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["source"], "src-20261102T000000Z-0002");
    assert_eq!(skipped[0]["collected_at"], "2026-11-02T09:00:00+09:00");
    assert_eq!(skipped[0]["database_state"], "DirtyShutdown");
    assert!(skipped[0]["reason"].as_str().unwrap().contains("-550"));

    // The selected copy is read end to end; the other app is not included.
    let records = srum["records"].as_array().unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["path"], r"C:\envs\a\python.exe");
    assert_eq!(record["identifiers"], json!([PYTHON]));
    assert_eq!(record["users"], json!(["S-1-5-21-1-2-3-1001"]));
    assert_eq!(record["active_days"], 2);
    assert!(record["observed_from"].as_str().unwrap().starts_with("2026-11-01"));
    let totals = &record["totals"];
    assert_eq!(totals["records"], 2);
    assert_eq!(totals["foreground_cycles"], 3000);
    assert_eq!(totals["background_cycles"], 30);
    assert_eq!(totals["foreground_bytes_read"], 1200);
    assert_eq!(totals["foreground_bytes_written"], 120);
    assert_eq!(totals["network_records"], 1);
    assert_eq!(totals["bytes_sent"], 300);
    assert_eq!(totals["bytes_received"], 700);
}

#[test]
fn the_newest_copy_is_used_when_it_can_be_read() {
    let dir = TempDir::new("srum-newest");
    let case = dir.join("case");
    add_source(&case, "src-20261101T000000Z-0001", "2026-11-01T09:00:00+09:00", database_without_tables);
    add_source(&case, "src-20261102T000000Z-0002", "2026-11-02T09:00:00+09:00", readable_srum);

    let srum = srum_result(&dir, &[]);
    assert_eq!(srum["status"], "ok");
    assert_eq!(srum["origin"]["source"], "src-20261102T000000Z-0002");
    assert_eq!(srum["skipped_copies"], json!([]));
}

#[test]
fn every_copy_that_cannot_be_read_is_reported() {
    let dir = TempDir::new("srum-none");
    let case = dir.join("case");
    add_source(&case, "src-20261101T000000Z-0001", "2026-11-01T09:00:00+09:00", database_without_tables);
    add_source(&case, "src-20261102T000000Z-0002", "2026-11-02T09:00:00+09:00", not_a_database);

    let srum = srum_result(&dir, &[]);
    assert_eq!(srum["status"], "unsupported_format");
    assert_eq!(srum["origin"]["kind"], "copy");
    assert!(srum["hint"].as_str().unwrap().contains("collect"));
    let skipped = srum["skipped_copies"].as_array().unwrap();
    let order: Vec<&str> = skipped.iter().map(|copy| copy["source"].as_str().unwrap()).collect();
    assert_eq!(order, ["src-20261102T000000Z-0002", "src-20261101T000000Z-0001"]);
    assert!(skipped[0]["database_state"].is_null());
    assert!(skipped[0]["reason"].as_str().unwrap().contains("read database header"));
    assert!(skipped[1]["reason"].as_str().unwrap().contains("SruDbIdMapTable"));
}

#[test]
fn an_explicit_database_is_read_as_given() {
    let dir = TempDir::new("srum-explicit");
    let database = dir.join("SRUDB.dat");
    readable_srum(&database);
    // A collected copy exists but the explicit file takes precedence.
    add_source(&dir.join("case"), "src-20261102T000000Z-0002", "2026-11-02T09:00:00+09:00", not_a_database);

    let srum = srum_result(&dir, &["--srum-db", text(&database)]);
    assert_eq!(srum["status"], "ok");
    assert_eq!(srum["origin"]["kind"], "explicit");
    assert_eq!(srum["skipped_copies"], json!([]));
    assert_eq!(srum["records"][0]["totals"]["records"], 2);
    assert_eq!(srum["records"][0]["identifiers"], json!([PYTHON]));
}
