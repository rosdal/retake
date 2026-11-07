//! The window: one plain Win32 window with Status, Clips, Settings and About tabs, made of
//! classic controls with the system font. It lives on the tray thread (the tray's message loop
//! drives it); closing it only hides it.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use retake_core::config::{self, Config};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontIndirectW, HFONT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ICC_LISTVIEW_CLASSES, ICC_TAB_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_TEXT,
    LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM,
    LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW,
    LVNI_SELECTED, LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS, NMHDR, TCIF_TEXT, TCITEMW,
    TCM_GETCURSEL, TCM_INSERTITEMW, TCN_SELCHANGE,
};
use windows::Win32::UI::Shell::{FO_DELETE, FOF_ALLOWUNDO, SHFILEOPSTRUCTW, SHFileOperationW};
use windows::Win32::UI::WindowsAndMessaging::{
    BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL,
    CB_SETCURSEL, CBS_DROPDOWNLIST, CreateWindowExW, DefWindowProcW, ES_AUTOHSCROLL, ES_MULTILINE,
    ES_READONLY, GetWindowTextLengthW, GetWindowTextW, HMENU, IsWindowVisible, KillTimer,
    NONCLIENTMETRICSW, RegisterClassW, SPI_GETNONCLIENTMETRICS, SW_HIDE, SW_SHOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW, SetForegroundWindow, SetTimer,
    SetWindowTextW, ShowWindow, SystemParametersInfoW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE,
    WM_COMMAND, WM_NOTIFY, WM_SETFONT, WM_TIMER, WNDCLASSW, WS_BORDER, WS_CAPTION, WS_CHILD,
    WS_EX_CLIENTEDGE, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
    WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};

use crate::tray::{MediaAction, dispatch};

/// What the window needs to know.
pub struct UiSetup {
    pub config: PathBuf,
    pub clips: PathBuf,
    pub logs: PathBuf,
}

const W: i32 = 780;
const H: i32 = 540;
const TIMER: usize = 7;

// Control ids.
const ID_TABS: i32 = 100;
const ID_STATUS_TEXT: i32 = 110;
const ID_PAUSE: i32 = 111;
const ID_SAVE60: i32 = 112;
const ID_OPEN_CLIPS: i32 = 113;
const ID_LIST: i32 = 120;
const ID_PLAY: i32 = 121;
const ID_REGRADE: i32 = 122;
const ID_SHARE: i32 = 123;
const ID_VERTICAL: i32 = 124;
const ID_FOLDER: i32 = 125;
const ID_DELETE: i32 = 126;
const ID_REFRESH: i32 = 127;
const ID_SAVE: i32 = 140;
const ID_REVERT: i32 = 141;
const ID_OPEN_CONFIG: i32 = 142;
const ID_SETTINGS_MSG: i32 = 143;
const ID_ABOUT_TEXT: i32 = 150;
const ID_OPEN_LOGS: i32 = 151;
const ID_COPY_DIAG: i32 = 152;

/// A settings control and the config key it edits.
#[derive(Clone, Copy)]
enum Field {
    /// A drop-down: key and the values it offers (as written in TOML).
    Combo(&'static str, &'static str, &'static [&'static str]),
    Check(&'static str, &'static str),
    /// A number or text field; `quoted` = a TOML string.
    Text(&'static str, &'static str, bool),
}

const FIELDS: &[(&str, Field)] = &[
    (
        "Strict safety (device audio, never read the game's files)",
        Field::Check("safety", "strict"),
    ),
    (
        "Capture fps",
        Field::Combo(
            "capture",
            "fps",
            &["\"auto\"", "60", "120", "144", "165", "240"],
        ),
    ),
    (
        "Encoder",
        Field::Combo(
            "capture",
            "encoder",
            &["\"auto\"", "\"amf\"", "\"nvenc\"", "\"qsv\""],
        ),
    ),
    (
        "Encoder preset",
        Field::Combo(
            "capture",
            "preset",
            &["\"speed\"", "\"balanced\"", "\"quality\""],
        ),
    ),
    ("Game audio", Field::Check("audio", "game")),
    ("Microphone", Field::Check("audio", "mic")),
    ("Discord as a third track", Field::Check("audio", "comms")),
    (
        "Seconds before the hotkey",
        Field::Text("clips", "pre_roll", false),
    ),
    (
        "Seconds after the hotkey",
        Field::Text("clips", "post_roll", false),
    ),
    ("Clip hotkey", Field::Text("clips", "hotkey", true)),
    (
        "Whole-buffer hotkey",
        Field::Text("clips", "hotkey_full", true),
    ),
    (
        "Sound when a clip is saved",
        Field::Check("clips", "sound_cue"),
    ),
    (
        "Auto-clip",
        Field::Combo(
            "autoclip",
            "clip",
            &["\"every\"", "\"2k\"", "\"3k\"", "\"aces\"", "\"off\""],
        ),
    ),
    (
        "Grade clips",
        Field::Combo("grade", "mode", &["\"on\"", "\"off\""]),
    ),
    (
        "Look strength (0–1)",
        Field::Text("grade", "look_strength", false),
    ),
    (
        "Motion blur on share/vertical",
        Field::Check("motion_blur", "enabled"),
    ),
];

struct Ui {
    setup: UiSetup,
    hwnd: HWND,
    /// Kept so the font outlives the controls using it.
    _font: HFONT,
    tabs: HWND,
    /// Controls per tab, shown only when their tab is selected.
    pages: [Vec<HWND>; 4],
    status_text: HWND,
    pause: HWND,
    list: HWND,
    fields: Vec<HWND>,
    settings_msg: HWND,
}

thread_local! {
    static SETUP: RefCell<Option<UiSetup>> = const { RefCell::new(None) };
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Gives the window its paths. Call before the tray loop starts.
pub fn init(setup: UiSetup) {
    SETUP.with(|s| *s.borrow_mut() = Some(setup));
}

fn wstr(s: &str) -> HSTRING {
    HSTRING::from(s)
}

fn send(h: HWND, msg: u32, w: usize, l: isize) -> isize {
    // SAFETY: messages to our own controls with plain values or pointers that outlive the call.
    unsafe { SendMessageW(h, msg, Some(WPARAM(w)), Some(LPARAM(l))).0 }
}

fn text_of(h: HWND) -> String {
    // SAFETY: the buffer is sized from the control's own length.
    unsafe {
        let n = GetWindowTextLengthW(h) as usize;
        let mut buf = vec![0u16; n + 1];
        let got = GetWindowTextW(h, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..got])
    }
}

fn set_text(h: HWND, s: &str) {
    // SAFETY: valid control and string.
    let _ = unsafe { SetWindowTextW(h, &wstr(s)) };
}

/// Shows the window (creating it the first time) and brings it to the front.
pub fn show() {
    let exists = UI.with(|u| u.borrow().is_some());
    if !exists {
        let Some(setup) = SETUP.with(|s| s.borrow_mut().take()) else {
            tracing::warn!("window not initialised");
            return;
        };
        match create(setup) {
            Ok(ui) => UI.with(|u| *u.borrow_mut() = Some(ui)),
            Err(e) => {
                tracing::error!("opening the window: {e:#}");
                return;
            }
        }
    }
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            // SAFETY: our own window.
            unsafe {
                let _ = ShowWindow(ui.hwnd, SW_SHOW);
                let _ = SetForegroundWindow(ui.hwnd);
                SetTimer(Some(ui.hwnd), TIMER, 1_000, None);
            }
            refresh_status(ui);
            fill_clips_of(ui);
            load_settings(ui);
        }
    });
}

fn create(setup: UiSetup) -> anyhow::Result<Ui> {
    // SAFETY: standard window creation on this thread; every string outlives its call.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_TAB_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
        let inst = GetModuleHandleW(None)?;
        let class = w!("RetakeWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: class,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH(16 as _), // COLOR_BTNFACE + 1
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Retake"),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            200,
            150,
            W,
            H,
            None,
            None,
            Some(inst.into()),
            None,
        )?;
        let mut ncm = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let _ = SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some((&raw mut ncm).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let font = CreateFontIndirectW(&ncm.lfMessageFont);
        let child = |class: PCWSTR,
                     text: &str,
                     style: WINDOW_STYLE,
                     ex: WINDOW_EX_STYLE,
                     x: i32,
                     y: i32,
                     cw: i32,
                     ch: i32,
                     id: i32|
         -> HWND {
            let h = CreateWindowExW(
                ex,
                class,
                &wstr(text),
                WS_CHILD | style,
                x,
                y,
                cw,
                ch,
                Some(hwnd),
                Some(HMENU(id as isize as *mut _)),
                Some(inst.into()),
                None,
            )
            .unwrap_or_default();
            SendMessageW(
                h,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
            h
        };
        let btn = |text: &str, x: i32, y: i32, id: i32| {
            child(
                w!("BUTTON"),
                text,
                WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                WINDOW_EX_STYLE(0),
                x,
                y,
                130,
                28,
                id,
            )
        };
        let tabs = child(
            w!("SysTabControl32"),
            "",
            WS_VISIBLE,
            WINDOW_EX_STYLE(0),
            8,
            8,
            W - 32,
            H - 56,
            ID_TABS,
        );
        for (i, name) in ["Status", "Clips", "Settings", "About"].iter().enumerate() {
            let mut n: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: windows::core::PWSTR(n.as_mut_ptr()),
                ..Default::default()
            };
            send(tabs, TCM_INSERTITEMW, i, (&raw const item) as isize);
        }
        let (x0, y0, cw) = (24, 44, W - 64);
        // Status
        let status_text = child(
            w!("EDIT"),
            "",
            WS_VSCROLL | WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32),
            WS_EX_CLIENTEDGE,
            x0,
            y0,
            cw,
            H - 160,
            ID_STATUS_TEXT,
        );
        let by = H - 104;
        let pause = btn("Pause recording", x0, by, ID_PAUSE);
        let save60 = btn("Save last 60 s", x0 + 140, by, ID_SAVE60);
        let open_clips = btn("Open clips folder", x0 + 280, by, ID_OPEN_CLIPS);
        // Clips
        let list = child(
            w!("SysListView32"),
            "",
            WS_TABSTOP | WS_BORDER | WINDOW_STYLE(LVS_REPORT | LVS_SHOWSELALWAYS),
            WS_EX_CLIENTEDGE,
            x0,
            y0,
            cw,
            H - 160,
            ID_LIST,
        );
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            LVS_EX_FULLROWSELECT as isize,
        );
        for (i, (name, width)) in [
            ("Clip", 270),
            ("Play", 70),
            ("Graded", 60),
            ("Size MB", 70),
            ("Saved", 150),
        ]
        .iter()
        .enumerate()
        {
            let mut n: Vec<u16> = name.encode_utf16().chain([0]).collect();
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                cx: *width,
                pszText: windows::core::PWSTR(n.as_mut_ptr()),
                ..Default::default()
            };
            send(list, LVM_INSERTCOLUMNW, i, (&raw const col) as isize);
        }
        let clip_btns = [
            btn("Play", x0, by, ID_PLAY),
            btn("Regrade", x0 + 100, by, ID_REGRADE),
            btn("Export share", x0 + 200, by, ID_SHARE),
            btn("Export vertical", x0 + 300, by, ID_VERTICAL),
            btn("Open folder", x0 + 400, by, ID_FOLDER),
            btn("Delete", x0 + 500, by, ID_DELETE),
            btn("Refresh", x0 + 600, by, ID_REFRESH),
        ];
        for b in &clip_btns {
            windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                *b,
                None,
                0,
                0,
                94,
                28,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
            )
            .ok();
        }
        // Settings: two columns of label + control.
        let mut settings = Vec::new();
        let mut fields = Vec::new();
        for (i, (label, field)) in FIELDS.iter().enumerate() {
            let col = (i / 9) as i32;
            let row = (i % 9) as i32;
            let (lx, ly) = (x0 + col * 370, y0 + row * 34);
            let ctl = match field {
                Field::Check(..) => child(
                    w!("BUTTON"),
                    label,
                    WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32),
                    WINDOW_EX_STYLE(0),
                    lx,
                    ly,
                    350,
                    24,
                    200 + i as i32,
                ),
                Field::Combo(_, _, values) => {
                    settings.push(child(
                        w!("STATIC"),
                        label,
                        WINDOW_STYLE(0),
                        WINDOW_EX_STYLE(0),
                        lx,
                        ly + 4,
                        190,
                        22,
                        0,
                    ));
                    let c = child(
                        w!("COMBOBOX"),
                        "",
                        WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                        WINDOW_EX_STYLE(0),
                        lx + 195,
                        ly,
                        150,
                        200,
                        200 + i as i32,
                    );
                    for v in *values {
                        let s: Vec<u16> = v.trim_matches('"').encode_utf16().chain([0]).collect();
                        send(c, CB_ADDSTRING, 0, s.as_ptr() as isize);
                    }
                    c
                }
                Field::Text(..) => {
                    settings.push(child(
                        w!("STATIC"),
                        label,
                        WINDOW_STYLE(0),
                        WINDOW_EX_STYLE(0),
                        lx,
                        ly + 4,
                        190,
                        22,
                        0,
                    ));
                    child(
                        w!("EDIT"),
                        "",
                        WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                        WS_EX_CLIENTEDGE,
                        lx + 195,
                        ly,
                        150,
                        24,
                        200 + i as i32,
                    )
                }
            };
            settings.push(ctl);
            fields.push(ctl);
        }
        let settings_msg = child(
            w!("STATIC"),
            "",
            WINDOW_STYLE(0),
            WINDOW_EX_STYLE(0),
            x0,
            by - 36,
            cw,
            30,
            ID_SETTINGS_MSG,
        );
        settings.push(settings_msg);
        settings.push(btn("Save", x0, by, ID_SAVE));
        settings.push(btn("Revert", x0 + 140, by, ID_REVERT));
        settings.push(btn("Open config file", x0 + 280, by, ID_OPEN_CONFIG));
        // About
        let about = child(
            w!("EDIT"),
            &about_text(&setup),
            WINDOW_STYLE((ES_MULTILINE | ES_READONLY) as u32),
            WS_EX_CLIENTEDGE,
            x0,
            y0,
            cw,
            220,
            ID_ABOUT_TEXT,
        );
        let about_btns = [
            btn("Open logs", x0, by, ID_OPEN_LOGS),
            btn("Copy diagnostics", x0 + 140, by, ID_COPY_DIAG),
        ];

        let pages = [
            vec![status_text, pause, save60, open_clips],
            std::iter::once(list).chain(clip_btns).collect(),
            settings,
            std::iter::once(about).chain(about_btns).collect(),
        ];
        let ui = Ui {
            setup,
            hwnd,
            _font: font,
            tabs,
            pages,
            status_text,
            pause,
            list,
            fields,
            settings_msg,
        };
        select_page(&ui, 0);
        Ok(ui)
    }
}

fn about_text(s: &UiSetup) -> String {
    format!(
        "Retake {}\r\nA lightweight game recorder: standalone clips, kill clips, montages.\r\n\r\nGPU: {}\r\nWindows build: {}\r\nConfig: {}\r\nClips: {}\r\nLogs: {}\r\n\r\nMIT licence. ffmpeg and r6-dissect are separate programs with their own licences.",
        env!("CARGO_PKG_VERSION"),
        crate::sysinfo::gpu_name(),
        crate::sysinfo::windows_build(),
        s.config.display(),
        s.clips.display(),
        s.logs.display()
    )
}

fn select_page(ui: &Ui, page: usize) {
    for (i, controls) in ui.pages.iter().enumerate() {
        for &c in controls {
            // SAFETY: our own controls.
            let _ = unsafe { ShowWindow(c, if i == page { SW_SHOW } else { SW_HIDE }) };
        }
    }
}

fn refresh_status(ui: &Ui) {
    let b = crate::status::get();
    let mut s = format!(
        "Status: {}\r\nGame: {}\r\nCapture: {}   speed {:.2}x\r\nBuffer: {} s   Disk free: {}\r\nClips today: {}   This session: {}\r\nGrading/exports waiting: {}{}\r\n",
        if b.state.is_empty() {
            "Starting"
        } else {
            &b.state
        },
        b.game.as_deref().unwrap_or("not running"),
        if b.capture.is_empty() {
            "—"
        } else {
            &b.capture
        },
        b.speed,
        b.buffer_secs,
        b.disk_free_gb.map_or("—".to_owned(), |g| format!("{g} GB")),
        b.clips_today,
        b.clips_session,
        b.queued,
        if b.exporting { " (working)" } else { "" }
    );
    for w in &b.warnings {
        s.push_str(&format!("Warning: {w}\r\n"));
    }
    s.push_str("\r\nLast events:\r\n");
    for (t, e) in &b.events {
        s.push_str(&format!("{t}  {e}\r\n"));
    }
    set_text(ui.status_text, &s);
    set_text(
        ui.pause,
        if b.paused {
            "Resume recording"
        } else {
            "Pause recording"
        },
    );
}

/// Raw clips, newest first: path, play type, graded, size, saved.
fn list_clips(clips: &Path) -> Vec<(PathBuf, String, bool, u64, std::time::SystemTime)> {
    let raw = clips.join("raw");
    let Ok(entries) = std::fs::read_dir(&raw) else {
        return Vec::new();
    };
    let mut out: Vec<_> = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_stem()?.to_string_lossy().into_owned();
            if p.extension()? != "mkv" || name.ends_with("_graded") {
                return None;
            }
            let meta = e.metadata().ok()?;
            let play = std::fs::read_to_string(p.with_extension("json"))
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| {
                    v.pointer("/autoclip/play")
                        .and_then(|x| x.as_str())
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| {
                    if name.ends_with("_full") {
                        "full".into()
                    } else {
                        "manual".into()
                    }
                });
            let graded = p.with_file_name(format!("{name}_graded.mkv")).is_file();
            Some((p, play, graded, meta.len(), meta.modified().ok()?))
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.4));
    out
}

fn fill_clips_of(ui: &Ui) {
    let clips = list_clips(&ui.setup.clips);
    send(ui.list, LVM_DELETEALLITEMS, 0, 0);
    for (i, (p, play, graded, len, when)) in clips.iter().enumerate() {
        let cells = [
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            play.clone(),
            if *graded { "yes".into() } else { "".into() },
            format!("{:.0}", *len as f64 / 1_048_576.0),
            humantime(*when),
        ];
        for (c, text) in cells.iter().enumerate() {
            let mut t: Vec<u16> = text.encode_utf16().chain([0]).collect();
            let item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: i as i32,
                iSubItem: c as i32,
                pszText: windows::core::PWSTR(t.as_mut_ptr()),
                ..Default::default()
            };
            let msg = if c == 0 {
                LVM_INSERTITEMW
            } else {
                LVM_SETITEMTEXTW
            };
            send(
                ui.list,
                msg,
                if c == 0 { 0 } else { i },
                (&raw const item) as isize,
            );
        }
    }
    let paths = clips.into_iter().map(|c| c.0).collect();
    // The list is refilled under a shared borrow; store the paths after it.
    UI_CLIPS.with(|c| *c.borrow_mut() = paths);
}

thread_local! {
    static UI_CLIPS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

fn humantime(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let s = crate::clock::stamp_of_unix(secs);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        s.year, s.month, s.day, s.hour, s.minute
    )
}

fn selected_clip(ui: &Ui) -> Option<PathBuf> {
    let i = send(ui.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    if i < 0 {
        return None;
    }
    UI_CLIPS.with(|c| c.borrow().get(i as usize).cloned())
}

/// The current config file parsed (for the Settings tab), and its text.
fn read_config(path: &Path) -> (String, Option<Config>) {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let c = config::load_str(&text).ok();
    (text, c)
}

fn value_of(text: &str, section: &str, key: &str) -> Option<String> {
    let doc: toml_edit::DocumentMut = text.parse().ok()?;
    doc.get(section)?
        .get(key)
        .map(|v| v.to_string().trim().to_owned())
}

fn load_settings(ui: &Ui) {
    let (text, _) = read_config(&ui.setup.config);
    let defaults = config::DEFAULT_CONFIG_TOML;
    for (&ctl, (_, field)) in ui.fields.iter().zip(FIELDS) {
        let get = |s: &str, k: &str| {
            value_of(&text, s, k)
                .or_else(|| value_of(defaults, s, k))
                .unwrap_or_default()
        };
        match *field {
            Field::Check(s, k) => {
                send(ctl, BM_SETCHECK, usize::from(get(s, k) == "true"), 0);
            }
            Field::Combo(s, k, values) => {
                let v = get(s, k);
                let i = values.iter().position(|x| *x == v).unwrap_or(0);
                send(ctl, CB_SETCURSEL, i, 0);
            }
            Field::Text(s, k, quoted) => {
                let v = get(s, k);
                set_text(ctl, if quoted { v.trim_matches('"') } else { &v });
            }
        }
    }
    set_text(
        ui.settings_msg,
        "Changes apply when you press Save (after the match if a game is running).",
    );
}

fn save_settings(ui: &Ui) {
    let path = &ui.setup.config;
    let (text, _) = read_config(path);
    let mut doc: toml_edit::DocumentMut = match text.parse() {
        Ok(d) => d,
        Err(e) => {
            set_text(ui.settings_msg, &format!("config.toml can't be read: {e}"));
            return;
        }
    };
    for (&ctl, (_, field)) in ui.fields.iter().zip(FIELDS) {
        let (section, key, raw) = match *field {
            Field::Check(s, k) => (s, k, (send(ctl, BM_GETCHECK, 0, 0) == 1).to_string()),
            Field::Combo(s, k, values) => {
                let i = send(ctl, CB_GETCURSEL, 0, 0).max(0) as usize;
                (s, k, values.get(i).copied().unwrap_or(values[0]).to_owned())
            }
            Field::Text(s, k, quoted) => {
                let v = text_of(ctl).trim().to_owned();
                (
                    s,
                    k,
                    if quoted {
                        format!("\"{}\"", v.replace('"', ""))
                    } else {
                        v
                    },
                )
            }
        };
        let value: toml_edit::Value = match raw.parse() {
            Ok(v) => v,
            Err(_) => {
                set_text(
                    ui.settings_msg,
                    &format!("{section}.{key}: \"{raw}\" isn't a valid value"),
                );
                return;
            }
        };
        if doc.get(section).is_none() {
            doc[section] = toml_edit::table();
        }
        doc[section][key] = toml_edit::value(value);
    }
    let new = doc.to_string();
    if let Err(e) = config::load_str(&new) {
        set_text(ui.settings_msg, &format!("Not saved: {e}"));
        return;
    }
    match std::fs::write(path, new) {
        Ok(()) => set_text(
            ui.settings_msg,
            "Saved. Retake restarts with the new settings now, or after the match if a game is running.",
        ),
        Err(e) => set_text(ui.settings_msg, &format!("Not saved: {e}")),
    }
}

fn delete_clip(p: &Path) {
    let stem = p
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut files = vec![
        p.to_path_buf(),
        p.with_extension("json"),
        p.with_file_name(format!("{stem}_graded.mkv")),
    ];
    files.retain(|f| f.is_file());
    // A double-NUL-terminated list of paths.
    let mut list: Vec<u16> = Vec::new();
    for f in &files {
        list.extend(f.as_os_str().to_string_lossy().encode_utf16());
        list.push(0);
    }
    list.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(list.as_ptr()),
        fFlags: FOF_ALLOWUNDO.0 as u16,
        ..Default::default()
    };
    // SAFETY: the list outlives the call; Windows asks for confirmation itself.
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc != 0 {
        tracing::warn!(code = rc, "deleting {}", p.display());
    }
}

fn on_command(id: i32) {
    let action = UI.with(|u| -> Option<MediaAction> {
        let u = u.borrow();
        let ui = u.as_ref()?;
        match id {
            ID_PAUSE => return Some(MediaAction::Pause(!crate::status::get().paused)),
            ID_SAVE60 => return Some(MediaAction::SaveFull),
            ID_OPEN_CLIPS | ID_FOLDER => crate::tray::open_path(&ui.setup.clips.join("raw")),
            ID_PLAY => {
                if let Some(p) = selected_clip(ui) {
                    crate::tray::open_path(&p);
                }
            }
            ID_REGRADE => return selected_clip(ui).map(MediaAction::Regrade),
            ID_SHARE => return selected_clip(ui).map(|p| MediaAction::Export(p, "share".into())),
            ID_VERTICAL => {
                return selected_clip(ui).map(|p| MediaAction::Export(p, "vertical".into()));
            }
            ID_DELETE => {
                if let Some(p) = selected_clip(ui) {
                    delete_clip(&p);
                    fill_clips_of(ui);
                }
            }
            ID_REFRESH => fill_clips_of(ui),
            ID_SAVE => save_settings(ui),
            ID_REVERT => load_settings(ui),
            ID_OPEN_CONFIG => crate::tray::open_path(&ui.setup.config),
            ID_OPEN_LOGS => crate::tray::open_path(&ui.setup.logs),
            ID_COPY_DIAG => return Some(MediaAction::CopyDiagnostics),
            _ => {}
        }
        None
    });
    if let Some(a) = action {
        dispatch(a);
        UI.with(|u| {
            if let Some(ui) = u.borrow().as_ref() {
                refresh_status(ui);
            }
        });
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            // SAFETY: our own window; it stays alive, hidden.
            unsafe {
                let _ = ShowWindow(hwnd, SW_HIDE);
                let _ = KillTimer(Some(hwnd), TIMER);
            }
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER => {
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref()
                    // SAFETY: our own window.
                    && unsafe { IsWindowVisible(ui.hwnd) }.as_bool()
                    && send(ui.tabs, TCM_GETCURSEL, 0, 0) == 0
                {
                    refresh_status(ui);
                }
            });
            LRESULT(0)
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY's lParam points to an NMHDR.
            let hdr = unsafe { &*(lparam.0 as *const NMHDR) };
            if hdr.idFrom == ID_TABS as usize && hdr.code == TCN_SELCHANGE {
                UI.with(|u| {
                    if let Some(ui) = u.borrow().as_ref() {
                        let page = send(ui.tabs, TCM_GETCURSEL, 0, 0).max(0) as usize;
                        select_page(ui, page);
                        if page == 1 {
                            fill_clips_of(ui);
                        }
                    }
                });
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // Notifications from combo boxes and edits (high word ≠ 0) aren't commands.
            if (wparam.0 >> 16) & 0xFFFF == 0 {
                on_command((wparam.0 & 0xFFFF) as i32);
            }
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
