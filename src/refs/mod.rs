//! `refs`: which projects reference which Python environments, and which environments
//! nothing references.
//!
//! A reference is recorded with the file and setting it came from. References that cannot be
//! tied to one environment (a bare version, a run-time value, an ambiguous name) are kept as
//! unresolved instead of being guessed.

mod envs;
mod projects;
mod resolve;

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::case::{Case, Kind};
use crate::cli::RefsArgs;
use crate::output::{Fail, Res};
use crate::scan::snapshot::Snapshot;
use crate::usage::prefetch::PfRecord;
use crate::usage::{self, PrefetchSource};
use crate::win;
use envs::{Env, Registry};
use projects::{JdkTable, RawRef, Target};

const NAME_NOT_FOUND: &str = "no environment with this name in the investigated scope";
const NAME_AMBIGUOUS: &str = "several environments have this name";

struct Project {
    name: String,
    path: PathBuf,
}

fn project(path: &Path) -> Res<Project> {
    let path = std::path::absolute(path).map_err(|err| Fail::io("resolve project folder", &err))?;
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Project { name, path })
}

/// What a set of Prefetch records says about one environment's interpreters.
pub struct PrefetchActivity {
    /// FILETIME of the most recent run.
    pub last_run: u64,
    pub run_count: u64,
    /// Projects with files among those the interpreter touched right after starting.
    pub projects: BTreeSet<String>,
}

/// `interpreters` holds path keys; `projects` holds `(name, path key)`.
pub fn prefetch_activity(
    interpreters: &HashSet<String>,
    records: &[PfRecord],
    projects: &[(String, String)],
) -> Option<PrefetchActivity> {
    let mut activity: Option<PrefetchActivity> = None;
    for record in records {
        let Some(path) = &record.path else {
            continue;
        };
        if !interpreters.contains(&win::path_key(Path::new(path))) {
            continue;
        }
        let entry = activity.get_or_insert(PrefetchActivity { last_run: 0, run_count: 0, projects: BTreeSet::new() });
        entry.last_run = entry.last_run.max(record.last_runs.first().copied().unwrap_or(0));
        entry.run_count += u64::from(record.run_count);
        for referenced in &record.referenced {
            let key = win::path_key(Path::new(referenced));
            for (name, root) in projects {
                if key.starts_with(root) && key.as_bytes().get(root.len()) == Some(&b'\\') {
                    entry.projects.insert(name.clone());
                }
            }
        }
    }
    activity
}

/// Size of an environment taken from the newest snapshot in which it is a direct-child row.
fn size_from_snapshots(snapshots: &[Snapshot], env: &Env) -> Option<Value> {
    let locations: Vec<&PathBuf> = std::iter::once(&env.final_path).chain(std::iter::once(&env.path)).chain(&env.aliases).collect();
    for snapshot in snapshots {
        let root = win::path_key(Path::new(&snapshot.root.final_path));
        for location in &locations {
            let (Some(parent), Some(name)) = (location.parent(), location.file_name()) else {
                continue;
            };
            if win::path_key(parent) != root {
                continue;
            }
            let name = name.to_string_lossy();
            if let Some(row) = snapshot.rows.iter().find(|row| row.kind == "dir" && row.name.eq_ignore_ascii_case(&name)) {
                return Some(json!({
                    "bytes": row.bytes,
                    "allocated_bytes": row.allocated_bytes,
                    "status": row.status,
                    "snapshot": snapshot.id,
                    "measured_at": snapshot.ended_at,
                }));
            }
        }
    }
    None
}

fn load_snapshots(case: Option<&Case>) -> Vec<Snapshot> {
    let Some(case) = case else {
        return Vec::new();
    };
    case.ids(Kind::Scan)
        .into_iter()
        .filter_map(|id| serde_json::from_value(case.read_json(&id).ok()?).ok())
        .collect()
}

fn source_summary<T>(source: &Result<T, Value>, origin: impl Fn(&T) -> Value) -> Value {
    match source {
        Ok(source) => json!({ "status": "ok", "origin": origin(source) }),
        Err(result) => result.clone(),
    }
}

pub fn run(args: RefsArgs) -> Res<Option<Value>> {
    // Projects in scope.
    let mut projects: Vec<Project> = Vec::new();
    for path in &args.project {
        projects.push(project(path)?);
    }
    for parent in &args.projects_in {
        let entries = std::fs::read_dir(parent).map_err(|err| Fail::io(&format!("list {}", parent.display()), &err))?;
        let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()).collect();
        children.sort();
        for child in children {
            projects.push(project(&child)?);
        }
    }

    // Environments in scope.
    let mut registry = Registry::default();
    for store in &args.envs_in {
        registry.add_children(store).map_err(|err| Fail::io(&format!("list {}", store.display()), &err))?;
    }
    for path in &args.env {
        registry.add(path, true).ok_or_else(|| Fail::input(format!("not a folder: {}", path.display())))?;
    }
    for path in &args.shared {
        let index = registry.add(path, true).ok_or_else(|| Fail::input(format!("not a folder: {}", path.display())))?;
        registry.envs[index].shared = true;
    }
    for project in &projects {
        for name in [".venv", "venv"] {
            if let Some(index) = registry.add(&project.path.join(name), false) {
                registry.envs[index].located_in_project.get_or_insert_with(|| project.name.clone());
            }
        }
    }
    if projects.is_empty() && registry.envs.is_empty() {
        return Err(Fail::input("give projects (--project, --projects-in) or environments (--envs-in, --env, --shared)"));
    }

    // References.
    let table = JdkTable::load(&args.jdk_table);
    let mut referenced: Vec<Vec<Value>> = registry.envs.iter().map(|_| Vec::new()).collect();
    let mut unresolved = Vec::new();
    let mut outside = Vec::new();
    let mut unreadable = Vec::new();
    let mut classify = |raw: RawRef, owner: Option<&str>| {
        let mut entry = json!({
            "project": owner,
            "file": raw.source.to_string_lossy(),
            "key": raw.key,
            "value": raw.raw,
        });
        match raw.target {
            Target::Path(path) => match registry.containing(&path) {
                Some(index) => referenced[index].push(entry),
                None => {
                    entry["resolved_path"] = json!(path.to_string_lossy());
                    outside.push(entry);
                }
            },
            Target::CondaName(name) => match registry.named(&name).as_slice() {
                [index] => referenced[*index].push(entry),
                [] => {
                    entry["reason"] = json!(NAME_NOT_FOUND);
                    unresolved.push(entry);
                }
                several => {
                    entry["reason"] = json!(NAME_AMBIGUOUS);
                    entry["candidates"] =
                        json!(several.iter().map(|&index| registry.envs[index].path.to_string_lossy()).collect::<Vec<_>>());
                    unresolved.push(entry);
                }
            },
            Target::Unresolved(reason) => {
                entry["reason"] = json!(reason);
                unresolved.push(entry);
            }
        }
    };
    for project in &projects {
        match projects::read_project(&project.path, &table) {
            Ok(refs) => refs.into_iter().for_each(|raw| classify(raw, Some(&project.name))),
            Err(err) => unreadable.push(json!({
                "project": project.name,
                "path": project.path.to_string_lossy(),
                "message": err.to_string(),
            })),
        }
    }
    let mut scripts = Vec::new();
    for script in &args.script {
        let script = std::path::absolute(script).map_err(|err| Fail::io("resolve script path", &err))?;
        // A script belongs to the project whose folder contains it.
        let owner = projects
            .iter()
            .filter(|project| script.starts_with(&project.path))
            .max_by_key(|project| project.path.as_os_str().len())
            .map(|project| project.name.as_str());
        match projects::read_script(&script) {
            Ok(refs) => {
                scripts.push(json!({ "path": script.to_string_lossy(), "project": owner, "references": refs.len() }));
                refs.into_iter().for_each(|raw| classify(raw, owner));
            }
            Err(err) => unreadable.push(json!({ "script": script.to_string_lossy(), "message": err.to_string() })),
        }
    }

    // Sizes come from stored snapshots; activity from the execution records.
    let case = args.case.as_deref().map(Case::create).transpose()?;
    let snapshots = load_snapshots(case.as_ref());
    let project_keys: Vec<(String, String)> =
        projects.iter().map(|project| (project.name.clone(), win::path_key(&project.path))).collect();
    let interpreter_keys: Vec<HashSet<String>> = registry
        .envs
        .iter()
        .map(|env| env.python_paths().iter().map(|path| win::path_key(path)).collect())
        .collect();

    let mut activity_sources = Value::Null;
    let mut prefetch: Option<Result<PrefetchSource, Value>> = None;
    let mut srum_apps: Option<Result<Vec<usage::srum::AppUsage>, Value>> = None;
    if !args.no_activity {
        let prefetch_source = usage::open_prefetch(case.as_ref(), args.prefetch_dir.as_deref());
        let all_keys: HashSet<&String> = interpreter_keys.iter().flatten().collect();
        let wanted =
            |path: Option<&str>, _: &str| path.is_some_and(|path| all_keys.contains(&win::path_key(Path::new(path))));
        let srum_summary;
        srum_apps = Some(match usage::read_srum(case.as_ref(), args.srum_db.as_deref(), &wanted) {
            Ok(read) => {
                srum_summary = json!({
                    "status": "ok",
                    "origin": read.origin,
                    "database_state": read.result.db_state,
                    "skipped_copies": read.skipped,
                });
                Ok(read.result.apps)
            }
            Err(result) => {
                srum_summary = result.clone();
                Err(result)
            }
        });
        activity_sources = json!({
            "prefetch": source_summary(&prefetch_source, |source| source.origin.clone()),
            "srum": srum_summary,
        });
        prefetch = Some(prefetch_source);
    }

    let mut environments = Vec::new();
    let mut candidates = Vec::new();
    for (index, env) in registry.envs.iter().enumerate() {
        let size = size_from_snapshots(&snapshots, env);
        let keys = &interpreter_keys[index];
        let mut last_activity: Option<String> = None;
        let mut activity = Value::Null;
        if !args.no_activity {
            let prefetch_part = match prefetch.as_ref() {
                Some(Ok(source)) => match prefetch_activity(keys, &source.load.records, &project_keys) {
                    Some(found) => {
                        let last_run = win::filetime_rfc3339(found.last_run);
                        last_activity = last_run.clone();
                        json!({
                            "status": "ok",
                            "last_run": last_run,
                            "run_count": found.run_count,
                            "projects": found.projects,
                        })
                    }
                    None => json!({ "status": "no_record" }),
                },
                Some(Err(result)) => json!({ "status": result["status"] }),
                None => Value::Null,
            };
            let srum_part = match srum_apps.as_ref() {
                Some(Ok(apps)) => {
                    let mine: Vec<&usage::srum::AppUsage> = apps
                        .iter()
                        .filter(|app| app.path.as_deref().is_some_and(|path| keys.contains(&win::path_key(Path::new(path)))))
                        .collect();
                    if mine.is_empty() {
                        json!({ "status": "no_record" })
                    } else {
                        let last_seen = mine.iter().filter_map(|app| app.last_seen()).max();
                        if last_seen > last_activity {
                            last_activity = last_seen.clone();
                        }
                        json!({
                            "status": "ok",
                            "last_seen": last_seen,
                            "active_days": mine.iter().map(|app| app.active_days()).max(),
                            "interpreters": mine.iter().map(|app| app.to_json(0)).collect::<Vec<_>>(),
                        })
                    }
                }
                Some(Err(result)) => json!({ "status": result["status"] }),
                None => Value::Null,
            };
            activity = json!({ "prefetch": prefetch_part, "srum": srum_part });
        }

        let references = &referenced[index];
        let projects_using: BTreeSet<&str> =
            references.iter().filter_map(|reference| reference["project"].as_str()).collect();
        let mut entry = json!({
            "name": env.name,
            "path": env.path.to_string_lossy(),
            "kind": env.kind,
            "version": env.version,
            "base_python": env.base_python,
            "shared": env.shared,
            "located_in_project": env.located_in_project,
            "aliases": env.aliases.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
            "referenced_by_projects": projects_using,
            "references": references,
            "size_state": if size.is_some() { "measured" } else { "unmeasured" },
            "size": size,
            "activity": activity,
            "last_activity": last_activity,
        });
        if win::path_key(&env.final_path) != win::path_key(&env.path) {
            entry["final_path"] = json!(env.final_path.to_string_lossy());
        }
        if references.is_empty() && !env.shared {
            candidates.push(json!({
                "name": env.name,
                "path": env.path.to_string_lossy(),
                "size_state": entry["size_state"],
                "size_bytes": entry["size"]["bytes"],
                "size_status": entry["size"]["status"],
                "located_in_project": env.located_in_project,
                "last_activity": entry["last_activity"],
            }));
        }
        environments.push(entry);
    }
    // Largest first; environments without a size come last.
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate["size_bytes"].as_u64()));

    let mut result = json!({
        "command": "refs",
        "created_at": win::now_rfc3339(),
        "scope": {
            "projects": projects.iter().map(|project| json!({ "name": project.name, "path": project.path.to_string_lossy() })).collect::<Vec<_>>(),
            "environment_folders": args.envs_in.iter().map(|path| path.to_string_lossy()).collect::<Vec<_>>(),
            "scripts": scripts,
            "unreadable": unreadable,
        },
        "activity_sources": activity_sources,
        "environments": environments,
        // No project reference and not designated as shared, by size.
        "unreferenced": candidates,
        "unresolved_references": unresolved,
        "references_outside_scope": outside,
    });
    if let Some(case) = &case {
        let id = case.new_id(Kind::Refs);
        result["id"] = json!(id);
        let path = case.write_json(&id, &result)?;
        result["stored"] = json!(case.relative(&path));
    }
    Ok(Some(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str, last_run: u64, run_count: u32, referenced: &[&str]) -> PfRecord {
        PfRecord {
            file: "PYTHON.EXE-00000000.pf".to_owned(),
            version: 31,
            executable: "PYTHON.EXE".to_owned(),
            path: Some(path.to_owned()),
            path_basis: Some("only_candidate"),
            path_candidates: Vec::new(),
            run_count,
            last_runs: vec![last_run],
            referenced: referenced.iter().map(|path| (*path).to_owned()).collect(),
        }
    }

    #[test]
    fn execution_records_attach_only_to_the_environment_that_ran() {
        let records = [
            record(r"C:\ENVS\D\PYTHON.EXE", 200, 4, &[r"C:\WORK\PROJ-A\MAIN.PY", r"C:\WINDOWS\SYSTEM32\NTDLL.DLL"]),
            record(r"C:\ENVS\OTHER\PYTHON.EXE", 900, 9, &[r"C:\WORK\PROJ-B\RUN.PY"]),
        ];
        let projects = [("proj-a".to_owned(), r"c:\work\proj-a".to_owned()), ("proj-b".to_owned(), r"c:\work\proj-b".to_owned())];
        let d: HashSet<String> = [r"c:\envs\d\python.exe".to_owned()].into();
        let unused: HashSet<String> = [r"c:\envs\fai\python.exe".to_owned()].into();

        let activity = prefetch_activity(&d, &records, &projects).expect("D ran");
        assert_eq!(activity.last_run, 200);
        assert_eq!(activity.run_count, 4);
        assert_eq!(activity.projects.into_iter().collect::<Vec<_>>(), ["proj-a"]);

        assert!(prefetch_activity(&unused, &records, &projects).is_none());
    }

    #[test]
    fn a_project_name_prefix_is_not_a_project_match() {
        let records = [record(r"C:\ENVS\D\PYTHON.EXE", 1, 1, &[r"C:\WORK\PROJ-A-OLD\X.PY"])];
        let projects = [("proj-a".to_owned(), r"c:\work\proj-a".to_owned())];
        let d: HashSet<String> = [r"c:\envs\d\python.exe".to_owned()].into();
        assert!(prefetch_activity(&d, &records, &projects).unwrap().projects.is_empty());
    }
}
