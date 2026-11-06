//! Pure, platform-independent logic for Retake. No Windows APIs here.
pub mod audio;
pub mod autoconf;
pub mod buffer;
pub mod clip;
pub mod config;
pub mod cue;
pub mod disk;
pub mod display;
pub mod ffmpeg_args;
pub mod ffmpeg_caps;
pub mod hotkey;
pub mod progress;
pub mod quiet;
pub mod segments;
pub mod siege_settings;
pub mod sync;
pub mod watcher;

/// Milliseconds since an arbitrary start. Pure code takes time as a value so tests are exact.
pub type Ms = u64;
