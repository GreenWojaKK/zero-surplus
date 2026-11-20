//! `list`: IDs, kinds, times and targets of completed results in a case folder.

use std::fs;

use serde_json::{Value, json};

use crate::case::{Case, Kind};
use crate::cli::ListArgs;
use crate::output::{Fail, Res};

pub fn run(args: ListArgs) -> Res<Option<Value>> {
    let case = Case::open(&args.case)?;
    let kinds: Vec<Kind> = match &args.kind {
        Some(name) => vec![Kind::from_name(name).ok_or_else(|| Fail::input(format!("unknown kind: {name}")))?],
        None => Kind::ALL.to_vec(),
    };
    let mut items = Vec::new();
    for kind in kinds {
        for id in case.ids(kind) {
            items.push(describe(&case, kind, &id));
        }
    }
    items.sort_by(|left, right| right["at"].as_str().cmp(&left["at"].as_str()));
    Ok(Some(json!({ "command": "list", "case": case.root().to_string_lossy(), "items": items })))
}

fn pick<'a>(value: &'a Value, paths: &[&str]) -> Option<&'a Value> {
    paths.iter().find_map(|path| {
        let found = path.split('.').try_fold(value, |node, key| node.get(key))?;
        (!found.is_null()).then_some(found)
    })
}

fn describe(case: &Case, kind: Kind, id: &str) -> Value {
    let mut item = json!({ "id": id, "kind": kind.name() });
    let Some(path) = case.existing(id) else {
        return item;
    };
    item["path"] = json!(case.relative(&path));
    let content = match kind {
        Kind::Note | Kind::Diagnosis => {
            // The text itself is not parsed; the sidecar of a diagnosis lists what it cites.
            let sidecar = path.with_extension("json");
            fs::read(sidecar).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        }
        Kind::Watch => fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.lines().next().and_then(|line| serde_json::from_str::<Value>(line).ok())),
        _ => fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()),
    };
    if let Some(content) = content {
        if let Some(at) = pick(&content, &["created_at", "started_at", "measured_at", "collected_at", "time"]) {
            item["at"] = at.clone();
        }
        if let Some(target) = pick(&content, &["target", "root.final_path", "root", "volume.mount", "path"]) {
            item["target"] = target.clone();
        }
        if let Some(cites) = content.get("cites") {
            item["cites"] = cites.clone();
        }
        if kind == Kind::Diff {
            item["before"] = content["before"].clone();
            item["after"] = content["after"].clone();
        }
    }
    item
}
