//! The recording-setup check: finds Siege's `GameSettings.ini` files and logs settings that
//! spoil footage. Runs as a background job, so only while Siege is closed (anti-cheat rule 4:
//! no live game files); it checks the gate again before each file in case Siege just started.

use std::path::{Path, PathBuf};

use retake_core::siege_settings::check;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Documents, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

use crate::background::{SharedGate, gate_open};

/// The user's Documents folder, wherever it is redirected (OneDrive included).
fn documents() -> Option<PathBuf> {
    // SAFETY: the returned string is copied, then freed with CoTaskMemFree as documented.
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None).ok()?;
        let path = p.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(p.0 as *const _));
        path
    }
}

/// `GameSettings.ini` in each profile folder under `base` (`My Games\Rainbow Six - Siege`).
fn settings_files_in(base: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path().join("GameSettings.ini"))
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    files
}

/// Reads every Siege settings file and logs what the check finds.
pub fn run(gate: &SharedGate) {
    let Some(docs) = documents() else {
        tracing::warn!("setup check: Documents folder not found");
        return;
    };
    let files = settings_files_in(&docs.join("My Games").join("Rainbow Six - Siege"));
    if files.is_empty() {
        tracing::info!("setup check: no Siege settings file found");
        return;
    }
    for file in files {
        if !gate_open(gate) {
            tracing::info!("setup check: Siege is running; stopped");
            return;
        }
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(file = %file.display(), "setup check: {e}");
                continue;
            }
        };
        let report = check(&text);
        for w in &report.warnings {
            tracing::warn!(file = %file.display(), "{w}");
        }
        for (key, value) in &report.unconfirmed {
            tracing::info!(file = %file.display(), key, value, "Siege setting (name not confirmed yet)");
        }
        if report.warnings.is_empty() {
            tracing::info!(file = %file.display(), "setup check: settings look right");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_profile_settings_files() {
        let t = tempfile::tempdir().unwrap();
        for (dir, file) in [("aaa", true), ("bbb", false), ("ccc", true)] {
            std::fs::create_dir(t.path().join(dir)).unwrap();
            if file {
                std::fs::write(t.path().join(dir).join("GameSettings.ini"), "x").unwrap();
            }
        }
        let got = settings_files_in(t.path());
        assert_eq!(got.len(), 2);
        assert!(got[0].ends_with(r"aaa\GameSettings.ini"));
        assert!(settings_files_in(&t.path().join("missing")).is_empty());
    }

    #[test]
    fn documents_folder_exists() {
        assert!(documents().is_some_and(|d| d.is_dir()));
    }
}
