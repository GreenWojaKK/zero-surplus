//! `watch`: create/delete/rename events under a path for a fixed time, as JSON Lines.
//!
//! The existing parent directory is watched with its subtree, so the target itself may be
//! created or deleted while watching. Windows change notifications carry no process identity;
//! the `process` field is therefore always null. A buffer overflow is reported as a `gap`.

use std::ffi::c_void;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ACTION_ADDED, FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_DIR_NAME,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    ReadDirectoryChangesW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent, WaitForSingleObject};

use crate::case::{Case, Kind};
use crate::cli::WatchArgs;
use crate::output::{self, Fail, Res};
use crate::win;

/// Buffer handed to the kernel for one batch of notifications (in `u32` units: 256 KiB).
const BUFFER_UNITS: usize = 64 * 1024;
const POLL: Duration = Duration::from_millis(200);

pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let (number, unit) = match text.char_indices().find(|(_, c)| !c.is_ascii_digit()) {
        Some((index, _)) => text.split_at(index),
        None => (text, "s"),
    };
    let value: u64 = number.parse().ok()?;
    let seconds = match unit {
        "s" => value,
        "m" => value.checked_mul(60)?,
        "h" => value.checked_mul(3600)?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

/// Where events go: standard output always, and the log in the case folder when one is given.
struct Sink {
    log: Option<Log>,
}

struct Log {
    file: fs::File,
    tmp: PathBuf,
    path: PathBuf,
    /// First write error; nothing more is written after it.
    error: Option<String>,
}

impl Sink {
    fn emit(&mut self, value: &Value) {
        output::print_line(value);
        self.write(value);
    }

    fn write(&mut self, value: &Value) {
        if let Some(log) = &mut self.log
            && log.error.is_none()
            && let Err(err) = writeln!(log.file, "{}", serde_json::to_string(value).unwrap_or_default())
        {
            log.error = Some(format!("write {}: {err}", log.tmp.display()));
        }
    }

    /// Write the last line and give the log its final name. Returns the final path, or why the
    /// log was not stored; an incomplete log is removed. `None` when there is no log.
    fn finish(mut self, last: &Value) -> Option<Result<PathBuf, String>> {
        self.write(last);
        let log = self.log?;
        let stored = match log.error {
            Some(error) => Err(error),
            None => log
                .file
                .sync_all()
                .map_err(|err| format!("flush {}: {err}", log.tmp.display()))
                .and_then(|()| {
                    drop(log.file);
                    fs::rename(&log.tmp, &log.path).map_err(|err| format!("rename to {}: {err}", log.path.display()))
                }),
        };
        if stored.is_err() {
            let _ = fs::remove_file(&log.tmp);
        }
        Some(stored.map(|()| log.path))
    }
}

struct Handles {
    directory: HANDLE,
    event: HANDLE,
}

impl Drop for Handles {
    fn drop(&mut self) {
        // SAFETY: both handles were created by `open` and are closed once.
        unsafe {
            CloseHandle(self.directory);
            CloseHandle(self.event);
        }
    }
}

fn open(directory: &Path) -> std::io::Result<Handles> {
    let name = win::wide(directory);
    // SAFETY: `name` is null-terminated; unused parameters are null.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_LIST_DIRECTORY,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: creates an unnamed manual-reset event.
    let event = unsafe { CreateEventW(null(), 1, 0, null()) };
    if event.is_null() {
        let err = std::io::Error::last_os_error();
        // SAFETY: `handle` is valid and owned here.
        unsafe { CloseHandle(handle) };
        return Err(err);
    }
    Ok(Handles { directory: handle, event })
}

/// Queue one notification read. `Ok(())` means the request is registered with the kernel.
fn request(handles: &Handles, buffer: &mut [u32], overlapped: &mut OVERLAPPED) -> std::io::Result<()> {
    // SAFETY: the event belongs to this watch.
    unsafe { ResetEvent(handles.event) };
    *overlapped = unsafe { std::mem::zeroed() };
    overlapped.hEvent = handles.event;
    // SAFETY: buffer and overlapped outlive the request; it is cancelled before they are dropped.
    let ok = unsafe {
        ReadDirectoryChangesW(
            handles.directory,
            buffer.as_mut_ptr().cast::<c_void>(),
            (buffer.len() * 4) as u32,
            1,
            FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_DIR_NAME,
            null_mut(),
            overlapped,
            None,
        )
    };
    if ok == 0 {
        // SAFETY: reads the calling thread's last error.
        let code = unsafe { GetLastError() };
        if code != ERROR_IO_PENDING {
            return Err(std::io::Error::from_raw_os_error(code as i32));
        }
    }
    Ok(())
}

/// Decode `(action, path relative to the watched directory)` records from a filled buffer.
fn decode(buffer: &[u32], bytes: usize) -> Vec<(u32, String)> {
    let mut records = Vec::new();
    let mut offset = 0usize;
    // SAFETY: a `u32` slice is always valid to view as bytes.
    let data = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), buffer.len() * 4) };
    let data = &data[..bytes.min(data.len())];
    loop {
        let Some(header) = data.get(offset..offset + 12) else {
            break;
        };
        let next = u32::from_le_bytes(header[0..4].try_into().unwrap()) as usize;
        let action = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let name_bytes = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
        let Some(name) = data.get(offset + 12..offset + 12 + name_bytes) else {
            break;
        };
        let units: Vec<u16> = name.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        records.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        offset += next;
    }
    records
}

pub fn run(args: WatchArgs) -> Res<Option<Value>> {
    let duration = parse_duration(&args.duration)
        .ok_or_else(|| Fail::input(format!("--duration takes a number with s, m or h, got {}", args.duration)))?;
    let target = std::path::absolute(&args.path).map_err(|err| Fail::io("resolve watch path", &err))?;

    // Watch the nearest existing ancestor; a drive root has none and is watched itself.
    let mut directory = target.parent().map(Path::to_path_buf);
    while let Some(candidate) = &directory {
        if candidate.is_dir() {
            break;
        }
        directory = candidate.parent().map(Path::to_path_buf);
    }
    let (directory, everything) = match directory {
        Some(directory) => (directory, false),
        None => (target.clone(), true),
    };
    let target_key = win::path_key(&target);
    let below_key = format!("{target_key}\\");
    let inside = |path: &Path| {
        let key = win::path_key(path);
        everything || key == target_key || key.starts_with(&below_key)
    };

    let case = args.case.as_deref().map(Case::create).transpose()?;
    let id = case.as_ref().map(|case| case.new_id(Kind::Watch));
    let mut sink = Sink { log: None };
    if let (Some(case), Some(id)) = (&case, &id) {
        let path = case.path_of(id).ok_or_else(|| Fail::run("watch", "cannot name the watch log"))?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| Fail::io("create watch folder", &err))?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let file = fs::File::create(&tmp).map_err(|err| Fail::io("create watch log", &err))?;
        sink.log = Some(Log { file, tmp, path, error: None });
    }

    win::install_interrupt_flag();
    let handles = open(&directory).map_err(|err| Fail::io(&format!("open {}", directory.display()), &err))?;
    let mut buffer = vec![0u32; BUFFER_UNITS];
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    request(&handles, &mut buffer, &mut overlapped).map_err(|err| Fail::io("register change notifications", &err))?;

    let started = Instant::now();
    let mut last_received = win::now_rfc3339();
    sink.emit(&json!({
        "event": "ready",
        "id": id,
        "time": last_received,
        "path": target.to_string_lossy(),
        "watching": directory.to_string_lossy(),
        "target_exists": target.exists(),
        "duration_seconds": duration.as_secs(),
    }));

    let (mut events, mut gaps) = (0u64, 0u64);
    let mut pending_old: Option<PathBuf> = None;
    let mut failure: Option<String> = None;
    let reason = loop {
        if win::interrupted() {
            break "interrupted";
        }
        let Some(remaining) = duration.checked_sub(started.elapsed()) else {
            break "duration_elapsed";
        };
        let wait = remaining.min(POLL).as_millis() as u32;
        // SAFETY: the event handle is valid for the lifetime of `handles`.
        if unsafe { WaitForSingleObject(handles.event, wait) } != WAIT_OBJECT_0 {
            continue;
        }
        let mut bytes = 0u32;
        // SAFETY: `overlapped` belongs to the request that just signalled.
        let ok = unsafe { GetOverlappedResult(handles.directory, &overlapped, &mut bytes, 0) };
        let now = win::now_rfc3339();
        // SAFETY: reads the calling thread's last error.
        let error = if ok == 0 { unsafe { GetLastError() } } else { 0 };
        if (ok != 0 && bytes == 0) || error == ERROR_NOTIFY_ENUM_DIR {
            // The kernel dropped notifications: everything since the last batch is unknown.
            gaps += 1;
            sink.emit(&json!({ "event": "gap", "reason": "buffer_overflow", "from": last_received, "to": now }));
        } else if ok == 0 {
            failure = Some(std::io::Error::from_raw_os_error(error as i32).to_string());
            break "error";
        } else {
            for (action, relative) in decode(&buffer, bytes as usize) {
                let path = directory.join(&relative);
                let event = match action {
                    FILE_ACTION_ADDED => inside(&path).then(|| ("created", None)),
                    FILE_ACTION_REMOVED => inside(&path).then(|| ("deleted", None)),
                    FILE_ACTION_RENAMED_OLD_NAME => {
                        pending_old = Some(path.clone());
                        None
                    }
                    FILE_ACTION_RENAMED_NEW_NAME => {
                        let old = pending_old.take();
                        (inside(&path) || old.as_deref().is_some_and(&inside)).then(|| ("renamed", old))
                    }
                    _ => None,
                };
                if let Some((kind, old)) = event {
                    events += 1;
                    let mut line = json!({ "event": kind, "time": now, "path": path.to_string_lossy() });
                    if kind == "renamed" {
                        line["old_path"] = json!(old.map(|old| old.to_string_lossy().into_owned()));
                    }
                    line["process"] = Value::Null;
                    sink.emit(&line);
                }
            }
        }
        last_received = now;
        if let Err(err) = request(&handles, &mut buffer, &mut overlapped) {
            failure = Some(err.to_string());
            break "error";
        }
    };

    // SAFETY: cancels the outstanding read and waits until the kernel no longer uses the buffer.
    unsafe {
        CancelIoEx(handles.directory, &overlapped);
        let mut ignored = 0u32;
        GetOverlappedResult(handles.directory, &overlapped, &mut ignored, 1);
    }
    let mut end = json!({
        "event": "end",
        "time": win::now_rfc3339(),
        "reason": reason,
        "target_exists": target.exists(),
        "events": events,
        "gaps": gaps,
    });
    if let Some(message) = failure {
        end["message"] = json!(message);
    }
    // The stored log ends with the same line, without the outcome of storing it.
    let stored = sink.finish(&end);
    match (&stored, &case) {
        (Some(Ok(path)), Some(case)) => end["stored"] = json!(case.relative(path)),
        (Some(Err(reason)), _) => {
            end["stored"] = Value::Null;
            end["store_error"] = json!(reason);
        }
        _ => {}
    }
    output::print_line(&end);
    match stored {
        Some(Err(reason)) => Err(Fail::run("log_not_stored", format!("the watch log was not stored: {reason}"))),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_take_seconds_minutes_and_hours() {
        assert_eq!(parse_duration("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("15"), Some(Duration::from_secs(15)));
        assert_eq!(parse_duration("soon"), None);
        assert_eq!(parse_duration("5d"), None);
    }

    #[test]
    fn decodes_chained_notification_records() {
        // Two records: ADDED "a", REMOVED "bc".
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&[b'a', 0, 0, 0]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&2u32.to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        bytes.extend_from_slice(&[b'b', 0, b'c', 0]);
        let buffer: Vec<u32> =
            bytes.chunks_exact(4).map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap())).collect();
        assert_eq!(decode(&buffer, bytes.len()), vec![(1, "a".to_owned()), (2, "bc".to_owned())]);
    }
}
