//! Game-time quiet mode: while Siege runs, Retake is only a screen recorder. Background work
//! (grading, exports, scans, the setup check) may start only while the game isn't running and
//! once it has been closed for [`RESUME_AFTER`].

use crate::Ms;

/// How long after Siege closes background work waits before starting.
pub const RESUME_AFTER: Ms = 30_000;

/// Whether background work may start. Times are monotonic clock ms.
#[derive(Debug, Default, Clone)]
pub struct QuietGate {
    running: bool,
    closed_at: Option<Ms>,
}

impl QuietGate {
    pub fn new() -> QuietGate {
        QuietGate::default()
    }

    /// Records whether the game is running now. Returns the new state when it changed.
    pub fn on_game(&mut self, running: bool, now: Ms) -> Option<bool> {
        if running == self.running {
            return None;
        }
        self.running = running;
        if !running {
            self.closed_at = Some(now);
        }
        Some(running)
    }

    pub fn game_running(&self) -> bool {
        self.running
    }

    /// Whether a background job may start at `now`.
    pub fn may_start(&self, now: Ms) -> bool {
        !self.running && self.closed_at.is_none_or(|t| now >= t + RESUME_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_until_the_game_starts() {
        let mut g = QuietGate::new();
        assert!(g.may_start(0));
        assert_eq!(g.on_game(true, 1_000), Some(true));
        assert!(g.game_running());
        assert!(!g.may_start(1_000));
        assert_eq!(g.on_game(true, 3_000), None);
    }

    #[test]
    fn reopens_thirty_seconds_after_the_game_closes() {
        let mut g = QuietGate::new();
        g.on_game(true, 0);
        assert_eq!(g.on_game(false, 100_000), Some(false));
        assert!(!g.may_start(129_999));
        assert!(g.may_start(130_000));
        assert_eq!(g.on_game(false, 140_000), None);
    }

    #[test]
    fn restarting_the_game_closes_it_again() {
        let mut g = QuietGate::new();
        g.on_game(true, 0);
        g.on_game(false, 10_000);
        g.on_game(true, 20_000);
        assert!(!g.may_start(100_000));
    }
}
