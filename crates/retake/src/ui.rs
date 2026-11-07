//! The window: one plain Win32 window with Status, Clips, Montage, Settings and About tabs, made of
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
    LVCF_WIDTH, LVCOLUMNW, LVIF_STATE, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW,
    LVM_DELETEALLITEMS, LVM_ENSUREVISIBLE, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMSTATE, LVM_SETITEMTEXTW, LVN_ITEMCHANGED,
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

use crate::library::Library;
use crate::montage::Job;
use crate::tray::{MediaAction, dispatch};
use retake_core::montage::Format;

/// What the window needs to know.
pub struct UiSetup {
    pub config: PathBuf,
    pub clips: PathBuf,
    pub logs: PathBuf,
}

const W: i32 = 920;
const H: i32 = 600;
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
const ID_STAR: i32 = 128;
const ID_RESCAN: i32 = 129;
const ID_SONG: i32 = 160;
const ID_BROWSE: i32 = 161;
const ID_FORMAT: i32 = 162;
const ID_PICK: i32 = 163;
const ID_LENGTH: i32 = 164;
const ID_NEW_MONTAGE: i32 = 165;
const ID_PROJECTS: i32 = 166;
const ID_PCLIPS: i32 = 167;
const ID_UP: i32 = 168;
const ID_DOWN: i32 = 169;
const ID_INCLUDE: i32 = 170;
const ID_PREVIEW: i32 = 171;
const ID_RENDER: i32 = 172;
const ID_MONTAGE_FOLDER: i32 = 173;
const ID_MONTAGE_REFRESH: i32 = 174;
const ID_MONTAGE_MSG: i32 = 175;
const ID_MONTAGE_GAME: i32 = 176;
const ID_CLIPS_GAME: i32 = 177;
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
    pages: [Vec<HWND>; 5],
    status_text: HWND,
    pause: HWND,
    list: HWND,
    library: Option<Library>,
    song: HWND,
    format: HWND,
    pick: HWND,
    length: HWND,
    projects: HWND,
    pclips: HWND,
    montage_msg: HWND,
    montage_game: HWND,
    clips_game: HWND,
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
        for (i, name) in ["Status", "Clips", "Montage", "Settings", "About"]
            .iter()
            .enumerate()
        {
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
            y0 + 34,
            cw,
            H - 160 - 34,
            ID_LIST,
        );
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            LVS_EX_FULLROWSELECT as isize,
        );
        add_columns(
            list,
            &[
                ("Clip", 270),
                ("Game", 110),
                ("★", 30),
                ("Play", 70),
                ("Graded", 60),
                ("Used", 50),
                ("Size MB", 70),
                ("Saved", 150),
            ],
        );
        let narrow = |b: HWND| {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                b,
                None,
                0,
                0,
                90,
                28,
                windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                    | windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER,
            );
            b
        };
        let clip_btns: Vec<HWND> = [
            ("Play", ID_PLAY),
            ("Star", ID_STAR),
            ("Regrade", ID_REGRADE),
            ("Export share", ID_SHARE),
            ("Export vertical", ID_VERTICAL),
            ("Open folder", ID_FOLDER),
            ("Delete", ID_DELETE),
            ("Refresh", ID_REFRESH),
            ("Rescan library", ID_RESCAN),
        ]
        .iter()
        .enumerate()
        .map(|(k, (text, id))| narrow(btn(text, x0 + k as i32 * 94, by, *id)))
        .collect();
        // Montage: song and options, the projects, the selected project's clips.
        let label = |text: &str, x: i32, y: i32, lw: i32| {
            child(
                w!("STATIC"),
                text,
                WINDOW_STYLE(0),
                WINDOW_EX_STYLE(0),
                x,
                y + 4,
                lw,
                22,
                0,
            )
        };
        let combo = |x: i32, y: i32, cw: i32, id: i32, values: &[&str]| {
            let c = child(
                w!("COMBOBOX"),
                "",
                WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
                WINDOW_EX_STYLE(0),
                x,
                y,
                cw,
                200,
                id,
            );
            for v in values {
                let s: Vec<u16> = v.encode_utf16().chain([0]).collect();
                send(c, CB_ADDSTRING, 0, s.as_ptr() as isize);
            }
            send(c, CB_SETCURSEL, 0, 0);
            c
        };
        let edit = |text: &str, x: i32, y: i32, ew: i32, id: i32| {
            child(
                w!("EDIT"),
                text,
                WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                WS_EX_CLIENTEDGE,
                x,
                y,
                ew,
                24,
                id,
            )
        };
        let clips_label = label("Game", x0, y0, 45);
        let clips_game = combo(x0 + 50, y0, 220, ID_CLIPS_GAME, &["All games"]);
        let mut montage = Vec::new();
        montage.push(label("Song", x0, y0, 60));
        let song = edit("", x0 + 65, y0, 330, ID_SONG);
        montage.push(song);
        montage.push(label("Game", x0 + 410, y0, 45));
        let montage_game = combo(x0 + 455, y0, 170, ID_MONTAGE_GAME, &["All games (mixed)"]);
        montage.push(montage_game);
        montage.push(btn("Browse…", x0 + 640, y0 - 2, ID_BROWSE));
        let y1 = y0 + 34;
        montage.push(label("Format", x0, y1, 60));
        let format = combo(
            x0 + 65,
            y1,
            150,
            ID_FORMAT,
            &["TikTok 9:16", "YouTube 16:9"],
        );
        montage.push(format);
        montage.push(label("Clips", x0 + 230, y1, 45));
        let pick = combo(x0 + 280, y1, 150, ID_PICK, &["Best kills", "Starred only"]);
        montage.push(pick);
        montage.push(label("Length (s)", x0 + 445, y1, 70));
        let length = edit("", x0 + 520, y1, 60, ID_LENGTH);
        montage.push(length);
        montage.push(btn("New montage", x0 + 640, y1 - 2, ID_NEW_MONTAGE));
        let y2 = y1 + 38;
        let lv = |y: i32, lh: i32, id: i32| {
            let l = child(
                w!("SysListView32"),
                "",
                WS_TABSTOP | WS_BORDER | WINDOW_STYLE(LVS_REPORT | LVS_SHOWSELALWAYS),
                WS_EX_CLIENTEDGE,
                x0,
                y,
                cw,
                lh,
                id,
            );
            send(
                l,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                0,
                LVS_EX_FULLROWSELECT as isize,
            );
            l
        };
        let projects = lv(y2, 130, ID_PROJECTS);
        add_columns(
            projects,
            &[
                ("Montage", 330),
                ("Format", 90),
                ("Length", 60),
                ("Clips", 60),
                ("Rendered", 200),
            ],
        );
        montage.push(projects);
        let y3 = y2 + 140;
        let pclips = lv(y3, by - 40 - y3, ID_PCLIPS);
        add_columns(
            pclips,
            &[
                ("#", 36),
                ("Clip", 330),
                ("Kills", 50),
                ("Role", 70),
                ("In montage", 80),
                ("Moved", 60),
            ],
        );
        montage.push(pclips);
        let montage_msg = child(
            w!("STATIC"),
            "",
            WINDOW_STYLE(0),
            WINDOW_EX_STYLE(0),
            x0,
            by - 32,
            cw,
            24,
            ID_MONTAGE_MSG,
        );
        montage.push(montage_msg);
        for (k, (text, id)) in [
            ("Up", ID_UP),
            ("Down", ID_DOWN),
            ("In / out", ID_INCLUDE),
            ("Preview", ID_PREVIEW),
            ("Render", ID_RENDER),
            ("Open folder", ID_MONTAGE_FOLDER),
            ("Refresh", ID_MONTAGE_REFRESH),
        ]
        .iter()
        .enumerate()
        {
            montage.push(narrow(btn(text, x0 + k as i32 * 94, by, *id)));
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
            [list, clips_label, clips_game]
                .into_iter()
                .chain(clip_btns)
                .collect(),
            montage,
            settings,
            std::iter::once(about).chain(about_btns).collect(),
        ];
        let library = Library::open(&setup.clips)
            .inspect_err(|e| tracing::warn!("window: clip library unavailable: {e:#}"))
            .ok();
        let ui = Ui {
            setup,
            hwnd,
            _font: font,
            tabs,
            pages,
            status_text,
            pause,
            list,
            library,
            song,
            format,
            pick,
            length,
            projects,
            pclips,
            montage_msg,
            montage_game,
            clips_game,
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

fn add_columns(list: HWND, cols: &[(&str, i32)]) {
    for (i, (name, width)) in cols.iter().enumerate() {
        let mut n: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: *width,
            pszText: windows::core::PWSTR(n.as_mut_ptr()),
            ..Default::default()
        };
        send(list, LVM_INSERTCOLUMNW, i, (&raw const col) as isize);
    }
}

/// Replaces a list view's rows.
fn fill_rows(list: HWND, rows: &[Vec<String>]) {
    send(list, LVM_DELETEALLITEMS, 0, 0);
    for (i, cells) in rows.iter().enumerate() {
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
                list,
                msg,
                if c == 0 { 0 } else { i },
                (&raw const item) as isize,
            );
        }
    }
}

fn selected_row(list: HWND) -> Option<usize> {
    let i = send(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize);
    usize::try_from(i).ok()
}

fn select_row(list: HWND, i: usize) {
    let item = LVITEMW {
        mask: LVIF_STATE,
        state: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED.0 | LVIS_FOCUSED.0,
        ),
        stateMask: windows::Win32::UI::Controls::LIST_VIEW_ITEM_STATE_FLAGS(
            LVIS_SELECTED.0 | LVIS_FOCUSED.0,
        ),
        ..Default::default()
    };
    send(list, LVM_SETITEMSTATE, i, (&raw const item) as isize);
    send(list, LVM_ENSUREVISIBLE, i, 0);
}

/// A game's display name from its profile id.
fn game_name(id: &str) -> String {
    if id.is_empty() {
        return "Unknown".into();
    }
    retake_core::games::BUILTIN
        .iter()
        .find(|p| p.id == id)
        .map_or_else(|| id.to_owned(), |p| p.name.to_owned())
}

thread_local! {
    /// The game ids behind each game combo's items (after the first, "all").
    static CLIPS_GAMES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static MONTAGE_GAMES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Refills a game combo with "all" and the games in the library, keeping the selection.
fn fill_game_combo(
    combo: HWND,
    all: &str,
    games: &'static std::thread::LocalKey<RefCell<Vec<String>>>,
    lib: &[retake_core::montage::LibClip],
) {
    use windows::Win32::UI::WindowsAndMessaging::CB_RESETCONTENT;
    let keep = selected_game(combo, games);
    let mut ids: Vec<String> = lib.iter().map(|c| c.game.clone()).collect();
    ids.sort();
    ids.dedup();
    send(combo, CB_RESETCONTENT, 0, 0);
    for name in std::iter::once(all.to_owned()).chain(ids.iter().map(|i| game_name(i))) {
        let s: Vec<u16> = name.encode_utf16().chain([0]).collect();
        send(combo, CB_ADDSTRING, 0, s.as_ptr() as isize);
    }
    let sel = keep
        .and_then(|k| ids.iter().position(|i| *i == k))
        .map_or(0, |i| i + 1);
    send(combo, CB_SETCURSEL, sel, 0);
    games.with(|g| *g.borrow_mut() = ids);
}

/// The game picked in a combo; `None` = all.
fn selected_game(
    combo: HWND,
    games: &'static std::thread::LocalKey<RefCell<Vec<String>>>,
) -> Option<String> {
    let i = send(combo, CB_GETCURSEL, 0, 0);
    let i = usize::try_from(i).ok()?.checked_sub(1)?;
    games.with(|g| g.borrow().get(i).cloned())
}

fn fill_clips_of(ui: &Ui) {
    let lib = ui.library.as_ref().map(Library::list).unwrap_or_default();
    fill_game_combo(ui.clips_game, "All games", &CLIPS_GAMES, &lib);
    let game = selected_game(ui.clips_game, &CLIPS_GAMES);
    let clips: Vec<_> = list_clips(&ui.setup.clips)
        .into_iter()
        .filter(|c| {
            game.as_ref()
                .is_none_or(|g| lib.iter().any(|l| l.path == c.0 && l.game == *g))
        })
        .collect();
    let rows: Vec<Vec<String>> = clips
        .iter()
        .map(|(p, play, graded, len, when)| {
            let l = lib.iter().find(|c| c.path == *p);
            vec![
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                game_name(l.map_or("", |c| c.game.as_str())),
                if l.is_some_and(|c| c.starred) {
                    "★".into()
                } else {
                    String::new()
                },
                play.clone(),
                if *graded { "yes".into() } else { String::new() },
                l.map_or(String::new(), |c| {
                    if c.used > 0 {
                        c.used.to_string()
                    } else {
                        String::new()
                    }
                }),
                format!("{:.0}", *len as f64 / 1_048_576.0),
                humantime(*when),
            ]
        })
        .collect();
    fill_rows(ui.list, &rows);
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
    let i = selected_row(ui.list)?;
    UI_CLIPS.with(|c| c.borrow().get(i).cloned())
}

thread_local! {
    /// The montage projects shown, in list order.
    static UI_PROJECTS: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
    /// Whether a montage job was running at the last timer tick.
    static WAS_BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn selected_project(ui: &Ui) -> Option<PathBuf> {
    let i = selected_row(ui.projects)?;
    UI_PROJECTS.with(|p| p.borrow().get(i).cloned())
}

fn rendered_of(dir: &Path) -> String {
    let when = |f: &str| {
        std::fs::metadata(dir.join(f))
            .and_then(|m| m.modified())
            .ok()
            .map(humantime)
    };
    match (when("montage.mp4"), when("preview.mp4")) {
        (Some(t), _) => t,
        (None, Some(t)) => format!("preview {t}"),
        (None, None) => String::new(),
    }
}

fn fill_projects(ui: &Ui) {
    let lib = ui.library.as_ref().map(Library::list).unwrap_or_default();
    fill_game_combo(ui.montage_game, "All games (mixed)", &MONTAGE_GAMES, &lib);
    let keep = selected_project(ui);
    let list = crate::montage::projects(&ui.setup.clips);
    let rows: Vec<Vec<String>> = list
        .iter()
        .map(|(d, p)| {
            vec![
                d.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                match p.format {
                    Format::Tiktok => "TikTok 9:16".into(),
                    Format::Youtube => "YouTube 16:9".into(),
                },
                format!("{:.0} s", p.length),
                p.clips.iter().filter(|c| c.include).count().to_string(),
                rendered_of(d),
            ]
        })
        .collect();
    fill_rows(ui.projects, &rows);
    let dirs: Vec<PathBuf> = list.into_iter().map(|(d, _)| d).collect();
    let sel = keep
        .and_then(|k| dirs.iter().position(|d| *d == k))
        .or(if dirs.is_empty() { None } else { Some(0) });
    UI_PROJECTS.with(|p| *p.borrow_mut() = dirs);
    if let Some(i) = sel {
        select_row(ui.projects, i);
    }
    fill_project_clips(ui, None);
    set_montage_msg(ui);
}

fn fill_project_clips(ui: &Ui, select: Option<usize>) {
    let rows: Vec<Vec<String>> = selected_project(ui)
        .and_then(|d| crate::montage::load(&d).ok())
        .map(|p| {
            p.clips
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    vec![
                        (i + 1).to_string(),
                        c.file
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        c.kills.len().to_string(),
                        c.role.map_or(String::new(), |r| format!("{r:?}")),
                        if c.include { "yes".into() } else { "no".into() },
                        if c.manual {
                            "yes".into()
                        } else {
                            String::new()
                        },
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    fill_rows(ui.pclips, &rows);
    if let Some(i) = select {
        select_row(ui.pclips, i);
    }
}

fn set_montage_msg(ui: &Ui) {
    set_text(
        ui.montage_msg,
        if crate::montage::busy() {
            "Working on a montage… (you can keep playing; you'll get a notification)"
        } else {
            "Pick a song and press New montage, or select a montage below to change and render it."
        },
    );
}

/// Applies `f` to the selected project's file and refreshes its clip list.
fn edit_project(ui: &Ui, f: impl FnOnce(&mut retake_core::montage::Project, usize) -> usize) {
    let (Some(dir), Some(i)) = (selected_project(ui), selected_row(ui.pclips)) else {
        return;
    };
    let Ok(mut p) = crate::montage::load(&dir) else {
        return;
    };
    let j = f(&mut p, i);
    if let Err(e) = crate::montage::save(&dir, &p) {
        tracing::warn!("saving the montage: {e:#}");
    }
    fill_project_clips(ui, Some(j));
}

fn browse_song(ui: &Ui) {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "Music\0*.mp3;*.wav;*.ogg;*.flac;*.m4a;*.aac;*.opus\0All files\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: ui.hwnd,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: every buffer outlives the modal call.
    if unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        set_text(ui.song, &String::from_utf16_lossy(&buf[..n]));
    }
}

fn new_montage(ui: &Ui) -> Option<MediaAction> {
    let song = PathBuf::from(text_of(ui.song).trim().trim_matches('"'));
    if !song.is_file() {
        set_text(ui.montage_msg, "Pick a song first (Browse…).");
        return None;
    }
    let format = if send(ui.format, CB_GETCURSEL, 0, 0) == 1 {
        Format::Youtube
    } else {
        Format::Tiktok
    };
    let length = text_of(ui.length).trim().parse::<f64>().ok();
    Some(MediaAction::Montage(Job::New {
        song,
        format,
        starred: send(ui.pick, CB_GETCURSEL, 0, 0) == 1,
        length,
        game: selected_game(ui.montage_game, &MONTAGE_GAMES),
    }))
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
            ID_STAR => {
                if let (Some(p), Some(lib)) = (selected_clip(ui), ui.library.as_ref()) {
                    let on = !lib.list().iter().any(|c| c.path == p && c.starred);
                    if lib.list().iter().all(|c| c.path != p) {
                        // Not in the library yet (saved before it existed): add it first.
                        return Some(MediaAction::Montage(Job::Rescan));
                    }
                    if let Err(e) = lib.set_star(&p, on) {
                        tracing::warn!("starring {}: {e:#}", p.display());
                    }
                    let i = selected_row(ui.list);
                    fill_clips_of(ui);
                    if let Some(i) = i {
                        select_row(ui.list, i);
                    }
                }
            }
            ID_RESCAN => return Some(MediaAction::Montage(Job::Rescan)),
            ID_BROWSE => browse_song(ui),
            ID_NEW_MONTAGE => return new_montage(ui),
            ID_UP => edit_project(ui, |p, i| p.move_clip(i, true)),
            ID_DOWN => edit_project(ui, |p, i| p.move_clip(i, false)),
            ID_INCLUDE => edit_project(ui, |p, i| {
                if let Some(c) = p.clips.get_mut(i) {
                    c.include = !c.include;
                }
                i
            }),
            ID_PREVIEW | ID_RENDER => {
                return selected_project(ui).map(|dir| {
                    MediaAction::Montage(Job::Render {
                        dir,
                        preview: id == ID_PREVIEW,
                    })
                });
            }
            ID_MONTAGE_FOLDER => {
                let dir = selected_project(ui).unwrap_or_else(|| ui.setup.clips.join("montages"));
                crate::tray::open_path(&dir);
            }
            ID_MONTAGE_REFRESH => fill_projects(ui),
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
                set_montage_msg(ui);
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
                {
                    match send(ui.tabs, TCM_GETCURSEL, 0, 0) {
                        0 => refresh_status(ui),
                        2 => {
                            // A montage job finished (or started): show its result.
                            let busy = crate::montage::busy();
                            if WAS_BUSY.with(|b| b.replace(busy)) != busy {
                                fill_projects(ui);
                            }
                        }
                        _ => {}
                    }
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
                        match page {
                            1 => fill_clips_of(ui),
                            2 => fill_projects(ui),
                            _ => {}
                        }
                    }
                });
            } else if hdr.idFrom == ID_PROJECTS as usize && hdr.code == LVN_ITEMCHANGED {
                UI.with(|u| {
                    if let Some(ui) = u.try_borrow().ok().as_ref().and_then(|b| b.as_ref()) {
                        fill_project_clips(ui, None);
                    }
                });
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            // Notifications from combo boxes and edits (high word ≠ 0) aren't commands, except
            // a new pick in the Clips tab's game filter.
            let (code, id) = ((wparam.0 >> 16) & 0xFFFF, (wparam.0 & 0xFFFF) as i32);
            if code == 0 {
                on_command(id);
            } else if code == 1 && id == ID_CLIPS_GAME {
                UI.with(|u| {
                    if let Some(ui) = u.borrow().as_ref() {
                        fill_clips_of(ui);
                    }
                });
            }
            LRESULT(0)
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
