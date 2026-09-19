//! Everything that asks Windows something: drives, free space, the user's
//! language, rights, and handing a path to Explorer.

use std::os::windows::ffi::OsStrExt;
use std::process::Command;

use windows_sys::Win32::Globalization::GetUserDefaultLocaleName;
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumeInformationW};

/// One drive letter, as the drive picker shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Drive {
    pub letter: char,
    pub label: String,
    pub filesystem: String,
    pub total: u64,
    pub free: u64,
}

impl Drive {
    pub fn is_ntfs(&self) -> bool {
        self.filesystem.eq_ignore_ascii_case("NTFS")
    }

    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

fn wide(text: &str) -> Vec<u16> {
    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(Some(0))
        .collect()
}

/// Every mounted drive letter with a filesystem.
pub fn drives() -> Vec<Drive> {
    ('A'..='Z').filter_map(drive).collect()
}

pub fn drive(letter: char) -> Option<Drive> {
    let root = format!("{letter}:\\");
    if !std::path::Path::new(&root).exists() {
        return None;
    }
    let root_w = wide(&root);

    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    let ok = unsafe {
        GetVolumeInformationW(
            root_w.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    if ok == 0 {
        return None;
    }
    let (total, free) = space(letter).unwrap_or((0, 0));
    Some(Drive {
        letter,
        label: from_wide(&label),
        filesystem: from_wide(&fs),
        total,
        free,
    })
}

/// `(total, free)` bytes of a volume.
pub fn space(letter: char) -> Option<(u64, u64)> {
    let root = wide(&format!("{letter}:\\"));
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0).then_some((total, free))
}

fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// The drive Windows is installed on — the one that is usually full.
pub fn system_drive() -> char {
    std::env::var("SystemDrive")
        .ok()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('C')
}

/// The user's locale as a BCP-47 tag, e.g. `tr-TR`.
pub fn user_locale() -> Option<String> {
    let mut buffer = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(buffer.as_mut_ptr(), buffer.len() as i32) };
    (len > 0).then(|| from_wide(&buffer))
}

/// Whether this process can read a raw volume — asked directly, by trying.
pub fn is_elevated() -> bool {
    for drive in drives().iter().filter(|d| d.is_ntfs()) {
        match ferret_core::Volume::open(drive.letter) {
            Ok(_) => return true,
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return false,
            Err(_) => continue,
        }
    }
    false
}

/// Relaunch with administrator rights. The caller then exits.
pub fn restart_elevated() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("restart_failed:{e}"))?;
    // Single quotes are PowerShell's literal string; an apostrophe in the path
    // must be doubled.
    let quoted = exe.display().to_string().replace('\'', "''");
    Command::new("powershell")
        .args(["-NoProfile", "-WindowStyle", "Hidden", "-Command"])
        .arg(format!("Start-Process -FilePath '{quoted}' -Verb RunAs"))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("restart_failed:{e}"))
}

/// A file's current sizes and modification time, read through the normal
/// filesystem — which, unlike the raw volume, sees what was written a moment
/// ago. `None` when the file is already gone again, which is common: the
/// journal reports work that finished before anyone looked.
pub fn stat(path: &str) -> Option<ferret_core::FileStat> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileStandardInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_STANDARD_INFO,
    };

    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .ok()?;
    let handle = file.as_raw_handle() as _;

    let mut standard: FILE_STANDARD_INFO = unsafe { std::mem::zeroed() };
    let mut basic: FILE_BASIC_INFO = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            &mut standard as *mut _ as *mut _,
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        ) != 0
            && GetFileInformationByHandleEx(
                handle,
                FileBasicInfo,
                &mut basic as *mut _ as *mut _,
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            ) != 0
    };
    ok.then(|| ferret_core::FileStat {
        size: standard.EndOfFile.max(0) as u64,
        allocated: standard.AllocationSize.max(0) as u64,
        modified: basic.LastWriteTime.max(0) as u64,
    })
}

/// What sending files to the recycle bin achieved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Recycled {
    pub requested: usize,
    /// Paths that no longer exist afterwards.
    pub gone: usize,
    /// The person cancelled a prompt Windows showed.
    pub aborted: bool,
}

/// Move paths to the recycle bin. Never deletes permanently on its own.
///
/// `FOF_ALLOWUNDO` is what makes this the recycle bin rather than a delete.
/// `FOF_NOCONFIRMATION` skips the "are you sure" the window already asked,
/// but `FOF_WANTNUKEWARNING` takes one case back from it: a file too large
/// for the recycle bin would otherwise be destroyed silently — with this
/// flag, Windows asks first.
///
/// Files in use are left where they are; the count of what actually went
/// is measured afterwards rather than trusted from the call.
pub fn recycle(paths: &[String]) -> Recycled {
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
        FOF_WANTNUKEWARNING, FO_DELETE, SHFILEOPSTRUCTW,
    };

    let mut result = Recycled {
        requested: paths.len(),
        ..Recycled::default()
    };
    // In chunks, so one failing file does not hold back thousands of others,
    // and so the double-null list stays a sensible size.
    for chunk in paths.chunks(500) {
        // A list of null-terminated paths, ended by an extra null.
        let mut list: Vec<u16> = Vec::new();
        for path in chunk {
            list.extend(std::ffi::OsStr::new(path).encode_wide());
            list.push(0);
        }
        list.push(0);

        let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
        op.wFunc = FO_DELETE;
        op.pFrom = list.as_ptr();
        op.fFlags =
            (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING | FOF_SILENT | FOF_NOERRORUI)
                as u16;
        unsafe { SHFileOperationW(&mut op) };
        if op.fAnyOperationsAborted != 0 {
            result.aborted = true;
        }
    }
    result.gone = paths
        .iter()
        .filter(|p| std::fs::symlink_metadata(p).is_err())
        .count();
    result
}

/// Open the recycle bin in Explorer, so the person can empty it themselves.
pub fn open_recycle_bin() -> Result<(), String> {
    open("shell:RecycleBinFolder")
}

/// Windows' own Disk Cleanup, which is what removes `Windows.old` properly.
pub fn open_disk_cleanup() -> Result<(), String> {
    Command::new("cleanmgr")
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open_failed:{e}"))
}

/// Open a file or folder with its default handler.
pub fn open(path: &str) -> Result<(), String> {
    Command::new("explorer")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open_failed:{e}"))
}

/// Open the containing folder with the item selected.
pub fn reveal(path: &str) -> Result<(), String> {
    Command::new("explorer")
        .arg(format!("/select,{path}"))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("open_failed:{e}"))
}
