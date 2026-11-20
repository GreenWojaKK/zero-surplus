//! ShellBags: folders opened in Explorer, kept as a tree of shell items under BagMRU.

use std::io;

use serde_json::{Value, json};
use winparsingtools::structs::shell_items::{Name, ShellItem};

use super::{source_result, status_of_io};
use crate::win::{self, RegKey};

/// (registry path under HKEY_CURRENT_USER, hive the key is stored in)
const ROOTS: [(&str, &str); 2] = [
    (r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\BagMRU", "UsrClass.dat"),
    (r"Software\Microsoft\Windows\Shell\BagMRU", "NTUSER.DAT"),
];
const MAX_DEPTH: usize = 64;

pub struct Node {
    pub hive: &'static str,
    /// Item names joined from the tree root, e.g. `MyComputer\C:\Users\me\Documents`.
    pub display_path: String,
    /// File-system path when the chain contains a drive item.
    pub fs_path: Option<String>,
    /// Last write time of the node's key (FILETIME).
    pub key_last_write: Option<u64>,
}

pub struct Walk {
    pub nodes: Vec<Node>,
    pub undecoded_items: usize,
}

fn is_drive(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Long name from the item's `0xBEEF0004` extension block.
///
/// `winparsingtools` 2.1.4 reads this UTF-16 name as UTF-8 whenever the block also carries a
/// localized name (`Users`, `Program Files`, ...), which leaves only the first character. The
/// name is therefore read here; item classification stays with the crate.
pub fn long_name(item: &[u8]) -> Option<String> {
    const SIGNATURE: [u8; 4] = [0x04, 0x00, 0xef, 0xbe];
    let signature = item.windows(4).position(|window| window == SIGNATURE)?;
    let version = u16::from_le_bytes(item.get(signature.checked_sub(2)?..signature)?.try_into().ok()?);
    if version < 3 {
        return None;
    }
    // creation and access time (8), identifier (2)
    let mut offset = signature + 4 + 10;
    if version >= 7 {
        // unknown (2), file reference (8), unknown (8)
        offset += 18;
    }
    // size of the localized name
    offset += 2;
    if version >= 9 {
        offset += 4;
    }
    if version >= 8 {
        offset += 4;
    }
    let units: Vec<u16> = item
        .get(offset..)?
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    (!units.is_empty()).then(|| String::from_utf16_lossy(&units))
}

fn visit(key: &RegKey, hive: &'static str, display: &str, fs: Option<&str>, depth: usize, walk: &mut Walk) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(values) = key.values() else {
        return;
    };
    for value in values {
        // Child items are stored under numeric value names; the same number names the subkey.
        if value.name.is_empty() || !value.name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let name = match (long_name(&value.data), ShellItem::from_buffer(&value.data)) {
            (Some(name), _) => name,
            (None, Ok(item)) => item.name(),
            (None, Err(_)) => {
                walk.undecoded_items += 1;
                continue;
            }
        };
        let name = name.trim_end_matches(['\0', '\\']).to_owned();
        let child_display = if display.is_empty() { name.clone() } else { format!("{display}\\{name}") };
        let child_fs = if is_drive(&name) {
            Some(name.clone())
        } else {
            fs.filter(|_| name != "{NONE}").map(|parent| format!("{parent}\\{name}"))
        };
        let Ok(child_key) = key.open_subkey(&value.name) else {
            continue;
        };
        walk.nodes.push(Node {
            hive,
            display_path: child_display.clone(),
            fs_path: child_fs.clone(),
            key_last_write: child_key.last_write(),
        });
        visit(&child_key, hive, &child_display, child_fs.as_deref(), depth + 1, walk);
    }
}

pub fn walk_all() -> io::Result<Walk> {
    let mut walk = Walk { nodes: Vec::new(), undecoded_items: 0 };
    let mut opened = 0;
    let mut first_error = None;
    for (path, hive) in ROOTS {
        match RegKey::open_current_user(path) {
            Ok(key) => {
                opened += 1;
                visit(&key, hive, "", None, 0, &mut walk);
            }
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    if opened == 0 {
        return Err(first_error.unwrap_or_else(|| io::Error::from(io::ErrorKind::NotFound)));
    }
    Ok(walk)
}

/// Nodes for `folder` and everything recorded below it.
pub fn lookup(folder: &std::path::Path, top: usize) -> Value {
    let walk = match walk_all() {
        Ok(walk) => walk,
        Err(err) => return source_result(status_of_io(&err), "live", json!({ "message": err.to_string() })),
    };
    let target = win::path_key(folder);
    let below = format!("{target}\\");
    let mut matches: Vec<&Node> = walk
        .nodes
        .iter()
        .filter(|node| {
            node.fs_path.as_deref().is_some_and(|path| {
                let key = win::path_key(std::path::Path::new(path));
                key == target || key.starts_with(&below)
            })
        })
        .collect();
    matches.sort_by(|left, right| right.key_last_write.cmp(&left.key_last_write));
    let exact = matches
        .iter()
        .any(|node| node.fs_path.as_deref().is_some_and(|path| win::path_key(std::path::Path::new(path)) == target));
    let status = if !matches.is_empty() {
        "ok"
    } else if walk.nodes.is_empty() && walk.undecoded_items > 0 {
        "unsupported_format"
    } else {
        "no_record"
    };
    let records: Vec<Value> = matches
        .iter()
        .take(top)
        .map(|node| {
            json!({
                "path": node.fs_path,
                "display_path": node.display_path,
                "hive": node.hive,
                "key_last_write": node.key_last_write.and_then(win::filetime_rfc3339),
            })
        })
        .collect();
    source_result(
        status,
        "live",
        json!({
            "folder_itself_recorded": exact,
            "nodes_total": walk.nodes.len(),
            "undecoded_items": walk.undecoded_items,
            "matches_total": matches.len(),
            "matches_shown": records.len(),
            // A key's last write changes when an entry below that folder is added or reordered.
            "records": records,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// File entry item for `test.txt` with a version 9 extension block.
    const ITEM: [u8; 90] = [
        0x5A, 0x00, 0x32, 0x00, 0x00, 0x00, 0x00, 0x00, 0x85, 0x51, 0x79, 0x62, 0x20, 0x00, 0x74, 0x65, 0x73, 0x74,
        0x2E, 0x74, 0x78, 0x74, 0x00, 0x00, 0x42, 0x00, 0x09, 0x00, 0x04, 0x00, 0xEF, 0xBE, 0x85, 0x51, 0x79, 0x62,
        0x85, 0x51, 0x79, 0x62, 0x2E, 0x00, 0x00, 0x00, 0x26, 0x74, 0x02, 0x00, 0x00, 0x00, 0x08, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x39, 0xD2, 0x1B, 0x01, 0x74, 0x00,
        0x65, 0x00, 0x73, 0x00, 0x74, 0x00, 0x2E, 0x00, 0x74, 0x00, 0x78, 0x00, 0x74, 0x00, 0x00, 0x00, 0x18, 0x00,
    ];

    #[test]
    fn reads_the_long_name_from_the_extension_block() {
        assert_eq!(long_name(&ITEM).as_deref(), Some("test.txt"));
    }

    #[test]
    fn a_localized_name_does_not_shorten_the_long_name() {
        // Same item with a non-zero localized-name size, as Explorer writes for `Users`.
        let mut item = ITEM;
        item[0x3C] = 0x1C;
        assert_eq!(long_name(&item).as_deref(), Some("test.txt"));
    }

    #[test]
    fn items_without_an_extension_block_have_no_long_name() {
        assert_eq!(long_name(&[0x14, 0x00, 0x1F, 0x50, 0xE0, 0x4F, 0xD0, 0x20]), None);
    }
}
