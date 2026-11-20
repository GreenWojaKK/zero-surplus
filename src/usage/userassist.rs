//! UserAssist: per-user launch and focus counters kept by Explorer.

use std::io;

use serde_json::{Value, json};

use super::{AppQuery, source_result, status_of_io};
use crate::win::{self, RegKey};

const ROOT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist";
/// (GUID, group name): entries recorded for executables and for shortcuts.
const GROUPS: [(&str, &str); 2] = [
    ("{CEBFF5CD-ACE2-4F4F-9178-9926F41749EA}", "executable"),
    ("{F4E57C4B-2036-45F0-A9AB-443BCFE33D9F}", "shortcut"),
];
const RECORD_LEN: usize = 72;

#[derive(Debug, PartialEq)]
pub struct Record {
    pub run_count: u32,
    pub focus_count: u32,
    pub focus_ms: u32,
    /// FILETIME of the most recent launch; zero when never set.
    pub last_run: u64,
}

/// Decode a 72-byte record: run count @4, focus count @8, focus time (ms) @12, FILETIME @60.
pub fn parse_record(data: &[u8]) -> Option<Record> {
    if data.len() != RECORD_LEN {
        return None;
    }
    let u32_at = |offset: usize| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    Some(Record {
        run_count: u32_at(4),
        focus_count: u32_at(8),
        focus_ms: u32_at(12),
        last_run: u64::from_le_bytes(data[60..68].try_into().unwrap()),
    })
}

pub fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
            other => other,
        })
        .collect()
}

/// Replace a leading known-folder GUID (`{6D80...}\App\app.exe`) with that folder's path.
pub fn resolve_name(name: &str) -> Option<String> {
    if let Some(rest) = name.strip_prefix('{')
        && let Some((guid, tail)) = rest.split_once('}')
    {
        let folder = win::known_folder(&win::parse_guid(guid)?)?;
        return Some(format!("{}{}", folder.to_string_lossy(), tail));
    }
    let bytes = name.as_bytes();
    (bytes.len() > 2 && bytes[1] == b':' && bytes[2] == b'\\').then(|| name.to_owned())
}

struct GroupListing {
    group: &'static str,
    /// (decoded value name, record)
    records: Vec<(String, Record)>,
    other_values: usize,
}

fn read_group(guid: &str, group: &'static str) -> io::Result<GroupListing> {
    let key = RegKey::open_current_user(&format!(r"{ROOT}\{guid}\Count"))?;
    let mut listing = GroupListing { group, records: Vec::new(), other_values: 0 };
    for value in key.values()? {
        match parse_record(&value.data) {
            Some(record) => listing.records.push((rot13(&value.name), record)),
            // Session control values and other layouts are counted, not decoded.
            None => listing.other_values += 1,
        }
    }
    Ok(listing)
}

/// Every decoded record of both groups as `(decoded value name, record)`. Fails only when
/// neither group can be read.
pub fn all() -> io::Result<Vec<(String, Record)>> {
    let mut records = Vec::new();
    let mut first_error = None;
    let mut opened = 0;
    for (guid, group) in GROUPS {
        match read_group(guid, group) {
            Ok(listing) => {
                opened += 1;
                records.extend(listing.records);
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    if opened == 0 {
        return Err(first_error.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)));
    }
    Ok(records)
}

pub fn lookup(query: &AppQuery) -> Value {
    let mut groups = Vec::new();
    let mut first_error = None;
    for (guid, group) in GROUPS {
        match read_group(guid, group) {
            Ok(listing) => groups.push(listing),
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    if groups.is_empty() {
        let err = first_error.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound));
        return source_result(status_of_io(&err), "live", json!({ "message": err.to_string() }));
    }

    let mut matches = Vec::new();
    let mut summary = Vec::new();
    let mut decoded_total = 0;
    for listing in &groups {
        decoded_total += listing.records.len();
        summary.push(json!({
            "group": listing.group,
            "records": listing.records.len(),
            "values_with_other_length": listing.other_values,
        }));
        for (name, record) in &listing.records {
            let path = resolve_name(name);
            let candidate = path.as_deref().unwrap_or(name);
            let hit = match listing.group {
                "shortcut" => query.matches_shortcut(candidate),
                _ => query.matches(candidate),
            };
            if hit {
                matches.push(json!({
                    "group": listing.group,
                    "identifier": name,
                    "path": path,
                    "last_run": win::filetime_rfc3339(record.last_run),
                    "run_count": record.run_count,
                    "focus_count": record.focus_count,
                    "focus_ms": record.focus_ms,
                }));
            }
        }
    }
    let status = if !matches.is_empty() {
        "ok"
    } else if decoded_total == 0 && groups.iter().any(|listing| listing.other_values > 0) {
        "unsupported_format"
    } else {
        "no_record"
    };
    source_result(status, "live", json!({ "groups": summary, "records": matches }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_the_72_byte_record() {
        let mut data = vec![0u8; 72];
        data[4..8].copy_from_slice(&7u32.to_le_bytes());
        data[8..12].copy_from_slice(&3u32.to_le_bytes());
        data[12..16].copy_from_slice(&45_000u32.to_le_bytes());
        data[60..68].copy_from_slice(&134_000_000_000_000_000u64.to_le_bytes());
        assert_eq!(
            parse_record(&data),
            Some(Record { run_count: 7, focus_count: 3, focus_ms: 45_000, last_run: 134_000_000_000_000_000 })
        );
    }

    #[test]
    fn other_lengths_are_not_records() {
        assert_eq!(parse_record(&[0u8; 1612]), None);
        assert_eq!(parse_record(&[0u8; 16]), None);
    }

    #[test]
    fn rot13_round_trips_and_keeps_other_characters() {
        assert_eq!(rot13("P:\\Hfref\\n.rkr"), "C:\\Users\\a.exe");
        assert_eq!(rot13(&rot13("{6D809377-6AF0}\\App 1.exe")), "{6D809377-6AF0}\\App 1.exe");
    }

    #[test]
    fn plain_paths_resolve_to_themselves() {
        assert_eq!(resolve_name(r"C:\Tools\app.exe").as_deref(), Some(r"C:\Tools\app.exe"));
        assert_eq!(resolve_name("Microsoft.App_8wekyb3d8bbwe!App"), None);
    }
}
