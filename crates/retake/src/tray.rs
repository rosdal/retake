//! Tray icon, its menu, and the Win32 message loop.
//!
//! A hidden top-level window (not message-only, which would miss session broadcasts) receives
//! menu clicks posted by the tray's event handler and `WM_QUERYENDSESSION`/`WM_ENDSESSION`.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};
use retake_core::hotkey::Hotkey;
use retake_core::watcher::TrayStatus;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SW_SHOWNORMAL, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
    WM_ENDSESSION, WM_HOTKEY, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, w};

/// Folders and files the tray menu opens.
#[derive(Debug, Clone)]
pub struct TrayPaths {
    pub config: PathBuf,
    pub clips: PathBuf,
    pub logs: PathBuf,
}

const WM_TRAY_MENU: u32 = WM_APP + 1;
const WM_TRAY_STATUS: u32 = WM_APP + 2;
const WM_TRAY_CLIP: u32 = WM_APP + 3;

/// One hotkey to register; `label` is the config text, used in warnings.
pub struct HotkeySpec {
    pub label: String,
    pub key: Hotkey,
}

/// The newest saved clip, written by the saver thread, read by "Open last clip".
static LAST_CLIP: Mutex<Option<PathBuf>> = Mutex::new(None);

const STATUSES: [TrayStatus; 4] = [
    TrayStatus::Idle,
    TrayStatus::Recording,
    TrayStatus::Warning,
    TrayStatus::Failed,
];

fn status_to_wparam(s: TrayStatus) -> usize {
    STATUSES.iter().position(|x| *x == s).expect("listed")
}

fn status_from_wparam(w: usize) -> Option<TrayStatus> {
    STATUSES.get(w).copied()
}

/// Icon colour and tooltip for a status, plus a hotkey problem if there is one.
fn look(s: TrayStatus, problem: Option<&str>) -> ([u8; 3], String) {
    const YELLOW: [u8; 3] = [234, 179, 8];
    let (rgb, tip) = match s {
        TrayStatus::Idle => ([140, 140, 140], "Retake — idle"),
        TrayStatus::Recording => ([220, 38, 38], "Retake — recording"),
        TrayStatus::Warning => (YELLOW, "Retake — warning, see the log"),
        TrayStatus::Failed => (YELLOW, "Retake — recording failed, see the log"),
    };
    match problem {
        Some(p) => (YELLOW, format!("{tip}\n{p}")),
        None => (rgb, tip.to_owned()),
    }
}

/// The tray line for hotkeys that could not be registered.
fn hotkey_problem(failed: &[&str]) -> Option<String> {
    match failed {
        [] => None,
        [one] => Some(format!(
            "Hotkey {one} unavailable — another app may be using it"
        )),
        many => Some(format!(
            "Hotkeys {} unavailable — another app may be using them",
            many.join(", ")
        )),
    }
}

/// Sets the tray status from any thread.
#[derive(Clone, Copy)]
pub struct TrayNotifier(isize);

impl TrayNotifier {
    pub fn set(&self, s: TrayStatus) {
        // SAFETY: the window lives for the whole run; posting is thread-safe.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.0 as *mut _)),
                WM_TRAY_STATUS,
                WPARAM(status_to_wparam(s)),
                LPARAM(0),
            )
        };
    }

    /// Remembers `path` for "Open last clip" and enables that item.
    pub fn clip_saved(&self, path: PathBuf) {
        *LAST_CLIP.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
        // SAFETY: the window lives for the whole run; posting is thread-safe.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.0 as *mut _)),
                WM_TRAY_CLIP,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

#[derive(Clone, Copy)]
enum Action {
    OpenConfig = 0,
    OpenClips = 1,
    OpenLogs = 2,
    Quit = 3,
    OpenLastClip = 4,
}

impl Action {
    fn from_wparam(w: usize) -> Option<Self> {
        [
            Self::OpenConfig,
            Self::OpenClips,
            Self::OpenLogs,
            Self::Quit,
            Self::OpenLastClip,
        ]
        .into_iter()
        .find(|a| *a as usize == w)
    }
}

struct LoopState {
    paths: TrayPaths,
    on_exit: Option<Box<dyn FnOnce()>>,
    on_hotkey: Box<dyn Fn(usize)>,
    /// Hotkeys that could not be registered, as a tooltip line.
    problem: Option<String>,
    status: TrayStatus,
}

thread_local! {
    static STATE: RefCell<Option<LoopState>> = const { RefCell::new(None) };
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    static LAST_ITEM: RefCell<Option<MenuItem>> = const { RefCell::new(None) };
}

/// Runs `on_exit` once, whichever way we leave.
fn run_exit_hook() {
    let hook = STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.on_exit.take()));
    if let Some(hook) = hook {
        hook();
    }
}

fn open(path: &std::path::Path, fallback_notepad: bool) {
    let file = HSTRING::from(path.as_os_str());
    // SAFETY: all strings outlive the call.
    let rc = unsafe { ShellExecuteW(None, w!("open"), &file, None, None, SW_SHOWNORMAL) };
    if rc.0 as isize > 32 {
        return;
    }
    if fallback_notepad {
        if let Err(e) = std::process::Command::new("notepad.exe").arg(path).spawn() {
            tracing::warn!(path = %path.display(), "could not open: {e}");
        }
    } else {
        tracing::warn!(path = %path.display(), code = rc.0 as isize, "ShellExecuteW failed");
    }
}

fn set_status(s: TrayStatus) {
    let problem = STATE.with(|st| {
        st.borrow_mut().as_mut().and_then(|st| {
            st.status = s;
            st.problem.clone()
        })
    });
    let (rgb, tip) = look(s, problem.as_deref());
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            if let Err(e) = icon(rgb).and_then(|i| tray.set_icon(Some(i)).map_err(Into::into)) {
                tracing::warn!("setting tray icon: {e:#}");
            }
            if let Err(e) = tray.set_tooltip(Some(tip)) {
                tracing::warn!("setting tray tooltip: {e}");
            }
        }
    });
}

fn on_menu(action: Action) {
    let paths = STATE.with(|s| s.borrow().as_ref().map(|st| st.paths.clone()));
    let Some(paths) = paths else { return };
    match action {
        Action::OpenConfig => open(&paths.config, true),
        Action::OpenClips => open(&paths.clips, false),
        Action::OpenLogs => open(&paths.logs, false),
        Action::OpenLastClip => {
            let last = LAST_CLIP.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(p) = last {
                open(&p, false);
            }
        }
        Action::Quit => {
            tracing::info!("quit from tray");
            // SAFETY: called on the message-loop thread.
            unsafe { PostQuitMessage(0) };
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY_MENU => {
            if let Some(a) = Action::from_wparam(wparam.0) {
                on_menu(a);
            }
            LRESULT(0)
        }
        WM_TRAY_STATUS => {
            if let Some(s) = status_from_wparam(wparam.0) {
                set_status(s);
            }
            LRESULT(0)
        }
        WM_TRAY_CLIP => {
            LAST_ITEM.with(|i| {
                if let Some(i) = i.borrow().as_ref() {
                    i.set_enabled(true);
                }
            });
            LRESULT(0)
        }
        WM_HOTKEY => {
            // The callback only sends on a channel and plays a sound; it never touches the
            // tray state, so calling it under this shared borrow is fine.
            STATE.with(|s| {
                if let Some(st) = s.borrow().as_ref() {
                    (st.on_hotkey)(wparam.0);
                }
            });
            LRESULT(0)
        }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION => {
            if wparam.0 != 0 {
                // The process may be terminated as soon as we return, so shut down here.
                tracing::info!("session ending");
                run_exit_hook();
            }
            LRESULT(0)
        }
        // SAFETY: default handling for everything else.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn create_hidden_window() -> Result<HWND> {
    // SAFETY: class and window creation with static strings; the window lives until exit.
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        let class = w!("RetakeHiddenWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            anyhow::bail!(
                "RegisterClassW failed: {}",
                windows::core::Error::from_thread()
            );
        }
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Retake"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .context("CreateWindowExW")
    }
}

/// A 32×32 icon drawn in code: a filled circle of colour `rgb` with a soft edge.
fn icon(rgb: [u8; 3]) -> Result<Icon> {
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let alpha = (14.5 - d).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (alpha * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, N, N).context("building tray icon")
}

/// Shows the tray icon and pumps messages until Quit or session end. `on_ready` runs once the
/// icon exists, with a handle for setting its status from any thread. `on_exit` runs exactly
/// once on every way out, including setup failures; a failure is logged before it runs so the
/// error reaches the log even when `on_exit` flushes and closes it.
pub fn run(
    paths: TrayPaths,
    hotkeys: Vec<HotkeySpec>,
    on_hotkey: impl Fn(usize) + 'static,
    on_ready: impl FnOnce(TrayNotifier) + 'static,
    on_exit: impl FnOnce() + 'static,
) -> Result<()> {
    STATE.with(|s| {
        *s.borrow_mut() = Some(LoopState {
            paths,
            on_exit: Some(Box::new(on_exit)),
            on_hotkey: Box::new(on_hotkey),
            problem: None,
            status: TrayStatus::Idle,
        })
    });
    let result = run_loop(&hotkeys, on_ready);
    if let Err(e) = &result {
        tracing::error!("tray failed: {e:#}");
    }
    run_exit_hook();
    result
}

fn run_loop(hotkeys: &[HotkeySpec], on_ready: impl FnOnce(TrayNotifier)) -> Result<()> {
    let hwnd = create_hidden_window()?;
    let hwnd_raw = hwnd.0 as isize;

    let mut failed = Vec::new();
    for (i, h) in hotkeys.iter().enumerate() {
        let mods = HOT_KEY_MODIFIERS(h.key.modifiers) | MOD_NOREPEAT;
        // SAFETY: our own window; the id is unique per hotkey.
        match unsafe { RegisterHotKey(Some(hwnd), i as i32, mods, h.key.vk) } {
            Ok(()) => tracing::info!(hotkey = %h.label, "hotkey registered"),
            Err(e) => {
                tracing::warn!(hotkey = %h.label, "RegisterHotKey failed: {e}");
                failed.push(h.label.as_str());
            }
        }
    }
    let problem = hotkey_problem(&failed);
    STATE.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.problem = problem.clone();
        }
    });

    let items = [
        (
            MenuItem::new("Open last clip", false, None),
            Action::OpenLastClip,
        ),
        (MenuItem::new("Open config", true, None), Action::OpenConfig),
        (
            MenuItem::new("Open clips folder", true, None),
            Action::OpenClips,
        ),
        (MenuItem::new("Open logs", true, None), Action::OpenLogs),
        (MenuItem::new("Quit", true, None), Action::Quit),
    ];
    let menu = Menu::new();
    menu.append(&items[0].0)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append_items(&[&items[1].0, &items[2].0, &items[3].0])?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&items[4].0)?;

    let ids: Vec<_> = items.iter().map(|(i, a)| (i.id().clone(), *a)).collect();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some((_, action)) = ids.iter().find(|(id, _)| *id == event.id) {
            let hwnd = HWND(hwnd_raw as *mut _);
            // SAFETY: the window lives for the whole run; posting is thread-safe.
            let _ = unsafe {
                PostMessageW(
                    Some(hwnd),
                    WM_TRAY_MENU,
                    WPARAM(*action as usize),
                    LPARAM(0),
                )
            };
        }
    }));

    let (rgb, tip) = look(TrayStatus::Idle, problem.as_deref());
    let tray = TrayIconBuilder::new()
        .with_tooltip(tip)
        .with_icon(icon(rgb)?)
        .with_menu(Box::new(menu))
        .build()
        .context("creating tray icon")?;
    TRAY.with(|t| *t.borrow_mut() = Some(tray));
    LAST_ITEM.with(|i| *i.borrow_mut() = Some(items[0].0.clone()));
    tracing::info!("tray ready");
    on_ready(TrayNotifier(hwnd_raw));

    let mut msg = MSG::default();
    // SAFETY: standard message loop on the thread that owns the window.
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    TRAY.with(|t| t.borrow_mut().take());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [TrayStatus; 4] = [
        TrayStatus::Idle,
        TrayStatus::Recording,
        TrayStatus::Warning,
        TrayStatus::Failed,
    ];

    #[test]
    fn status_round_trips_through_wparam() {
        for s in ALL {
            assert_eq!(status_from_wparam(status_to_wparam(s)), Some(s));
        }
        assert_eq!(status_from_wparam(99), None);
    }

    #[test]
    fn colours_are_grey_red_yellow() {
        assert_eq!(look(TrayStatus::Idle, None).0, [140, 140, 140]);
        assert_eq!(look(TrayStatus::Recording, None).0, [220, 38, 38]);
        assert_eq!(look(TrayStatus::Warning, None).0, [234, 179, 8]);
        assert_eq!(look(TrayStatus::Failed, None).0, [234, 179, 8]);
        assert_eq!(look(TrayStatus::Recording, None).1, "Retake — recording");
    }

    #[test]
    fn hotkey_problem_turns_yellow_and_adds_a_line() {
        let p = hotkey_problem(&["F9"]).unwrap();
        assert_eq!(p, "Hotkey F9 unavailable — another app may be using it");
        let (rgb, tip) = look(TrayStatus::Recording, Some(&p));
        assert_eq!(rgb, [234, 179, 8]);
        assert_eq!(
            tip,
            format!(
                "Retake — recording
{p}"
            )
        );
        assert_eq!(
            hotkey_problem(&["F9", "Ctrl+F9"]).unwrap(),
            "Hotkeys F9, Ctrl+F9 unavailable — another app may be using them"
        );
        assert_eq!(hotkey_problem(&[]), None);
    }

    #[test]
    fn actions_round_trip() {
        for a in [
            Action::OpenConfig,
            Action::OpenClips,
            Action::OpenLogs,
            Action::Quit,
            Action::OpenLastClip,
        ] {
            assert_eq!(
                Action::from_wparam(a as usize).map(|x| x as usize),
                Some(a as usize)
            );
        }
    }

    #[test]
    fn icon_builds_for_every_status() {
        for s in ALL {
            icon(look(s, None).0).unwrap();
        }
    }
}
