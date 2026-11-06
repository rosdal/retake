//! One Retake per user session, via the named mutex `Local\Retake`.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

/// Holds the mutex for as long as it lives.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// `None` if another Retake already holds the mutex.
    pub fn acquire() -> Option<Self> {
        // SAFETY: plain Win32 call; GetLastError is read immediately after it.
        unsafe {
            let handle = CreateMutexW(None, true, w!("Local\\Retake")).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(SingleInstance(handle))
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A Windows toast. Uses PowerShell's app id, so no registration is needed.
pub fn toast(msg: &str) {
    use tauri_winrt_notification::Toast;
    if let Err(e) = Toast::new(Toast::POWERSHELL_APP_ID)
        .title("Retake")
        .text1(msg)
        .show()
    {
        tracing::warn!("toast failed: {e:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_fails_until_first_is_dropped() {
        // Same process, but CreateMutexW still reports ERROR_ALREADY_EXISTS.
        let first = SingleInstance::acquire();
        if first.is_none() {
            // A real Retake is running on this machine; nothing to assert.
            return;
        }
        assert!(SingleInstance::acquire().is_none());
        drop(first);
        assert!(SingleInstance::acquire().is_some());
    }
}
