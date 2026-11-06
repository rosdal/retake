//! The buffer folder: which files are Retake's, clearing them, its README, and which drives
//! may hold it.

use std::fs;
use std::io;
use std::path::Path;

use crate::ffmpeg_args::INDEX_FILE;

pub const README_NAME: &str = "README.txt";
pub const README_TEXT: &str = "This folder is Retake's scratch area for the rolling video buffer.

Everything here is overwritten while Siege runs and cleared when a new session starts,
so it is safe to delete. For speed, exclude this folder from Windows Defender real-time
scanning and keep it out of OneDrive or any other synced folder.
";

/// Below this, starting a session logs a warning.
pub const MIN_FREE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// `seg_<digits>.ts` or `index.csv`, ignoring ASCII case.
pub fn is_buffer_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower == INDEX_FILE {
        return true;
    }
    lower
        .strip_prefix("seg_")
        .and_then(|r| r.strip_suffix(".ts"))
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Deletes Retake's own files in `dir` (never folders or anything else). Returns how many.
pub fn clear_dir(dir: &Path) -> io::Result<usize> {
    let mut n = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_str().is_some_and(is_buffer_file) {
            fs::remove_file(entry.path())?;
            n += 1;
        }
    }
    Ok(n)
}

/// Writes the README unless one is already there.
pub fn write_readme(dir: &Path) -> io::Result<()> {
    let p = dir.join(README_NAME);
    if p.exists() {
        return Ok(());
    }
    fs::write(p, README_TEXT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Ram,
    Removable,
    Remote,
    CdRom,
    Unknown,
}

/// Why `dir`'s drive can't hold the buffer, if it can't.
pub fn drive_problem(kind: DriveKind, dir: &Path) -> Option<String> {
    let why = match kind {
        DriveKind::Fixed | DriveKind::Ram => return None,
        DriveKind::Remote => "is on a network drive; use a local SSD",
        DriveKind::Removable => "is on a removable drive; use a local SSD",
        DriveKind::CdRom => "is on a CD/DVD drive; use a local SSD",
        DriveKind::Unknown => "is on a drive of unknown type; use a local SSD",
    };
    Some(format!("capture.buffer_dir = {}: {why}", dir.display()))
}

/// A warning when less than [`MIN_FREE_BYTES`] is free.
pub fn low_space_warning(free: u64, dir: &Path) -> Option<String> {
    (free < MIN_FREE_BYTES).then(|| {
        let gb = free as f64 / (1024.0 * 1024.0 * 1024.0);
        format!(
            "capture.buffer_dir = {}: only {:.1} GB free (5 GB recommended)",
            dir.display(),
            (gb * 10.0).floor() / 10.0
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn recognises_own_files_only() {
        for n in [
            "seg_00.ts",
            "seg_29.ts",
            "seg_123.ts",
            "index.csv",
            "SEG_01.TS",
        ] {
            assert!(is_buffer_file(n), "{n}");
        }
        for n in [
            "README.txt",
            "seg_.ts",
            "seg_ab.ts",
            "myvideo.ts",
            "index.csv.bak",
            "seg_01.mp4",
        ] {
            assert!(!is_buffer_file(n), "{n}");
        }
    }

    #[test]
    fn clear_deletes_only_buffer_files() {
        let t = tempfile::tempdir().unwrap();
        for n in [
            "seg_00.ts",
            "seg_01.ts",
            "index.csv",
            "keep.mp4",
            README_NAME,
        ] {
            fs::write(t.path().join(n), b"x").unwrap();
        }
        fs::create_dir(t.path().join("seg_02.ts")).unwrap(); // a directory is never touched
        assert_eq!(clear_dir(t.path()).unwrap(), 3);
        let mut left: Vec<_> = fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, [README_NAME, "keep.mp4", "seg_02.ts"]);
    }

    #[test]
    fn readme_written_once() {
        let t = tempfile::tempdir().unwrap();
        write_readme(t.path()).unwrap();
        let p = t.path().join(README_NAME);
        assert!(fs::read_to_string(&p).unwrap().contains("safe to delete"));
        fs::write(&p, "edited").unwrap();
        write_readme(t.path()).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "edited");
    }

    #[test]
    fn only_local_drives_allowed() {
        let d = Path::new(r"X:\buf");
        assert_eq!(drive_problem(DriveKind::Fixed, d), None);
        assert_eq!(drive_problem(DriveKind::Ram, d), None);
        assert_eq!(
            drive_problem(DriveKind::Remote, d).unwrap(),
            r"capture.buffer_dir = X:\buf: is on a network drive; use a local SSD"
        );
        assert!(
            drive_problem(DriveKind::Removable, d)
                .unwrap()
                .contains("removable drive")
        );
        assert!(drive_problem(DriveKind::CdRom, d).is_some());
        assert!(drive_problem(DriveKind::Unknown, d).is_some());
    }

    #[test]
    fn warns_under_five_gb() {
        let d = Path::new(r"X:\buf");
        assert_eq!(low_space_warning(MIN_FREE_BYTES, d), None);
        assert_eq!(
            low_space_warning(MIN_FREE_BYTES - 1, d).unwrap(),
            r"capture.buffer_dir = X:\buf: only 4.9 GB free (5 GB recommended)"
        );
    }
}
