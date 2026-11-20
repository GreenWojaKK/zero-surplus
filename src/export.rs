//! `export`: bundle notes, stored results and diagnoses into a folder that can be moved.
//!
//! The manifest holds only relative paths. Every reference is checked before anything is
//! written; a missing ID stops the export and is returned.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::case::{self, Case, Kind};
use crate::cli::ExportArgs;
use crate::output::{Fail, Res};
use crate::win;

const MANIFEST_FORMAT: u32 = 1;

fn sidecar(path: &Path) -> PathBuf {
    path.with_extension("json")
}

/// IDs a stored diagnosis cites, read from the file stored next to its text.
fn cites_of(case: &Case, id: &str) -> Result<Vec<String>, String> {
    let path = case.existing(id).ok_or_else(|| format!("{id} is not in the case folder"))?;
    let file = sidecar(&path);
    let bytes = fs::read(&file).map_err(|err| format!("read {}: {err}", case.relative(&file)))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|err| format!("parse {}: {err}", case.relative(&file)))?;
    serde_json::from_value::<Vec<String>>(value["cites"].clone())
        .map_err(|err| format!("{}: `cites` is not a list of IDs: {err}", case.relative(&file)))
}

/// References gathered for a bundle.
#[derive(Default)]
struct Gathered {
    items: BTreeSet<String>,
    /// Cited IDs of each diagnosis included.
    cites: HashMap<String, Vec<String>>,
    missing: BTreeSet<String>,
    /// Diagnoses whose cited IDs could not be read.
    unreadable: Vec<Value>,
}

/// Add `id` and, for diagnoses, everything they cite.
fn collect(case: &Case, id: &str, gathered: &mut Gathered) {
    if gathered.items.contains(id) || gathered.missing.contains(id) {
        return;
    }
    if case.existing(id).is_none() {
        gathered.missing.insert(id.to_owned());
        return;
    }
    gathered.items.insert(id.to_owned());
    if Kind::of_id(id) == Some(Kind::Diagnosis) {
        match cites_of(case, id) {
            Ok(cited) => {
                for other in &cited {
                    collect(case, other, gathered);
                }
                gathered.cites.insert(id.to_owned(), cited);
            }
            Err(message) => gathered.unreadable.push(json!({ "diagnosis": id, "message": message })),
        }
    }
}

fn read_utf8(path: &Path) -> Res<Vec<u8>> {
    let bytes = fs::read(path).map_err(|err| Fail::io(&format!("read {}", path.display()), &err))?;
    std::str::from_utf8(&bytes).map_err(|_| Fail::input(format!("{} is not UTF-8 text", path.display())))?;
    Ok(bytes)
}

pub fn run(args: ExportArgs) -> Res<Option<Value>> {
    let case = Case::create(&args.case)?;
    if args.add_diagnosis.is_none() && !args.cites.is_empty() {
        return Err(Fail::input("--cites needs --add-diagnosis"));
    }

    // Read every input before writing anything.
    let mut notes: Vec<Vec<u8>> = Vec::new();
    for path in &args.note_file {
        notes.push(read_utf8(path)?);
    }
    if args.note_stdin {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes).map_err(|err| Fail::io("read note from standard input", &err))?;
        std::str::from_utf8(&bytes).map_err(|_| Fail::input("the note on standard input is not UTF-8 text"))?;
        notes.push(bytes);
    }
    let new_diagnosis = args.add_diagnosis.as_deref().map(read_utf8).transpose()?;

    let mut gathered = Gathered::default();
    for id in args.include.iter().chain(&args.diagnosis).chain(&args.cites) {
        collect(&case, id, &mut gathered);
    }
    for id in &args.diagnosis {
        if Kind::of_id(id) != Some(Kind::Diagnosis) {
            return Err(Fail::input(format!("--diagnosis takes diagnosis IDs, got {id}")));
        }
    }
    if !gathered.unreadable.is_empty() {
        return Err(Fail::blocked(
            "unreadable_citations",
            "the cited IDs of some diagnoses could not be read; no bundle was created",
            json!({ "unreadable": gathered.unreadable, "missing": gathered.missing }),
        ));
    }
    if !gathered.missing.is_empty() {
        return Err(Fail::blocked(
            "missing_references",
            "some referenced IDs are not in the case folder; no bundle was created",
            json!({ "missing": gathered.missing }),
        ));
    }
    let Gathered { mut items, mut cites, .. } = gathered;

    let name = match &args.name {
        Some(name) => {
            if name.is_empty() || name.contains(['/', '\\', ':']) || name.contains("..") {
                return Err(Fail::input(format!("invalid bundle name: {name}")));
            }
            name.clone()
        }
        None => format!("bundle-{}-{:04x}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"), std::process::id() & 0xffff),
    };
    let out = std::path::absolute(&args.out).map_err(|err| Fail::io("resolve output folder", &err))?;
    let bundle = out.join(&name);
    if bundle.exists() {
        return Err(Fail::blocked(
            "bundle_exists",
            format!("{} already exists and was left untouched", bundle.display()),
            json!({ "bundle": bundle.to_string_lossy() }),
        ));
    }

    // Store the new notes and the new diagnosis in the case under fresh IDs.
    let mut note_ids = Vec::new();
    for bytes in &notes {
        let id = case.new_id(Kind::Note);
        case.write_bytes(&id, bytes)?;
        items.insert(id.clone());
        note_ids.push(id);
    }
    let mut diagnosis_id = None;
    if let Some(bytes) = &new_diagnosis {
        let id = case.new_id(Kind::Diagnosis);
        let path = case.path_of(&id).ok_or_else(|| Fail::run("export", "cannot name the diagnosis"))?;
        let info = json!({ "id": id, "kind": "diagnosis", "created_at": win::now_rfc3339(), "cites": args.cites });
        // The sidecar goes first: the text file's final name marks the diagnosis as complete.
        case::write_atomic(&sidecar(&path), &serde_json::to_vec_pretty(&info).unwrap_or_default())?;
        case.write_bytes(&id, bytes)?;
        items.insert(id.clone());
        cites.insert(id.clone(), args.cites.clone());
        diagnosis_id = Some(id);
    }

    // Build the bundle under a temporary name, then give it its final name.
    let staging = out.join(format!(".{name}.tmp-{}", std::process::id()));
    let _ = fs::remove_dir_all(&staging);
    let mut manifest_items = Vec::new();
    let copied = (|| -> Res<()> {
        for id in &items {
            let kind = Kind::of_id(id).ok_or_else(|| Fail::input(format!("invalid id: {id}")))?;
            let source = case.existing(id).ok_or_else(|| Fail::run("export", format!("{id} disappeared")))?;
            let relative = case.relative(&source);
            copy_into(&source, &staging.join(&relative))?;
            let mut entry = json!({ "kind": kind.name(), "id": id, "path": relative });
            if kind == Kind::Diagnosis {
                let cites_source = sidecar(&source);
                if cites_source.is_file() {
                    let cites_relative = case.relative(&cites_source);
                    copy_into(&cites_source, &staging.join(&cites_relative))?;
                    entry["cites_path"] = json!(cites_relative);
                }
                entry["cites"] = json!(cites.get(id).cloned().unwrap_or_default());
            }
            manifest_items.push(entry);
        }
        let manifest = json!({
            "format": MANIFEST_FORMAT,
            "kind": "zero-surplus-bundle",
            "created_at": win::now_rfc3339(),
            "items": manifest_items,
        });
        case::write_atomic(&staging.join("manifest.json"), &serde_json::to_vec_pretty(&manifest).unwrap_or_default())?;
        fs::rename(&staging, &bundle).map_err(|err| Fail::io(&format!("name bundle {}", bundle.display()), &err))
    })();
    if let Err(fail) = copied {
        let _ = fs::remove_dir_all(&staging);
        return Err(fail);
    }

    Ok(Some(json!({
        "command": "export",
        "bundle": bundle.to_string_lossy(),
        "manifest": "manifest.json",
        "items": items.len(),
        "notes": note_ids,
        "diagnosis": diagnosis_id,
        "included": items,
    })))
}

fn copy_into(source: &Path, target: &Path) -> Res<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|err| Fail::io(&format!("create {}", parent.display()), &err))?;
    }
    fs::copy(source, target).map(|_| ()).map_err(|err| Fail::io(&format!("copy {}", source.display()), &err))
}
