//! A/V sync stand-in game: a window that flashes white with a 1 kHz click once a second. Run
//! Retake with `avsync.exe` in `game.processes`, save a clip, then measure it with
//! `cargo run --example avsync_check -- <clip.mkv>`.
//!
//! `avsync [x y [quiet_from quiet_secs]]` puts the 400x400 window at screen position x, y
//! (default 100, 100) and, optionally, stays dark and silent from `quiet_from` seconds after
//! start for `quiet_secs` seconds.
//! The click is rendered into the very buffer written when the window is painted white, so the
//! two leave the program within one 10 ms audio period of each other.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, DeleteObject, EndPaint, FillRect, InvalidateRect, PAINTSTRUCT,
    UpdateWindow,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX,
    eConsole, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostQuitMessage,
    RegisterClassW, WINDOW_EX_STYLE, WM_DESTROY, WM_PAINT, WNDCLASSW, WS_OVERLAPPEDWINDOW,
    WS_VISIBLE,
};
use windows::core::w;

fn unix_us() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as i64)
}

static WHITE: AtomicBool = AtomicBool::new(false);
static mut WINDOW: HWND = HWND(std::ptr::null_mut());

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: standard window procedure calls on our own window.
    unsafe {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let color = if WHITE.load(Ordering::Relaxed) {
                    0x00FF_FFFF
                } else {
                    0
                };
                let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(color));
                let rc = RECT {
                    left: 0,
                    top: 0,
                    right: 4000,
                    bottom: 4000,
                };
                FillRect(hdc, &rc, brush);
                let _ = DeleteObject(brush.into());
                // Fixed colour patches along the bottom (red, green, blue, mid-grey) to check
                // the capture's colour matrix and range. COLORREF is 0x00BBGGRR.
                for (i, c) in [0x0000_00FFu32, 0x0000_FF00, 0x00FF_0000, 0x0080_8080]
                    .into_iter()
                    .enumerate()
                {
                    let b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(c));
                    let x = i as i32 * 90 + 10;
                    let r = RECT {
                        left: x,
                        top: 300,
                        right: x + 80,
                        bottom: 350,
                    };
                    FillRect(hdc, &r, b);
                    let _ = DeleteObject(b.into());
                }
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

fn repaint() {
    // SAFETY: WINDOW is set once before the audio thread starts; repainting is thread-safe
    // enough for a test tool (InvalidateRect may be called from any thread).
    unsafe {
        let h = WINDOW;
        let _ = InvalidateRect(Some(h), None, true);
        let _ = UpdateWindow(h);
    }
}

fn audio(stop: Arc<AtomicBool>, quiet: (u64, u64)) -> windows::core::Result<()> {
    // SAFETY: COM and WASAPI calls with valid arguments; buffers are released after use.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let dev = e.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let client: IAudioClient = dev.Activate(CLSCTX_ALL, None)?;
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 192_000,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            100_000,
            0,
            &fmt,
            None,
        )?;
        let ev = CreateEventW(None, false, false, None)?;
        client.SetEventHandle(ev)?;
        let render: IAudioRenderClient = client.GetService()?;
        let (mut def, mut min) = (0i64, 0i64);
        let _ = client.GetDevicePeriod(Some(&mut def), Some(&mut min));
        eprintln!(
            "render: stream latency {:.1} ms, device period {:.1} ms (min {:.1}), buffer {} frames",
            client.GetStreamLatency().unwrap_or(0) as f64 / 10_000.0,
            def as f64 / 10_000.0,
            min as f64 / 10_000.0,
            client.GetBufferSize().unwrap_or(0)
        );
        let size = client.GetBufferSize()?;
        client.Start()?;
        let start = Instant::now();
        let mut next_flash = Duration::from_secs(1);
        let mut click_left = 0usize; // frames of click still to write
        let mut phase = 0f32;
        let mut white_until: Option<Instant> = None;
        while !stop.load(Ordering::Relaxed) {
            WaitForSingleObject(ev, 200);
            let pad = client.GetCurrentPadding()?;
            let n = (size - pad) as usize;
            if n == 0 {
                continue;
            }
            if start.elapsed() >= next_flash {
                next_flash += Duration::from_secs(1);
                let s = start.elapsed().as_secs();
                if s >= quiet.0 && s < quiet.0 + quiet.1 {
                    continue;
                }
                click_left = 480; // 10 ms
                phase = 0.0;
                WHITE.store(true, Ordering::Relaxed);
                let before = unix_us();
                repaint();
                eprintln!("click {before} painted {} padding {pad}", unix_us());
                white_until = Some(Instant::now() + Duration::from_millis(100));
            }
            if white_until.is_some_and(|t| Instant::now() >= t) {
                white_until = None;
                WHITE.store(false, Ordering::Relaxed);
                repaint();
            }
            let buf = render.GetBuffer(n as u32)?.cast::<i16>();
            let out = std::slice::from_raw_parts_mut(buf, n * 2);
            for f in out.as_chunks_mut::<2>().0 {
                let v = if click_left > 0 {
                    click_left -= 1;
                    phase += 2.0 * std::f32::consts::PI * 1_000.0 / 48_000.0;
                    (phase.sin() * 6_000.0) as i16
                } else {
                    0
                };
                f[0] = v;
                f[1] = v;
            }
            render.ReleaseBuffer(n as u32, 0)?;
        }
        client.Stop()?;
    }
    Ok(())
}

fn main() {
    let args: Vec<i32> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let (x, y) = (
        args.first().copied().unwrap_or(100),
        args.get(1).copied().unwrap_or(100),
    );
    let quiet = (
        args.get(2).copied().unwrap_or(0).max(0) as u64,
        args.get(3).copied().unwrap_or(0).max(0) as u64,
    );
    // SAFETY: plain window creation and a message loop on this thread.
    unsafe {
        let inst = GetModuleHandleW(None).unwrap();
        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: inst.into(),
            lpszClassName: w!("retake_avsync"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("retake_avsync"),
            w!("Retake A/V sync test"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            x,
            y,
            400,
            400,
            None,
            None,
            Some(inst.into()),
            None,
        )
        .unwrap();
        WINDOW = hwnd;
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let t = std::thread::spawn(move || {
            if let Err(e) = audio(s, quiet) {
                eprintln!("audio: {e}");
            }
        });
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }
        stop.store(true, Ordering::Relaxed);
        let _ = t.join();
    }
}
