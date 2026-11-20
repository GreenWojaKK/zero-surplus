//! Case folder layout, ID allocation and atomic writes.
//!
//! Every stored item gets a final name only after it was written completely: data goes to a
//! temporary name first and is renamed inside the same directory.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::output::{Fail, Res};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Scan,
    Volume,
    Diff,
    Usage,
    Refs,
    Watch,
    Note,
    Diagnosis,
    Source,
    Activity,
}

impl Kind {
    pub const ALL: [Kind; 10] = [
        Kind::Scan,
        Kind::Volume,
        Kind::Diff,
        Kind::Usage,
        Kind::Refs,
        Kind::Watch,
        Kind::Note,
        Kind::Diagnosis,
        Kind::Source,
        Kind::Activity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Scan => "scan",
            Kind::Volume => "volume",
            Kind::Diff => "diff",
            Kind::Usage => "usage",
            Kind::Refs => "refs",
            Kind::Watch => "watch",
            Kind::Note => "note",
            Kind::Diagnosis => "diagnosis",
            Kind::Source => "source",
            Kind::Activity => "activity",
        }
    }

    fn prefix(self) -> &'static str {
        match self {
            Kind::Scan => "scan",
            Kind::Volume => "vol",
            Kind::Diff => "diff",
            Kind::Usage => "usage",
            Kind::Refs => "refs",
            Kind::Watch => "watch",
            Kind::Note => "note",
            Kind::Diagnosis => "diag",
            Kind::Source => "src",
            Kind::Activity => "act",
        }
    }

    fn dir(self) -> &'static str {
        match self {
            Kind::Scan | Kind::Volume => "scans",
            Kind::Diff => "diffs",
            Kind::Usage => "usage",
            Kind::Refs => "refs",
            Kind::Watch => "watches",
            Kind::Note => "notes",
            Kind::Diagnosis => "diagnoses",
            Kind::Source => "sources",
            Kind::Activity => "activity",
        }
    }

    /// File name of the item's main file, relative to the kind's directory.
    fn file_name(self, id: &str) -> String {
        match self {
            Kind::Watch => format!("{id}.jsonl"),
            Kind::Note => format!("{id}.txt"),
            Kind::Diagnosis => format!("{id}.md"),
            Kind::Source => format!("{id}/source.json"),
            _ => format!("{id}.json"),
        }
    }

    pub fn of_id(id: &str) -> Option<Kind> {
        let prefix = id.split('-').next()?;
        Kind::ALL.into_iter().find(|kind| kind.prefix() == prefix)
    }

    pub fn from_name(name: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|kind| kind.name() == name)
    }
}

pub struct Case {
    root: PathBuf,
}

impl Case {
    /// Open a case folder for writing, creating it when missing.
    pub fn create(path: &Path) -> Res<Self> {
        fs::create_dir_all(path).map_err(|err| Fail::io(&format!("create case folder {}", path.display()), &err))?;
        Self::open(path)
    }

    /// Open an existing case folder.
    pub fn open(path: &Path) -> Res<Self> {
        let root = std::path::absolute(path).map_err(|err| Fail::io("resolve case folder", &err))?;
        if !root.is_dir() {
            return Err(Fail::input(format!("case folder does not exist: {}", root.display())));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn new_id(&self, kind: Kind) -> String {
        let now = chrono::Utc::now();
        let nanos = now.timestamp_subsec_nanos();
        let mut salt = (nanos ^ std::process::id().wrapping_mul(2_654_435_761)) & 0xffff;
        loop {
            let id = format!("{}-{}-{salt:04x}", kind.prefix(), now.format("%Y%m%dT%H%M%SZ"));
            let taken = self.path_of(&id).is_some_and(|path| {
                let mut tmp = path.as_os_str().to_owned();
                tmp.push(".tmp");
                path.exists() || Path::new(&tmp).exists()
            });
            if !taken {
                return id;
            }
            salt = (salt + 1) & 0xffff;
        }
    }

    pub fn dir(&self, kind: Kind) -> PathBuf {
        self.root.join(kind.dir())
    }

    /// Path of an item's main file, whether or not it exists.
    pub fn path_of(&self, id: &str) -> Option<PathBuf> {
        if id.contains(['/', '\\', ':']) || id.contains("..") {
            return None;
        }
        let kind = Kind::of_id(id)?;
        Some(self.dir(kind).join(kind.file_name(id)))
    }

    /// Path of a completed item; `None` when the ID is unknown or was never finished.
    pub fn existing(&self, id: &str) -> Option<PathBuf> {
        self.path_of(id).filter(|path| path.is_file())
    }

    pub fn write_json<T: Serialize>(&self, id: &str, value: &T) -> Res<PathBuf> {
        let text = serde_json::to_vec_pretty(value).map_err(|err| Fail::run("serialize", err.to_string()))?;
        self.write_bytes(id, &text)
    }

    pub fn write_bytes(&self, id: &str, bytes: &[u8]) -> Res<PathBuf> {
        let path = self.path_of(id).ok_or_else(|| Fail::input(format!("invalid id: {id}")))?;
        write_atomic(&path, bytes)?;
        Ok(path)
    }

    pub fn read_json(&self, id: &str) -> Res<Value> {
        let path = self
            .existing(id)
            .ok_or_else(|| Fail::input(format!("no stored result with id {id} in {}", self.root.display())))?;
        let bytes = fs::read(&path).map_err(|err| Fail::io(&format!("read {}", path.display()), &err))?;
        serde_json::from_slice(&bytes).map_err(|err| Fail::run("parse", format!("{}: {err}", path.display())))
    }

    /// IDs of completed items of one kind, newest first.
    pub fn ids(&self, kind: Kind) -> Vec<String> {
        let mut ids = Vec::new();
        let Ok(entries) = fs::read_dir(self.dir(kind)) else {
            return ids;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = match kind {
                Kind::Source => name.clone(),
                _ => match name.rsplit_once('.') {
                    Some((stem, _)) => stem.to_owned(),
                    None => continue,
                },
            };
            if Kind::of_id(&id) == Some(kind) && self.existing(&id).is_some() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids.sort();
        ids.reverse();
        ids
    }

    /// Path relative to the case root, with forward slashes.
    pub fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root).unwrap_or(path).to_string_lossy().replace('\\', "/")
    }
}

/// Write to `<name>.tmp`, then rename to the final name in the same directory.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Res<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| Fail::io(&format!("create {}", parent.display()), &err))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes).map_err(|err| Fail::io(&format!("write {}", tmp.display()), &err))?;
    if std::env::var_os("ZERO_SURPLUS_TEST_ABORT_BEFORE_COMMIT").is_some() {
        // Test hook: behave like a measurement cancelled before it was committed.
        std::process::exit(130);
    }
    fs::rename(&tmp, path).map_err(|err| Fail::io(&format!("rename to {}", path.display()), &err))
}
