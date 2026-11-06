//! Outbound named pipes that ffmpeg reads audio from. Overlapped I/O, so a connect or a write
//! can time out instead of blocking an audio thread forever.

use std::io;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
    WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, PIPE_ACCESS_OUTBOUND, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::HSTRING;

const BUFFER: u32 = 64 * 1024;

/// `\\.\pipe\retake_<track>_<pid>_<run>`: unique per Retake process and ffmpeg run, so a pipe
/// left over from a stuck run never collides with the next one.
pub fn pipe_name(track: &str, run: u64) -> String {
    format!(r"\\.\pipe\retake_{track}_{}_{run}", std::process::id())
}

struct Handle(HANDLE);
impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The server end of an outbound byte pipe.
pub struct OutPipe {
    pipe: Handle,
    event: Handle,
    name: String,
}

// SAFETY: kernel handles, usable from any thread; one thread uses a pipe at a time.
unsafe impl Send for OutPipe {}

impl OutPipe {
    pub fn create(name: &str) -> Result<OutPipe> {
        // SAFETY: plain Win32 calls with valid arguments.
        unsafe {
            let pipe = CreateNamedPipeW(
                &HSTRING::from(name),
                PIPE_ACCESS_OUTBOUND | FILE_FLAG_OVERLAPPED | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                BUFFER,
                BUFFER,
                0,
                None,
            );
            if pipe == INVALID_HANDLE_VALUE {
                bail!(
                    "creating pipe {name}: {}",
                    windows::core::Error::from_thread()
                );
            }
            let pipe = Handle(pipe);
            let event = Handle(CreateEventW(None, true, false, None).context("CreateEventW")?);
            Ok(OutPipe {
                pipe,
                event,
                name: name.to_owned(),
            })
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    fn overlapped(&self) -> OVERLAPPED {
        OVERLAPPED {
            hEvent: self.event.0,
            ..Default::default()
        }
    }

    /// Waits until a client (ffmpeg) opens the pipe.
    pub fn wait_connected(&self, timeout: Duration) -> Result<()> {
        let mut ov = self.overlapped();
        // SAFETY: `ov` lives until the operation completes or is cancelled below.
        unsafe {
            match ConnectNamedPipe(self.pipe.0, Some(&mut ov)) {
                Ok(()) => return Ok(()),
                Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => return Ok(()),
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
                Err(e) => bail!("ConnectNamedPipe {}: {e}", self.name),
            }
            if !self.finish(&mut ov, timeout)? {
                bail!("nothing opened {} within {timeout:?}", self.name);
            }
        }
        Ok(())
    }

    /// Writes all of `bytes`. `Ok(false)` = the reader didn't take them within `timeout`; the
    /// write was cancelled and the data dropped. An error means the reader is gone.
    pub fn write(&self, mut bytes: &[u8], timeout: Duration) -> io::Result<bool> {
        while !bytes.is_empty() {
            let mut ov = self.overlapped();
            let mut n = 0u32;
            // SAFETY: `bytes` and `ov` outlive the operation: it completes, or is cancelled and
            // waited for, before this function returns.
            let done = unsafe {
                match WriteFile(self.pipe.0, Some(bytes), None, Some(&mut ov)) {
                    Ok(()) => {
                        GetOverlappedResult(self.pipe.0, &ov, &mut n, false)?;
                        true
                    }
                    Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                        let ok = self.finish(&mut ov, timeout).map_err(io::Error::other)?;
                        if ok {
                            GetOverlappedResult(self.pipe.0, &ov, &mut n, false)?;
                        }
                        ok
                    }
                    Err(e) => return Err(e.into()),
                }
            };
            if !done {
                return Ok(false);
            }
            bytes = &bytes[n as usize..];
        }
        Ok(true)
    }

    /// Waits for a pending operation on `ov`. On timeout cancels it and waits for the
    /// cancellation, so the buffers it uses are free again; returns false.
    ///
    /// # Safety
    /// `ov` must be the OVERLAPPED of a pending operation on this pipe.
    unsafe fn finish(&self, ov: &mut OVERLAPPED, timeout: Duration) -> Result<bool> {
        let ms = timeout.as_millis().min(u128::from(u32::MAX - 1)) as u32;
        // SAFETY: valid event handle; see the function's contract for `ov`.
        unsafe {
            if WaitForSingleObject(self.event.0, ms) == WAIT_OBJECT_0 {
                return Ok(true);
            }
            let _ = CancelIoEx(self.pipe.0, Some(ov));
            let mut n = 0u32;
            // Waits for the cancellation (or a completion that raced it).
            let completed = GetOverlappedResult(self.pipe.0, ov, &mut n, true).is_ok();
            Ok(completed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn writes_reach_a_reader() {
        let name = pipe_name("test_rw", rand_run());
        let p = OutPipe::create(&name).unwrap();
        let reader = std::thread::spawn({
            let name = name.clone();
            move || {
                let mut f = std::fs::File::open(&name).unwrap();
                let mut buf = Vec::new();
                f.read_to_end(&mut buf).unwrap();
                buf
            }
        });
        p.wait_connected(Duration::from_secs(5)).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        assert!(p.write(&data, Duration::from_secs(5)).unwrap());
        drop(p);
        assert_eq!(reader.join().unwrap(), data);
    }

    #[test]
    fn connect_times_out_without_a_reader() {
        let p = OutPipe::create(&pipe_name("test_noconn", rand_run())).unwrap();
        let t = std::time::Instant::now();
        assert!(p.wait_connected(Duration::from_millis(200)).is_err());
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn write_times_out_when_the_reader_stalls() {
        let name = pipe_name("test_stall", rand_run());
        let p = OutPipe::create(&name).unwrap();
        // Opens the pipe but never reads.
        let f = std::fs::File::open(&name).unwrap();
        p.wait_connected(Duration::from_secs(5)).unwrap();
        let data = vec![0u8; 1024 * 1024];
        let t = std::time::Instant::now();
        assert!(!p.write(&data, Duration::from_millis(200)).unwrap());
        assert!(t.elapsed() < Duration::from_secs(2));
        drop(f);
    }

    #[test]
    fn write_fails_once_the_reader_is_gone() {
        let name = pipe_name("test_gone", rand_run());
        let p = OutPipe::create(&name).unwrap();
        let f = std::fs::File::open(&name).unwrap();
        p.wait_connected(Duration::from_secs(5)).unwrap();
        drop(f);
        assert!(p.write(&[1, 2, 3], Duration::from_millis(200)).is_err());
    }

    fn rand_run() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }
}
