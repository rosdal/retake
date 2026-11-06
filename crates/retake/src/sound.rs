//! The press and saved cues, played asynchronously from memory.

use std::sync::LazyLock;

use retake_core::cue::{PRESS_CUE, SAVED_CUE, tone_wav};
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows::core::PCWSTR;

static PRESS: LazyLock<Vec<u8>> = LazyLock::new(|| tone_wav(PRESS_CUE));
static SAVED: LazyLock<Vec<u8>> = LazyLock::new(|| tone_wav(SAVED_CUE));

fn play(wav: &'static [u8]) {
    // SAFETY: with SND_MEMORY the pointer is a whole WAV file in memory; it is 'static, so it
    // outlives the asynchronous playback.
    let ok = unsafe {
        PlaySoundW(
            PCWSTR(wav.as_ptr().cast()),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        )
    };
    if !ok.as_bool() {
        tracing::debug!("PlaySoundW failed");
    }
}

pub fn play_press() {
    play(PRESS.as_slice());
}

pub fn play_saved() {
    play(SAVED.as_slice());
}

#[cfg(test)]
mod tests {
    /// Audible, so opt-in: `cargo test -- --ignored`, on a machine where sound is fine.
    #[test]
    #[ignore]
    fn cues_play_without_error() {
        // On a machine without audio output PlaySoundW returns false; that is only logged.
        super::play_press();
        super::play_saved();
    }
}
