mod common;

use std::path::Path;

use common::ese::{self, CURRENCY, Cell, DATE_TIME, LONG, LONG_BINARY, Table};
use common::*;
use serde_json::{Value, json};

const NOV1_NOON_UTC: i64 = 1_793_534_400;
const NOV2_NOON_UTC: i64 = 1_793_620_800;
const DEVICE: &str = r"\Device\HarddiskVolume3";

fn ole(unix: i64) -> Cell {
    Cell::Date(unix as f64 / 86_400.0 + 25_569.0)
}

fn utf16(text: &str) -> Cell {
    Cell::Blob(text.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect())
}

/// A SRUM database where each app has one usage interval at the given time.
fn srum_with(path: &Path, apps: &[(&str, i64)]) {
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
    let ids = apps.iter().enumerate().map(|(index, (identifier, _))| vec![Cell::Long(0), Cell::Long(100 + index as i32), utf16(identifier)]);
    let rows = apps.iter().enumerate().map(|(index, (_, time))| {
        let mut row = vec![Cell::Long(100 + index as i32), Cell::Long(1), ole(*time)];
        row.extend([1, 0, 0, 0, 0, 0].map(Cell::Money));
        row
    });
    ese::build(
        path,
        &[
            Table {
                name: "SruDbIdMapTable",
                columns: vec![("IdType", LONG), ("IdIndex", LONG), ("IdBlob", LONG_BINARY)],
                rows: ids.collect(),
            },
            Table { name: "{D10CA2FE-6FCF-4F6D-848E-B2E99266FA89}", columns: app_columns, rows: rows.collect() },
            Table {
                name: "{973F5D5C-1D90-4944-BE8E-24B94231A174}",
                columns: vec![
                    ("AppId", LONG),
                    ("UserId", LONG),
                    ("TimeStamp", DATE_TIME),
                    ("BytesSent", CURRENCY),
                    ("BytesRecvd", CURRENCY),
                ],
                rows: Vec::new(),
            },
        ],
    );
}

/// The device path of a path on drive C: as SRUM stores it.
fn device(path: &Path) -> String {
    format!("{DEVICE}{}", &text(path)[2..])
}

fn row<'a>(rows: &'a Value, folder: &Path) -> &'a Value {
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row["folder"] == text(folder))
        .unwrap_or_else(|| panic!("{} not in {rows}", folder.display()))
}

#[test]
fn folders_take_the_last_use_of_their_strongest_link() {
    let dir = TempDir::new("activity");
    assert!(text(dir.path()).starts_with("C:"), "the fixture maps drive C:");
    let root = dir.join("root");
    for folder in ["Alpha", "Beta", "Kappa Data", "Delta", "Gamma", "Microsoft", "Code"] {
        write_file(&root.join(folder).join("data.bin"), 10);
    }
    let alpha_exe = root.join("Alpha").join("bin").join("alpha.exe");
    let apps = [
        (device(&alpha_exe), NOV2_NOON_UTC),
        (format!(r"{DEVICE}\Program Files\Some Vendor\beta.exe"), NOV1_NOON_UTC),
        (format!(r"{DEVICE}\Apps\KappaTool.exe"), NOV1_NOON_UTC),
        (format!(r"{DEVICE}\Vendors\Delta\run.exe"), NOV2_NOON_UTC),
        (format!(r"{DEVICE}\Program Files\Microsoft\Office\winword.exe"), NOV2_NOON_UTC),
        // `Code.exe` matches the folder exactly; the newer program only shares the word.
        (format!(r"{DEVICE}\Program Files\VS\Code.exe"), NOV1_NOON_UTC),
        (format!(r"{DEVICE}\Tools\codex-code-mode-host.exe"), NOV2_NOON_UTC),
    ];
    let apps: Vec<(&str, i64)> = apps.iter().map(|(identifier, time)| (identifier.as_str(), *time)).collect();

    let case = dir.join("case");
    let source = case.join("sources").join("src-20261102T000000Z-0001");
    std::fs::create_dir_all(source.join("srum")).unwrap();
    srum_with(&source.join("srum").join("SRUDB.dat"), &apps);
    let info = json!({
        "id": "src-20261102T000000Z-0001",
        "kind": "source",
        "collected_at": "2026-11-02T22:00:00+09:00",
        "srum": { "path": "srum/SRUDB.dat" },
        "drives": [{ "drive": "C:", "device": DEVICE, "serial": null }],
    });
    std::fs::write(source.join("source.json"), serde_json::to_vec(&info).unwrap()).unwrap();

    // Without a snapshot of the root there is nothing to list.
    let empty = dir.join("no-prefetch");
    std::fs::create_dir_all(&empty).unwrap();
    let args = ["activity", "--case", text(&case), "--root", text(&root), "--min-bytes", "0", "--prefetch-dir", text(&empty)];
    let missing = cli(&args);
    assert_eq!(missing.code, 3);
    assert_eq!(missing.json["error"]["kind"], "no_snapshot");

    assert_eq!(cli(&["scan", "--case", text(&case), "--root", text(&root)]).code, 0);
    let outcome = cli(&args);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let rows = &outcome.json["rows"];
    assert_eq!(outcome.json["coverage"]["srum"]["status"], "ok");

    let expect = |folder: &str, link: &str, program: &str, time: i64| {
        let found = row(rows, &root.join(folder));
        assert_eq!(found["evidence"]["link"], link, "{folder}: {found}");
        assert!(found["evidence"]["program"].as_str().unwrap().ends_with(program), "{folder}: {found}");
        let expected = chrono::DateTime::from_timestamp(time, 0).unwrap().with_timezone(&chrono::Local);
        assert_eq!(found["last_use"], expected.to_rfc3339_opts(chrono::SecondsFormat::Secs, false), "{folder}");
        assert_eq!(found["evidence"]["source"], "SRUM");
    };
    expect("Alpha", "inside", "alpha.exe", NOV2_NOON_UTC);
    expect("Beta", "exact", "beta.exe", NOV1_NOON_UTC);
    expect("Kappa Data", "program", "KappaTool.exe", NOV1_NOON_UTC);
    expect("Delta", "folder", "run.exe", NOV2_NOON_UTC);
    expect("Code", "exact", "Code.exe", NOV1_NOON_UTC);

    // No program can be told from these names.
    for folder in ["Gamma", "Microsoft"] {
        let found = row(rows, &root.join(folder));
        assert!(found["last_use"].is_null() && found["evidence"].is_null(), "{folder}: {found}");
    }
    assert_eq!(row(rows, &root.join("Microsoft"))["name_words"], json!([]));

    // The result is stored and listed.
    let listing = cli(&["list", "--case", text(&case), "--kind", "activity"]);
    assert_eq!(listing.json["items"][0]["id"], outcome.json["id"]);
}
