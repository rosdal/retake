//! Pure, platform-independent logic for Retake. No Windows APIs here.
pub mod buffer;
pub mod config;
pub mod display;
pub mod ffmpeg_args;
pub mod ffmpeg_caps;
pub mod hotkey;
pub mod progress;
pub mod watcher;

/// Milliseconds since an arbitrary start. Pure code takes time as a value so tests are exact.
pub type Ms = u64;
