//! Finding and identifying Python environments.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::win;

pub struct Env {
    /// Path the environment was first found at.
    pub path: PathBuf,
    /// Path with links resolved; two paths with the same final path are one environment.
    pub final_path: PathBuf,
    /// Other paths that lead to the same environment.
    pub aliases: Vec<PathBuf>,
    /// `venv` (has `pyvenv.cfg`), `conda` (has `conda-meta`), or `unidentified`.
    pub kind: &'static str,
    pub name: String,
    pub version: Option<String>,
    /// `home` from `pyvenv.cfg`: the Python the environment was created from. This is not a
    /// project reference.
    pub base_python: Option<String>,
    pub shared: bool,
    /// Name of the project whose folder contains the environment.
    pub located_in_project: Option<String>,
}

impl Env {
    /// Interpreter paths an execution record can carry for this environment.
    pub fn python_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for root in std::iter::once(&self.final_path).chain(std::iter::once(&self.path)).chain(&self.aliases) {
            for relative in ["python.exe", "pythonw.exe", r"Scripts\python.exe", r"Scripts\pythonw.exe"] {
                let path = root.join(relative);
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        paths
    }
}

/// `(kind, version, base python)` when `dir` looks like an environment.
pub fn identify(dir: &Path) -> Option<(&'static str, Option<String>, Option<String>)> {
    if let Ok(text) = fs::read_to_string(dir.join("pyvenv.cfg")) {
        let mut version = None;
        let mut home = None;
        for line in text.lines() {
            if let Some((key, value)) = line.split_once('=') {
                match key.trim() {
                    "version" | "version_info" => version = Some(value.trim().to_owned()),
                    "home" => home = Some(value.trim().to_owned()),
                    _ => {}
                }
            }
        }
        return Some(("venv", version, home));
    }
    let meta = dir.join("conda-meta");
    if meta.is_dir() {
        // conda-meta/python-3.12.7-h14ffc60_0.json names the interpreter version.
        let version = fs::read_dir(&meta).ok().and_then(|entries| {
            entries.flatten().find_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let rest = name.strip_prefix("python-")?;
                rest.ends_with(".json").then(|| rest.split('-').next().unwrap_or(rest).to_owned())
            })
        });
        return Some(("conda", version, None));
    }
    None
}

#[derive(Default)]
pub struct Registry {
    pub envs: Vec<Env>,
    by_key: HashMap<String, usize>,
}

impl Registry {
    /// Register `dir` as an environment. `force` registers it even without markers.
    pub fn add(&mut self, dir: &Path, force: bool) -> Option<usize> {
        let absolute = std::path::absolute(dir).ok()?;
        let identified = identify(&absolute);
        if identified.is_none() && !(force && absolute.is_dir()) {
            return None;
        }
        let final_path = win::final_path(&absolute).unwrap_or_else(|_| absolute.clone());
        let key = win::path_key(&final_path);
        if let Some(&index) = self.by_key.get(&key) {
            let env = &mut self.envs[index];
            let known = std::iter::once(&env.path).chain(&env.aliases).any(|path| win::path_key(path) == win::path_key(&absolute));
            if !known {
                env.aliases.push(absolute.clone());
                self.by_key.insert(win::path_key(&absolute), index);
            }
            return Some(index);
        }
        let (kind, version, base_python) = identified.unwrap_or(("unidentified", None, None));
        let index = self.envs.len();
        self.by_key.insert(key, index);
        self.by_key.insert(win::path_key(&absolute), index);
        self.envs.push(Env {
            name: final_path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
            path: absolute,
            final_path,
            aliases: Vec::new(),
            kind,
            version,
            base_python,
            shared: false,
            located_in_project: None,
        });
        Some(index)
    }

    /// Register every direct child of `store` that is an environment.
    pub fn add_children(&mut self, store: &Path) -> std::io::Result<()> {
        for entry in fs::read_dir(store)? {
            let path = entry?.path();
            if path.is_dir() {
                self.add(&path, false);
            }
        }
        Ok(())
    }

    /// Environment that contains `path` (an interpreter, an activate script, or the folder).
    pub fn containing(&self, path: &Path) -> Option<usize> {
        let mut candidates = vec![path.to_path_buf()];
        if let Ok(resolved) = win::final_path(path) {
            candidates.push(resolved);
        }
        for candidate in candidates {
            // The environment folder is at most a few levels above its interpreter.
            for ancestor in candidate.ancestors().take(4) {
                if let Some(&index) = self.by_key.get(&win::path_key(ancestor)) {
                    return Some(index);
                }
            }
        }
        None
    }

    /// Conda environments with this folder name.
    pub fn named(&self, name: &str) -> Vec<usize> {
        self.envs
            .iter()
            .enumerate()
            .filter(|(_, env)| env.kind == "conda" && env.name.eq_ignore_ascii_case(name))
            .map(|(index, _)| index)
            .collect()
    }
}
