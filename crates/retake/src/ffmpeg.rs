//! Finding the pinned ffmpeg and checking it has what Retake needs.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use retake_core::ffmpeg_caps::missing_features;

use crate::child::run_capture;

/// Where to look, in order: `RETAKE_FFMPEG`, `<exe dir>\ffmpeg\ffmpeg.exe`, then the dev
/// layout `<exe dir>\..\..\third_party\ffmpeg\bin\ffmpeg.exe` (from `target\<profile>`).
pub fn candidates(exe_dir: &Path, env: Option<PathBuf>) -> Vec<PathBuf> {
    let mut c: Vec<PathBuf> = env.into_iter().collect();
    c.push(exe_dir.join("ffmpeg").join("ffmpeg.exe"));
    c.push(exe_dir.join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe"));
    c
}

/// The first candidate that exists.
pub fn locate(exe_dir: &Path) -> Result<PathBuf> {
    let tried = candidates(
        exe_dir,
        std::env::var_os("RETAKE_FFMPEG").map(PathBuf::from),
    );
    match tried.iter().find(|p| p.is_file()) {
        Some(p) => Ok(p.clone()),
        None => {
            let list: Vec<String> = tried.iter().map(|p| p.display().to_string()).collect();
            bail!("ffmpeg.exe not found. Looked in: {}", list.join(", "))
        }
    }
}

/// Fails, naming every missing encoder and filter, if `ffmpeg` lacks a required feature.
pub fn self_check(ffmpeg: &Path) -> Result<()> {
    let encoders = run_capture(ffmpeg, &["-hide_banner", "-encoders"])?;
    let filters = run_capture(ffmpeg, &["-hide_banner", "-filters"])?;
    let missing = missing_features(&encoders, &filters);
    if !missing.is_empty() {
        bail!("{} is missing: {}", ffmpeg.display(), missing.join(", "));
    }
    tracing::info!(ffmpeg = %ffmpeg.display(), "ffmpeg self-check passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_override_comes_first() {
        let c = candidates(
            Path::new(r"C:\app"),
            Some(PathBuf::from(r"C:\x\ffmpeg.exe")),
        );
        assert_eq!(c[0], PathBuf::from(r"C:\x\ffmpeg.exe"));
        assert_eq!(c[1], PathBuf::from(r"C:\app\ffmpeg\ffmpeg.exe"));
    }

    #[test]
    fn locate_reports_where_it_looked() {
        let t = std::env::temp_dir().join("retake-no-ffmpeg-here");
        let err = locate(&t).unwrap_err().to_string();
        assert!(err.contains("retake-no-ffmpeg-here"), "{err}");
    }

    /// Needs `scripts/fetch-ffmpeg.ps1` to have run, so it is opt-in:
    /// `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn pinned_ffmpeg_passes_self_check() {
        let p =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe");
        self_check(&p).unwrap();
    }

    #[test]
    fn self_check_fails_on_a_non_ffmpeg() {
        let cmd = PathBuf::from(std::env::var("ComSpec").unwrap());
        let err = self_check(&cmd).unwrap_err().to_string();
        assert!(err.contains("hevc_amf") || err.contains("exited"), "{err}");
    }
}
