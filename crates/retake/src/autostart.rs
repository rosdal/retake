//! "Start with Windows": a `HKCU\…\Run` entry (no service, no admin rights). Started that way,
//! Retake waits 30 s before it starts watching for the game, so it doesn't slow the boot.

use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
};
use windows::core::w;

const RUN: windows::core::PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const NAME: windows::core::PCWSTR = w!("Retake");
/// The argument the Run entry passes.
pub const ARG: &str = "--autostart";
/// How long an autostarted Retake waits before watching for the game.
pub const DELAY_MS: u64 = 30_000;

/// Whether the Run entry exists.
pub fn enabled() -> bool {
    let mut len = 0u32;
    // SAFETY: querying the size only.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN,
            NAME,
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut len),
        )
    };
    rc == ERROR_SUCCESS
}

/// Adds or removes the Run entry for this exe.
pub fn set(on: bool) -> windows::core::Result<()> {
    // SAFETY: plain registry calls with valid strings and a correctly sized buffer.
    unsafe {
        if on {
            let exe = std::env::current_exe().map_err(|e| {
                windows::core::Error::new(windows::core::HRESULT(-1), e.to_string())
            })?;
            let value: Vec<u16> = format!("\"{}\" {ARG}", exe.display())
                .encode_utf16()
                .chain([0])
                .collect();
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                NAME,
                REG_SZ.0,
                Some(value.as_ptr().cast()),
                (value.len() * 2) as u32,
            )
            .ok()
        } else {
            let rc = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, NAME);
            if rc == ERROR_SUCCESS || !enabled() {
                Ok(())
            } else {
                rc.ok()
            }
        }
    }
}
