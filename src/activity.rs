//! `activity`: the folders of a stored snapshot, each with the last use of the programs linked
//! to it.
//!
//! Every program run found in UserAssist, Prefetch and SRUM is linked to a folder in one of four
//! ways, strongest first:
//! - `inside`: the program's executable lies inside the folder;
//! - `exact`: the executable's name without extension is the folder name or one of its words
//!   (`Code` and `Code.exe`, `go-build` and `go.exe`);
//! - `program`: a word of the folder name is a word of the executable's name (`Kakao` and
//!   `KakaoTalk.exe`);
//! - `folder`: a word of the folder name is a word of one of the executable's parent folders
//!   (`PreSonus` and `C:\Program Files\PreSonus\Studio One 6\Studio One.exe`).
//! Generic words (`Microsoft`, `Programs`, `Temp`, `Cache`, ...) do not link. A folder's last use
//! is the latest run among the strongest kind of link it has.
//!
//! A folder without a linked run either belongs to a program that has not run within the
//! periods the records cover, or to no program that can be told from its name.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{Value, json};
use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_LocalAppDataLow, FOLDERID_RoamingAppData};

use crate::case::{Case, Kind};
use crate::cli::ActivityArgs;
use crate::output::{Fail, Res};
use crate::scan::snapshot::Snapshot;
use crate::usage::{self, userassist};
use crate::win;

/// Words that name no particular program.
const GENERIC: &[&str] = &[
    "app", "apps", "appdata", "application", "applications", "audio", "bin", "cache", "caches", "client", "com",
    "common", "config", "data", "default", "desktop", "files", "game", "games", "inc", "launcher", "lib", "local",
    "locallow", "logs", "microsoft", "net", "org", "package", "packages", "program", "programs", "roaming",
    "runtime", "settings", "software", "studio", "system32", "temp", "tmp", "tool", "tools", "update", "updater",
    "user", "users", "windows", "windowsapps", "x64", "x86", "build", "release", "debug", "dist", "src", "out",
    "target", "node", "modules", "vendor", "assets", "resources", "plugins", "extensions", "helper", "host",
    "service", "services", "crash", "crashpad", "dumps", "reporter", "setup", "install", "installer", "uninstall",
];

const LINKS: [&str; 4] = ["inside", "exact", "program", "folder"];
const EXECUTABLE: [&str; 3] = [".exe", ".com", ".scr"];

/// Words of one run's executable: of its name without extension, and of its parent folders.
struct RunWords {
    stem: String,
    name: BTreeSet<String>,
    parents: BTreeSet<String>,
    executable_path: Option<String>,
}

fn run_words(run: &Run) -> RunWords {
    let full = run.path.as_deref().unwrap_or(&run.name);
    let mut components: Vec<&str> = full.split('\\').collect();
    let file = components.pop().unwrap_or_default();
    // Keep the original letter case for splitting `KakaoTalk` into words.
    let lower = file.to_lowercase();
    let cut = [".exe", ".com", ".scr", ".lnk"].iter().find(|extension| lower.ends_with(*extension)).map_or(0, |e| e.len());
    let original = &file[..file.len() - cut];
    let stem = original.to_lowercase();
    let mut parents = BTreeSet::new();
    for (index, component) in components.iter().enumerate() {
        if !(index == 0 && component.ends_with(':')) {
            parents.extend(words(component));
        }
    }
    let executable_path = run
        .path
        .as_deref()
        .filter(|path| EXECUTABLE.iter().any(|extension| path.to_lowercase().ends_with(extension)))
        .map(|path| win::path_key(Path::new(path)));
    RunWords { name: words(original), stem, parents, executable_path }
}

/// One program run from one source.
struct Run {
    /// Executable path when the source knows it.
    path: Option<String>,
    /// Executable file name, or the identifier when there is no path.
    name: String,
    unix: i64,
    source: &'static str,
}

/// Lower-case words of a name. Each run of letters and digits between separators is a word;
/// its parts at lower-to-upper case changes and letter-digit changes are words too when they
/// have at least four characters (`SpliceSettings` gives `splicesettings`, `splice`,
/// `settings`; `PreSonus` gives `presonus`, `sonus`).
pub fn words(name: &str) -> BTreeSet<String> {
    let mut words = BTreeSet::new();
    for chunk in name.split(|c: char| !c.is_alphanumeric()).filter(|chunk| !chunk.is_empty()) {
        words.insert(chunk.to_lowercase());
        let mut part = String::new();
        let mut previous: Option<char> = None;
        for c in chunk.chars() {
            let boundary = previous
                .is_some_and(|p| (p.is_lowercase() && c.is_uppercase()) || (p.is_alphabetic() != c.is_alphabetic()));
            if boundary {
                if part.chars().count() >= 4 {
                    words.insert(part.to_lowercase());
                }
                part.clear();
            }
            part.push(c);
            previous = Some(c);
        }
        if part.chars().count() >= 4 {
            words.insert(part.to_lowercase());
        }
    }
    words.retain(|word| word.chars().count() >= 2 && !word.chars().all(|c| c.is_ascii_digit()));
    words
}

/// Words of a folder name that can identify a program.
pub fn identifying_words(name: &str) -> BTreeSet<String> {
    let mut words = words(name);
    words.retain(|word| word.chars().count() >= 3 && !GENERIC.contains(&word.as_str()));
    words
}

/// Words of an executable path: its file name and every parent folder below the drive.
fn rfc3339(unix: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|utc| utc.with_timezone(&chrono::Local).to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

fn unix_of(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text).ok().map(|time| time.timestamp())
}

fn file_name(path: &str) -> String {
    path.rsplit('\\').next().unwrap_or(path).to_owned()
}

/// Every run the three sources know, and what each source covered.
fn collect_runs(case: &Case, args: &ActivityArgs) -> (Vec<Run>, Value) {
    let mut runs = Vec::new();
    let mut coverage = serde_json::Map::new();

    match userassist::all() {
        Ok(records) => {
            let count = records.len();
            for (name, record) in records {
                if record.last_run == 0 || (record.run_count == 0 && record.focus_ms == 0) {
                    continue;
                }
                let Some(unix) = win::filetime_rfc3339(record.last_run).as_deref().and_then(unix_of) else {
                    continue;
                };
                let path = userassist::resolve_name(&name);
                runs.push(Run { name: file_name(path.as_deref().unwrap_or(&name)), path, unix, source: "UserAssist" });
            }
            coverage.insert("userassist".to_owned(), json!({ "status": "ok", "records": count }));
        }
        Err(err) => {
            coverage.insert("userassist".to_owned(), json!({ "status": usage::status_of_io(&err), "message": err.to_string() }));
        }
    }

    match usage::open_prefetch(Some(case), args.prefetch_dir.as_deref()) {
        Ok(source) => {
            let mut oldest: Option<u64> = None;
            for record in &source.load.records {
                oldest = record.last_runs.iter().copied().chain(oldest).min();
                let Some(unix) = record.last_runs.first().copied().and_then(win::filetime_rfc3339).as_deref().and_then(unix_of)
                else {
                    continue;
                };
                runs.push(Run { path: record.path.clone(), name: record.executable.clone(), unix, source: "Prefetch" });
            }
            coverage.insert(
                "prefetch".to_owned(),
                json!({
                    "status": "ok",
                    "origin": source.origin,
                    "programs": source.load.records.len(),
                    // Prefetch keeps the last eight runs of each program it still tracks.
                    "oldest_run": oldest.and_then(win::filetime_rfc3339),
                }),
            );
        }
        Err(result) => {
            coverage.insert("prefetch".to_owned(), result);
        }
    }

    match usage::read_srum(Some(case), args.srum_db.as_deref(), &|_, _| true) {
        Ok(read) => {
            let mut first: Option<i64> = None;
            for app in &read.result.apps {
                first = Some(first.map_or(app.first_unix(), |known| known.min(app.first_unix())));
                let identifier = app.identifiers.first().cloned().unwrap_or_default();
                let name = app.path.as_deref().map(file_name).unwrap_or_else(|| usage::srum::identifier_name(&identifier).to_owned());
                runs.push(Run { path: app.path.clone(), name, unix: app.last_unix(), source: "SRUM" });
            }
            coverage.insert(
                "srum".to_owned(),
                json!({
                    "status": "ok",
                    "origin": read.origin,
                    "skipped_copies": read.skipped,
                    "apps": read.result.apps.len(),
                    // Records older than this are no longer kept.
                    "observed_from": first.and_then(rfc3339),
                }),
            );
        }
        Err(result) => {
            coverage.insert("srum".to_owned(), result);
        }
    }
    (runs, Value::Object(coverage))
}

fn latest_snapshot(case: &Case, root: &Path) -> Option<Snapshot> {
    let key = win::path_key(&win::final_path(root).unwrap_or_else(|_| root.to_path_buf()));
    case.ids(Kind::Scan).into_iter().find_map(|id| {
        let snapshot: Snapshot = serde_json::from_value(case.read_json(&id).ok()?).ok()?;
        (win::path_key(Path::new(&snapshot.root.final_path)) == key).then_some(snapshot)
    })
}

fn evidence(run: &Run, link: &str) -> Value {
    json!({
        "time": rfc3339(run.unix),
        "source": run.source,
        "program": run.path.clone().unwrap_or_else(|| run.name.clone()),
        "link": link,
    })
}

pub fn run(args: ActivityArgs) -> Res<Option<Value>> {
    let mut roots = args.root.clone();
    if args.appdata {
        for id in [&FOLDERID_LocalAppData, &FOLDERID_RoamingAppData, &FOLDERID_LocalAppDataLow] {
            roots.push(win::known_folder(id).ok_or_else(|| Fail::run("known_folder", "AppData folder not found"))?);
        }
    }
    if roots.is_empty() {
        return Err(Fail::input("give --root or --appdata"));
    }
    let case = Case::open(&args.case)?;
    let mut snapshots = Vec::new();
    let mut missing = Vec::new();
    for root in &roots {
        match latest_snapshot(&case, root) {
            Some(snapshot) => snapshots.push(snapshot),
            None => missing.push(root.to_string_lossy().into_owned()),
        }
    }
    if !missing.is_empty() {
        return Err(Fail::blocked(
            "no_snapshot",
            "measure these roots with `zero-surplus scan --root` first",
            json!({ "roots": missing }),
        ));
    }

    let (runs, coverage) = collect_runs(&case, &args);
    let run_words: Vec<RunWords> = runs.iter().map(run_words).collect();

    let mut rows = Vec::new();
    for snapshot in &snapshots {
        for row in snapshot.rows.iter().filter(|row| row.kind == "dir" && row.bytes >= args.min_bytes) {
            let folder = Path::new(&snapshot.root.final_path).join(&row.name);
            let prefix = format!("{}\\", win::path_key(&folder));
            let identifying = identifying_words(&row.name);
            // Whole-name matches may be short (`uv`, `go`) but never generic.
            let mut exact_names = words(&row.name);
            exact_names.insert(row.name.to_lowercase());
            exact_names.retain(|word| !GENERIC.contains(&word.as_str()));

            // Strongest link kind first; within a kind, the latest run.
            let mut best: [Option<&Run>; 4] = [None; 4];
            let mut linked = BTreeSet::new();
            for (run, words) in runs.iter().zip(&run_words) {
                let kind = if words.executable_path.as_deref().is_some_and(|path| path.starts_with(&prefix)) {
                    Some(0)
                } else if exact_names.contains(&words.stem) {
                    Some(1)
                } else if !identifying.is_disjoint(&words.name) {
                    Some(2)
                } else if !identifying.is_disjoint(&words.parents) {
                    Some(3)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    if best[kind].is_none_or(|known| run.unix > known.unix) {
                        best[kind] = Some(run);
                    }
                    linked.insert(run.path.clone().unwrap_or_else(|| run.name.clone()).to_lowercase());
                }
            }
            let best = best.iter().enumerate().find_map(|(kind, run)| run.map(|run| (run, LINKS[kind])));
            rows.push(json!({
                "folder": folder.to_string_lossy(),
                "bytes": row.bytes,
                "status": row.status,
                "last_use": best.and_then(|(run, _)| rfc3339(run.unix)),
                "evidence": best.map(|(run, link)| evidence(run, link)),
                "programs_linked": linked.len(),
                "name_words": identifying,
            }));
        }
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row["bytes"].as_u64().unwrap_or(0)));

    let id = case.new_id(Kind::Activity);
    let mut result = json!({
        "command": "activity",
        "id": id,
        "created_at": win::now_rfc3339(),
        "snapshots": snapshots.iter().map(|s| json!({ "id": s.id, "root": s.root.final_path, "measured_at": s.ended_at })).collect::<Vec<_>>(),
        "min_bytes": args.min_bytes,
        "coverage": coverage,
        "rows": rows,
    });
    let path = case.write_json(&id, &result)?;
    result["stored"] = json!(case.relative(&path));
    if let Some(rows) = result["rows"].as_array_mut() {
        let total = rows.len();
        rows.truncate(args.top);
        result["rows_total"] = json!(total);
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn words_keep_whole_names_and_add_long_parts() {
        assert_eq!(words("com.celemony.melodyne"), set(&["celemony", "com", "melodyne"]));
        assert_eq!(words("SpliceSettings"), set(&["settings", "splice", "splicesettings"]));
        assert_eq!(words("PreSonus"), set(&["presonus", "sonus"]));
        assert_eq!(words("Telegram Desktop"), set(&["desktop", "telegram"]));
        assert_eq!(words("cpython-3.14.3-win"), set(&["cpython", "win"]));
        let package = words("OpenAI.Codex_26.1118.904.0_x64__3k8sg7r9htsxt");
        assert!(package.contains("codex") && package.contains("openai"));
    }

    #[test]
    fn generic_folder_names_identify_nothing() {
        assert!(identifying_words("Microsoft").is_empty());
        assert!(identifying_words("Programs").is_empty());
        assert!(identifying_words("Temp").is_empty());
        assert_eq!(identifying_words("Claude-Data"), set(&["claude"]));
        assert_eq!(identifying_words("com.celemony.melodyne"), set(&["celemony", "melodyne"]));
        assert_eq!(identifying_words("Waves Audio"), set(&["waves"]));
    }

    fn run(path: &str) -> RunWords {
        run_words(&Run { path: Some(path.to_owned()), name: file_name(path), unix: 0, source: "test" })
    }

    #[test]
    fn run_words_separate_the_program_name_from_its_folders() {
        let studio = run(r"C:\Program Files\PreSonus\Studio One 6\Studio One.exe");
        assert_eq!(studio.stem, "studio one");
        assert!(studio.parents.contains("presonus"));
        assert!(!studio.name.contains("presonus"));
        let kakao = run(r"C:\Program Files (x86)\Kakao\KakaoTalk\KakaoTalk.exe");
        assert!(kakao.name.contains("kakao"));
        assert_eq!(run(r"C:\Users\me\AppData\Roaming\Microsoft\Windows\Start Menu\Chrome.lnk").executable_path, None);
        assert!(run(r"C:\Tools\uv.exe").executable_path.is_some());
    }
}
