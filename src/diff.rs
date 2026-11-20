//! `diff`: compare two stored snapshots. Only the two stored files are opened.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::case::{Case, Kind};
use crate::cli::DiffArgs;
use crate::output::{Fail, Res};
use crate::scan::snapshot::{Row, Snapshot, VolumeSnapshot};
use crate::win;

const FREE_SPACE_LABEL: &str = "free space change";
const FREE_SPACE_NOTE: &str = "includes every other write and delete between the two measurement times";

pub fn run(args: DiffArgs) -> Res<Option<Value>> {
    let case = Case::open(&args.case)?;
    let kinds = (Kind::of_id(&args.before), Kind::of_id(&args.after));
    let mut result = match kinds {
        (Some(Kind::Scan), Some(Kind::Scan)) => diff_roots(&case, &args)?,
        (Some(Kind::Volume), Some(Kind::Volume)) => diff_volumes(&case, &args)?,
        _ => {
            return Err(Fail::input("diff takes two scan IDs or two volume IDs of the same kind"));
        }
    };
    let id = case.new_id(Kind::Diff);
    result["id"] = json!(id);
    result["created_at"] = json!(win::now_rfc3339());
    let path = case.write_json(&id, &result)?;
    result["stored"] = json!(case.relative(&path));

    // Print the largest changes only; the stored file keeps every row.
    if let Some(rows) = result.get_mut("rows").and_then(Value::as_array_mut) {
        let total = rows.len();
        let unchanged = rows.iter().filter(|row| row["state"] == "unchanged").count();
        rows.retain(|row| row["state"] != "unchanged");
        let changed = rows.len();
        rows.truncate(args.top);
        let shown = rows.len();
        result["rows_total"] = json!(total);
        result["rows_unchanged"] = json!(unchanged);
        result["rows_not_shown"] = json!(changed - shown);
    }
    Ok(Some(result))
}

fn load<T: serde::de::DeserializeOwned>(case: &Case, id: &str) -> Res<T> {
    serde_json::from_value(case.read_json(id)?)
        .map_err(|err| Fail::run("parse", format!("stored result {id} has an unexpected format: {err}")))
}

fn not_comparable(mismatches: Vec<Value>) -> Fail {
    Fail::blocked(
        "not_comparable",
        "the two snapshots were not measured under the same conditions",
        json!({ "mismatches": mismatches }),
    )
}

fn mismatch(field: &str, before: impl serde::Serialize, after: impl serde::Serialize) -> Value {
    json!({ "field": field, "before": before, "after": after })
}

fn diff_volumes(case: &Case, args: &DiffArgs) -> Res<Value> {
    let before: VolumeSnapshot = load(case, &args.before)?;
    let after: VolumeSnapshot = load(case, &args.after)?;
    if before.volume.guid_path != after.volume.guid_path || before.volume.serial != after.volume.serial {
        return Err(not_comparable(vec![mismatch("volume", &before.volume, &after.volume)]));
    }
    Ok(json!({
        "command": "diff",
        "kind": "volume",
        "before": before.id,
        "after": after.id,
        "volume": after.volume,
        "free_space_change": {
            "label": FREE_SPACE_LABEL,
            "note": FREE_SPACE_NOTE,
            "before_free_bytes": before.space.free_bytes,
            "after_free_bytes": after.space.free_bytes,
            "delta_bytes": after.space.free_bytes as i128 - before.space.free_bytes as i128,
            "before_at": before.measured_at,
            "after_at": after.measured_at,
        },
    }))
}

fn diff_roots(case: &Case, args: &DiffArgs) -> Res<Value> {
    let before: Snapshot = load(case, &args.before)?;
    let after: Snapshot = load(case, &args.after)?;

    let mut mismatches = Vec::new();
    if before.volume.guid_path != after.volume.guid_path || before.volume.serial != after.volume.serial {
        mismatches.push(mismatch("volume", &before.volume, &after.volume));
    }
    if win::path_key(Path::new(&before.root.final_path)) != win::path_key(Path::new(&after.root.final_path)) {
        mismatches.push(mismatch("root", &before.root.final_path, &after.root.final_path));
    }
    if before.rules != after.rules {
        mismatches.push(mismatch("rules", &before.rules, &after.rules));
    }
    let excluded = |snapshot: &Snapshot| {
        let mut keys: Vec<String> = snapshot.excluded.iter().map(|path| win::path_key(Path::new(path))).collect();
        keys.sort();
        keys
    };
    if excluded(&before) != excluded(&after) {
        mismatches.push(mismatch("excluded", &before.excluded, &after.excluded));
    }
    if !mismatches.is_empty() {
        return Err(not_comparable(mismatches));
    }

    let key = |row: &Row| if row.kind == "root_files" { String::new() } else { row.name.clone() };
    let before_rows: BTreeMap<String, &Row> = before.rows.iter().map(|row| (key(row), row)).collect();
    let after_rows: BTreeMap<String, &Row> = after.rows.iter().map(|row| (key(row), row)).collect();
    let names_complete = before.names_complete && after.names_complete;

    let mut rows = Vec::new();
    let mut names: Vec<&String> = before_rows.keys().chain(after_rows.keys()).collect();
    names.sort();
    names.dedup();
    for name in names {
        rows.push(compare(name, before_rows.get(name).copied(), after_rows.get(name).copied(), names_complete));
    }
    // Largest absolute change first; rows without a computable change after them.
    rows.sort_by_key(|row| {
        let delta = row["delta_bytes"].as_i64();
        (delta.is_none(), std::cmp::Reverse(delta.map_or(0, i64::unsigned_abs)))
    });

    Ok(json!({
        "command": "diff",
        "kind": "root",
        "before": before.id,
        "after": after.id,
        "root": after.root.final_path,
        "volume": after.volume,
        "rules": after.rules,
        "names_complete": { "before": before.names_complete, "after": after.names_complete },
        "free_space_change": {
            "label": FREE_SPACE_LABEL,
            "note": FREE_SPACE_NOTE,
            "before_free_bytes": before.free_bytes_end,
            "after_free_bytes": after.free_bytes_end,
            "delta_bytes": after.free_bytes_end as i128 - before.free_bytes_end as i128,
            "before_at": before.ended_at,
            "after_at": after.ended_at,
        },
        "rows": rows,
    }))
}

/// A row's size is fully measured when it is complete; a link row is complete by rule (0 bytes).
fn measured(row: &Row) -> bool {
    row.status == "complete" || row.status == "not_followed"
}

fn compare(name: &str, before: Option<&Row>, after: Option<&Row>, names_complete: bool) -> Value {
    let side = |row: Option<&Row>| match row {
        Some(row) => json!({ "kind": row.kind, "bytes": row.bytes, "status": row.status, "failures": row.failures }),
        None => Value::Null,
    };
    let (state, delta): (&str, Option<i128>) = match (before, after) {
        (Some(before), Some(after)) => {
            if measured(before) && measured(after) {
                let delta = after.bytes as i128 - before.bytes as i128;
                (if delta == 0 && before.kind == after.kind { "unchanged" } else { "changed" }, Some(delta))
            } else {
                // Part of the folder could not be read: keep both observed sizes, no delta.
                ("unknown", None)
            }
        }
        (Some(before), None) if names_complete => {
            ("disappeared", measured(before).then(|| -(before.bytes as i128)))
        }
        (None, Some(after)) if names_complete => ("appeared", measured(after).then_some(after.bytes as i128)),
        // The name is missing on one side but that side's listing was incomplete.
        _ => ("unknown", None),
    };
    json!({
        "name": name,
        "kind": after.or(before).map(|row| row.kind.as_str()),
        "state": state,
        "delta_bytes": delta,
        "before": side(before),
        "after": side(after),
    })
}
