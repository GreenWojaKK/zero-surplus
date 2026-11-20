//! Prefetch: last run times and the files a program touched right after starting.
//!
//! The compressed container is unwrapped here (header of 8 bytes, or 12 when the CRC flag is
//! set) and decompressed by ntdll; `frnsc-prefetch` then reads the plain SCCA structure.

use std::ffi::c_void;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::Path;

use forensic_rs::err::ForensicResult;
use forensic_rs::traits::vfs::{VFileType, VMetadata, VirtualFile};
use frnsc_prefetch::prelude::read_prefetch_file_no_compressed;

use super::PathMap;

const COMPRESSION_FORMAT_XPRESS_HUFF: u16 = 4;
const MAX_DECOMPRESSED: u32 = 64 * 1024 * 1024;

#[link(name = "ntdll", kind = "raw-dylib")]
unsafe extern "system" {
    fn RtlGetCompressionWorkSpaceSize(format: u16, buffer_workspace: *mut u32, fragment_workspace: *mut u32) -> i32;
    fn RtlDecompressBufferEx(
        format: u16,
        uncompressed: *mut u8,
        uncompressed_size: u32,
        compressed: *const u8,
        compressed_size: u32,
        final_size: *mut u32,
        workspace: *mut c_void,
    ) -> i32;
}

/// Why a file could not be read as Prefetch: not a Prefetch file, a container or version this
/// build does not read, or an I/O error.
#[derive(Debug)]
pub struct PfError(pub String);

/// Return the plain SCCA bytes of a Prefetch file.
pub fn unwrap_container(file: &[u8]) -> Result<Vec<u8>, PfError> {
    if file.len() >= 8 && &file[4..8] == b"SCCA" {
        return Ok(file.to_vec());
    }
    if file.len() < 8 || &file[0..3] != b"MAM" {
        return Err(PfError("missing MAM/SCCA signature".to_owned()));
    }
    let algorithm = u16::from(file[3] & 0x0f);
    let has_crc = file[3] & 0xf0 != 0;
    if algorithm != COMPRESSION_FORMAT_XPRESS_HUFF {
        return Err(PfError(format!("compression algorithm {algorithm}")));
    }
    let size = u32::from_le_bytes(file[4..8].try_into().unwrap());
    if size == 0 || size > MAX_DECOMPRESSED {
        return Err(PfError(format!("decompressed size {size}")));
    }
    let offset = if has_crc { 12 } else { 8 };
    let compressed = file.get(offset..).ok_or_else(|| PfError("truncated header".to_owned()))?;

    let (mut workspace_size, mut fragment_size) = (0u32, 0u32);
    // SAFETY: both outputs are valid for writes.
    let status = unsafe {
        RtlGetCompressionWorkSpaceSize(COMPRESSION_FORMAT_XPRESS_HUFF, &mut workspace_size, &mut fragment_size)
    };
    if status < 0 {
        return Err(PfError(format!("workspace query failed: {status:#x}")));
    }
    let mut workspace = vec![0u8; workspace_size as usize];
    let mut output = vec![0u8; size as usize];
    let mut written = 0u32;
    // SAFETY: every buffer is valid for the length passed alongside it.
    let status = unsafe {
        RtlDecompressBufferEx(
            COMPRESSION_FORMAT_XPRESS_HUFF,
            output.as_mut_ptr(),
            size,
            compressed.as_ptr(),
            compressed.len() as u32,
            &mut written,
            workspace.as_mut_ptr().cast::<c_void>(),
        )
    };
    if status < 0 {
        return Err(PfError(format!("decompression failed: {status:#x}")));
    }
    output.truncate(written as usize);
    Ok(output)
}

struct MemFile(Cursor<Vec<u8>>);

impl Read for MemFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.0.read(buffer)
    }
}

impl Seek for MemFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.0.seek(position)
    }
}

impl VirtualFile for MemFile {
    fn metadata(&self) -> ForensicResult<VMetadata> {
        Ok(VMetadata {
            created: None,
            accessed: None,
            modified: None,
            file_type: VFileType::File,
            size: self.0.get_ref().len() as u64,
        })
    }
}

pub struct PfRecord {
    /// Prefetch file name, e.g. `PYTHON.EXE-1A2B3C4D.pf`.
    pub file: String,
    pub version: u32,
    /// Executable name as stored in the file (upper case, at most 29 characters).
    pub executable: String,
    /// Full path of the executable, when exactly one referenced file can be it.
    pub path: Option<String>,
    /// How `path` was chosen: `only_candidate` or `file_name_hash`.
    pub path_basis: Option<&'static str>,
    /// Referenced files with the executable's name, when more than one remains and `path` is
    /// therefore empty.
    pub path_candidates: Vec<String>,
    pub run_count: u32,
    /// FILETIME values, most recent first, zeros removed.
    pub last_runs: Vec<u64>,
    /// Files referenced during start-up, with drive letters where the volume is known.
    pub referenced: Vec<String>,
}

pub fn parse(file_name: &str, bytes: &[u8], map: &PathMap) -> Result<PfRecord, PfError> {
    let scca = unwrap_container(bytes)?;
    let parsed = read_prefetch_file_no_compressed(file_name, Box::new(MemFile(Cursor::new(scca))))
        .map_err(|err| PfError(err.to_string()))?;
    let referenced: Vec<String> = parsed.metrics.iter().map(|metric| map.from_prefetch(&metric.file)).collect();
    let executable = parsed.name.trim_end_matches('\0').to_owned();
    let candidates: Vec<&String> = referenced
        .iter()
        .filter(|path| {
            let name = path.rsplit('\\').next().unwrap_or(path);
            let upper = name.to_ascii_uppercase();
            // The stored name is cut at 29 characters, so a longer executable name matches by
            // prefix. Other files that share the prefix (logs, data) are not executables.
            name.eq_ignore_ascii_case(&executable)
                || (executable.len() >= 29
                    && upper.starts_with(&executable.to_ascii_uppercase())
                    && [".EXE", ".COM", ".SCR"].iter().any(|extension| upper.ends_with(extension)))
        })
        .collect();
    let (path, path_basis, path_candidates) = match candidates.as_slice() {
        [] => (None, None, Vec::new()),
        [only] => (Some((*only).clone()), Some("only_candidate"), Vec::new()),
        several => {
            let hash = file_name_hash(file_name);
            let hits: Vec<&&String> = several
                .iter()
                .filter(|path| hash.is_some() && map.to_device(path).map(|device| path_hash(&device)) == hash)
                .collect();
            match hits.as_slice() {
                [hit] => (Some((**hit).clone()), Some("file_name_hash"), Vec::new()),
                _ => (None, None, several.iter().map(|path| (*path).clone()).collect()),
            }
        }
    };
    let mut last_runs: Vec<u64> =
        parsed.last_run_times.iter().map(|time| time.filetime()).filter(|&time| time != 0).collect();
    last_runs.sort_unstable_by(|left, right| right.cmp(left));
    Ok(PfRecord {
        file: file_name.to_owned(),
        version: parsed.version,
        executable,
        path,
        path_basis,
        path_candidates,
        run_count: parsed.run_count,
        last_runs,
        referenced,
    })
}

/// The hash in a Prefetch file name (`NAME-1A2B3C4D.pf`).
pub fn file_name_hash(file_name: &str) -> Option<u32> {
    let stem = file_name.get(..file_name.len().checked_sub(3)?).filter(|_| file_name.to_ascii_lowercase().ends_with(".pf"))?;
    u32::from_str_radix(stem.rsplit_once('-')?.1, 16).ok()
}

/// Prefetch file-name hash of an executable's NT path (`\DEVICE\HARDDISKVOLUME3\...`): start at
/// 314159 and, for every byte of the upper-case UTF-16LE path, take `hash * 37 + byte` modulo
/// 2^32. Hosts whose hash also covers the command line (service hosts, packaged and
/// multi-process apps) do not match.
pub fn path_hash(device_path: &str) -> u32 {
    device_path
        .to_uppercase()
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .fold(314_159u32, |hash, byte| hash.wrapping_mul(37).wrapping_add(u32::from(byte)))
}

pub struct PfLoad {
    pub records: Vec<PfRecord>,
    pub files_total: usize,
    /// `(file name, reason)` of files that could not be read.
    pub unsupported: Vec<(String, String)>,
}

/// Parse every `*.pf` in `dir`.
pub fn load(dir: &Path, map: &PathMap) -> io::Result<PfLoad> {
    let mut load = PfLoad { records: Vec::new(), files_total: 0, unsupported: Vec::new() };
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().ends_with(".pf") {
            continue;
        }
        load.files_total += 1;
        let parsed = std::fs::read(entry.path())
            .map_err(|err| PfError(err.to_string()))
            .and_then(|bytes| parse(&name, &bytes, map));
        match parsed {
            Ok(record) => load.records.push(record),
            Err(PfError(reason)) => load.unsupported.push((name, reason)),
        }
    }
    Ok(load)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_hash_is_read_from_the_name() {
        assert_eq!(file_name_hash("TASKKILL.EXE-BE180FC8.pf"), Some(0xBE18_0FC8));
        assert_eq!(file_name_hash("AM_DELTA_PATCH_1.463.359.0.EX-FD1010EB.PF"), Some(0xFD10_10EB));
        assert_eq!(file_name_hash("TASKKILL.EXE.pf"), None);
    }

    #[test]
    fn path_hash_matches_names_windows_gave_these_files() {
        assert_eq!(path_hash(r"\DEVICE\HARDDISKVOLUME3\WINDOWS\SYSTEM32\TASKKILL.EXE"), 0xBE18_0FC8);
        assert_eq!(path_hash(r"\Device\HarddiskVolume3\Windows\System32\cmd.exe"), 0x0BD3_0981);
    }

    #[test]
    fn rejects_data_without_a_signature() {
        assert!(matches!(unwrap_container(b"not a prefetch file"), Err(PfError(_))));
    }

    #[test]
    fn rejects_an_unknown_compression_algorithm() {
        let mut data = b"MAM\x02".to_vec();
        data.extend_from_slice(&100u32.to_le_bytes());
        data.extend_from_slice(&[0; 16]);
        assert!(matches!(unwrap_container(&data), Err(PfError(_))));
    }

    #[test]
    fn plain_scca_passes_through() {
        let mut data = vec![0x1f, 0, 0, 0];
        data.extend_from_slice(b"SCCA");
        data.extend_from_slice(&[0; 8]);
        assert_eq!(unwrap_container(&data).unwrap(), data);
    }
}
