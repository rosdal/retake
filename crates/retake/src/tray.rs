//! Tray icon, its menu, and the Win32 message loop.
//!
//! A hidden top-level window (not message-only, which would miss session broadcasts) receives
//! menu clicks posted by the tray's event handler and `WM_QUERYENDSESSION`/`WM_ENDSESSION`.

use std::cell::RefCell;
use std::path::PathBuf;

use anyhow::{Context, Result};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, SW_SHOWNORMAL, TranslateMessage, WINDOW_EX_STYLE, WM_APP,
    WM_ENDSESSION, WM_QUERYENDSESSION, WNDCLASSW, WS_OVERLAPPED,
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

#[derive(Clone, Copy)]
enum Action {
    OpenConfig = 0,
    OpenClips = 1,
    OpenLogs = 2,
    Quit = 3,
}

impl Action {
    fn from_wparam(w: usize) -> Option<Self> {
        [
            Self::OpenConfig,
            Self::OpenClips,
            Self::OpenLogs,
            Self::Quit,
        ]
        .into_iter()
        .find(|a| *a as usize == w)
    }
}

struct LoopState {
    paths: TrayPaths,
    on_exit: Option<Box<dyn FnOnce()>>,
}

thread_local! {
    static STATE: RefCell<Option<LoopState>> = const { RefCell::new(None) };
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

fn on_menu(action: Action) {
    let paths = STATE.with(|s| s.borrow().as_ref().map(|st| st.paths.clone()));
    let Some(paths) = paths else { return };
    match action {
        Action::OpenConfig => open(&paths.config, true),
        Action::OpenClips => open(&paths.clips, false),
        Action::OpenLogs => open(&paths.logs, false),
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

/// A 32×32 icon drawn in code: a filled red circle with a soft edge.
fn icon() -> Result<Icon> {
    const N: u32 = 32;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = (N as f32 - 1.0) / 2.0;
    for y in 0..N {
        for x in 0..N {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            let alpha = (14.5 - d).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[220, 38, 38, (alpha * 255.0) as u8]);
        }
    }
    Icon::from_rgba(rgba, N, N).context("building tray icon")
}

/// Shows the tray icon and pumps messages until Quit or session end. `on_exit` runs exactly
/// once on every way out, including setup failures; a failure is logged before it runs so the
/// error reaches the log even when `on_exit` flushes and closes it.
pub fn run(paths: TrayPaths, on_exit: impl FnOnce() + 'static) -> Result<()> {
    STATE.with(|s| {
        *s.borrow_mut() = Some(LoopState {
            paths,
            on_exit: Some(Box::new(on_exit)),
        })
    });
    let result = run_loop();
    if let Err(e) = &result {
        tracing::error!("tray failed: {e:#}");
    }
    run_exit_hook();
    result
}

fn run_loop() -> Result<()> {
    let hwnd = create_hidden_window()?;
    let hwnd_raw = hwnd.0 as isize;

    let items = [
        (MenuItem::new("Open config", true, None), Action::OpenConfig),
        (
            MenuItem::new("Open clips folder", true, None),
            Action::OpenClips,
        ),
        (MenuItem::new("Open logs", true, None), Action::OpenLogs),
        (MenuItem::new("Quit", true, None), Action::Quit),
    ];
    let menu = Menu::new();
    menu.append_items(&[&items[0].0, &items[1].0, &items[2].0])?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&items[3].0)?;

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

    let _tray = TrayIconBuilder::new()
        .with_tooltip("Retake")
        .with_icon(icon()?)
        .with_menu(Box::new(menu))
        .build()
        .context("creating tray icon")?;
    tracing::info!("tray ready");

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
