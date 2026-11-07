//! Pure, platform-independent logic for Retake. No Windows APIs here.
pub mod audio;
pub mod autoclip;
pub mod autoconf;
pub mod buffer;
pub mod clip;
pub mod config;
pub mod control;
pub mod cue;
pub mod disk;
pub mod display;
pub mod export;
pub mod fcpxml;
pub mod ffmpeg_args;
pub mod ffmpeg_caps;
pub mod games;
pub mod grade;
pub mod gsi;
pub mod hotkey;
pub mod liveclient;
pub mod logo;
pub mod montage;
pub mod montage_render;
pub mod progress;
pub mod queue;
pub mod quiet;
pub mod replay;
pub mod segments;
pub mod siege_settings;
pub mod song;
pub mod sync;
pub mod update;
pub mod watcher;

/// Milliseconds since an arbitrary start. Pure code takes time as a value so tests are exact.
pub type Ms = u64;
pub use serde;
