// Used from the saver thread and wiring added next; removed once they land.
#![allow(dead_code)]
//! Wall-clock time for presses and sessions, and the local time a clip is named after.

use std::time::{SystemTime, UNIX_EPOCH};

use retake_core::Ms;
use retake_core::clip::Stamp;
use windows::Win32::System::SystemInformation::GetLocalTime;

/// Milliseconds since the Unix epoch.
pub fn wall_ms() -> Ms {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as Ms)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_is_after_2026() {
        assert!(wall_ms() > 1_767_225_600_000); // 2026-01-01
    }

    #[test]
    fn local_stamp_is_plausible() {
        let s = local_stamp();
        assert!(s.year >= 2026 && (1..=12).contains(&s.month) && (1..=31).contains(&s.day));
        assert!(s.hour < 24 && s.minute < 60 && s.second < 61);
    }
}
