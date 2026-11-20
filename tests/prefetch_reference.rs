//! Prefetch records compared with PECmd output kept next to the same files.

mod common;

use std::path::{Path, PathBuf};

use common::*;
use serde_json::Value;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("prefetch")
}

/// Both tools' paths in one notation: the fixture volume as `C:`, upper case.
fn normalize(path: &str, volume: &str) -> String {
    let upper = path.to_uppercase();
    match upper.strip_prefix(&volume.to_uppercase()) {
        Some(rest) => format!("C:{rest}"),
        None => upper,
    }
}

fn list(text: Option<&str>, volume: &str) -> Vec<String> {
    text.unwrap_or_default().split(", ").filter(|item| !item.is_empty()).map(|item| normalize(item, volume)).collect()
}

/// Seconds since 1970 from either tool's time text.
fn seconds(text: &str) -> i64 {
    let text = text.replace('Z', "+00:00");
    let (main, offset) = text.split_at(text.len() - 6);
    let main = main.split('.').next().unwrap();
    let parsed = chrono::NaiveDateTime::parse_from_str(main, "%Y-%m-%dT%H:%M:%S").unwrap();
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let hours: i64 = offset[1..3].parse().unwrap();
    let minutes: i64 = offset[4..6].parse().unwrap();
    parsed.and_utc().timestamp() - sign * (hours * 3600 + minutes * 60)
}

#[test]
fn prefetch_records_match_pecmd() {
    let reference: Value = serde_json::from_slice(&std::fs::read(fixtures().join("pecmd.json")).unwrap()).unwrap();
    let volume = reference["volume"]["name"].as_str().unwrap();
    let dir = TempDir::new("prefetch-reference");
    let case = dir.join("case");
    let no_srum = dir.join("no-srum.dat");

    for expected in reference["files"].as_array().unwrap() {
        let file = expected["file"].as_str().unwrap();
        let pecmd = &expected["pecmd"];

        // The fixture is the file PECmd read.
        let bytes = std::fs::read(fixtures().join(file)).unwrap();
        let digest = format!("{:x}", sha256(&bytes));
        assert_eq!(digest, expected["sha256"].as_str().unwrap(), "{file} changed");

        let name = pecmd["ExecutableName"].as_str().unwrap();
        let outcome = cli(&[
            "usage",
            "--case",
            text(&case),
            "--app",
            name,
            "--prefetch-dir",
            text(&fixtures()),
            "--srum-db",
            text(&no_srum),
        ]);
        assert_eq!(outcome.code, 0, "{}", outcome.stderr);
        let stored = stored_json(&case, outcome.json["stored"].as_str().unwrap());
        let record = stored["sources"]["prefetch"]["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["prefetch_file"] == file)
            .unwrap_or_else(|| panic!("{file} not returned"))
            .clone();

        assert_eq!(record["executable"], pecmd["ExecutableName"], "{file}");
        assert_eq!(record["run_count"].as_u64().unwrap().to_string(), pecmd["RunCount"].as_str().unwrap(), "{file}");

        // Same run times; zero-surplus lists them newest first.
        let mut theirs: Vec<i64> = std::iter::once("LastRun".to_owned())
            .chain((0..7).map(|index| format!("PreviousRun{index}")))
            .filter_map(|key| pecmd[key.as_str()].as_str().map(seconds))
            .collect();
        theirs.sort_unstable_by(|left, right| right.cmp(left));
        let ours: Vec<i64> =
            record["last_runs"].as_array().unwrap().iter().map(|time| seconds(time.as_str().unwrap())).collect();
        assert_eq!(ours, theirs, "{file}");
        assert_eq!(seconds(record["last_run"].as_str().unwrap()), theirs[0], "{file}");

        let referenced: Vec<String> = record["referenced"]
            .as_array()
            .unwrap()
            .iter()
            .map(|path| normalize(path.as_str().unwrap(), volume))
            .collect();
        assert_eq!(referenced, list(pecmd["FilesLoaded"].as_str(), volume), "{file}");

        // PECmd lists every executable candidate. zero-surplus names one when only one exists
        // or the file-name hash tells which; otherwise it returns all of them.
        let candidates = list(pecmd["FullPath"].as_str(), volume);
        match record["path"].as_str() {
            Some(path) => {
                assert!(candidates.contains(&normalize(path, volume)), "{file}: {path} not among {candidates:?}");
                // PECmd may also settle on one path by the hash, so one PECmd path allows both.
                let basis = record["path_basis"].as_str().unwrap();
                assert!(["only_candidate", "file_name_hash"].contains(&basis), "{file}: {basis}");
                if candidates.len() > 1 {
                    assert_eq!(basis, "file_name_hash", "{file}");
                }
            }
            None => {
                let mut ours: Vec<String> = record["path_candidates"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|path| normalize(path.as_str().unwrap(), volume))
                    .collect();
                ours.sort();
                let mut theirs = candidates.clone();
                theirs.sort();
                assert_eq!(ours, theirs, "{file}");
            }
        }
    }
}

#[test]
fn the_file_name_hash_separates_candidates_on_the_volume_that_ran_them() {
    // On this volume the hash tells the two git.exe apart; the ambiguous file stays ambiguous.
    let dir = TempDir::new("prefetch-hash");
    let case = dir.join("case");
    let no_srum = dir.join("no-srum.dat");
    let run = |name: &str| {
        let outcome = cli(&[
            "usage", "--case", text(&case), "--app", name, "--prefetch-dir", text(&fixtures()), "--srum-db", text(&no_srum),
        ]);
        let stored = stored_json(&case, outcome.json["stored"].as_str().unwrap());
        stored["sources"]["prefetch"]["records"].as_array().unwrap().clone()
    };
    let drive_serial = std::process::Command::new("cmd").args(["/c", "vol", "C:"]).output().unwrap();
    if !String::from_utf8_lossy(&drive_serial.stdout).contains("4E01-4E2C") {
        // The fixtures were recorded on a volume with this serial number.
        return;
    }
    let git = &run("GIT.EXE")[0];
    assert_eq!(git["path_basis"], "file_name_hash");
    assert_eq!(git["path"], r"C:\PROGRAM FILES\GIT\CMD\GIT.EXE");

    let edge = &run("MSEDGEWEBVIEW2.EXE")[0];
    assert!(edge["path"].is_null());
    assert_eq!(edge["path_candidates"].as_array().unwrap().len(), 2);

    // A path query matches a record whose path is one of its candidates.
    let candidate = edge["path_candidates"][0].as_str().unwrap().to_owned();
    let by_path = run(&candidate);
    assert_eq!(by_path.len(), 1);
    assert_eq!(by_path[0]["prefetch_file"], "MSEDGEWEBVIEW2.EXE-FD2467EE.pf");
}

/// SHA-256, so the fixture check needs no extra crate.
fn sha256(data: &[u8]) -> Sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (index, word) in block.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes(word.try_into().unwrap());
        }
        for index in 16..64 {
            let s0 = w[index - 15].rotate_right(7) ^ w[index - 15].rotate_right(18) ^ (w[index - 15] >> 3);
            let s1 = w[index - 2].rotate_right(17) ^ w[index - 2].rotate_right(19) ^ (w[index - 2] >> 10);
            w[index] = w[index - 16].wrapping_add(s0).wrapping_add(w[index - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(choice).wrapping_add(K[index]).wrapping_add(w[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(majority);
            (h, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    Sha256(state)
}

struct Sha256([u32; 8]);

impl std::fmt::LowerHex for Sha256 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|word| write!(formatter, "{word:08x}"))
    }
}
