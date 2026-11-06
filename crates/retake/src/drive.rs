//! Drive type and free space for the buffer folder.

use std::path::Path;

use anyhow::{Context, Result};
use retake_core::buffer::{DriveKind, low_space_warning};
use windows::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumePathNameW};
use windows::core::{HSTRING, PCWSTR};

// GetDriveTypeW return values (winbase.h).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// The kind of drive `dir` (which must exist) is on.
pub fn drive_kind(dir: &Path) -> DriveKind {
    let mut root = [0u16; 261];
    // SAFETY: `root` is a writable buffer of the length passed; strings outlive the calls.
    let kind = unsafe {
        if GetVolumePathNameW(&HSTRING::from(dir.as_os_str()), &mut root).is_err() {
            return DriveKind::Unknown;
        }
        GetDriveTypeW(PCWSTR(root.as_ptr()))
    };
    match kind {
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_RAMDISK => DriveKind::Ram,
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_REMOTE => DriveKind::Remote,
        DRIVE_CDROM => DriveKind::CdRom,
        _ => DriveKind::Unknown,
    }
}

/// Bytes free to this user on `dir`'s drive.
pub fn free_bytes(dir: &Path) -> Result<u64> {
    let mut free = 0u64;
    // SAFETY: `free` is a valid u64 out-pointer; the path outlives the call.
    unsafe { GetDiskFreeSpaceExW(&HSTRING::from(dir.as_os_str()), Some(&mut free), None, None) }
        .with_context(|| format!("GetDiskFreeSpaceExW({})", dir.display()))?;
    Ok(free)
}

/// Logs a warning if `dir`'s drive has less free space than the buffer wants.
pub fn warn_if_low_space(dir: &Path) {
    match free_bytes(dir) {
        Ok(free) => {
            if let Some(w) = low_space_warning(free, dir) {
                tracing::warn!("{w}");
            }
        }
        Err(e) => tracing::warn!("{e:#}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_is_a_local_drive_with_space() {
        let t = std::env::temp_dir();
        assert!(matches!(drive_kind(&t), DriveKind::Fixed | DriveKind::Ram));
        assert!(free_bytes(&t).unwrap() > 0);
    }
}
