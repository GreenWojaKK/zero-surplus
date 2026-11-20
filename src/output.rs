//! JSON output and the error type shared by every command.

use serde_json::{Value, json};

/// Exit codes: 1 = execution failure, 2 = input error, 3 = the request cannot be carried out
/// in the current state (incomparable snapshots, missing references, declined elevation).
#[derive(Debug)]
pub struct Fail {
    pub code: i32,
    pub kind: &'static str,
    pub message: String,
    pub detail: Option<Value>,
}

pub type Res<T> = Result<T, Fail>;

impl Fail {
    pub fn run(kind: &'static str, message: impl Into<String>) -> Self {
        Self { code: 1, kind, message: message.into(), detail: None }
    }

    pub fn input(message: impl Into<String>) -> Self {
        Self { code: 2, kind: "invalid_input", message: message.into(), detail: None }
    }

    pub fn blocked(kind: &'static str, message: impl Into<String>, detail: Value) -> Self {
        Self { code: 3, kind, message: message.into(), detail: Some(detail) }
    }

    pub fn io(context: &str, err: &std::io::Error) -> Self {
        Self::run("io_error", format!("{context}: {err}"))
    }

    pub fn to_json(&self) -> Value {
        let mut error = json!({ "kind": self.kind, "message": self.message });
        if let Some(detail) = &self.detail {
            error["detail"] = detail.clone();
        }
        json!({ "error": error })
    }
}

pub fn print(value: &Value, pretty: bool) {
    let text = if pretty {
        serde_json::to_string_pretty(value)
    } else {
        serde_json::to_string(value)
    };
    // A reader that closed the pipe early must not turn a finished command into a panic.
    use std::io::Write;
    let _ = writeln!(std::io::stdout().lock(), "{}", text.unwrap_or_else(|_| "{}".to_owned()));
}

/// One line of a JSON Lines stream (used by `watch`).
pub fn print_line(value: &Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string(value).unwrap_or_default());
    let _ = out.flush();
}
