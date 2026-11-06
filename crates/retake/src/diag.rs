//! `--diag`: memory and handle counts of Retake and its recording ffmpeg, logged every
//! 5 minutes, for soak tests. Only our own process and our own child are queried.

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

/// Interval between diag lines.
pub const EVERY_MS: u64 = 5 * 60 * 1_000;

/// Open handles of `process` (our own process or our own child).
pub fn handle_count(process: HANDLE) -> Option<u32> {
    let mut n = 0u32;
    // SAFETY: a valid process handle we own; `n` is a valid out-pointer.
    unsafe { GetProcessHandleCount(process, &mut n) }.ok()?;
    Some(n)
}

/// Retake's own working set in bytes and handle count.
pub fn own() -> (Option<u64>, Option<u32>) {
    // SAFETY: the pseudo-handle of the current process needs no closing.
    let me = unsafe { GetCurrentProcess() };
    let mut c = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is sized correctly.
    let mem = unsafe { GetProcessMemoryInfo(me, &mut c, c.cb) }
        .ok()
        .map(|()| c.WorkingSetSize as u64);
    (mem, handle_count(me))
}

#[cfg(test)]
mod tests {
    #[test]
    fn own_numbers_are_plausible() {
        let (mem, handles) = super::own();
        assert!(mem.unwrap() > 1024 * 1024);
        assert!(handles.unwrap() > 5);
    }
}
