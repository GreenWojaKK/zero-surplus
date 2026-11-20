mod common;

use std::path::Path;

use common::*;
use serde_json::Value;

fn conda_env(dir: &Path, version: &str, payload: usize) {
    write_file(&dir.join(format!("conda-meta/python-{version}-h0_0.json")), 2);
    write_file(&dir.join("python.exe"), payload);
}

fn venv(dir: &Path, version: &str, payload: usize) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("pyvenv.cfg"), format!("home = C:\\Python\nversion = {version}\n")).unwrap();
    write_file(&dir.join("Scripts/python.exe"), payload);
}

fn write_text(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn env<'a>(result: &'a Value, name: &str) -> &'a Value {
    row(&result["environments"], name)
}

/// Projects A and B use the shared environment FAI, C only asks for Python 3.12 while two
/// 3.12 environments exist, D is referenced by nobody.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        let envs = dir.join("envs");
        conda_env(&envs.join("FAI"), "3.12.7", 100);
        conda_env(&envs.join("py312b"), "3.12.3", 300);
        venv(&envs.join("D"), "3.11.9", 5000);
        // A second path to FAI.
        std::fs::create_dir_all(dir.join("links")).unwrap();
        junction(&dir.join("links/fai-link"), &envs.join("FAI"));

        let projects = dir.join("projects");
        // A: VS Code setting (with a comment, as VS Code allows) and a launch script.
        write_text(
            &projects.join("A/.vscode/settings.json"),
            &format!(
                "{{\n  // interpreter\n  \"python.defaultInterpreterPath\": \"{}\",\n}}\n",
                envs.join("FAI/python.exe").to_string_lossy().replace('\\', "/")
            ),
        );
        write_text(&projects.join("A/run.bat"), "@echo off\r\ncall conda activate FAI\r\npython main.py\r\n");
        write_text(&projects.join("A/dynamic.bat"), "\"%CONDA_PREFIX%\\python.exe\" main.py\r\n");
        // B: JetBrains SDK name; the SDK table points at FAI through the junction.
        write_text(
            &projects.join("B/.idea/misc.xml"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="ProjectRootManager" version="2" project-jdk-name="Python 3.12 (FAI)" project-jdk-type="Python SDK" />
</project>"#,
        );
        write_text(
            &dir.join("jdk.table.xml"),
            &format!(
                r#"<application><component name="ProjectJdkTable">
  <jdk version="2"><name value="Python 3.12 (FAI)" /><type value="Python SDK" />
    <homePath value="{}" /></jdk>
</component></application>"#,
                dir.join("links/fai-link/python.exe").to_string_lossy().replace('\\', "/")
            ),
        );
        // C: a version, nothing else.
        write_text(&projects.join("C/.python-version"), "3.12\n");
        write_text(&projects.join("C/pyproject.toml"), "[project]\nname = \"c\"\nrequires-python = \">=3.12\"\n");
        // E: an environment inside the project folder that no setting points at.
        venv(&projects.join("E/.venv"), "3.12.1", 40);
        // A project that cannot be read.
        write_file(&projects.join("locked/secret.txt"), 1);
        deny_listing(&projects.join("locked"));
        Self { dir }
    }

    fn refs(&self, extra: &[&str]) -> Value {
        let (projects, envs) = (self.dir.join("projects"), self.dir.join("envs"));
        let (case, table) = (self.dir.join("case"), self.dir.join("jdk.table.xml"));
        let (shared, alias) = (envs.join("FAI"), self.dir.join("links/fai-link"));
        let (run, dynamic) = (projects.join("A/run.bat"), projects.join("A/dynamic.bat"));
        let mut args = vec![
            "refs",
            "--case",
            text(&case),
            "--projects-in",
            text(&projects),
            "--envs-in",
            text(&envs),
            "--shared",
            text(&shared),
            "--env",
            text(&alias),
            "--script",
            text(&run),
            "--script",
            text(&dynamic),
            "--jdk-table",
            text(&table),
        ];
        args.extend_from_slice(extra);
        let outcome = cli(&args);
        assert_eq!(outcome.code, 0, "refs failed: {}", outcome.stderr);
        outcome.json
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        allow_listing(&self.dir.join("projects/locked"));
    }
}

#[test]
fn shared_environment_lists_its_projects_in_one_row() {
    let fixture = Fixture::new("refs-shared");
    let result = fixture.refs(&["--no-activity"]);

    let fai = env(&result, "FAI");
    assert_eq!(fai["shared"], true);
    assert_eq!(fai["referenced_by_projects"], serde_json::json!(["A", "B"]));
    // The junction is the same environment, not a second row.
    let rows = result["environments"].as_array().unwrap();
    assert_eq!(rows.iter().filter(|row| row["name"] == "FAI").count(), 1);
    assert_eq!(fai["aliases"].as_array().unwrap().len(), 1);
    assert_eq!(fai["kind"], "conda");
    assert_eq!(fai["version"], "3.12.7");

    // Every reference names the file and setting it came from.
    let references = fai["references"].as_array().unwrap();
    let keys: Vec<&str> = references.iter().map(|reference| reference["key"].as_str().unwrap()).collect();
    assert!(keys.contains(&"python.defaultInterpreterPath"));
    assert!(keys.contains(&"project-jdk-name"));
    assert!(keys.contains(&"conda activate"));
    assert!(references.iter().all(|reference| reference["file"].is_string()));
}

#[test]
fn version_only_and_runtime_references_stay_unresolved() {
    let fixture = Fixture::new("refs-unresolved");
    let result = fixture.refs(&["--no-activity"]);

    let unresolved = result["unresolved_references"].as_array().unwrap();
    let of_c: Vec<&Value> = unresolved.iter().filter(|reference| reference["project"] == "C").collect();
    assert_eq!(of_c.len(), 2);
    assert!(of_c.iter().all(|reference| reference["reason"] == "version only"));
    // Two 3.12 environments exist; neither is assigned to C.
    for name in ["FAI", "py312b"] {
        let projects = env(&result, name)["referenced_by_projects"].as_array().unwrap();
        assert!(!projects.iter().any(|project| project == "C"));
    }

    let dynamic = unresolved.iter().find(|reference| reference["value"].as_str().unwrap().contains("CONDA_PREFIX")).unwrap();
    assert_eq!(dynamic["reason"], "path decided at run time");
    assert_eq!(dynamic["project"], "A");

    // The unreadable project is named.
    let unreadable = result["scope"]["unreadable"].as_array().unwrap();
    assert!(unreadable.iter().any(|entry| entry["project"] == "locked"));
    let scope: Vec<&str> =
        result["scope"]["projects"].as_array().unwrap().iter().map(|project| project["name"].as_str().unwrap()).collect();
    assert_eq!(scope, ["A", "B", "C", "E", "locked"]);
}

#[test]
fn unreferenced_environments_are_ordered_by_size_and_unmeasured_ones_are_marked() {
    let fixture = Fixture::new("refs-size");
    // Only the environment folder is measured; the in-project environment is not.
    let scan = cli(&["scan", "--case", text(&fixture.dir.join("case")), "--root", text(&fixture.dir.join("envs"))]);
    assert_eq!(scan.code, 0, "{}", scan.stderr);
    let result = fixture.refs(&["--no-activity"]);

    let candidates = result["unreferenced"].as_array().unwrap();
    let names: Vec<&str> = candidates.iter().map(|candidate| candidate["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["D", "py312b", ".venv"]);
    assert_eq!(candidates[0]["size_state"], "measured");
    assert!(candidates[0]["size_bytes"].as_u64().unwrap() > candidates[1]["size_bytes"].as_u64().unwrap());
    assert_eq!(candidates[2]["size_state"], "unmeasured");
    assert!(candidates[2]["size_bytes"].is_null());
    assert_eq!(candidates[2]["located_in_project"], "E");

    // The size carries the snapshot it came from.
    let d = env(&result, "D");
    assert_eq!(d["size"]["snapshot"], scan.json["snapshots"][0]["id"]);
    assert_eq!(d["base_python"], "C:\\Python");

    // The result is stored and listed.
    let listing = cli(&["list", "--case", text(&fixture.dir.join("case")), "--kind", "refs"]);
    assert_eq!(listing.json["items"][0]["id"], result["id"]);
}

#[test]
fn activity_reports_each_source_with_its_own_status() {
    let fixture = Fixture::new("refs-activity");
    let empty = fixture.dir.join("no-prefetch-files");
    std::fs::create_dir_all(&empty).unwrap();
    let missing = fixture.dir.join("no-such-SRUDB.dat");
    let result = fixture.refs(&["--prefetch-dir", text(&empty), "--srum-db", text(&missing)]);

    assert_eq!(result["activity_sources"]["prefetch"]["status"], "ok");
    assert_eq!(result["activity_sources"]["srum"]["status"], "source_absent");
    let d = env(&result, "D");
    assert_eq!(d["activity"]["prefetch"]["status"], "no_record");
    assert_eq!(d["activity"]["srum"]["status"], "source_absent");
    assert!(d["last_activity"].is_null());
}
