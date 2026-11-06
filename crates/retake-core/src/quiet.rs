//! Game-time quiet mode: while Siege runs, Retake is only a screen recorder. Background work
//! (grading, exports, scans, the setup check) may start only while the game isn't running and
//! once it has been closed for [`RESUME_AFTER`].

use crate::Ms;

/// How long after Siege closes background work waits before starting.
pub const RESUME_AFTER: Ms = 30_000;

/// Whether background work may start. Times are monotonic clock ms.
///
/// The gate starts closed: until the first process poll it isn't known whether Siege is
/// already running, and a job must not read its files while it might be.
#[derive(Debug, Default, Clone)]
pub struct QuietGate {
    /// `None` until the first [`QuietGate::on_game`].
    running: Option<bool>,
    closed_at: Option<Ms>,
}

impl QuietGate {
    pub fn new() -> QuietGate {
        QuietGate::default()
    }

    /// Records whether the game is running now. Returns the new state when it changed.
    /// A game that isn't running at the first call didn't just close: that opens the gate at
    /// once and isn't reported as a change.
    pub fn on_game(&mut self, running: bool, now: Ms) -> Option<bool> {
        let before = self.running.replace(running);
        if before == Some(running) || (before.is_none() && !running) {
            return None;
        }
        if before == Some(true) {
            self.closed_at = Some(now);
        }
        Some(running)
    }

    pub fn game_running(&self) -> bool {
        self.running == Some(true)
    }

    /// Whether a background job may start at `now`.
    pub fn may_start(&self, now: Ms) -> bool {
        self.running == Some(false) && self.closed_at.is_none_or(|t| now >= t + RESUME_AFTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_until_the_first_poll() {
        let mut g = QuietGate::new();
        assert!(!g.may_start(0));
        assert!(!g.game_running());
        assert_eq!(g.on_game(true, 0), Some(true));
        assert!(!g.may_start(100_000));
    }

    #[test]
    fn open_until_the_game_starts() {
        let mut g = QuietGate::new();
        assert_eq!(g.on_game(false, 0), None);
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
