//! Stored snapshot formats.

use serde::{Deserialize, Serialize};

use crate::win::{VolumeIdent, VolumeSpace};

pub const FORMAT: u32 = 1;
pub const RULES_VERSION: u32 = 1;

/// Aggregation rules; two snapshots are comparable only when these are equal.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Rules {
    pub version: u32,
    /// `logical`: file lengths are summed.
    pub size: String,
    /// `each_path`: a hard-linked file is counted at every path.
    pub hardlinks: String,
    /// `not_followed`: junctions and symbolic links are never entered.
    pub links: String,
    /// Rows are the root's direct children.
    pub depth: u32,
}

impl Rules {
    pub fn current() -> Self {
        Self {
            version: RULES_VERSION,
            size: "logical".to_owned(),
            hardlinks: "each_path".to_owned(),
            links: "not_followed".to_owned(),
            depth: 1,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct RootIdent {
    /// Path as given on the command line.
    pub input: String,
    /// Absolute path without link resolution.
    pub absolute: String,
    /// Path with every link above the root resolved; identifies the root in `diff`.
    pub final_path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TopFile {
    pub name: String,
    pub bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Row {
    /// `dir`, `link` or `root_files`.
    pub kind: String,
    /// Name relative to the root; empty for `root_files`.
    pub name: String,
    /// Observed logical bytes.
    pub bytes: u64,
    /// Observed allocated bytes.
    pub allocated_bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Number of enumeration failures inside this row.
    pub failures: u64,
    /// `complete`, `partial`, or `not_followed` for links.
    pub status: String,
    /// Largest files directly in the root; only on the `root_files` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top: Option<Vec<TopFile>>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Failure {
    /// Row the failure belongs to; empty when the root itself could not be listed further.
    pub row: String,
    /// Path relative to the root. Everything below this path is missing from the totals.
    pub path: String,
    pub code: Option<i32>,
    pub message: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Stats {
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub elapsed_ms: u64,
    pub peak_working_set_bytes: u64,
    pub threads: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Snapshot {
    pub format: u32,
    pub id: String,
    /// Always `root`.
    pub kind: String,
    pub rules: Rules,
    pub volume: VolumeIdent,
    pub root: RootIdent,
    pub started_at: String,
    pub ended_at: String,
    /// Volume free bytes when the measurement started and ended.
    pub free_bytes_start: u64,
    pub free_bytes_end: u64,
    /// Paths relative to the root that were skipped on purpose (the case folder).
    pub excluded: Vec<String>,
    /// Whether the root's child names were enumerated to the end.
    pub names_complete: bool,
    pub rows: Vec<Row>,
    pub failures: Vec<Failure>,
    pub stats: Stats,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VolumeSnapshot {
    pub format: u32,
    pub id: String,
    /// Always `volume`.
    pub kind: String,
    pub volume: VolumeIdent,
    pub measured_at: String,
    #[serde(flatten)]
    pub space: VolumeSpace,
    /// `total_bytes - free_bytes`, as reported by Windows for the whole volume.
    pub used_bytes: u64,
}
