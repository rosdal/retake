//! ffmpeg `-stats` lines and the "encoder falling behind" rule.
//!
//! ffmpeg's own `speed=` is output time divided by wall time since ffmpeg started, an average
//! over the whole run: a slow first second keeps it low for a long time, and after an hour a
//! real slowdown barely moves it. So the monitor ignores it and measures the rate itself, from
//! how far `time=` advanced over the last [`BEHIND_FOR`] of wall time.

use std::collections::VecDeque;

use crate::Ms;

/// One progress report.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stats {
    pub frame: u64,
    pub fps: f32,
    /// Output position (`time=`), in seconds.
    pub time: f64,
    /// ffmpeg's cumulative `speed=`; logged, not used for decisions.
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

/// `HH:MM:SS.xx` (hours may exceed two digits) as seconds.
fn parse_time(s: &str) -> Option<f64> {
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || h < 0.0 || m < 0.0 || sec < 0.0 {
        return None;
    }
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Parses `frame= 103 fps= 51 … time=00:00:01.71 … speed=0.848x`. `None` for any other
/// line, or while ffmpeg still reports `time=N/A` or `speed=N/A`.
pub fn parse_stats(line: &str) -> Option<Stats> {
    Some(Stats {
        frame: field(line, "frame")?.parse().ok()?,
        fps: field(line, "fps")?.parse().ok()?,
        time: parse_time(field(line, "time")?)?,
        speed: field(line, "speed")?.strip_suffix('x')?.parse().ok()?,
    })
}

/// Below this rate the encoder can't keep up with real time.
pub const BEHIND_SPEED: f64 = 0.98;
/// The window the rate is measured over.
pub const BEHIND_FOR: Ms = 10_000;
/// Samples from a run's first seconds are ignored: capture and encoder start-up lag there.
pub const WARMUP: Ms = 5_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedEvent {
    FallingBehind,
    Recovered,
}

/// Turns a stream of (wall time, output time) samples from one ffmpeg run into one event
/// when the encoder starts falling behind and one when it recovers.
#[derive(Debug, Clone, Default)]
pub struct SpeedMonitor {
    /// Wall time of this run's first sample.
    first: Option<Ms>,
    samples: VecDeque<(Ms, f64)>,
    behind: bool,
}

impl SpeedMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn behind(&self) -> bool {
        self.behind
    }

    /// Forget everything. Call whenever a new ffmpeg run starts: its output time restarts.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `out` is the output position in seconds at wall time `now`.
    pub fn observe(&mut self, now: Ms, out: f64) -> Option<SpeedEvent> {
        let first = *self.first.get_or_insert(now);
        if now.saturating_sub(first) < WARMUP {
            return None;
        }
        self.samples.push_back((now, out));
        // Keep the newest sample that is at least a full window old as the baseline.
        while self.samples.len() >= 2 && now.saturating_sub(self.samples[1].0) >= BEHIND_FOR {
            self.samples.pop_front();
        }
        let (t0, o0) = self.samples[0];
        let wall = now.saturating_sub(t0);
        if wall < BEHIND_FOR {
            return None;
        }
        let slow = (out - o0) / (wall as f64 / 1000.0) < BEHIND_SPEED;
        match (slow, self.behind) {
            (true, false) => {
                self.behind = true;
                Some(SpeedEvent::FallingBehind)
            }
            (false, true) => {
                self.behind = false;
                Some(SpeedEvent::Recovered)
            }
            _ => None,
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
                time: 1.71,
                speed: 0.848
            })
        );
        assert_eq!(
            parse_stats(LAST),
            Some(Stats {
                frame: 274,
                fps: 67.0,
                time: 4.56,
                speed: 1.11
            })
        );
    }

    #[test]
    fn long_times_parse() {
        let s = parse_stats("frame=1 fps=1 time=02:03:04.50 speed=1x").unwrap();
        assert_eq!(s.time, 2.0 * 3600.0 + 3.0 * 60.0 + 4.5);
    }

    #[test]
    fn other_lines_are_not_stats() {
        assert_eq!(parse_stats("[hevc_amf @ 0000] some warning"), None);
        assert_eq!(
            parse_stats("frame=  10 fps=0.0 q=0.0 time=00:00:00.10 speed=N/A"),
            None
        );
        assert_eq!(
            parse_stats("frame=  10 fps=0.0 q=0.0 time=N/A speed=1x"),
            None
        );
        assert_eq!(parse_stats(""), None);
    }

    #[test]
    fn key_must_start_a_token() {
        // `xframe=` must not be read as `frame=`.
        assert_eq!(
            parse_stats("xframe=5 fps=1 time=00:00:01.00 speed=1x"),
            None
        );
    }

    /// Feeds one sample per second from `from` to `to` (inclusive) at `rate`, continuing
    /// from output position `out`. Returns the events and the final output position.
    fn feed(
        m: &mut SpeedMonitor,
        from: Ms,
        to: Ms,
        rate: f64,
        mut out: f64,
    ) -> (Vec<SpeedEvent>, f64) {
        let mut ev = Vec::new();
        let mut t = from;
        while t <= to {
            ev.extend(m.observe(t, out));
            out += rate;
            t += 1_000;
        }
        (ev, out)
    }

    #[test]
    fn real_time_is_fine_even_after_a_slow_start() {
        // ffmpeg's cumulative speed would sit under 0.98 for many seconds here.
        let mut m = SpeedMonitor::new();
        assert!(m.observe(0, 0.0).is_none());
        let (ev, _) = feed(&mut m, 1_000, 60_000, 1.0, 0.6);
        assert!(ev.is_empty(), "{ev:?}");
    }

    #[test]
    fn behind_after_a_slow_window_then_recovers() {
        let mut m = SpeedMonitor::new();
        let (ev, out) = feed(&mut m, 0, 20_000, 1.0, 0.0);
        assert!(ev.is_empty());
        let (ev, out) = feed(&mut m, 21_000, 40_000, 0.8, out);
        assert_eq!(ev, [SpeedEvent::FallingBehind], "reported once");
        assert!(m.behind());
        let (ev, _) = feed(&mut m, 41_000, 60_000, 1.0, out);
        assert_eq!(ev, [SpeedEvent::Recovered]);
        assert!(!m.behind());
    }

    #[test]
    fn a_short_hiccup_is_not_falling_behind() {
        let mut m = SpeedMonitor::new();
        let (_, out) = feed(&mut m, 0, 20_000, 1.0, 0.0);
        // Two seconds at half speed, then real time again: the 10 s rate stays above 0.98
        // only once the hiccup has left the window, and never drops far.
        let (ev, out) = feed(&mut m, 21_000, 21_000, 0.9, out);
        let (ev2, _) = feed(&mut m, 22_000, 60_000, 1.0, out);
        assert!(ev.is_empty() && ev2.is_empty(), "{ev:?} {ev2:?}");
    }

    #[test]
    fn needs_warmup_and_a_full_window_before_judging() {
        let mut m = SpeedMonitor::new();
        let (ev, _) = feed(&mut m, 0, WARMUP + 9_000, 0.5, 0.0);
        assert!(ev.is_empty());
    }

    #[test]
    fn reset_forgets_history() {
        let mut m = SpeedMonitor::new();
        let (_, _) = feed(&mut m, 0, 20_000, 0.5, 0.0);
        assert!(m.behind());
        m.reset();
        assert!(!m.behind());
        // A new run's output time starts at 0 again; that is not a slowdown.
        let (ev, _) = feed(&mut m, 21_000, 40_000, 1.0, 0.0);
        assert!(ev.is_empty());
    }
}
