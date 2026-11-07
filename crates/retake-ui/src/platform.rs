//! Windows glue: one window at a time, the file picker, opening files, sending commands to
//! the service and starting it.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use retake_core::control::{Command, PIPE};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError, HWND};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::{HSTRING, w};

pub const TITLE: &str = "Retake";

/// Holds the single-instance mutex for the process's life. Returns `false` (after bringing
/// the open window forward) if another window is already open.
pub fn first_instance() -> bool {
    // SAFETY: a named mutex we keep (leaked) for the process's life.
    unsafe {
        let m = CreateMutexW(None, true, w!("Local\\RetakeWindow"));
        if m.is_ok() && GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(None, &HSTRING::from(TITLE))
                && h != HWND::default()
            {
                let _ = ShowWindow(h, SW_RESTORE);
                let _ = SetForegroundWindow(h);
            }
            return false;
        }
        std::mem::forget(m);
    }
    true
}

/// Sends a command to the service. Fails if Retake isn't running.
pub fn send(c: &Command) -> Result<(), String> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .open(PIPE)
        .map_err(|_| "Retake isn't running".to_owned())?;
    f.write_all(c.to_line().as_bytes())
        .map_err(|e| e.to_string())
}

/// Opens a file or folder with its default app.
pub fn open(p: &Path) {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // SAFETY: plain call with valid strings.
    unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &HSTRING::from(p.as_os_str()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Shows `p` selected in Explorer.
pub fn reveal(p: &Path) {
    let _ = std::process::Command::new("explorer.exe")
        .arg(format!("/select,{}", p.display()))
        .spawn();
}

/// Starts retake.exe (next to this exe) when the service isn't running.
pub fn start_service() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("retake.exe");
    std::process::Command::new(&exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("starting {}: {e}", exe.display()))
}

/// The Windows file picker for a song.
pub fn pick_song() -> Option<PathBuf> {
    use windows::Win32::UI::Controls::Dialogs::{
        GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    let mut buf = [0u16; 1024];
    let filter: Vec<u16> = "Music\0*.mp3;*.wav;*.ogg;*.flac;*.m4a;*.aac;*.opus\0All files\0*.*\0\0"
        .encode_utf16()
        .collect();
    let mut ofn = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        lpstrFilter: windows::core::PCWSTR(filter.as_ptr()),
        lpstrFile: windows::core::PWSTR(buf.as_mut_ptr()),
        nMaxFile: buf.len() as u32,
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: the buffers outlive the modal call.
    if unsafe { GetOpenFileNameW(&mut ofn) }.as_bool() {
        let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    } else {
        None
    }
}

/// `%LOCALAPPDATA%\Retake`.
pub fn app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Retake")
}

/// ffmpeg (and ffprobe) next to retake.exe, or the dev layout.
pub fn ffmpeg() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    [
        std::env::var_os("RETAKE_FFMPEG").map(PathBuf::from),
        Some(dir.join("ffmpeg").join("ffmpeg.exe")),
        Some(dir.join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe")),
    ]
    .into_iter()
    .flatten()
    .find(|p| p.is_file())
}

/// A child process without a console window.
pub fn quiet_command(exe: &Path) -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut c = std::process::Command::new(exe);
    c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    c
}
