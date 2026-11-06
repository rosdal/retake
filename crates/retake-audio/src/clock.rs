//! Clocks: the performance counter (what WASAPI stamps packets with) and the precise system
//! time (what ffmpeg's `RTCTIME` reads).

use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;

/// 100 ns intervals between 1601-01-01 (FILETIME) and 1970-01-01 (Unix).
const FILETIME_UNIX_DIFF: i64 = 116_444_736_000_000_000;

/// The performance counter in 100 ns units, the unit of WASAPI's QPC positions.
pub fn qpc_100ns() -> u64 {
    let (mut count, mut freq) = (0i64, 0i64);
    // SAFETY: both pointers are valid locals; these calls cannot fail on XP or later.
    unsafe {
        let _ = QueryPerformanceCounter(&mut count);
        let _ = QueryPerformanceFrequency(&mut freq);
    }
    (i128::from(count) * 10_000_000 / i128::from(freq.max(1))) as u64
}

/// The precise system time in Unix microseconds (ffmpeg's `av_gettime`).
pub fn unix_us() -> i64 {
    // SAFETY: no arguments; returns a value.
    let ft = unsafe { GetSystemTimePreciseAsFileTime() };
    let t = (i64::from(ft.dwHighDateTime) << 32) | i64::from(ft.dwLowDateTime);
    (t - FILETIME_UNIX_DIFF) / 10
}

/// Converts a QPC time (100 ns units) to Unix microseconds using the current offset between the
/// two clocks. Re-reading the offset each time means a slewed system clock never accumulates
/// into drift.
pub fn qpc_to_unix_us(qpc: u64) -> i64 {
    let now_qpc = qpc_100ns();
    let now = unix_us();
    now - (now_qpc as i64 - qpc as i64) / 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_agree() {
        let u = unix_us();
        // After 2024-01-01 and before 2100.
        assert!(
            u > 1_704_067_200_000_000 && u < 4_102_444_800_000_000,
            "{u}"
        );
        let q = qpc_100ns();
        let back = qpc_to_unix_us(q);
        assert!((back - unix_us()).abs() < 50_000, "{back}");
        // 10 ms earlier in QPC is 10 ms earlier in Unix time.
        let earlier = qpc_to_unix_us(q - 100_000);
        assert!(
            (back - earlier - 10_000).abs() < 2_000,
            "{}",
            back - earlier
        );
    }
}
