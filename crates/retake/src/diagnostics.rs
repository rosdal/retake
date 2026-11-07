//! "Copy diagnostics": version, machine, config (with the user's profile path anonymised) and
//! the last 200 log lines, on the clipboard, for a bug report.

use std::path::Path;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

/// CF_UNICODETEXT.
const CF_UNICODETEXT: u32 = 13;

/// The diagnostics text.
pub fn text(config: &Path) -> String {
    let profile = std::env::var("USERPROFILE").unwrap_or_default();
    let anon = |s: String| {
        if profile.is_empty() {
            s
        } else {
            s.replace(&profile, "%USERPROFILE%")
                .replace(&profile.replace('\\', "/"), "%USERPROFILE%")
        }
    };
    let mut out = format!(
        "Retake {} ({})\nWindows {}\nGPU {}\n\n--- config.toml ---\n",
        env!("CARGO_PKG_VERSION"),
        option_env!("RETAKE_GIT").unwrap_or("dev"),
        crate::sysinfo::windows_build(),
        crate::sysinfo::gpu_name()
    );
    out.push_str(&anon(
        std::fs::read_to_string(config).unwrap_or_else(|e| format!("(unreadable: {e})")),
    ));
    out.push_str("\n--- last 200 log lines ---\n");
    let log = std::fs::read_dir(crate::logging::log_dir())
        .ok()
        .and_then(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("retake"))
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        })
        .and_then(|e| std::fs::read_to_string(e.path()).ok())
        .unwrap_or_default();
    let lines: Vec<&str> = log.lines().collect();
    for l in &lines[lines.len().saturating_sub(200)..] {
        out.push_str(&anon((*l).to_owned()));
        out.push('\n');
    }
    out
}

/// Puts `s` on the clipboard as Unicode text.
pub fn to_clipboard(s: &str) -> Result<()> {
    let wide: Vec<u16> = s.encode_utf16().chain([0]).collect();
    // SAFETY: standard clipboard sequence; the memory is handed to the clipboard on success.
    unsafe {
        OpenClipboard(None).context("OpenClipboard")?;
        let r = (|| -> Result<()> {
            EmptyClipboard().context("EmptyClipboard")?;
            let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).context("GlobalAlloc")?;
            let p = GlobalLock(mem).cast::<u16>();
            if p.is_null() {
                bail!("GlobalLock failed");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))).context("SetClipboardData")?;
            Ok(())
        })();
        let _ = CloseClipboard();
        r
    }
}

/// Copies the diagnostics for `config` to the clipboard.
pub fn copy(config: &Path) -> Result<()> {
    to_clipboard(&text(config))
}
