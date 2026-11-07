//! Native audio capture with WASAPI: the game by process id (process loopback), the output
//! device (plain loopback) and microphones. Each [`Capture`] runs on its own thread and hands
//! every packet to a callback as 48 kHz interleaved stereo i16, stamped with its capture time
//! in Unix microseconds (the clock ffmpeg's `RTCTIME` uses).
//!
//! Nothing here opens a process handle: process loopback only needs the id.

mod clock;
mod wasapi;

pub use clock::{qpc_100ns, qpc_to_unix_us, unix_us};
pub use wasapi::{Capture, Source, capture_devices, default_device_id};
