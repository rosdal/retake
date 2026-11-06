//! Tray icon, its menu, and the Win32 message loop.
//!
//! The icon is plain `Shell_NotifyIconW` and the menu a `TrackPopupMenu` built on each right
//! click: no tray crate, so nothing links input-simulation APIs (the `muda` menu crate imports
//! one for its Copy/Paste items, which anti-cheat rule 5 forbids). A hidden top-level
//! window (not message-only, which would miss session broadcasts) receives the icon's mouse
//! messages, hotkeys and `WM_QUERYENDSESSION`/`WM_ENDSESSION`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use retake_core::hotkey::Hotkey;
use retake_core::watcher::TrayStatus;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, HICON, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, MSG, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SW_SHOWNORMAL, SetForegroundWindow, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CONTEXTMENU, WM_ENDSESSION,
    WM_HOTKEY, WM_LBUTTONUP, WM_NULL, WM_QUERYENDSESSION, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Folders and files the tray menu opens.
#[derive(Debug, Clone)]
pub struct TrayPaths {
    pub config: PathBuf,
    pub clips: PathBuf,
    pub logs: PathBuf,
}

const WM_TRAY_STATUS: u32 = WM_APP + 2;
/// Mouse messages from the notification-area icon.
const WM_TRAY_ICON: u32 = WM_APP + 4;
/// Our single icon's id.
const ICON_ID: u32 = 1;

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

/// Icon colour and tooltip for a status, plus a hotkey problem if there is one. The problem
/// is a second tooltip line; it turns an idle icon yellow but never hides red (recording).
fn look(s: TrayStatus, problem: Option<&str>) -> ([u8; 3], String) {
    const YELLOW: [u8; 3] = [234, 179, 8];
    let (rgb, tip) = match s {
        TrayStatus::Idle => ([140, 140, 140], "Retake — idle"),
        TrayStatus::Recording => ([220, 38, 38], "Retake — recording"),
        TrayStatus::Warning => (YELLOW, "Retake — warning, see the log"),
        TrayStatus::Failed => (YELLOW, "Retake — recording failed, see the log"),
    };
    match problem {
        Some(p) => {
            let rgb = if s == TrayStatus::Idle { YELLOW } else { rgb };
            (rgb, format!("{tip}\n{p}"))
        }
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

/// `text` as the fixed 128-unit, NUL-terminated buffer a tooltip takes; longer text is cut.
fn tip_buffer(text: &str) -> [u16; 128] {
    let mut buf = [0u16; 128];
    for (slot, unit) in buf.iter_mut().take(127).zip(text.encode_utf16()) {
        *slot = unit;
    }
    buf
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

    /// Remembers `path` for "Open last clip"; the menu enables that item from then on.
    pub fn clip_saved(&self, path: PathBuf) {
        *LAST_CLIP.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }
}

/// Menu commands; the value is the menu item id (0 means "nothing chosen").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    OpenConfig = 1,
    OpenClips = 2,
    OpenLogs = 3,
    Quit = 4,
    OpenLastClip = 5,
}

impl Action {
    fn from_id(id: usize) -> Option<Self> {
        [
            Self::OpenConfig,
            Self::OpenClips,
            Self::OpenLogs,
            Self::Quit,
            Self::OpenLastClip,
        ]
        .into_iter()
        .find(|a| *a as usize == id)
    }
}

/// The menu, top to bottom: `None` is a separator, the bool says whether the item is enabled.
fn menu_items(have_clip: bool) -> Vec<Option<(Action, &'static str, bool)>> {
    vec![
        Some((Action::OpenLastClip, "Open last clip", have_clip)),
        None,
        Some((Action::OpenConfig, "Open config", true)),
        Some((Action::OpenClips, "Open clips folder", true)),
        Some((Action::OpenLogs, "Open logs", true)),
        None,
        Some((Action::Quit, "Quit", true)),
    ]
}

struct LoopState {
    paths: TrayPaths,
    on_exit: Option<Box<dyn FnOnce()>>,
    on_hotkey: Box<dyn Fn(usize)>,
    /// Hotkeys that could not be registered, as a tooltip line.
    problem: Option<String>,
}

/// What the notification-area icon currently shows, so it can be re-added after Explorer
/// restarts.
struct Shown {
    hwnd: HWND,
    icon: HICON,
    tip: String,
}

thread_local! {
    static STATE: RefCell<Option<LoopState>> = const { RefCell::new(None) };
    static SHOWN: RefCell<Option<Shown>> = const { RefCell::new(None) };
    /// The "TaskbarCreated" message Explorer broadcasts when it (re)starts.
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
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

/// Sends the shown icon and tooltip to the shell with `op` (add, modify or delete).
fn notify(op: NOTIFY_ICON_MESSAGE, shown: &Shown) -> bool {
    let data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: shown.hwnd,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_TIP | NIF_MESSAGE,
        uCallbackMessage: WM_TRAY_ICON,
        hIcon: shown.icon,
        szTip: tip_buffer(&shown.tip),
        ..Default::default()
    };
    // SAFETY: `data` is a fully initialised, correctly sized struct that outlives the call.
    unsafe { Shell_NotifyIconW(op, &data) }.as_bool()
}

fn set_status(s: TrayStatus) {
    let problem = STATE.with(|st| st.borrow().as_ref().and_then(|st| st.problem.clone()));
    let (rgb, tip) = look(s, problem.as_deref());
    // After remove_icon there is nothing to update, and a new icon would leak.
    if SHOWN.with(|sh| sh.borrow().is_none()) {
        return;
    }
    let icon = match icon(rgb) {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!("building tray icon: {e:#}");
            return;
        }
    };
    SHOWN.with(|sh| {
        if let Some(shown) = sh.borrow_mut().as_mut() {
            let old = std::mem::replace(&mut shown.icon, icon);
            shown.tip = tip;
            if !notify(NIM_MODIFY, shown) {
                tracing::warn!("updating the tray icon failed");
            }
            // SAFETY: the shell has the new icon; the old one is ours to free.
            let _ = unsafe { DestroyIcon(old) };
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

/// Shows the menu at the cursor and returns the chosen action.
fn show_menu(hwnd: HWND) -> Option<Action> {
    let have_clip = LAST_CLIP
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some();
    // SAFETY: the menu is created, used and destroyed here; strings outlive the calls.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for item in menu_items(have_clip) {
            let r = match item {
                None => AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()),
                Some((action, label, enabled)) => {
                    let flags = if enabled {
                        MF_STRING
                    } else {
                        MF_STRING | MF_GRAYED
                    };
                    AppendMenuW(menu, flags, action as usize, &HSTRING::from(label))
                }
            };
            if let Err(e) = r {
                tracing::warn!("building the tray menu: {e}");
            }
        }
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without this the menu doesn't close when you click elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let id = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
            at.x,
            at.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        Action::from_id(id.0 as usize)
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY_ICON => {
            // Without NOTIFYICON_VERSION_4 the low word of lParam is the mouse message.
            let mouse = (lparam.0 & 0xFFFF) as u32;
            if matches!(mouse, WM_RBUTTONUP | WM_LBUTTONUP | WM_CONTEXTMENU)
                && let Some(a) = show_menu(hwnd)
            {
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
                remove_icon();
                run_exit_hook();
            }
            LRESULT(0)
        }
        m if m != 0 && m == TASKBAR_CREATED.get() => {
            tracing::info!("taskbar restarted; adding the tray icon again");
            SHOWN.with(|sh| {
                if let Some(shown) = sh.borrow().as_ref()
                    && !notify(NIM_ADD, shown)
                {
                    tracing::warn!("re-adding the tray icon failed");
                }
            });
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
            bail!(
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

const ICON_SIZE: u32 = 32;

/// A filled circle of colour `rgb` with a soft edge, as 32-bit BGRA rows top to bottom.
fn icon_pixels(rgb: [u8; 3]) -> Vec<u8> {
    let n = ICON_SIZE;
    let mut bgra = Vec::with_capacity((n * n * 4) as usize);
    let c = (n as f32 - 1.0) / 2.0;
    for y in 0..n {
        for x in 0..n {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let alpha = (14.5 - d).clamp(0.0, 1.0);
            bgra.extend_from_slice(&[rgb[2], rgb[1], rgb[0], (alpha * 255.0) as u8]);
        }
    }
    bgra
}

/// The icon for colour `rgb`. The caller owns it and frees it with `DestroyIcon`.
fn icon(rgb: [u8; 3]) -> Result<HICON> {
    let bgra = icon_pixels(rgb);
    // With 32-bit colour the alpha channel decides transparency; the AND mask stays clear.
    let mask = vec![0u8; (ICON_SIZE * ICON_SIZE / 8) as usize];
    let n = ICON_SIZE as i32;
    // SAFETY: both buffers have the sizes a 32×32, 1-plane, 32-bpp icon needs and outlive the call.
    unsafe {
        let instance = GetModuleHandleW(None).context("GetModuleHandleW")?;
        CreateIcon(
            Some(instance.into()),
            n,
            n,
            1,
            32,
            mask.as_ptr(),
            bgra.as_ptr(),
        )
        .context("CreateIcon")
    }
}

fn remove_icon() {
    SHOWN.with(|sh| {
        if let Some(shown) = sh.borrow_mut().take() {
            notify(NIM_DELETE, &shown);
            // SAFETY: the shell no longer uses it.
            let _ = unsafe { DestroyIcon(shown.icon) };
        }
    });
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
        })
    });
    let result = run_loop(&hotkeys, on_ready);
    if let Err(e) = &result {
        tracing::error!("tray failed: {e:#}");
    }
    remove_icon();
    run_exit_hook();
    result
}

fn run_loop(hotkeys: &[HotkeySpec], on_ready: impl FnOnce(TrayNotifier)) -> Result<()> {
    let hwnd = create_hidden_window()?;
    let hwnd_raw = hwnd.0 as isize;
    // SAFETY: a static string.
    TASKBAR_CREATED.set(unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) });

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

    let (rgb, tip) = look(TrayStatus::Idle, problem.as_deref());
    let shown = Shown {
        hwnd,
        icon: icon(rgb)?,
        tip,
    };
    if !notify(NIM_ADD, &shown) {
        // SAFETY: the shell never got it.
        let _ = unsafe { DestroyIcon(shown.icon) };
        bail!("Shell_NotifyIconW(NIM_ADD) failed: the tray icon could not be created");
    }
    SHOWN.with(|sh| *sh.borrow_mut() = Some(shown));
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
    fn hotkey_problem_adds_a_line_and_keeps_recording_red() {
        let p = hotkey_problem(&["F9"]).unwrap();
        assert_eq!(p, "Hotkey F9 unavailable — another app may be using it");
        let (rgb, tip) = look(TrayStatus::Recording, Some(&p));
        assert_eq!(rgb, [220, 38, 38], "recording must stay visible");
        assert_eq!(tip, format!("Retake — recording\n{p}"));
        let (rgb, tip) = look(TrayStatus::Idle, Some(&p));
        assert_eq!(rgb, [234, 179, 8], "idle shows the warning");
        assert_eq!(tip, format!("Retake — idle\n{p}"));
        assert_eq!(
            hotkey_problem(&["F9", "Ctrl+F9"]).unwrap(),
            "Hotkeys F9, Ctrl+F9 unavailable — another app may be using them"
        );
        assert_eq!(hotkey_problem(&[]), None);
    }

    #[test]
    fn actions_round_trip_and_zero_is_nothing() {
        for a in [
            Action::OpenConfig,
            Action::OpenClips,
            Action::OpenLogs,
            Action::Quit,
            Action::OpenLastClip,
        ] {
            assert_eq!(Action::from_id(a as usize), Some(a));
        }
        assert_eq!(
            Action::from_id(0),
            None,
            "TrackPopupMenu returns 0 when dismissed"
        );
    }

    #[test]
    fn last_clip_item_is_enabled_only_with_a_clip() {
        let first = |have| menu_items(have)[0].unwrap();
        assert_eq!(
            first(false),
            (Action::OpenLastClip, "Open last clip", false)
        );
        assert!(first(true).2);
        let actions: Vec<_> = menu_items(true)
            .into_iter()
            .flatten()
            .map(|i| i.0)
            .collect();
        assert_eq!(actions.len(), 5);
        assert_eq!(actions.last(), Some(&Action::Quit));
    }

    #[test]
    fn tooltip_is_cut_and_terminated() {
        let b = tip_buffer("Retake — idle");
        assert_eq!(String::from_utf16_lossy(&b[..13]), "Retake — idle");
        assert_eq!(b[13], 0);
        let long = "x".repeat(300);
        let b = tip_buffer(&long);
        assert_eq!(b[126], u16::from(b'x'));
        assert_eq!(b[127], 0);
    }

    #[test]
    fn icon_is_a_soft_circle_in_bgra() {
        let px = icon_pixels([220, 38, 38]);
        assert_eq!(px.len(), 32 * 32 * 4);
        let centre = ((16 * 32 + 16) * 4) as usize;
        assert_eq!(&px[centre..centre + 4], &[38, 38, 220, 255]);
        assert_eq!(px[3], 0, "corner is transparent");
    }

    #[test]
    fn icon_builds_for_every_status() {
        for s in ALL {
            let i = icon(look(s, None).0).unwrap();
            // SAFETY: we own it.
            unsafe { DestroyIcon(i) }.unwrap();
        }
    }
}
