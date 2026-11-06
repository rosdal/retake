//! When to start, stop and restart ffmpeg, as a state machine over game presence and ffmpeg
//! exits. Time is passed in, so every transition is deterministic.
//!
//! `Idle → Starting` (game seen) `→ Recording` (still there after 5 s) `→ Stopping` (game gone)
//! `→ Idle` (still gone after 5 s). ffmpeg crashes while recording are retried with backoff;
//! the sixth crash within 10 minutes goes to `Failed` until the game is next launched.

use crate::Ms;

pub const START_DEBOUNCE: Ms = 5_000;
pub const STOP_DEBOUNCE: Ms = 5_000;
/// Delay before the 1st, 2nd and every later restart.
pub const BACKOFF: [Ms; 3] = [1_000, 2_000, 5_000];
/// Restarts allowed within [`RESTART_WINDOW`]; the next crash gives up.
pub const MAX_RESTARTS: usize = 5;
pub const RESTART_WINDOW: Ms = 10 * 60 * 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Starting,
    Recording,
    Stopping,
    Failed,
}

/// What the caller must do after a transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// A session begins: empty the buffer folder, then start ffmpeg.
    StartSession,
    /// ffmpeg died mid-session: empty the buffer (the new run's segments and index start over)
    /// and start it again.
    Respawn,
    /// The session is over: stop ffmpeg gracefully.
    StopSession,
    /// Too many crashes: ffmpeg stays stopped until the next game launch.
    GiveUp,
}

#[derive(Debug, Clone)]
pub struct Watcher {
    phase: Phase,
    /// When the current phase began (debounce timers).
    since: Ms,
    respawn_at: Option<Ms>,
    /// When each restart in the current window was scheduled.
    restarts: Vec<Ms>,
}

impl Default for Watcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Watcher {
    pub fn new() -> Self {
        Watcher {
            phase: Phase::Idle,
            since: 0,
            respawn_at: None,
            restarts: Vec::new(),
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    fn enter(&mut self, phase: Phase, now: Ms) {
        self.phase = phase;
        self.since = now;
    }

    /// Feed the latest process snapshot.
    pub fn on_game(&mut self, now: Ms, running: bool) -> Option<Command> {
        let waited = now.saturating_sub(self.since);
        match (self.phase, running) {
            (Phase::Idle, true) => self.enter(Phase::Starting, now),
            (Phase::Starting, false) => self.enter(Phase::Idle, now),
            (Phase::Starting, true) if waited >= START_DEBOUNCE => {
                self.enter(Phase::Recording, now);
                self.restarts.clear();
                self.respawn_at = None;
                return Some(Command::StartSession);
            }
            (Phase::Recording, false) => self.enter(Phase::Stopping, now),
            (Phase::Stopping, true) => self.enter(Phase::Recording, now),
            (Phase::Stopping, false) if waited >= STOP_DEBOUNCE => {
                self.enter(Phase::Idle, now);
                self.respawn_at = None;
                return Some(Command::StopSession);
            }
            (Phase::Failed, false) => self.enter(Phase::Idle, now),
            _ => {}
        }
        None
    }

    /// ffmpeg exited without being asked to, or could not be started.
    pub fn on_ffmpeg_exit(&mut self, now: Ms) -> Option<Command> {
        if !matches!(self.phase, Phase::Recording | Phase::Stopping) {
            return None;
        }
        self.restarts
            .retain(|&t| now.saturating_sub(t) < RESTART_WINDOW);
        if self.restarts.len() >= MAX_RESTARTS {
            self.enter(Phase::Failed, now);
            self.respawn_at = None;
            return Some(Command::GiveUp);
        }
        let delay = BACKOFF[self.restarts.len().min(BACKOFF.len() - 1)];
        self.restarts.push(now);
        self.respawn_at = Some(now + delay);
        None
    }

    /// Call regularly; fires a due restart.
    pub fn on_tick(&mut self, now: Ms) -> Option<Command> {
        match self.respawn_at {
            Some(at) if self.phase == Phase::Recording && now >= at => {
                self.respawn_at = None;
                Some(Command::Respawn)
            }
            _ => None,
        }
    }
}

/// Whether any of `running` (exe file names) is one of `wanted`, ignoring ASCII case.
pub fn game_running<'a>(running: impl IntoIterator<Item = &'a str>, wanted: &[String]) -> bool {
    running
        .into_iter()
        .any(|r| wanted.iter().any(|w| w.eq_ignore_ascii_case(r)))
}

/// What the tray icon shows: grey, red, or yellow (for both warning and failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStatus {
    Idle,
    Recording,
    Warning,
    Failed,
}

pub fn tray_status(phase: Phase, warning: bool) -> TrayStatus {
    match phase {
        Phase::Failed => TrayStatus::Failed,
        _ if warning => TrayStatus::Warning,
        Phase::Recording | Phase::Stopping => TrayStatus::Recording,
        Phase::Idle | Phase::Starting => TrayStatus::Idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Command::*;
    use Phase::*;

    /// A watcher that has just started recording at t = 5 000.
    fn recording() -> Watcher {
        let mut w = Watcher::new();
        assert_eq!(w.on_game(0, true), None);
        assert_eq!(w.on_game(5_000, true), Some(StartSession));
        w
    }

    #[test]
    fn starts_after_debounce() {
        let mut w = Watcher::new();
        assert_eq!(w.on_game(0, true), None);
        assert_eq!(w.phase(), Starting);
        assert_eq!(w.on_game(4_999, true), None);
        assert_eq!(w.on_game(5_000, true), Some(StartSession));
        assert_eq!(w.phase(), Recording);
    }

    #[test]
    fn game_crashing_during_starting_goes_back_to_idle() {
        let mut w = Watcher::new();
        w.on_game(0, true);
        assert_eq!(w.on_game(2_000, false), None);
        assert_eq!(w.phase(), Idle);
        // The debounce starts over on the next launch.
        w.on_game(3_000, true);
        assert_eq!(w.on_game(7_000, true), None);
        assert_eq!(w.on_game(8_000, true), Some(StartSession));
    }

    #[test]
    fn stops_after_debounce() {
        let mut w = recording();
        assert_eq!(w.on_game(6_000, false), None);
        assert_eq!(w.phase(), Stopping);
        assert_eq!(w.on_game(10_999, false), None);
        assert_eq!(w.on_game(11_000, false), Some(StopSession));
        assert_eq!(w.phase(), Idle);
    }

    #[test]
    fn game_back_while_stopping_keeps_recording() {
        let mut w = recording();
        w.on_game(6_000, false);
        assert_eq!(w.on_game(8_000, true), None);
        assert_eq!(w.phase(), Recording);
        // A later exit needs the full debounce again.
        w.on_game(9_000, false);
        assert_eq!(w.on_game(13_999, false), None);
        assert_eq!(w.on_game(14_000, false), Some(StopSession));
    }

    #[test]
    fn crash_restarts_with_backoff() {
        let mut w = recording();
        assert_eq!(w.on_ffmpeg_exit(10_000), None);
        assert_eq!(w.on_tick(10_999), None);
        assert_eq!(w.on_tick(11_000), Some(Respawn));
        assert_eq!(w.on_tick(11_001), None, "fires once");
        w.on_ffmpeg_exit(20_000);
        assert_eq!(w.on_tick(21_999), None);
        assert_eq!(w.on_tick(22_000), Some(Respawn));
        w.on_ffmpeg_exit(30_000);
        assert_eq!(w.on_tick(34_999), None);
        assert_eq!(w.on_tick(35_000), Some(Respawn));
        w.on_ffmpeg_exit(40_000);
        assert_eq!(w.on_tick(45_000), Some(Respawn), "5 s from then on");
    }

    #[test]
    fn sixth_crash_in_ten_minutes_fails() {
        let mut w = recording();
        for i in 0..5 {
            let t = 10_000 + i * 10_000;
            assert_eq!(w.on_ffmpeg_exit(t), None);
            assert_eq!(w.on_tick(t + 5_000), Some(Respawn));
        }
        assert_eq!(w.on_ffmpeg_exit(70_000), Some(GiveUp));
        assert_eq!(w.phase(), Failed);
        assert_eq!(w.on_tick(80_000), None);
        assert_eq!(
            w.on_game(80_000, true),
            None,
            "no retry while the game runs"
        );
        assert_eq!(w.phase(), Failed);
    }

    #[test]
    fn old_crashes_leave_the_window() {
        let mut w = recording();
        for i in 0..5 {
            let t = 10_000 + i * 10_000;
            w.on_ffmpeg_exit(t);
            w.on_tick(t + 5_000);
        }
        // 10 minutes after the first crash it no longer counts.
        assert_eq!(w.on_ffmpeg_exit(10_000 + RESTART_WINDOW), None);
        assert_eq!(w.phase(), Recording);
    }

    #[test]
    fn failed_clears_on_next_launch() {
        let mut w = recording();
        for i in 0..6 {
            w.on_ffmpeg_exit(10_000 + i * 10_000);
            w.on_tick(15_000 + i * 10_000);
        }
        assert_eq!(w.phase(), Failed);
        assert_eq!(w.on_game(100_000, false), None);
        assert_eq!(w.phase(), Idle);
        w.on_game(101_000, true);
        assert_eq!(w.on_game(106_000, true), Some(StartSession));
        assert_eq!(
            w.on_ffmpeg_exit(107_000),
            None,
            "restart budget is per session"
        );
    }

    #[test]
    fn crash_while_stopping_respawns_only_if_game_returns() {
        let mut w = recording();
        w.on_game(6_000, false);
        w.on_ffmpeg_exit(7_000);
        assert_eq!(w.on_tick(9_000), None, "no respawn while stopping");
        w.on_game(9_000, true);
        assert_eq!(w.on_tick(9_000), Some(Respawn));
    }

    #[test]
    fn stopping_drops_a_pending_respawn() {
        let mut w = recording();
        w.on_game(6_000, false);
        w.on_ffmpeg_exit(7_000);
        assert_eq!(w.on_game(11_000, false), Some(StopSession));
        w.on_game(12_000, true);
        assert_eq!(w.on_tick(13_000), None);
    }

    #[test]
    fn exit_while_idle_is_ignored() {
        let mut w = Watcher::new();
        assert_eq!(w.on_ffmpeg_exit(0), None);
        assert_eq!(w.phase(), Idle);
    }

    #[test]
    fn game_names_match_case_insensitively() {
        let wanted = vec!["RainbowSix.exe".to_owned()];
        assert!(game_running(["explorer.exe", "rainbowsix.EXE"], &wanted));
        assert!(!game_running(["RainbowSix_BE.exe"], &wanted));
        assert!(!game_running([], &wanted));
    }

    #[test]
    fn tray_status_mapping() {
        assert_eq!(tray_status(Idle, false), TrayStatus::Idle);
        assert_eq!(tray_status(Starting, false), TrayStatus::Idle);
        assert_eq!(tray_status(Recording, false), TrayStatus::Recording);
        assert_eq!(tray_status(Stopping, false), TrayStatus::Recording);
        assert_eq!(tray_status(Recording, true), TrayStatus::Warning);
        assert_eq!(tray_status(Idle, true), TrayStatus::Warning);
        assert_eq!(tray_status(Failed, false), TrayStatus::Failed);
        assert_eq!(tray_status(Failed, true), TrayStatus::Failed);
    }
}
