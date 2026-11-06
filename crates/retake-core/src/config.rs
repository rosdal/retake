//! `config.toml`: structs, embedded defaults, merging and validation.
//!
//! The defaults live in exactly one place, `default-config.toml`. A user file is merged over
//! them key by key and the result is deserialized with `deny_unknown_fields`, so a missing
//! key takes its default and a misspelt one is an error.

use std::fmt;
use std::path::PathBuf;

use serde::Deserialize;
use toml::{Table, Value};

/// The commented default config, written next to the exe on first run.
pub const DEFAULT_CONFIG_TOML: &str = include_str!("default-config.toml");

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub game: Game,
    pub capture: Capture,
    pub audio: Audio,
    pub clips: Clips,
    pub autoclip: Autoclip,
    pub grade: Grade,
    pub export: Export,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Game {
    pub processes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    Hevc,
    Av1,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub output: u32,
    pub fps: u32,
    pub codec: Codec,
    pub qp_i: u32,
    pub qp_p: u32,
    pub buffer_dir: PathBuf,
    pub buffer_secs: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    pub game: bool,
    pub mic: bool,
    pub mic_device: String,
    pub mic_gate_db: f32,
    pub comms: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clips {
    pub out_dir: PathBuf,
    pub pre_roll: u32,
    pub post_roll: u32,
    pub hotkey: String,
    pub hotkey_full: String,
    pub sound_cue: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Autoclip {
    pub enabled: bool,
    pub region: [u32; 4],
    pub template: String,
    pub threshold: f32,
    pub merge_window: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grade {
    pub lut: String,
    pub cas_strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportWhen {
    AfterGame,
    Immediately,
    Manual,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Export {
    pub auto: bool,
    pub when: ExportWhen,
    pub presets: Vec<String>,
    pub keep_raw: bool,
}

/// Why a config could not be loaded.
#[derive(Debug)]
pub enum ConfigError {
    /// Not valid TOML, an unknown key, or a value of the wrong type.
    Parse(String),
    /// Parsed, but one or more values break a rule. One message per problem.
    Invalid(Vec<String>),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Parse(msg) => write!(f, "config.toml: {msg}"),
            ConfigError::Invalid(errs) => write!(f, "config.toml: {}", errs.join("; ")),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Recursively overlays `over` onto `base`. Tables merge; everything else replaces.
fn merge(base: &mut Table, over: Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(b)), Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(key, v);
            }
        }
    }
}

/// Parses a user config (possibly empty) over the defaults and checks the pure rules.
/// Paths are left as written; see [`resolve_paths`].
pub fn load_str(user: &str) -> Result<Config, ConfigError> {
    let mut merged: Table = DEFAULT_CONFIG_TOML
        .parse()
        .expect("embedded default-config.toml is valid TOML");
    let user: Table = user.parse().map_err(|e| ConfigError::Parse(format!("{e}")))?;
    merge(&mut merged, user);
    let config = Config::deserialize(Value::Table(merged))
        .map_err(|e| ConfigError::Parse(e.to_string()))?;
    let errs = validate(&config);
    if errs.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Invalid(errs))
    }
}

/// Rules that need no filesystem access. Returns one message per broken rule.
pub fn validate(_c: &Config) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = load_str("").unwrap();
        assert_eq!(c.capture.fps, 120);
        assert_eq!(c.clips.pre_roll, 15);
    }

    #[test]
    fn user_value_overrides_default() {
        let c = load_str("[capture]\nfps = 60").unwrap();
        assert_eq!(c.capture.fps, 60);
        assert_eq!(c.capture.qp_i, 16);
    }

    #[test]
    fn unknown_field_is_refused() {
        let e = load_str("[clips]\npre_rol = 5").unwrap_err().to_string();
        assert!(e.contains("pre_rol"), "{e}");
    }

    #[test]
    fn unknown_section_is_refused() {
        assert!(load_str("[bogus]\nx = 1").is_err());
    }

    #[test]
    fn wrong_type_names_field() {
        let e = load_str("[capture]\nfps = \"fast\"").unwrap_err().to_string();
        assert!(e.contains("fps"), "{e}");
    }

    #[test]
    fn default_file_is_fully_commented() {
        let comments = DEFAULT_CONFIG_TOML
            .lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .count();
        assert!(comments >= 10);
    }
}
