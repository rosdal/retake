//! Applying an edited `config.toml` without a manual restart. The file is checked every
//! second. Hotkeys, the sound cue, pre/post roll and the auto-clip threshold apply at once;
//! grading and export settings from the next job; anything else relaunches Retake when
//! nothing is being recorded (during a match it waits for the match to end and says so). An
//! invalid file keeps the old settings running and says what's wrong.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use retake_core::config;

/// The argument a relaunched Retake gets: wait for the old one to let go of the lock.
pub const RESTARTED: &str = "--restarted";

fn modified(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Starts the watcher thread. `quit` asks the running Retake to shut down (on the tray thread).
/// `apply` applies a live or job-level change in place and returns `false` if it can't (then
/// Retake restarts after the match instead).
pub fn start(
    path: PathBuf,
    quit: impl Fn() + Send + 'static,
    apply: impl Fn(&config::Config, config::Reload) -> bool + Send + 'static,
) {
    let spawn = std::thread::Builder::new()
        .name("config-watch".into())
        .spawn(move || {
            let mut seen = modified(&path);
            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
            let mut pending = false;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now = modified(&path);
                if now != seen {
                    // Let an editor finish writing.
                    std::thread::sleep(Duration::from_millis(500));
                    seen = modified(&path);
                    let new = std::fs::read_to_string(&path).unwrap_or_default();
                    if new == text {
                        continue;
                    }
                    match config::load_str(&new) {
                        Ok(c) => {
                            let kind = config::reload_kind(&text, &new);
                            tracing::info!(?kind, keys = ?config::changed_keys(&text, &new), "config.toml changed");
                            text = new;
                            match kind {
                                config::Reload::Nothing => {}
                                config::Reload::Live | config::Reload::Jobs if !pending => {
                                    if apply(&c, kind) {
                                        crate::status::event("Settings applied");
                                        crate::notify::toast("Settings applied.");
                                    } else {
                                        pending = true;
                                    }
                                }
                                _ => pending = true,
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                "config.toml has an error; keeping the old settings: {e}"
                            );
                            crate::notify::toast(&format!(
                                "config.toml has an error, so the old settings stay: {e}"
                            ));
                            continue;
                        }
                    }
                }
                if pending {
                    let recording = crate::status::get().state == "Recording";
                    if recording {
                        // Once per change: say it waits for the match.
                        static SAID: std::sync::atomic::AtomicBool =
                            std::sync::atomic::AtomicBool::new(false);
                        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            crate::status::event("Settings apply after this match");
                            crate::notify::toast("Settings saved: they apply after this match.");
                        }
                        continue;
                    }
                    tracing::info!("applying the new config: restarting Retake");
                    match std::env::current_exe()
                        .map(|exe| std::process::Command::new(exe).arg(RESTARTED).spawn())
                    {
                        Ok(Ok(_)) => {
                            crate::instance::toast("Settings applied.");
                            quit();
                            return;
                        }
                        Ok(Err(e)) | Err(e) => tracing::error!("restarting Retake: {e}"),
                    }
                    pending = false;
                }
            }
        });
    if let Err(e) = spawn {
        tracing::warn!("config changes need a restart: {e}");
    }
}
