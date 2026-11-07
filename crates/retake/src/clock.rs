//! Time for presses, sessions and quiet mode, and the local time a clip is named after.
//!
//! Timing uses a monotonic clock, so an NTP sync or a manual clock change can't move a clip's
//! window or its post-roll deadline. Local time is only for file names.

use std::sync::OnceLock;
use std::time::Instant;

use retake_core::Ms;
use retake_core::clip::Stamp;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// Where [`now_ms`] starts, so `now_ms() - x` for any interval Retake uses (minutes at most)
/// can't underflow early in a run.
pub const BASE: Ms = 24 * 60 * 60 * 1000;

/// Milliseconds on a monotonic clock: [`BASE`] plus the time since first use.
pub fn now_ms() -> Ms {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    BASE + EPOCH.get_or_init(Instant::now).elapsed().as_millis() as Ms
}

/// The local date and time now.
pub fn local_stamp() -> Stamp {
    // SAFETY: no arguments; returns a value.
    let t = unsafe { GetLocalTime() };
    Stamp {
        year: t.wYear,
        month: t.wMonth as u8,
        day: t.wDay as u8,
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
        second: t.wSecond as u8,
    }
}

/// The local date and time of a Unix time in seconds.
pub fn stamp_of_unix(secs: i64) -> Stamp {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let v = secs * 10_000_000 + 116_444_736_000_000_000;
    let ft = FILETIME {
        dwLowDateTime: v as u32,
        dwHighDateTime: (v >> 32) as u32,
    };
    let (mut utc, mut t) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: valid in/out structs.
    unsafe {
        let _ = FileTimeToSystemTime(&ft, &mut utc);
        let _ = SystemTimeToTzSpecificLocalTime(None, &utc, &mut t);
    }
    Stamp {
        year: t.wYear,
        month: t.wMonth as u8,
        day: t.wDay as u8,
        hour: t.wHour as u8,
        minute: t.wMinute as u8,
        second: t.wSecond as u8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic_and_starts_high() {
        let a = now_ms();
        assert!(a >= BASE);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let b = now_ms();
        assert!((50..1_000).contains(&(b - a)), "{a} -> {b}");
    }

    #[test]
    fn local_stamp_is_plausible() {
        let s = local_stamp();
        assert!(s.year >= 2026 && (1..=12).contains(&s.month) && (1..=31).contains(&s.day));
        assert!(s.hour < 24 && s.minute < 60 && s.second < 61);
    }
}
