mod common;

use common::*;

/// Fixture: known-size files, a junction, two hard links and a folder that cannot be listed.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        let root = dir.join("root");
        write_file(&root.join("alpha/a1.bin"), 1000);
        write_file(&root.join("alpha/sub/a2.bin"), 2000);
        write_file(&root.join("beta/b1.bin"), 500);
        write_file(&root.join("top-small.bin"), 10);
        write_file(&root.join("top-large.bin"), 7000);
        // Two paths, one file: counted at both paths.
        write_file(&root.join("links/original.bin"), 300);
        std::fs::hard_link(root.join("links/original.bin"), root.join("links/second.bin")).unwrap();
        // A junction to a folder outside the root: must not be followed.
        write_file(&dir.join("outside/big.bin"), 50_000);
        junction(&root.join("jump"), &dir.join("outside"));
        // A folder with a readable file and an unreadable subfolder.
        write_file(&root.join("guarded/open.bin"), 40);
        write_file(&root.join("guarded/locked/secret.bin"), 900);
        deny_listing(&root.join("guarded/locked"));
        Self { dir }
    }

    fn root(&self) -> std::path::PathBuf {
        self.dir.join("root")
    }

    fn case(&self) -> std::path::PathBuf {
        self.dir.join("case")
    }

    fn scan(&self) -> serde_json::Value {
        let outcome = cli(&["scan", "--case", text(&self.case()), "--root", text(&self.root())]);
        assert_eq!(outcome.code, 0, "scan failed: {}", outcome.stderr);
        outcome.json["snapshots"][0].clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        allow_listing(&self.root().join("guarded/locked"));
    }
}

#[test]
fn scan_reports_sizes_links_and_partial_rows() {
    let fixture = Fixture::new("scan");
    let snapshot = fixture.scan();
    let rows = &snapshot["dir_rows"];

    assert_eq!(row(rows, "alpha")["bytes"], 3000);
    assert_eq!(row(rows, "alpha")["files"], 2);
    assert_eq!(row(rows, "alpha")["status"], "complete");
    assert_eq!(row(rows, "beta")["bytes"], 500);

    // Hard links: each path adds the file length.
    assert_eq!(row(rows, "links")["bytes"], 600);

    // The junction is listed and not entered.
    assert_eq!(snapshot["links_not_followed"], serde_json::json!(["jump"]));
    assert!(rows.as_array().unwrap().iter().all(|row| row["name"] != "jump"));

    // The unreadable subfolder makes its row partial; the readable bytes are kept.
    let guarded = row(rows, "guarded");
    assert_eq!(guarded["status"], "partial");
    assert_eq!(guarded["bytes"], 40);
    assert_eq!(guarded["failures"], 1);
    let failure = &snapshot["failures_sample"][0];
    assert_eq!(failure["row"], "guarded");
    assert_eq!(failure["path"], "guarded\\locked");
    assert_eq!(failure["code"], 5);

    // Files directly in the root are one separate row with the largest files listed.
    let root_files = &snapshot["root_files"];
    assert_eq!(root_files["bytes"], 7010);
    assert_eq!(root_files["top"][0]["name"], "top-large.bin");
    assert_eq!(snapshot["names_complete"], true);

    // Rows are ordered by size.
    assert_eq!(rows[0]["name"], "alpha");

    // The stored file holds every row, including the link row.
    let stored = stored_json(&fixture.case(), snapshot["stored"].as_str().unwrap());
    let stored_rows = stored["rows"].as_array().unwrap();
    assert!(stored_rows.iter().any(|row| row["kind"] == "link" && row["name"] == "jump"));
    assert_eq!(stored["rules"]["size"], "logical");
}

#[test]
fn diff_follows_the_comparison_table_without_reopening_the_root() {
    let fixture = Fixture::new("diff");
    let before = fixture.scan();

    write_file(&fixture.root().join("alpha/a3.bin"), 4096);
    write_file(&fixture.root().join("gamma/g1.bin"), 123);
    std::fs::remove_dir_all(fixture.root().join("beta")).unwrap();
    write_file(&fixture.root().join("guarded/open2.bin"), 5);
    let after = fixture.scan();

    // The measured folder is gone before diff runs: diff may only read the two stored files.
    allow_listing(&fixture.root().join("guarded/locked"));
    std::fs::remove_dir_all(fixture.root()).unwrap();

    let outcome = cli(&[
        "diff",
        "--case",
        text(&fixture.case()),
        before["id"].as_str().unwrap(),
        after["id"].as_str().unwrap(),
    ]);
    assert_eq!(outcome.code, 0, "diff failed: {}", outcome.stderr);
    let rows = &outcome.json["rows"];

    // Both rows complete: later minus earlier.
    assert_eq!(row(rows, "alpha")["state"], "changed");
    assert_eq!(row(rows, "alpha")["delta_bytes"], 4096);

    // Present on one side only, with complete listings on both sides.
    assert_eq!(row(rows, "gamma")["state"], "appeared");
    assert_eq!(row(rows, "gamma")["delta_bytes"], 123);
    assert_eq!(row(rows, "beta")["state"], "disappeared");
    assert_eq!(row(rows, "beta")["delta_bytes"], -500);

    // Partly unread folder: observed bytes kept, no delta.
    let guarded = row(rows, "guarded");
    assert_eq!(guarded["state"], "unknown");
    assert!(guarded["delta_bytes"].is_null());
    assert_eq!(guarded["before"]["bytes"], 40);
    assert_eq!(guarded["after"]["bytes"], 45);

    // Unchanged rows are counted, not printed.
    assert!(rows.as_array().unwrap().iter().all(|row| row["name"] != "links"));
    assert!(outcome.json["rows_unchanged"].as_u64().unwrap() >= 2);

    assert_eq!(outcome.json["free_space_change"]["label"], "free space change");
    assert!(outcome.json["free_space_change"]["before_at"].is_string());
}

#[test]
fn diff_refuses_snapshots_measured_under_different_conditions() {
    let fixture = Fixture::new("mismatch");
    let first = fixture.scan();
    let other = cli(&["scan", "--case", text(&fixture.case()), "--root", text(&fixture.root().join("alpha"))]);
    let second = &other.json["snapshots"][0];

    let outcome = cli(&[
        "diff",
        "--case",
        text(&fixture.case()),
        first["id"].as_str().unwrap(),
        second["id"].as_str().unwrap(),
    ]);
    assert_eq!(outcome.code, 3);
    assert_eq!(outcome.json["error"]["kind"], "not_comparable");
    assert_eq!(outcome.json["error"]["detail"]["mismatches"][0]["field"], "root");
}

#[test]
fn cancelled_measurement_is_not_listed_or_comparable() {
    let fixture = Fixture::new("cancel");
    let kept = fixture.scan();
    let aborted = cli_env(
        &["scan", "--case", text(&fixture.case()), "--root", text(&fixture.root())],
        &[("ZERO_SURPLUS_TEST_ABORT_BEFORE_COMMIT", "1")],
    );
    assert_ne!(aborted.code, 0);

    let listing = cli(&["list", "--case", text(&fixture.case()), "--kind", "scan"]);
    let items = listing.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], kept["id"]);

    // The unfinished file exists only under its temporary name.
    let leftovers: Vec<_> = std::fs::read_dir(fixture.case().join("scans"))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert_eq!(leftovers.len(), 1);
    let unfinished_id = leftovers[0].trim_end_matches(".json.tmp").to_owned();
    let outcome = cli(&["diff", "--case", text(&fixture.case()), kept["id"].as_str().unwrap(), &unfinished_id]);
    assert_eq!(outcome.code, 2);
}

#[test]
fn case_folder_below_the_root_is_excluded_and_recorded() {
    let dir = TempDir::new("exclude");
    let root = dir.join("root");
    write_file(&root.join("data/d.bin"), 100);
    let case = root.join("investigation/case");
    write_file(&case.join("notes/old.txt"), 9999);

    let outcome = cli(&["scan", "--case", text(&case), "--root", text(&root)]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    let snapshot = &outcome.json["snapshots"][0];
    assert_eq!(snapshot["excluded"], serde_json::json!(["investigation\\case"]));
    assert_eq!(row(&snapshot["dir_rows"], "investigation")["bytes"], 0);
    assert_eq!(row(&snapshot["dir_rows"], "data")["bytes"], 100);
}

#[test]
fn root_that_is_a_link_is_not_followed() {
    let dir = TempDir::new("linkroot");
    write_file(&dir.join("real/file.bin"), 10);
    junction(&dir.join("alias"), &dir.join("real"));
    let outcome = cli(&["scan", "--case", text(&dir.join("case")), "--root", text(&dir.join("alias"))]);
    assert_eq!(outcome.code, 3);
    assert_eq!(outcome.json["error"]["kind"], "root_is_link");
}

#[test]
fn volume_scan_and_diff_report_free_space_change() {
    let dir = TempDir::new("volume");
    let case = dir.join("case");
    let drive = &text(dir.path())[..2];
    let first = cli(&["scan", "--case", text(&case), "--volume", drive]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let volume = &first.json["volumes"][0];
    let total = volume["total_bytes"].as_u64().unwrap();
    let free = volume["free_bytes"].as_u64().unwrap();
    assert_eq!(volume["used_bytes"].as_u64().unwrap(), total - free);

    let second = cli(&["scan", "--case", text(&case), "--volume", drive]);
    let outcome = cli(&[
        "diff",
        "--case",
        text(&case),
        volume["id"].as_str().unwrap(),
        second.json["volumes"][0]["id"].as_str().unwrap(),
    ]);
    assert_eq!(outcome.code, 0, "{}", outcome.stderr);
    assert_eq!(outcome.json["kind"], "volume");
    assert!(outcome.json["free_space_change"]["delta_bytes"].is_i64());
}
