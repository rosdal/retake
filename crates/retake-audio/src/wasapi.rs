//! WASAPI capture threads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use retake_core::audio::{CHANNELS, RATE};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    AUDIOCLIENT_ACTIVATION_PARAMS, AUDIOCLIENT_ACTIVATION_PARAMS_0,
    AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK, AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS,
    ActivateAudioInterfaceAsync, DEVICE_STATE_ACTIVE, IActivateAudioInterfaceAsyncOperation,
    IActivateAudioInterfaceCompletionHandler, IActivateAudioInterfaceCompletionHandler_Impl,
    IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE, VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
    WAVE_FORMAT_PCM, WAVEFORMATEX, eCapture, eConsole, eRender,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{
    BLOB, CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
    WaitForSingleObject,
};
use windows::Win32::System::Variant::VT_BLOB;
use windows::core::{HRESULT, Interface, implement, w};

use crate::clock::{qpc_100ns, qpc_to_unix_us};

/// What to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Only this process (and its children): Windows 10 2004 (build 19041) or later.
    ProcessLoopback { pid: u32 },
    /// Everything the default output device plays.
    DeviceLoopback,
    /// A microphone by friendly name; empty or `"default"` = the default capture device.
    Mic { name: String },
}

/// A running capture. Dropping it stops the thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    description: String,
}

/// Called with each packet's capture time (Unix µs) and its interleaved stereo frames.
type OnData = Box<dyn FnMut(i64, &[i16]) + Send>;

impl Capture {
    /// Starts capturing `source` on a new thread. Returns once the stream runs, or the error
    /// that stopped it from starting.
    pub fn start(
        source: Source,
        on_data: impl FnMut(i64, &[i16]) + Send + 'static,
    ) -> Result<Capture> {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(format!("audio-{}", short_name(&source)))
            .spawn({
                let stop = stop.clone();
                let source = source.clone();
                move || run(source, Box::new(on_data), &stop, tx)
            })
            .context("spawning audio thread")?;
        let description = match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(d)) => d,
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                bail!("{source:?}: audio stream did not start within 10 s");
            }
        };
        Ok(Capture {
            stop,
            thread: Some(thread),
            description,
        })
    }

    /// The device and format in use, for the log.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Whether the capture thread has ended (device removed, stream error).
    pub fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn short_name(s: &Source) -> &'static str {
    match s {
        Source::ProcessLoopback { .. } => "process",
        Source::DeviceLoopback => "device",
        Source::Mic { .. } => "mic",
    }
}

/// 48 kHz, 16-bit stereo PCM: what every pipe carries. `AUTOCONVERTPCM` makes WASAPI deliver
/// it whatever the device's own mix format is.
fn wanted_format() -> WAVEFORMATEX {
    let block = (CHANNELS * 2) as u16;
    WAVEFORMATEX {
        wFormatTag: WAVE_FORMAT_PCM as u16,
        nChannels: CHANNELS as u16,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * u32::from(block),
        nBlockAlign: block,
        wBitsPerSample: 16,
        cbSize: 0,
    }
}

/// Owns COM on this thread; uninitialises on drop.
struct ComGuard;
impl Drop for ComGuard {
    fn drop(&mut self) {
        // SAFETY: paired with the successful CoInitializeEx in `run`.
        unsafe { CoUninitialize() };
    }
}

struct EventHandle(HANDLE);
impl Drop for EventHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn run(source: Source, on_data: OnData, stop: &AtomicBool, started: Sender<Result<String>>) {
    // SAFETY: COM init for this thread only; the guard uninitialises it.
    if let Err(e) = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok() {
        let _ = started.send(Err(anyhow!("CoInitializeEx: {e}")));
        return;
    }
    let _com = ComGuard;
    let mut task_index = 0u32;
    // SAFETY: valid string and out-pointer. Failure only means normal thread priority.
    let mmcss = unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) };
    if let Err(e) = &mmcss {
        tracing::warn!("{source:?}: no Pro Audio priority: {e}");
    }
    let stream = match open(&source) {
        Ok(s) => s,
        Err(e) => {
            let _ = started.send(Err(e));
            return;
        }
    };
    let _ = started.send(Ok(stream.description.clone()));
    if let Err(e) = pump(&stream, on_data, stop) {
        tracing::error!("{source:?}: audio capture stopped: {e:#}");
    }
    // SAFETY: stopping a started client; errors don't matter on the way out.
    let _ = unsafe { stream.client.Stop() };
    if let Ok(h) = mmcss {
        // SAFETY: the handle came from AvSetMmThreadCharacteristicsW on this thread.
        let _ = unsafe { AvRevertMmThreadCharacteristics(h) };
    }
}

struct Stream {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: EventHandle,
    description: String,
    loopback: bool,
}

fn open(source: &Source) -> Result<Stream> {
    let (client, name, loopback) = match source {
        Source::ProcessLoopback { pid } => (
            activate_process_loopback(*pid)?,
            format!("process loopback (pid {pid})"),
            true,
        ),
        Source::DeviceLoopback => {
            let dev = default_device(true)?;
            let name = friendly_name(&dev).unwrap_or_else(|_| "default output".into());
            // SAFETY: activating an interface on a valid device.
            let c: IAudioClient = unsafe { dev.Activate(CLSCTX_ALL, None) }
                .context("activating the output device")?;
            (c, format!("loopback of {name}"), true)
        }
        Source::Mic { name } => {
            let dev = find_mic(name)?;
            let n = friendly_name(&dev).unwrap_or_else(|_| "default microphone".into());
            // SAFETY: activating an interface on a valid device.
            let c: IAudioClient =
                unsafe { dev.Activate(CLSCTX_ALL, None) }.context("activating the microphone")?;
            (c, format!("microphone {n}"), false)
        }
    };
    let fmt = wanted_format();
    let mut flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
        | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if loopback {
        flags |= AUDCLNT_STREAMFLAGS_LOOPBACK;
    }
    // SAFETY: `fmt` outlives the call; 100 ms shared buffer, event driven.
    unsafe { client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 1_000_000, 0, &fmt, None) }
        .with_context(|| format!("initialising {name}"))?;
    // SAFETY: an unnamed auto-reset event.
    let event = EventHandle(unsafe { CreateEventW(None, false, false, None) }?);
    // SAFETY: valid handle; the client keeps signalling it until stopped.
    unsafe { client.SetEventHandle(event.0) }.context("SetEventHandle")?;
    // SAFETY: the client is initialised.
    let capture: IAudioCaptureClient = unsafe { client.GetService() }.context("GetService")?;
    // SAFETY: the client is initialised and has an event handle.
    unsafe { client.Start() }.context("starting the stream")?;
    Ok(Stream {
        client,
        capture,
        event,
        description: format!("{name}, 48 kHz 16-bit stereo"),
        loopback,
    })
}

fn pump(s: &Stream, mut on_data: OnData, stop: &AtomicBool) -> Result<()> {
    let mut discontinuities = 0u64;
    let mut buf: Vec<i16> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: valid event handle. A timeout just loops so `stop` is seen.
        if unsafe { WaitForSingleObject(s.event.0, 100) } != WAIT_OBJECT_0 {
            continue;
        }
        loop {
            // SAFETY: the stream is started.
            let next = unsafe { s.capture.GetNextPacketSize() }.context("GetNextPacketSize")?;
            if next == 0 {
                break;
            }
            let (mut data, mut frames, mut flags, mut qpc) =
                (std::ptr::null_mut(), 0u32, 0u32, 0u64);
            // SAFETY: out-pointers are valid locals; the buffer is released below.
            unsafe {
                s.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, Some(&mut qpc))
            }
            .context("GetBuffer")?;
            let n = frames as usize * CHANNELS;
            buf.clear();
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                buf.resize(n, 0);
            } else {
                // SAFETY: WASAPI hands us `frames` frames in the format we initialised with
                // (16-bit stereo), valid until ReleaseBuffer.
                buf.extend_from_slice(unsafe { std::slice::from_raw_parts(data.cast::<i16>(), n) });
            }
            if flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0 {
                discontinuities += 1;
                // The first packet of a stream often carries it; only later ones mean a gap.
                if discontinuities > 1 {
                    tracing::debug!(discontinuities, "audio discontinuity");
                }
            }
            // SAFETY: releasing exactly what GetBuffer gave us.
            unsafe { s.capture.ReleaseBuffer(frames) }.context("ReleaseBuffer")?;
            // Loopback packets are stamped with when they will be played (after the output
            // device's latency), but the picture is captured when it is composed, before it is
            // shown. Placing loopback audio at its mix time, when the packet arrives here,
            // lines it up with the picture the way the game made them. A microphone's stamp
            // is its real capture time, so it is used as is.
            let at = if s.loopback || qpc == 0 {
                qpc_to_unix_us(qpc_100ns()) - i64::from(frames) * 1_000_000 / i64::from(RATE)
            } else {
                qpc_to_unix_us(qpc)
            };
            on_data(at, &buf);
        }
    }
    Ok(())
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: COM is initialised on this thread.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("creating the device enumerator")
}

fn default_device(render: bool) -> Result<IMMDevice> {
    let flow = if render { eRender } else { eCapture };
    // SAFETY: valid enumerator.
    unsafe { enumerator()?.GetDefaultAudioEndpoint(flow, eConsole) }.context(if render {
        "no default output device"
    } else {
        "no default microphone"
    })
}

fn friendly_name(dev: &IMMDevice) -> Result<String> {
    // SAFETY: read-only property store of a valid device; the PROPVARIANT clears itself on drop.
    unsafe {
        let store = dev.OpenPropertyStore(STGM_READ)?;
        let pv = store.GetValue(&PKEY_Device_FriendlyName)?;
        Ok(pv.Anonymous.Anonymous.Anonymous.pwszVal.to_string()?)
    }
}

/// The id of the default output (`render`) or capture device, to notice a switch.
pub fn default_device_id(render: bool) -> Option<String> {
    // SAFETY: COM init for this call; the guard uninitialises it.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .ok()?;
    let _com = ComGuard;
    let dev = default_device(render).ok()?;
    // SAFETY: GetId returns a CoTaskMem string we free after copying it.
    unsafe {
        let id = dev.GetId().ok()?;
        let s = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        s
    }
}

/// The friendly names of every active microphone.
pub fn capture_devices() -> Vec<String> {
    let inner = || -> Result<Vec<String>> {
        // SAFETY: COM init for this call; the guard uninitialises it.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()?;
        let _com = ComGuard;
        // SAFETY: valid enumerator and collection indices.
        unsafe {
            let all = enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
            (0..all.GetCount()?)
                .map(|i| friendly_name(&all.Item(i)?))
                .collect()
        }
    };
    inner().unwrap_or_default()
}

fn find_mic(name: &str) -> Result<IMMDevice> {
    if name.is_empty() || name.eq_ignore_ascii_case("default") {
        return default_device(false);
    }
    // SAFETY: valid enumerator and collection indices.
    unsafe {
        let all = enumerator()?.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut names = Vec::new();
        for i in 0..all.GetCount()? {
            let dev = all.Item(i)?;
            let n = friendly_name(&dev).unwrap_or_default();
            if n.eq_ignore_ascii_case(name) {
                return Ok(dev);
            }
            names.push(n);
        }
        bail!(
            "no microphone named \"{name}\"; found: {}",
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            }
        )
    }
}

// `implement` objects are agile (IAgileObject) by default, which this callback needs.
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion {
    done: Mutex<Option<Sender<()>>>,
}

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        _op: windows_core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows_core::Result<()> {
        if let Some(tx) = self.done.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

fn activate_process_loopback(pid: u32) -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
            },
        },
    };
    // `ManuallyDrop`: the windows crate's `Drop` for PROPVARIANT calls PropVariantClear, which
    // would free the blob pointer below, and that points at a stack value.
    let mut pv = std::mem::ManuallyDrop::new(PROPVARIANT::default());
    // SAFETY: writing the active union members of a zeroed PROPVARIANT. The blob points at
    // `params`, which outlives the activation.
    unsafe {
        let inner = &mut pv.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&raw const params).cast_mut().cast(),
        };
    }
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Completion {
        done: Mutex::new(Some(tx)),
    }
    .into();
    // SAFETY: all arguments are valid for the call; the operation is kept until completed.
    let op = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&raw const *pv),
            &handler,
        )
    }
    .context("ActivateAudioInterfaceAsync")?;
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| anyhow!("process loopback activation timed out"))?;
    let mut hr = HRESULT(0);
    let mut unk = None;
    // SAFETY: the operation completed; out-pointers are valid locals.
    unsafe { op.GetActivateResult(&mut hr, &mut unk) }.context("GetActivateResult")?;
    hr.ok()
        .with_context(|| format!("process loopback activation for pid {pid}"))?;
    unk.ok_or_else(|| anyhow!("process loopback returned no interface"))?
        .cast::<IAudioClient>()
        .context("process loopback interface")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn capture_for(source: Source, ms: u64) -> usize {
        let got = Arc::new(AtomicUsize::new(0));
        let g = got.clone();
        let c = Capture::start(source, move |_, f| {
            g.fetch_add(f.len() / CHANNELS, Ordering::Relaxed);
        })
        .unwrap();
        eprintln!("{}", c.description());
        std::thread::sleep(Duration::from_millis(ms));
        drop(c);
        got.load(Ordering::Relaxed)
    }

    #[test]
    #[ignore = "needs a microphone"]
    fn mic_captures() {
        let frames = capture_for(
            Source::Mic {
                name: String::new(),
            },
            1_000,
        );
        assert!(frames > 40_000, "{frames}");
    }

    #[test]
    #[ignore = "needs an output device"]
    fn device_loopback_starts() {
        // Loopback delivers nothing while nothing plays, so only starting is checked.
        capture_for(Source::DeviceLoopback, 300);
    }

    #[test]
    #[ignore = "needs Windows 10 2004 or later"]
    fn process_loopback_starts_on_own_process() {
        capture_for(
            Source::ProcessLoopback {
                pid: std::process::id(),
            },
            300,
        );
    }

    #[test]
    fn unknown_mic_lists_the_real_ones() {
        let e = Capture::start(
            Source::Mic {
                name: "no such mic 1234".into(),
            },
            |_, _| {},
        )
        .err();
        // On a machine without any microphone this still errors, just with "none"; on a CI
        // runner without the audio service the enumerator itself fails first.
        let e = format!("{:#}", e.expect("a missing mic must fail"));
        assert!(
            e.contains("no such mic 1234") || e.contains("enumerator"),
            "{e}"
        );
    }
}
