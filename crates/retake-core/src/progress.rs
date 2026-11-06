//! ffmpeg `-stats` lines and the "encoder falling behind" rule.

use crate::Ms;

/// One progress report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub frame: u64,
    pub fps: f32,
    pub speed: f32,
}

/// The value after `key=` where `key` starts a whitespace-separated token; ffmpeg pads
/// values with spaces (`fps= 51`), so leading spaces are skipped.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}=");
    let mut from = 0;
    while let Some(i) = line[from..].find(&pat) {
        let at = from + i;
        if at == 0 || line[..at].ends_with(char::is_whitespace) {
            return line[at + pat.len()..].split_whitespace().next();
        }
        from = at + pat.len();
    }
    None
}

/// Parses `frame= 103 fps= 51 … speed=0.848x`. `None` for any other line, or while ffmpeg
/// still reports `speed=N/A`.
pub fn parse_stats(line: &str) -> Option<Stats> {
    Some(Stats {
        frame: field(line, "frame")?.parse().ok()?,
        fps: field(line, "fps")?.parse().ok()?,
        speed: field(line, "speed")?.strip_suffix('x')?.parse().ok()?,
    })
}

/// Below this `speed` the encoder can't keep up with real time.
pub const BEHIND_SPEED: f32 = 0.98;
/// How long `speed` must stay below [`BEHIND_SPEED`] before it counts.
pub const BEHIND_FOR: Ms = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedEvent {
    FallingBehind,
    Recovered,
}

/// Turns a stream of `speed` samples into one event when the encoder starts falling behind
/// and one when it recovers.
#[derive(Debug, Clone, Default)]
pub struct SpeedMonitor {
    slow_since: Option<Ms>,
    behind: bool,
}

impl SpeedMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn behind(&self) -> bool {
        self.behind
    }

    /// Forget everything, e.g. when ffmpeg restarts.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn observe(&mut self, now: Ms, speed: f32) -> Option<SpeedEvent> {
        if speed < BEHIND_SPEED {
            let since = *self.slow_since.get_or_insert(now);
            if !self.behind && now.saturating_sub(since) >= BEHIND_FOR {
                self.behind = true;
                return Some(SpeedEvent::FallingBehind);
            }
            None
        } else {
            self.slow_since = None;
            if self.behind {
                self.behind = false;
                return Some(SpeedEvent::Recovered);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real lines from the pinned ffmpeg 8.0.1 with `-stats` (split on `\r`).
    const RUNNING: &str =
        "frame=  103 fps= 51 q=31.0 size=N/A time=00:00:01.71 bitrate=N/A speed=0.848x    ";
    const LAST: &str =
        "frame=  274 fps= 67 q=-1.0 Lsize=N/A time=00:00:04.56 bitrate=N/A speed=1.11x    ";

    #[test]
    fn parses_stats_lines() {
        assert_eq!(
            parse_stats(RUNNING),
            Some(Stats {
                frame: 103,
                fps: 51.0,
                speed: 0.848
            })
        );
        assert_eq!(
            parse_stats(LAST),
            Some(Stats {
                frame: 274,
                fps: 67.0,
                speed: 1.11
            })
        );
    }

    #[test]
    fn other_lines_are_not_stats() {
        assert_eq!(parse_stats("[hevc_amf @ 0000] some warning"), None);
        assert_eq!(
            parse_stats("frame=  10 fps=0.0 q=0.0 size=N/A speed=N/A"),
            None
        );
        assert_eq!(parse_stats(""), None);
    }

    #[test]
    fn key_must_start_a_token() {
        // `xframe=` must not be read as `frame=`.
        assert_eq!(parse_stats("xframe=5 fps=1 speed=1x"), None);
    }

    #[test]
    fn behind_after_ten_slow_seconds() {
        let mut m = SpeedMonitor::new();
        assert_eq!(m.observe(0, 0.9), None);
        assert_eq!(m.observe(9_999, 0.9), None);
        assert_eq!(m.observe(10_000, 0.9), Some(SpeedEvent::FallingBehind));
        assert!(m.behind());
        assert_eq!(m.observe(11_000, 0.9), None, "reported once");
        assert_eq!(m.observe(12_000, 1.0), Some(SpeedEvent::Recovered));
        assert!(!m.behind());
    }

    #[test]
    fn a_fast_sample_restarts_the_window() {
        let mut m = SpeedMonitor::new();
        m.observe(0, 0.5);
        m.observe(8_000, 1.0);
        assert_eq!(m.observe(9_000, 0.5), None);
        assert_eq!(m.observe(18_999, 0.5), None);
        assert_eq!(m.observe(19_000, 0.5), Some(SpeedEvent::FallingBehind));
    }

    #[test]
    fn exactly_threshold_is_not_slow() {
        let mut m = SpeedMonitor::new();
        m.observe(0, BEHIND_SPEED);
        assert_eq!(m.observe(20_000, BEHIND_SPEED), None);
    }

    #[test]
    fn reset_forgets_the_slow_window() {
        let mut m = SpeedMonitor::new();
        m.observe(0, 0.5);
        m.observe(10_000, 0.5);
        m.reset();
        assert!(!m.behind());
        assert_eq!(m.observe(11_000, 0.5), None);
    }
}
