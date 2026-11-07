//! The service ↔ window protocol. The window (`retake-ui.exe`, a separate process started
//! only while it's open) reads `status.json` and the clips folder, and sends [`Command`]s as
//! JSON lines over the local named pipe [`PIPE`]. The service does every write that matters
//! (recording, the library, grading, montages), so there is one writer.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The control pipe (local clients only).
pub const PIPE: &str = r"\\.\pipe\retake-control";
/// The status file's name, next to the logs folder.
pub const STATUS_FILE: &str = "status.json";

/// What the window asks the service to do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Pause {
        on: bool,
    },
    SaveFull,
    Export {
        clip: PathBuf,
        preset: String,
    },
    Regrade {
        clip: PathBuf,
    },
    /// Keep `from..to` seconds as a new clip.
    Trim {
        clip: PathBuf,
        from: f64,
        to: f64,
    },
    Star {
        clip: PathBuf,
        on: bool,
    },
    Delete {
        clip: PathBuf,
    },
    Rescan,
    MontageNew {
        song: PathBuf,
        youtube: bool,
        starred: bool,
        length: Option<f64>,
        game: Option<String>,
    },
    MontageRender {
        dir: PathBuf,
        preview: bool,
    },
    CopyDiagnostics,
    Autostart {
        on: bool,
    },
    /// Open the plain window (the "Lite UI").
    OpenLite,
}

impl Command {
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).expect("commands serialise");
        s.push('\n');
        s
    }

    pub fn from_line(line: &str) -> Option<Command> {
        serde_json::from_str(line.trim()).ok()
    }

    /// The files a command touches must be inside the clips folder: another local program
    /// can't make the service export, grade or delete files elsewhere.
    pub fn paths_inside(&self, clips: &Path) -> bool {
        let inside = |p: &Path| {
            p.is_absolute()
                && !p
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
                && p.starts_with(clips)
        };
        match self {
            Command::Export { clip, .. }
            | Command::Regrade { clip }
            | Command::Trim { clip, .. }
            | Command::Star { clip, .. }
            | Command::Delete { clip } => inside(clip),
            Command::MontageRender { dir, .. } => inside(dir),
            // The song may live anywhere; it is only read.
            _ => true,
        }
    }
}

/// What the window shows of the service, written as `status.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// Unix ms when written (the window treats an old file as "Retake isn't running").
    pub written: i64,
    pub state: String,
    pub game: Option<String>,
    /// The game's window has the focus: the window stops drawing.
    pub game_foreground: bool,
    pub capture: String,
    pub speed: f32,
    pub buffer_secs: u32,
    pub disk_free_gb: Option<u64>,
    pub clips_today: u32,
    pub clips_session: u32,
    pub queued: usize,
    pub exporting: bool,
    pub paused: bool,
    pub montage_busy: bool,
    pub warnings: Vec<String>,
    pub events: Vec<(String, String)>,
    pub clips_dir: PathBuf,
    pub config: PathBuf,
    pub hotkey: String,
    pub autostart: bool,
    pub version: String,
}

impl Status {
    /// Whether the service wrote this recently enough to be running.
    pub fn live(&self, now_ms: i64) -> bool {
        now_ms - self.written < 5_000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_as_lines() {
        let c = Command::Export {
            clip: PathBuf::from(r"D:\clips\raw\a.mkv"),
            preset: "vertical".into(),
        };
        let line = c.to_line();
        assert!(line.ends_with('\n') && line.contains("\"cmd\":\"export\""));
        assert_eq!(Command::from_line(&line), Some(c));
        assert_eq!(
            Command::from_line(r#"{"cmd":"pause","on":true}"#),
            Some(Command::Pause { on: true })
        );
        assert_eq!(Command::from_line("nonsense"), None);
    }

    #[test]
    fn paths_must_stay_in_the_clips_folder() {
        let clips = Path::new(r"D:\clips");
        let ok = Command::Delete {
            clip: PathBuf::from(r"D:\clips\raw\a.mkv"),
        };
        assert!(ok.paths_inside(clips));
        for bad in [
            r"C:\Windows\system32\x.dll",
            r"D:\clips\..\secret.txt",
            r"raw\a.mkv",
        ] {
            let c = Command::Delete {
                clip: PathBuf::from(bad),
            };
            assert!(!c.paths_inside(clips), "{bad}");
        }
        assert!(Command::SaveFull.paths_inside(clips));
    }

    #[test]
    fn old_status_means_not_running() {
        let s = Status {
            written: 1_000,
            ..Default::default()
        };
        assert!(s.live(3_000) && !s.live(7_000));
        let back: Status = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        // Unknown or missing fields are fine (window and service from different builds).
        let partial: Status = serde_json::from_str(r#"{"state":"Idle","extra":1}"#).unwrap();
        assert_eq!(partial.state, "Idle");
    }
}
