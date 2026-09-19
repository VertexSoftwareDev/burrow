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
