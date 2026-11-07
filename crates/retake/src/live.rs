//! Settings that change the moment config.toml is saved (no restart): pre/post roll, the
//! sound cue and the auto-clip threshold. Read where they are used.

use std::sync::RwLock;

use retake_core::autoclip::MinPlay;
use retake_core::config::Config;

#[derive(Debug, Clone, Copy)]
pub struct Live {
    pub pre_roll: u32,
    pub post_roll: u32,
    pub sound_cue: bool,
    pub min_play: MinPlay,
}

static LIVE: RwLock<Option<Live>> = RwLock::new(None);

pub fn set_from(c: &Config) {
    *LIVE.write().unwrap_or_else(|e| e.into_inner()) = Some(Live {
        pre_roll: c.clips.pre_roll,
        post_roll: c.clips.post_roll,
        sound_cue: c.clips.sound_cue,
        min_play: c.autoclip.clip.min_play(),
    });
}

pub fn get() -> Option<Live> {
    *LIVE.read().unwrap_or_else(|e| e.into_inner())
}

pub fn sound_cue() -> bool {
    get().is_none_or(|l| l.sound_cue)
}
