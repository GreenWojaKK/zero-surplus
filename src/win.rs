//! Thin wrappers over the Windows calls the commands need.

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_CANCELLED, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, FILETIME, GetLastError, HANDLE,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW,
    GetVolumePathNameW, QueryDosDeviceW,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW, RegQueryInfoKeyW,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHGetKnownFolderPath, ShellExecuteExW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows_sys::core::GUID;

pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

// ---------------------------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------------------------

pub fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
}

const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

/// Windows FILETIME (100 ns ticks since 1601) as local-offset RFC 3339; `None` for zero.
pub fn filetime_rfc3339(filetime: u64) -> Option<String> {
    if filetime < FILETIME_UNIX_EPOCH {
        return None;
    }
    let ticks = filetime - FILETIME_UNIX_EPOCH;
    let seconds = i64::try_from(ticks / 10_000_000).ok()?;
    let nanos = (ticks % 10_000_000) as u32 * 100;
    let utc = chrono::DateTime::from_timestamp(seconds, nanos)?;
    Some(utc.with_timezone(&chrono::Local).to_rfc3339_opts(chrono::SecondsFormat::Secs, false))
}

pub fn filetime_parts(filetime: &FILETIME) -> u64 {
    (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime)
}

// ---------------------------------------------------------------------------------------------
// Volumes and paths
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct VolumeIdent {
    /// Mount point, e.g. `C:\`.
    pub mount: String,
    /// `\\?\Volume{GUID}\`.
    pub guid_path: Option<String>,
    /// Volume serial number as eight hex digits.
    pub serial: Option<String>,
    pub filesystem: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct VolumeSpace {
    pub total_bytes: u64,
    /// Free bytes on the whole volume.
    pub free_bytes: u64,
    /// Free bytes available to the calling user (smaller under quotas).
    pub free_bytes_caller: u64,
}

pub fn volume_space(directory: &Path) -> io::Result<VolumeSpace> {
    let path = wide(directory);
    let (mut caller, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `path` is null-terminated and the three outputs are valid for writes.
    let ok = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), &mut caller, &mut total, &mut free) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(VolumeSpace { total_bytes: total, free_bytes: free, free_bytes_caller: caller })
}

pub fn volume_ident(path: &Path) -> io::Result<VolumeIdent> {
    let input = wide(path);
    let mut mount = [0u16; 512];
    // SAFETY: buffers are valid for the lengths passed.
    if unsafe { GetVolumePathNameW(input.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut guid = [0u16; 64];
    // SAFETY: `mount` is null-terminated; `guid` is valid for its length.
    let guid_path = (unsafe { GetVolumeNameForVolumeMountPointW(mount.as_ptr(), guid.as_mut_ptr(), guid.len() as u32) }
        != 0)
        .then(|| from_wide(&guid));
    let mut serial = 0u32;
    let mut filesystem = [0u16; 64];
    // SAFETY: optional outputs are null; the rest are valid for writes.
    let info_ok = unsafe {
        GetVolumeInformationW(
            mount.as_ptr(),
            null_mut(),
            0,
            &mut serial,
            null_mut(),
            null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    } != 0;
    Ok(VolumeIdent {
        mount: from_wide(&mount),
        guid_path,
        serial: info_ok.then(|| format!("{serial:08X}")),
        filesystem: info_ok.then(|| from_wide(&filesystem)),
    })
}

/// Path with every link resolved, without the `\\?\` prefix.
pub fn final_path(path: &Path) -> io::Result<PathBuf> {
    Ok(strip_verbatim(&std::fs::canonicalize(path)?))
}

pub fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// True when the path itself is a symbolic link or a junction.
pub fn is_link(path: &Path) -> io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_symlink())
}

/// Case-insensitive path comparison key.
pub fn path_key(path: &Path) -> String {
    path.to_string_lossy().trim_end_matches(['\\', '/']).replace('/', "\\").to_lowercase()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DriveMapping {
    /// `C:`
    pub drive: String,
    /// `\Device\HarddiskVolume3`
    pub device: String,
    pub serial: Option<String>,
}

/// Device path and volume serial of every drive letter.
pub fn drive_mappings() -> Vec<DriveMapping> {
    let mut mappings = Vec::new();
    // SAFETY: no arguments.
    let mask = unsafe { GetLogicalDrives() };
    for index in 0..26u32 {
        if mask & (1 << index) == 0 {
            continue;
        }
        let drive = format!("{}:", (b'A' + index as u8) as char);
        let name = wide(&drive);
        let mut target = [0u16; 512];
        // SAFETY: buffers are valid for the lengths passed.
        let len = unsafe { QueryDosDeviceW(name.as_ptr(), target.as_mut_ptr(), target.len() as u32) };
        if len == 0 {
            continue;
        }
        let serial = volume_ident(Path::new(&format!("{drive}\\"))).ok().and_then(|ident| ident.serial);
        mappings.push(DriveMapping { drive, device: from_wide(&target), serial });
    }
    mappings
}

// ---------------------------------------------------------------------------------------------
// Process
// ---------------------------------------------------------------------------------------------

pub fn peak_working_set() -> u64 {
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    // SAFETY: `counters` is valid for `cb` bytes; the pseudo handle needs no cleanup.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) };
    if ok == 0 { 0 } else { counters.PeakWorkingSetSize as u64 }
}

pub fn is_elevated() -> bool {
    let mut token: HANDLE = null_mut();
    // SAFETY: `token` receives a handle that is closed below.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0u32;
    // SAFETY: the output buffer matches the requested information class.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast::<c_void>(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        )
    };
    // SAFETY: `token` is a valid handle owned by this function.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

pub enum Elevation {
    /// The elevated process ran and exited with this code.
    Exited(u32),
    /// The user declined the UAC prompt.
    Declined,
}

/// Run `exe` with `parameters` elevated (UAC prompt) and wait until it exits.
pub fn run_elevated(exe: &Path, parameters: &str) -> io::Result<Elevation> {
    let verb = wide("runas");
    let file = wide(exe);
    let params = wide(parameters);
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: all string pointers outlive the call and `info` is fully initialised.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        // SAFETY: reads the calling thread's last error.
        let code = unsafe { GetLastError() };
        if code == ERROR_CANCELLED {
            return Ok(Elevation::Declined);
        }
        return Err(io::Error::from_raw_os_error(code as i32));
    }
    if info.hProcess.is_null() {
        return Err(io::Error::other("elevated process handle was not returned"));
    }
    let mut exit_code = 1u32;
    // SAFETY: `hProcess` is a process handle owned by this function.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut exit_code);
        CloseHandle(info.hProcess);
    }
    Ok(Elevation::Exited(exit_code))
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_console_event(_event: u32) -> i32 {
    INTERRUPTED.store(true, Ordering::SeqCst);
    1
}

/// Turn Ctrl-C into a flag the running command polls with [`interrupted`].
pub fn install_interrupt_flag() {
    // SAFETY: the handler only stores into an atomic.
    unsafe { SetConsoleCtrlHandler(Some(on_console_event), 1) };
}

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------------------------
// Known folders
// ---------------------------------------------------------------------------------------------

pub fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut raw: *mut u16 = null_mut();
    // SAFETY: on success `raw` points at a null-terminated string freed with CoTaskMemFree.
    let hr = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &mut raw) };
    let result = if hr >= 0 && !raw.is_null() {
        // SAFETY: `raw` is null-terminated.
        let len = (0..).take_while(|&offset| unsafe { *raw.add(offset) } != 0).count();
        // SAFETY: `len` units are readable.
        Some(PathBuf::from(String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(raw, len) })))
    } else {
        None
    };
    // SAFETY: freeing a null or CoTaskMem pointer is valid.
    unsafe { CoTaskMemFree(raw.cast::<c_void>()) };
    result
}

/// Parse `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` (braces optional).
pub fn parse_guid(text: &str) -> Option<GUID> {
    let text = text.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 5 || parts.iter().map(|part| part.len()).ne([8, 4, 4, 4, 12]) {
        return None;
    }
    let tail = format!("{}{}", parts[3], parts[4]);
    let mut data4 = [0u8; 8];
    for (index, byte) in data4.iter_mut().enumerate() {
        *byte = u8::from_str_radix(tail.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(GUID {
        data1: u32::from_str_radix(parts[0], 16).ok()?,
        data2: u16::from_str_radix(parts[1], 16).ok()?,
        data3: u16::from_str_radix(parts[2], 16).ok()?,
        data4,
    })
}

// ---------------------------------------------------------------------------------------------
// Registry (current user)
// ---------------------------------------------------------------------------------------------

pub struct RegKey(HKEY);

pub struct RegValue {
    pub name: String,
    pub data: Vec<u8>,
}

impl RegKey {
    /// Open `HKEY_CURRENT_USER\<subkey>` for reading.
    pub fn open_current_user(subkey: &str) -> io::Result<Self> {
        Self::open(HKEY_CURRENT_USER, subkey)
    }

    pub fn open_subkey(&self, subkey: &str) -> io::Result<Self> {
        Self::open(self.0, subkey)
    }

    fn open(parent: HKEY, subkey: &str) -> io::Result<Self> {
        let name = wide(subkey);
        let mut key: HKEY = null_mut();
        // SAFETY: `name` is null-terminated; `key` receives a handle closed in `Drop`.
        let status = unsafe { RegOpenKeyExW(parent, name.as_ptr(), 0, KEY_READ, &mut key) };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self(key))
    }

    pub fn values(&self) -> io::Result<Vec<RegValue>> {
        let (_, values, max_name, max_data, _) = self.info()?;
        let mut result = Vec::with_capacity(values as usize);
        let mut index = 0u32;
        loop {
            let mut name = vec![0u16; max_name as usize + 2];
            let mut name_len = name.len() as u32;
            let mut data = vec![0u8; max_data as usize + 2];
            let mut data_len = data.len() as u32;
            // SAFETY: buffers are valid for the lengths passed in.
            let status = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    null(),
                    null_mut(),
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            match status {
                ERROR_SUCCESS => {
                    data.truncate(data_len as usize);
                    result.push(RegValue { name: String::from_utf16_lossy(&name[..name_len as usize]), data });
                }
                ERROR_NO_MORE_ITEMS => break,
                // A value grew after the size query: skip it instead of failing the listing.
                ERROR_MORE_DATA => {}
                other => return Err(io::Error::from_raw_os_error(other as i32)),
            }
            index += 1;
        }
        Ok(result)
    }

    /// Last write time of the key as a FILETIME value.
    pub fn last_write(&self) -> Option<u64> {
        self.info().ok().map(|(_, _, _, _, written)| written)
    }

    /// (subkey count, value count, longest value name, largest value data, last write time)
    fn info(&self) -> io::Result<(u32, u32, u32, u32, u64)> {
        let (mut subkeys, mut values, mut max_name, mut max_data) = (0u32, 0u32, 0u32, 0u32);
        let mut written = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
        // SAFETY: requested outputs are valid; the rest are null.
        let status = unsafe {
            RegQueryInfoKeyW(
                self.0,
                null_mut(),
                null_mut(),
                null(),
                &mut subkeys,
                null_mut(),
                null_mut(),
                &mut values,
                &mut max_name,
                &mut max_data,
                null_mut(),
                &mut written,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok((subkeys, values, max_name, max_data, filetime_parts(&written)))
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: the key was opened by this wrapper and is closed once.
        unsafe { RegCloseKey(self.0) };
    }
}
