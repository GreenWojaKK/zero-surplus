#![allow(dead_code)]

pub mod ese;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// A unique folder under the system temp directory, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(label: &str) -> Self {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("zero-surplus-test-{label}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn join(&self, rest: &str) -> PathBuf {
        self.0.join(rest.replace('/', "\\"))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub struct Outcome {
    pub code: i32,
    pub json: Value,
    pub stdout: String,
    pub stderr: String,
}

pub fn cli(args: &[&str]) -> Outcome {
    cli_env(args, &[])
}

pub fn cli_env(args: &[&str], env: &[(&str, &str)]) -> Outcome {
    let mut command = Command::new(env!("CARGO_BIN_EXE_zero-surplus"));
    command.args(args);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = command.output().expect("run zero-surplus");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let json = serde_json::from_str(stdout.trim()).unwrap_or(Value::Null);
    Outcome {
        code: output.status.code().unwrap_or(-1),
        json,
        stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

pub fn write_file(path: &Path, bytes: usize) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, vec![b'x'; bytes]).unwrap();
}

pub fn text(path: &Path) -> &str {
    path.to_str().unwrap()
}

pub fn junction(link: &Path, target: &Path) {
    let status = Command::new("cmd")
        .args(["/c", "mklink", "/J", text(link), text(target)])
        .output()
        .expect("run mklink");
    assert!(status.status.success(), "mklink failed: {}", String::from_utf8_lossy(&status.stderr));
}

fn current_user() -> String {
    let output = Command::new("whoami").output().expect("run whoami");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Deny the current user the right to list `dir`; undone by [`allow_listing`].
pub fn deny_listing(dir: &Path) {
    let rule = format!("{}:(RD)", current_user());
    let output = Command::new("icacls").args([text(dir), "/deny", &rule]).output().expect("run icacls");
    assert!(output.status.success(), "icacls deny failed");
}

pub fn allow_listing(dir: &Path) {
    let _ = Command::new("icacls").args([text(dir), "/remove:d", &current_user()]).output();
}

/// Find a row by name in a JSON array of rows.
pub fn row<'a>(rows: &'a Value, name: &str) -> &'a Value {
    rows.as_array()
        .and_then(|rows| rows.iter().find(|row| row["name"] == name))
        .unwrap_or_else(|| panic!("row {name} not found in {rows}"))
}

pub fn stored_json(case: &Path, relative: &str) -> Value {
    let bytes = std::fs::read(case.join(relative)).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
