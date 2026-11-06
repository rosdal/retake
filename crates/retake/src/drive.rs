//! Drive type and free space for the buffer folder.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use retake_core::autoconf::{DriveInfo, Speed};
use retake_core::buffer::{DriveKind, low_space_warning};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    BusTypeNvme, CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumePathNameW, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery,
    STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_ID, STORAGE_PROPERTY_QUERY, StorageDeviceProperty,
    StorageDeviceSeekPenaltyProperty,
};
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

/// Every fixed local drive with its free space and speed class.
pub fn fixed_drives() -> Vec<DriveInfo> {
    // SAFETY: no arguments; returns a bit mask.
    let mask = unsafe { GetLogicalDrives() };
    (0u8..26)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let letter = char::from(b'A' + i);
            let root = PathBuf::from(format!("{letter}:\\"));
            if drive_kind(&root) != DriveKind::Fixed {
                return None;
            }
            let free = free_bytes(&root).ok()?;
            Some(DriveInfo {
                speed: speed(letter),
                root,
                free,
            })
        })
        .collect()
}

/// Asks the volume's storage driver for one property, into `out`.
///
/// # Safety
/// `T` must be the plain-data struct the driver returns for `id`.
unsafe fn query_property<T: Default>(
    volume: windows::Win32::Foundation::HANDLE,
    id: STORAGE_PROPERTY_ID,
) -> Option<T> {
    let query = STORAGE_PROPERTY_QUERY {
        PropertyId: id,
        QueryType: PropertyStandardQuery,
        ..Default::default()
    };
    let mut out = T::default();
    let mut got = 0u32;
    // SAFETY: both buffers are valid for their stated sizes for the duration of the call.
    unsafe {
        DeviceIoControl(
            volume,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some((&raw const query).cast()),
            size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some((&raw mut out).cast()),
            size_of::<T>() as u32,
            Some(&mut got),
            None,
        )
    }
    .ok()?;
    Some(out)
}

/// NVMe by bus type, else SSD or HDD by whether seeking costs time. The volume is opened with
/// no access rights, which needs no admin.
fn speed(letter: char) -> Speed {
    let path = HSTRING::from(format!(r"\\.\{letter}:"));
    // SAFETY: the path outlives the call; the handle is closed below.
    let Ok(volume) = (unsafe {
        CreateFileW(
            &path,
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }) else {
        return Speed::Unknown;
    };
    // SAFETY: each struct is the one the driver returns for its property id.
    let device: Option<STORAGE_DEVICE_DESCRIPTOR> =
        unsafe { query_property(volume, StorageDeviceProperty) };
    let seek: Option<DEVICE_SEEK_PENALTY_DESCRIPTOR> =
        unsafe { query_property(volume, StorageDeviceSeekPenaltyProperty) };
    // SAFETY: we opened it above.
    let _ = unsafe { CloseHandle(volume) };
    if device.is_some_and(|d| d.BusType == BusTypeNvme) {
        return Speed::Nvme;
    }
    match seek {
        Some(s) if !s.IncursSeekPenalty => Speed::Ssd,
        Some(_) => Speed::Hdd,
        None => Speed::Unknown,
    }
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
    fn system_drive_is_listed_with_space_and_a_speed() {
        let drives = fixed_drives();
        let c = drives
            .iter()
            .find(|d| d.root == Path::new("C:\\"))
            .unwrap_or_else(|| panic!("no C: in {drives:?}"));
        assert!(c.free > 0);
        eprintln!("{drives:?}");
    }

    #[test]
    fn temp_dir_is_a_local_drive_with_space() {
        let t = std::env::temp_dir();
        assert!(matches!(drive_kind(&t), DriveKind::Fixed | DriveKind::Ram));
        assert!(free_bytes(&t).unwrap() > 0);
    }
}
