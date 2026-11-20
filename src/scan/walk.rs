//! Directory traversal. All `dua-core` calls live in this file.
//!
//! The root's direct entries are read once; every direct child directory becomes one numbered
//! walk root. Entries are folded into per-row totals as they arrive, so no per-file tree or path
//! index is kept. Junctions and symbolic links are reported by `dua-core` but never entered, and
//! hard links are not deduplicated: each path adds its file length.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::path::{Path, PathBuf};

use dua_core::{Entry, Error, Options, Order, RootEvent};

pub struct RawFailure {
    pub path: PathBuf,
    pub code: Option<i32>,
    pub message: String,
}

#[derive(Default)]
pub struct Tally {
    pub bytes: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
    pub links: u64,
    pub failures: Vec<RawFailure>,
    pub finished: bool,
}

pub struct DirRow {
    pub name: String,
    pub tally: Tally,
}

pub struct WalkOutput {
    pub dirs: Vec<DirRow>,
    /// Names of direct children that are junctions or symbolic links.
    pub links: Vec<String>,
    /// Totals of the files directly in the root.
    pub root_files: Tally,
    pub top_files: Vec<(String, u64)>,
    /// Failures while listing the root itself.
    pub root_failures: Vec<RawFailure>,
    pub names_complete: bool,
    /// Absolute paths skipped on purpose.
    pub excluded: Vec<PathBuf>,
}

pub enum WalkError {
    /// The root directory could not be opened.
    Open(RawFailure),
    Cancelled,
}

fn failure(err: &Error, fallback: &Path) -> RawFailure {
    RawFailure {
        path: err.path().unwrap_or(fallback).to_path_buf(),
        code: err.io_error().raw_os_error(),
        message: err.io_error().to_string(),
    }
}

fn is_excluded(entry: &Entry, exclude: Option<&(PathBuf, std::ffi::OsString)>) -> bool {
    match exclude {
        Some((parent, name)) => {
            entry.file_name.eq_ignore_ascii_case(name)
                && crate::win::path_key(&entry.parent_path) == crate::win::path_key(parent)
        }
        None => false,
    }
}

fn add_file(tally: &mut Tally, entry: &Entry) {
    match &entry.metadata {
        Some(Ok(metadata)) => {
            tally.files += 1;
            tally.bytes += metadata.len();
            tally.allocated += metadata.allocated_size();
        }
        Some(Err(err)) => tally.failures.push(RawFailure {
            path: entry.path(),
            code: err.raw_os_error(),
            message: err.to_string(),
        }),
        None => tally.failures.push(RawFailure {
            path: entry.path(),
            code: None,
            message: "metadata was not returned".to_owned(),
        }),
    }
}

/// Measure the direct children of `root`. `exclude` is an absolute directory to skip.
pub fn walk(
    root: &Path,
    exclude: Option<&Path>,
    threads: usize,
    top_files: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<WalkOutput, WalkError> {
    let exclude = exclude.and_then(|path| Some((path.parent()?.to_path_buf(), path.file_name()?.to_owned())));
    let listing = dua_core::read_dir(root, Options::default()).map_err(|err| WalkError::Open(failure(&err, root)))?;

    let mut output = WalkOutput {
        dirs: Vec::new(),
        links: Vec::new(),
        root_files: Tally::default(),
        top_files: Vec::new(),
        root_failures: Vec::new(),
        names_complete: true,
        excluded: Vec::new(),
    };
    let mut heap: BinaryHeap<Reverse<(u64, String)>> = BinaryHeap::new();
    let mut roots: Vec<(usize, Result<Entry, Error>)> = Vec::new();

    for item in listing {
        let entry = match item {
            Ok(entry) => entry,
            Err(err) => {
                // The listing stopped early: names after this point are unknown.
                output.names_complete = false;
                output.root_failures.push(failure(&err, root));
                continue;
            }
        };
        let name = entry.file_name.to_string_lossy().into_owned();
        if entry.file_type.is_symlink() {
            output.links.push(name);
        } else if entry.file_type.is_dir() {
            if is_excluded(&entry, exclude.as_ref()) {
                output.excluded.push(entry.path());
                continue;
            }
            roots.push((output.dirs.len(), Ok(entry)));
            output.dirs.push(DirRow { name, tally: Tally::default() });
        } else {
            add_file(&mut output.root_files, &entry);
            if let Some(Ok(metadata)) = &entry.metadata
                && top_files > 0
            {
                heap.push(Reverse((metadata.len(), name)));
                if heap.len() > top_files {
                    heap.pop();
                }
            }
        }
    }
    output.root_files.finished = true;
    output.top_files = heap.into_sorted_vec().into_iter().map(|Reverse((bytes, name))| (name, bytes)).collect();

    let predicate_exclude = exclude.clone();
    let events = dua_core::walk_root_entries(
        roots,
        threads.max(1),
        Order::Completion,
        Options::default(),
        move |_root, entry| !is_excluded(entry, predicate_exclude.as_ref()),
    );
    for (index, event) in events {
        if cancelled() {
            return Err(WalkError::Cancelled);
        }
        let Some(row) = output.dirs.get_mut(index) else {
            continue;
        };
        match event {
            RootEvent::Finished => row.tally.finished = true,
            RootEvent::Entry(Err(err)) => {
                let fallback = root.join(&row.name);
                row.tally.failures.push(failure(&err, &fallback));
            }
            RootEvent::Entry(Ok(entry)) => {
                if entry.depth == 0 {
                    // The direct child itself; its contents follow as deeper entries.
                    continue;
                }
                if entry.file_type.is_symlink() {
                    row.tally.links += 1;
                } else if entry.file_type.is_dir() {
                    if is_excluded(&entry, exclude.as_ref()) {
                        output.excluded.push(entry.path());
                    } else {
                        row.tally.dirs += 1;
                    }
                } else {
                    add_file(&mut row.tally, &entry);
                }
            }
        }
    }
    Ok(output)
}
