//! What Retake is doing right now, for the Status tab and the tray tooltip. The recorder, the
//! saver and the grading queue write it; the window and the tray read it.

use std::collections::VecDeque;
use std::sync::Mutex;

#[derive(Debug, Clone, Default)]
pub struct Board {
    /// "Idle", "Recording", "Paused", "Stopped: disk almost full", …
    pub state: String,
    /// The game exe being recorded, if any.
    pub game: Option<String>,
    /// e.g. "HEVC 240 fps (AMF)".
    pub capture: String,
    pub speed: f32,
    pub buffer_secs: u32,
    pub disk_free_gb: Option<u64>,
    pub clips_today: u32,
    /// Clips saved since the game started.
    pub clips_session: u32,
    /// Grading and export jobs waiting.
    pub queued: usize,
    pub exporting: bool,
    pub paused: bool,
    pub warnings: Vec<String>,
    /// Newest first: (local time "HH:MM:SS", text).
    pub events: VecDeque<(String, String)>,
}

static BOARD: Mutex<Option<Board>> = Mutex::new(None);

/// Changes the board under its lock.
pub fn update(f: impl FnOnce(&mut Board)) {
    let mut g = BOARD.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(Board::default));
}

/// A copy of the board.
pub fn get() -> Board {
    BOARD
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default()
}

/// Adds an event line (keeps the last 10).
pub fn event(text: impl Into<String>) {
    let stamp = crate::clock::local_stamp();
    let t = format!("{:02}:{:02}:{:02}", stamp.hour, stamp.minute, stamp.second);
    update(|b| {
        b.events.push_front((t, text.into()));
        b.events.truncate(10);
    });
}

/// The tray tooltip's detail line, e.g. "Recording · HEVC 240 fps · 3 clips this session".
pub fn tooltip_detail(b: &Board) -> String {
    let mut parts = vec![b.state.clone()];
    if b.game.is_some() && !b.capture.is_empty() {
        parts.push(b.capture.clone());
    }
    if b.clips_session > 0 {
        parts.push(format!(
            "{} clip{} this session",
            b.clips_session,
            if b.clips_session == 1 { "" } else { "s" }
        ));
    }
    if b.exporting || b.queued > 0 {
        parts.push(format!("{} to grade/export", b.queued.max(1)));
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_reads_like_a_sentence() {
        let b = Board {
            state: "Recording".into(),
            game: Some("RainbowSix.exe".into()),
            capture: "HEVC 240 fps".into(),
            clips_session: 3,
            ..Board::default()
        };
        assert_eq!(
            tooltip_detail(&b),
            "Recording · HEVC 240 fps · 3 clips this session"
        );
        let idle = Board {
            state: "Idle".into(),
            ..Board::default()
        };
        assert_eq!(tooltip_detail(&idle), "Idle");
    }

    #[test]
    fn events_keep_the_newest_ten() {
        for i in 0..12 {
            event(format!("e{i}"));
        }
        let b = get();
        assert_eq!(b.events.len(), 10);
        assert_eq!(b.events[0].1, "e11");
    }
}
