//! `config.toml`: structs, embedded defaults, merging and validation.
//!
//! The defaults live in exactly one place, `default-config.toml`. A user file is merged over
//! them key by key and the result is deserialized with `deny_unknown_fields`, so a missing
//! key takes its default and a misspelt one is an error.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use toml::{Table, Value};

use crate::autoconf::{Auto, EncoderSetting, auto_dirs};

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
    /// The `ddagrab` output, or `auto` (the monitor Siege is on).
    pub output: Auto<u32>,
    /// Capture rate, or `auto` (the monitor's refresh rate).
    pub fps: Auto<u32>,
    /// Hardware encoder, or `auto` (from the GPU maker).
    pub encoder: EncoderSetting,
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
    pub template: PathBuf,
    pub threshold: f32,
    pub merge_window: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grade {
    pub lut: PathBuf,
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
    let user: Table = user
        .parse()
        .map_err(|e| ConfigError::Parse(format!("{e}")))?;
    merge(&mut merged, user);
    let config =
        Config::deserialize(Value::Table(merged)).map_err(|e| ConfigError::Parse(e.to_string()))?;
    let errs = validate(&config);
    if errs.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Invalid(errs))
    }
}

/// Allowed capture frame rates.
pub const ALLOWED_FPS: [u32; 5] = [60, 120, 144, 165, 240];

/// Name of the probe file `check_dirs` writes and deletes.
pub const WRITE_TEST_FILE: &str = ".retake-write-test";

/// Rules that need no filesystem access. Returns one message per broken rule.
pub fn validate(c: &Config) -> Vec<String> {
    let mut errs = Vec::new();
    let cap = &c.capture;

    if let Auto::Value(fps) = cap.fps
        && !ALLOWED_FPS.contains(&fps)
    {
        let list = ALLOWED_FPS.map(|f| f.to_string()).join(", ");
        errs.push(format!("capture.fps = {fps}: must be one of {list}"));
    }
    let mut qp_ok = true;
    for (name, v) in [("qp_i", cap.qp_i), ("qp_p", cap.qp_p)] {
        if v > 51 {
            errs.push(format!("capture.{name} = {v}: must be 0–51"));
            qp_ok = false;
        }
    }
    if qp_ok && cap.qp_i > cap.qp_p {
        errs.push(format!(
            "capture.qp_i = {}: must be ≤ capture.qp_p ({})",
            cap.qp_i, cap.qp_p
        ));
    }
    if !is_auto(&cap.buffer_dir) && cap.buffer_dir.to_string_lossy().contains('%') {
        errs.push(format!(
            "capture.buffer_dir = {}: must not contain '%'",
            cap.buffer_dir.display()
        ));
    }
    // u64 so huge roll values can't overflow.
    let min_buffer = u64::from(c.clips.pre_roll) + u64::from(c.clips.post_roll) + 4;
    if u64::from(cap.buffer_secs) < min_buffer {
        errs.push(format!(
            "capture.buffer_secs = {}: must be ≥ pre_roll + post_roll + 4 ({min_buffer})",
            cap.buffer_secs
        ));
    }
    let clips = &c.clips;
    let mut parsed = Vec::new();
    for (name, text) in [
        ("hotkey", &clips.hotkey),
        ("hotkey_full", &clips.hotkey_full),
    ] {
        match crate::hotkey::parse(text) {
            Ok(k) => parsed.push(k),
            Err(e) => errs.push(format!("clips.{name} = {text}: {e}")),
        }
    }
    if let [a, b] = parsed[..]
        && a == b
    {
        errs.push(format!(
            "clips.hotkey_full = {}: must differ from clips.hotkey",
            clips.hotkey_full
        ));
    }
    errs
}

/// The folder value that means "pick a drive for me".
pub const AUTO_DIR: &str = "auto";

fn is_auto(p: &Path) -> bool {
    p == Path::new(AUTO_DIR)
}

/// Makes every relative path in the config absolute against `base` (the folder holding
/// `config.toml`), so they don't depend on the working directory Retake was started from.
/// `"auto"` folders are left for [`resolve_auto_dirs`].
pub fn resolve_paths(c: &mut Config, base: &Path) {
    for p in [
        &mut c.capture.buffer_dir,
        &mut c.clips.out_dir,
        &mut c.autoclip.template,
        &mut c.grade.lut,
    ] {
        if p.is_relative() && !is_auto(p) {
            *p = base.join(&*p);
        }
    }
}

/// Whether `buffer_dir` or `out_dir` is `"auto"`.
pub fn needs_auto_dirs(c: &Config) -> bool {
    is_auto(&c.capture.buffer_dir) || is_auto(&c.clips.out_dir)
}

/// Puts each `"auto"` folder under `root` (`<root>\Retake\buffer`, `<root>\Retake\clips`).
pub fn resolve_auto_dirs(c: &mut Config, root: &Path) {
    let (buffer, clips) = auto_dirs(root);
    if is_auto(&c.capture.buffer_dir) {
        c.capture.buffer_dir = buffer;
    }
    if is_auto(&c.clips.out_dir) {
        c.clips.out_dir = clips;
    }
}

/// Checks each output directory exists (creating it if needed) and is writable.
pub fn check_dirs(c: &Config) -> Vec<String> {
    let mut errs = Vec::new();
    for (name, dir) in [
        ("capture.buffer_dir", &c.capture.buffer_dir),
        ("clips.out_dir", &c.clips.out_dir),
    ] {
        let probe = dir.join(WRITE_TEST_FILE);
        let result = fs::create_dir_all(dir)
            .and_then(|()| fs::write(&probe, b"ok"))
            .and_then(|()| fs::remove_file(&probe));
        if let Err(e) = result {
            errs.push(format!("{name} = {}: not writable ({e})", dir.display()));
        }
    }
    errs
}

/// Loads `path`, resolves its relative paths against the file's folder and its `"auto"` folders
/// under the drive `auto_root` picks (called only when needed), and checks every rule,
/// including that the output directories are writable.
pub fn load_file(
    path: &Path,
    auto_root: &dyn Fn() -> Option<PathBuf>,
) -> Result<Config, ConfigError> {
    let text = fs::read_to_string(path)
        .map_err(|e| ConfigError::Parse(format!("cannot read {}: {e}", path.display())))?;
    let mut config = load_str(&text)?;
    let base = path.parent().unwrap_or(Path::new("."));
    resolve_paths(&mut config, base);
    if needs_auto_dirs(&config) {
        let Some(root) = auto_root() else {
            let field = if is_auto(&config.capture.buffer_dir) {
                "capture.buffer_dir"
            } else {
                "clips.out_dir"
            };
            return Err(ConfigError::Invalid(vec![format!(
                "{field} = auto: no fixed local drive found; set a folder"
            )]));
        };
        resolve_auto_dirs(&mut config, &root);
    }
    let errs = check_dirs(&config);
    if errs.is_empty() {
        Ok(config)
    } else {
        Err(ConfigError::Invalid(errs))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses() {
        let c = load_str("").unwrap();
        assert_eq!(c.capture.fps, Auto::Auto);
        assert_eq!(c.capture.output, Auto::Auto);
        assert_eq!(c.capture.encoder, EncoderSetting::Auto);
        assert_eq!(c.capture.buffer_dir, Path::new(AUTO_DIR));
        assert_eq!(c.clips.out_dir, Path::new(AUTO_DIR));
        assert_eq!(c.clips.pre_roll, 15);
    }

    #[test]
    fn user_value_overrides_default() {
        let c = load_str("[capture]\nfps = 60").unwrap();
        assert_eq!(c.capture.fps, Auto::Value(60));
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
        let e = load_str("[capture]\nfps = \"fast\"")
            .unwrap_err()
            .to_string();
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

    fn errs(toml: &str) -> Vec<String> {
        match load_str(toml) {
            Err(ConfigError::Invalid(v)) => v,
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn fps_must_be_allowed() {
        assert_eq!(
            errs("[capture]\nfps = 100"),
            ["capture.fps = 100: must be one of 60, 120, 144, 165, 240"]
        );
    }

    #[test]
    fn every_allowed_fps_is_accepted() {
        for fps in [60, 120, 144, 165, 240] {
            assert!(
                load_str(&format!("[capture]\nfps = {fps}")).is_ok(),
                "{fps}"
            );
        }
    }

    #[test]
    fn qp_range() {
        assert_eq!(
            errs("[capture]\nqp_i = 60\nqp_p = 60"),
            [
                "capture.qp_i = 60: must be 0–51",
                "capture.qp_p = 60: must be 0–51"
            ]
        );
    }

    #[test]
    fn qp_order() {
        assert_eq!(
            errs("[capture]\nqp_i = 20\nqp_p = 18"),
            ["capture.qp_i = 20: must be ≤ capture.qp_p (18)"]
        );
    }

    #[test]
    fn buffer_long_enough() {
        assert_eq!(
            errs("[capture]\nbuffer_secs = 21"),
            ["capture.buffer_secs = 21: must be ≥ pre_roll + post_roll + 4 (22)"]
        );
        assert!(load_str("[capture]\nbuffer_secs = 22").is_ok());
    }

    #[test]
    fn buffer_dir_must_not_contain_percent() {
        assert_eq!(
            errs(
                "[capture]
buffer_dir = 'D:/100%/buf'"
            ),
            ["capture.buffer_dir = D:/100%/buf: must not contain '%'"]
        );
    }

    #[test]
    fn huge_roll_does_not_overflow() {
        let e = errs("[clips]\npre_roll = 4294967295");
        assert_eq!(
            e,
            ["capture.buffer_secs = 60: must be ≥ pre_roll + post_roll + 4 (4294967302)"]
        );
    }

    #[test]
    fn fps_message_lists_allowed_values() {
        let list = ALLOWED_FPS.map(|f| f.to_string()).join(", ");
        assert_eq!(
            errs("[capture]\nfps = 1"),
            [format!("capture.fps = 1: must be one of {list}")]
        );
    }

    #[test]
    fn every_relative_path_is_resolved() {
        let mut c = load_str("").unwrap();
        resolve_paths(&mut c, Path::new(r"C:\base"));
        assert_eq!(c.grade.lut, Path::new(r"C:\base\luts/siege.cube"));
        assert_eq!(c.autoclip.template, Path::new(r"C:\base\name.png"));
    }

    #[test]
    fn hotkeys_must_parse_and_differ() {
        assert_eq!(
            errs(
                "[clips]
hotkey = \"Ctrl+Q+\""
            ),
            ["clips.hotkey = Ctrl+Q+: \"Q\" is not a modifier (Ctrl, Alt, Shift, Win)"]
        );
        assert_eq!(
            errs(
                "[clips]
hotkey_full = \"F99\""
            ),
            ["clips.hotkey_full = F99: unknown key \"F99\""]
        );
        assert_eq!(
            errs(
                "[clips]
hotkey = \"ctrl+f9\""
            ),
            ["clips.hotkey_full = Ctrl+F9: must differ from clips.hotkey"]
        );
        assert!(
            load_str(
                "[clips]
hotkey = \"Alt+Shift+K\""
            )
            .is_ok()
        );
    }

    #[test]
    fn auto_values_skip_the_value_rules() {
        assert!(load_str("[capture]\nfps = \"auto\"\noutput = \"auto\"").is_ok());
        assert!(load_str("[capture]\nbuffer_dir = \"auto\"").is_ok());
        assert_eq!(
            load_str("[capture]\noutput = 2").unwrap().capture.output,
            Auto::Value(2)
        );
    }

    #[test]
    fn encoder_must_be_known() {
        let c = load_str("[capture]\nencoder = \"qsv\"").unwrap();
        assert_eq!(c.capture.encoder, EncoderSetting::Qsv);
        let e = load_str("[capture]\nencoder = \"x264\"")
            .unwrap_err()
            .to_string();
        assert!(e.contains("capture.encoder"), "{e}");
    }

    #[test]
    fn auto_dirs_wait_for_a_drive() {
        let mut c = load_str("").unwrap();
        resolve_paths(&mut c, Path::new(r"C:\base"));
        assert!(needs_auto_dirs(&c));
        assert_eq!(c.capture.buffer_dir, Path::new(AUTO_DIR));
        resolve_auto_dirs(&mut c, Path::new("D:\\"));
        assert!(!needs_auto_dirs(&c));
        assert_eq!(c.capture.buffer_dir, Path::new(r"D:\Retake\buffer"));
        assert_eq!(c.clips.out_dir, Path::new(r"D:\Retake\clips"));
    }

    #[test]
    fn codec_must_be_known() {
        let e = load_str("[capture]\ncodec = \"h264\"")
            .unwrap_err()
            .to_string();
        assert!(e.contains("capture.codec"), "{e}");
    }

    #[test]
    fn dirs_created_and_writable() {
        let t = tempfile::tempdir().unwrap();
        let mut c = load_str("").unwrap();
        resolve_paths(&mut c, t.path());
        resolve_auto_dirs(&mut c, t.path());
        assert!(check_dirs(&c).is_empty());
        let base = t.path().join("Retake");
        assert!(base.join("buffer").is_dir());
        assert!(base.join("clips").is_dir());
        assert!(!base.join("buffer").join(WRITE_TEST_FILE).exists());
    }

    #[test]
    fn absolute_paths_are_kept() {
        let t = tempfile::tempdir().unwrap();
        let abs = t.path().join("elsewhere");
        let toml = format!("[clips]\nout_dir = '{}'", abs.display());
        let mut c = load_str(&toml).unwrap();
        resolve_paths(&mut c, Path::new("C:/unused"));
        assert_eq!(c.clips.out_dir, abs);
    }

    #[test]
    fn bad_dir_reported() {
        let t = tempfile::tempdir().unwrap();
        let f = t.path().join("file");
        std::fs::write(&f, "").unwrap();
        let mut c = load_str("").unwrap();
        resolve_paths(&mut c, t.path());
        resolve_auto_dirs(&mut c, t.path());
        c.capture.buffer_dir = f.join("sub");
        let errs = check_dirs(&c);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].starts_with("capture.buffer_dir = "), "{}", errs[0]);
    }

    #[test]
    fn load_file_writes_nothing_but_checks_dirs() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.toml");
        std::fs::write(&p, "[capture]\nfps = 60").unwrap();
        let root = t.path().to_path_buf();
        let c = load_file(&p, &|| Some(root.clone())).unwrap();
        assert_eq!(c.capture.fps, Auto::Value(60));
        assert_eq!(c.capture.buffer_dir, t.path().join("Retake").join("buffer"));
    }

    #[test]
    fn load_file_asks_for_a_drive_only_when_needed() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("config.toml");
        std::fs::write(
            &p,
            "[capture]\nbuffer_dir = 'buffer'\n[clips]\nout_dir = 'clips'",
        )
        .unwrap();
        let c = load_file(&p, &|| panic!("no drive needed")).unwrap();
        assert_eq!(c.capture.buffer_dir, t.path().join("buffer"));
        std::fs::write(&p, "").unwrap();
        match load_file(&p, &|| None) {
            Err(ConfigError::Invalid(e)) => assert_eq!(
                e,
                ["capture.buffer_dir = auto: no fixed local drive found; set a folder"]
            ),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
