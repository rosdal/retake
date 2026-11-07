//! Which monitor the game's window is on, and whether the foreground app is fullscreen. Only
//! the list of top-level windows and the process id each belongs to are read: no game's
//! process is ever opened (anti-cheat rule 1).

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetShellWindow, GetWindowRect, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible,
};
use windows::core::BOOL;

struct Search<'a> {
    pids: &'a [u32],
    /// Largest visible window so far: (area, window).
    best: Option<(i64, HWND)>,
}

extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `Search` that `game_monitor` passed and keeps alive for the call.
    let s = unsafe { &mut *(lparam.0 as *mut Search) };
    // SAFETY: plain queries on a window handle Windows just gave us.
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return true.into();
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if !s.pids.contains(&pid) {
            return true.into();
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_ok() {
            let area = i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
            if s.best.is_none_or(|(a, _)| area > a) {
                s.best = Some((area, hwnd));
            }
        }
    }
    true.into()
}

/// The monitor (`HMONITOR` as a number) showing the largest visible window of any of `pids`,
/// or `None` if they have no such window yet.
pub fn game_monitor(pids: &[u32]) -> Option<isize> {
    if pids.is_empty() {
        return None;
    }
    let mut search = Search { pids, best: None };
    // SAFETY: `search` outlives the enumeration, which calls `visit` synchronously.
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(&raw mut search as isize)) };
    let (_, hwnd) = search.best?;
    // SAFETY: a plain query on a window handle.
    let m = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    (!m.is_invalid()).then_some(m.0 as isize)
}

/// The process id of the foreground window if it covers its whole monitor (borderless or
/// exclusive fullscreen), else `None`. The desktop itself never counts.
pub fn fullscreen_pid() -> Option<u32> {
    // SAFETY: plain queries on window and monitor handles Windows gives us.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_invalid() || hwnd == GetShellWindow() || !IsWindowVisible(hwnd).as_bool() {
            return None;
        }
        let mut r = RECT::default();
        GetWindowRect(hwnd, &mut r).ok()?;
        let m = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(m, &mut mi).as_bool() {
            return None;
        }
        let mr = mi.rcMonitor;
        if !retake_core::games::covers_monitor(
            (r.left, r.top, r.right, r.bottom),
            (mr.left, mr.top, mr.right, mr.bottom),
        ) {
            return None;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (pid != 0).then_some(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pids_or_no_windows_is_none() {
        assert_eq!(game_monitor(&[]), None);
        assert_eq!(game_monitor(&[u32::MAX]), None);
    }

    #[test]
    fn finds_explorers_monitor_on_a_desktop() {
        // Explorer owns the taskbar; a CI runner may have no interactive desktop, so only
        // check when it is running.
        let pids: Vec<u32> = crate::procs::processes()
            .unwrap()
            .into_iter()
            .filter(|(_, n)| n.eq_ignore_ascii_case("explorer.exe"))
            .map(|(p, _)| p)
            .collect();
        if !pids.is_empty() {
            assert!(game_monitor(&pids).is_some());
        }
    }
}
