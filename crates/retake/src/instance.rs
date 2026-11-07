//! One Retake per user session, via the named mutex `Local\Retake`, and the named events a
//! second launch (`retake.exe --save`, `--save-full`) uses to ask the running one for a clip.

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, EVENT_MODIFY_STATE, INFINITE, OpenEventW, SetEvent,
    WaitForMultipleObjects,
};
use windows::core::{PCWSTR, w};

/// Signalled by `--save` (index 0, a standard clip) and `--save-full` (index 1, the whole
/// buffer), in the order of the clip hotkeys.
const SAVE_EVENTS: [PCWSTR; 3] = [
    w!("Local\\RetakeSave"),
    w!("Local\\RetakeSaveFull"),
    // A second plain launch: open the window.
    w!("Local\\RetakeOpenWindow"),
];

/// Index of the "open the window" event.
pub const OPEN_WINDOW: usize = 2;

/// Asks the running Retake to open its window. Returns false if none is running.
pub fn signal_open_window() -> bool {
    // SAFETY: plain Win32 calls; the handle is closed after use.
    unsafe {
        let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, SAVE_EVENTS[OPEN_WINDOW]) else {
            return false;
        };
        let ok = SetEvent(h).is_ok();
        let _ = CloseHandle(h);
        ok
    }
}

/// Asks the running Retake for a clip. Returns false if none is running.
pub fn signal_save(full: bool) -> bool {
    let name = SAVE_EVENTS[usize::from(full)];
    // SAFETY: plain Win32 calls; the handle is closed after use.
    unsafe {
        let Ok(h) = OpenEventW(EVENT_MODIFY_STATE, false, name) else {
            return false;
        };
        let ok = SetEvent(h).is_ok();
        let _ = CloseHandle(h);
        ok
    }
}

/// Creates the save events and calls `on_save(index)` on a background thread each time one
/// is signalled, for as long as Retake runs.
pub fn listen_for_saves(on_save: impl Fn(usize) + Send + 'static) -> windows::core::Result<()> {
    // SAFETY: auto-reset events, kept open for the life of the process.
    let handles: Vec<HANDLE> = unsafe {
        SAVE_EVENTS
            .iter()
            .map(|n| CreateEventW(None, false, false, *n))
            .collect::<windows::core::Result<_>>()?
    };
    struct Handles(Vec<HANDLE>);
    // SAFETY: kernel handles are usable from any thread.
    unsafe impl Send for Handles {}
    let h = Handles(handles);
    std::thread::Builder::new()
        .name("save-events".into())
        .spawn(move || {
            let h = h;
            loop {
                // SAFETY: valid handles that are never closed.
                let r = unsafe { WaitForMultipleObjects(&h.0, false, INFINITE) };
                let i = r.0 as usize;
                if i < h.0.len() {
                    on_save(i);
                } else {
                    tracing::warn!("waiting for save events failed; --save stops working");
                    return;
                }
            }
        })
        .map_err(|e| windows::core::Error::new(windows::core::HRESULT(-1), e.to_string()))?;
    Ok(())
}

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
    fn save_signal_reaches_the_listener() {
        use std::sync::mpsc;
        // A running Retake listens on the same auto-reset events and would take the signal.
        let Some(_only) = SingleInstance::acquire() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        let tx = std::sync::Mutex::new(tx);
        listen_for_saves(move |i| {
            let _ = tx.lock().unwrap().send(i);
        })
        .unwrap();
        assert!(signal_save(true));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)), Ok(1));
        assert!(signal_save(false));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)), Ok(0));
    }

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
