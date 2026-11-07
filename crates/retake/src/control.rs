//! The service side of the window protocol (`retake_core::control`): starting the styled
//! window (`retake-ui.exe`, a separate process that only runs while it is open), the control
//! pipe it sends commands on, and `status.json`, which it reads.

use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Result, bail};
use retake_core::control::{Command, PIPE, STATUS_FILE, Status};
use windows::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{PIPE_ACCESS_INBOUND, ReadFile};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::core::HSTRING;

use crate::tray::MediaAction;

/// `%LOCALAPPDATA%\Retake\status.json`.
pub fn status_path() -> PathBuf {
    crate::logging::log_dir()
        .parent()
        .map_or_else(|| PathBuf::from(STATUS_FILE), |d| d.join(STATUS_FILE))
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// What the status file needs besides the status board.
pub struct StatusSetup {
    pub clips_dir: PathBuf,
    pub config: PathBuf,
    pub hotkey: String,
}

/// Writes `status.json` once a second while the styled window runs (and once more when it
/// closes), so the window shows live state and nothing is written while nobody looks.
pub fn start_status_writer(setup: StatusSetup) {
    let spawned = std::thread::Builder::new()
        .name("status-file".into())
        .spawn(move || {
            let path = status_path();
            let mut last = String::new();
            loop {
                std::thread::sleep(Duration::from_secs(1));
                if !window_running() {
                    continue;
                }
                let b = crate::status::get();
                let game = crate::current_game::get();
                let s = Status {
                    written: 0,
                    state: b.state.clone(),
                    game: game.as_ref().map(|g| g.name.clone()).or(b.game.clone()),
                    game_foreground: game.is_some() && game_foreground(),
                    capture: b.capture.clone(),
                    speed: b.speed,
                    buffer_secs: b.buffer_secs,
                    disk_free_gb: b.disk_free_gb,
                    clips_today: b.clips_today,
                    clips_session: b.clips_session,
                    queued: b.queued,
                    exporting: b.exporting,
                    paused: b.paused,
                    montage_busy: crate::montage::busy(),
                    warnings: b.warnings.clone(),
                    events: b.events.iter().cloned().collect(),
                    clips_dir: setup.clips_dir.clone(),
                    config: setup.config.clone(),
                    hotkey: setup.hotkey.clone(),
                    autostart: crate::autostart::enabled(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                };
                let body = serde_json::to_string(&s).unwrap_or_default();
                // Rewritten when something changed, and every 2 s as a heartbeat.
                let beat = unix_ms() / 1000 % 2 == 0;
                if body != last || beat {
                    let mut s = s;
                    s.written = unix_ms();
                    let tmp = path.with_extension("json.tmp");
                    if std::fs::write(&tmp, serde_json::to_string(&s).unwrap_or_default()).is_ok() {
                        let _ = std::fs::rename(&tmp, &path);
                    }
                    last = body;
                }
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("starting the status writer: {e}");
    }
}

/// Whether the foreground window belongs to the current game's process.
fn game_foreground() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    // SAFETY: plain queries.
    let pid = unsafe {
        let h = GetForegroundWindow();
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        pid
    };
    pid != 0 && crate::current_game::pids().contains(&pid)
}

static WINDOW: Mutex<Option<Child>> = Mutex::new(None);

fn window_running() -> bool {
    let mut w = WINDOW.lock().unwrap_or_else(|e| e.into_inner());
    match w.as_mut().map(|c| c.try_wait()) {
        Some(Ok(None)) => true,
        Some(_) => {
            *w = None;
            false
        }
        None => false,
    }
}

/// `retake-ui.exe` next to `retake.exe`.
fn ui_exe() -> Option<PathBuf> {
    let p = std::env::current_exe()
        .ok()?
        .with_file_name("retake-ui.exe");
    p.is_file().then_some(p)
}

/// Opens the styled window (or brings it forward); falls back to the plain window when
/// `retake-ui.exe` is missing or won't start.
pub fn open_window(config: &Path) {
    if window_running() {
        // The window activates its existing instance when started again.
    }
    let Some(exe) = ui_exe() else {
        crate::ui::show();
        return;
    };
    match std::process::Command::new(&exe)
        .arg("--config")
        .arg(config)
        .arg("--status")
        .arg(status_path())
        .spawn()
    {
        Ok(child) => {
            let mut w = WINDOW.lock().unwrap_or_else(|e| e.into_inner());
            if w.is_none() {
                *w = Some(child);
            }
        }
        Err(e) => {
            tracing::warn!("starting retake-ui.exe: {e}; opening the plain window");
            crate::ui::show();
        }
    }
}

/// The action a command from the window becomes, if it is allowed.
pub fn action_of(c: Command, clips: &Path) -> Option<MediaAction> {
    if !c.paths_inside(clips) {
        tracing::warn!(?c, "control command outside the clips folder ignored");
        return None;
    }
    use crate::montage::Job;
    use retake_core::montage::Format;
    Some(match c {
        Command::Pause { on } => MediaAction::Pause(on),
        Command::SaveFull => MediaAction::SaveFull,
        Command::Export { clip, preset } => MediaAction::Export(clip, preset),
        Command::Regrade { clip } => MediaAction::Regrade(clip),
        Command::Trim { clip, from, to } => MediaAction::Trim(clip, from, to),
        Command::Star { clip, on } => MediaAction::Star(clip, on),
        Command::Delete { clip } => MediaAction::Delete(clip),
        Command::Rescan => MediaAction::Montage(Job::Rescan),
        Command::MontageNew {
            song,
            youtube,
            starred,
            length,
            game,
        } => MediaAction::Montage(Job::New {
            song,
            format: if youtube {
                Format::Youtube
            } else {
                Format::Tiktok
            },
            starred,
            length,
            game,
        }),
        Command::MontageRender { dir, preview } => {
            MediaAction::Montage(Job::Render { dir, preview })
        }
        Command::CopyDiagnostics => MediaAction::CopyDiagnostics,
        Command::Autostart { on } => MediaAction::Autostart(on),
        Command::OpenLite => MediaAction::OpenLite,
    })
}

struct PipeHandle(HANDLE);
impl Drop for PipeHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// One client's bytes, until it closes its end.
fn read_client(pipe: &PipeHandle) -> Vec<u8> {
    let mut all = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut n = 0u32;
        // SAFETY: valid handle and buffer.
        let ok = unsafe { ReadFile(pipe.0, Some(&mut buf), Some(&mut n), None) };
        if ok.is_err() || n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n as usize]);
        if all.len() > 1 << 20 {
            break;
        }
    }
    all
}

/// Serves the control pipe: every line from a client becomes an action on the tray thread.
pub fn start_pipe(clips: PathBuf, deliver: crate::tray::TrayNotifier) {
    let spawned = std::thread::Builder::new()
        .name("control-pipe".into())
        .spawn(move || {
            loop {
                let pipe = match create_pipe() {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!("control pipe: {e:#}; the window can't send commands");
                        return;
                    }
                };
                // SAFETY: valid handle; blocking connect.
                let connected = match unsafe { ConnectNamedPipe(pipe.0, None) } {
                    Ok(()) => true,
                    Err(e) => e.code() == ERROR_PIPE_CONNECTED.to_hresult(),
                };
                if connected {
                    let bytes = read_client(&pipe);
                    for line in String::from_utf8_lossy(&bytes).lines() {
                        match Command::from_line(line) {
                            Some(c) => {
                                if let Some(a) = action_of(c, &clips) {
                                    deliver.action(a);
                                }
                            }
                            None if line.trim().is_empty() => {}
                            None => tracing::warn!(line, "unknown control command"),
                        }
                    }
                }
                // SAFETY: valid handle.
                let _ = unsafe { DisconnectNamedPipe(pipe.0) };
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("starting the control pipe: {e}");
    }
}

fn create_pipe() -> Result<PipeHandle> {
    // SAFETY: plain Win32 call; default security (the current user and administrators).
    let h = unsafe {
        CreateNamedPipeW(
            &HSTRING::from(PIPE),
            PIPE_ACCESS_INBOUND,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            None,
        )
    };
    if h.is_invalid() {
        bail!("CreateNamedPipeW: {}", windows::core::Error::from_thread());
    }
    Ok(PipeHandle(h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_become_actions_only_inside_the_clips_folder() {
        let clips = Path::new(r"D:\clips");
        assert_eq!(
            action_of(Command::Pause { on: true }, clips),
            Some(MediaAction::Pause(true))
        );
        assert_eq!(
            action_of(
                Command::Regrade {
                    clip: PathBuf::from(r"C:\elsewhere\x.mkv")
                },
                clips
            ),
            None
        );
        assert!(matches!(
            action_of(
                Command::MontageRender {
                    dir: PathBuf::from(r"D:\clips\montages\m"),
                    preview: true
                },
                clips
            ),
            Some(MediaAction::Montage(crate::montage::Job::Render {
                preview: true,
                ..
            }))
        ));
    }
}
