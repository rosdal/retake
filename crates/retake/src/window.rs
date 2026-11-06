//! Which monitor Siege's window is on. Only the list of top-level windows and the process id
//! each belongs to are read: Siege's process is never opened (anti-cheat rule 1).

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowThreadProcessId, IsIconic, IsWindowVisible,
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
