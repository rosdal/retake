//! The Settings screen's model: grouped fields over config.toml (edited with toml_edit, so
//! comments and order survive), a search, and per-game profiles. Saving writes the file and
//! the service restarts itself with it (after the match if a game is running).

use std::path::Path;

use retake_core::config;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Toggle,
    /// Choices as written in TOML (strings quoted).
    Choice(&'static [&'static str]),
    Number,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field {
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub section: &'static str,
    pub key: &'static str,
    pub kind: Kind,
}

const fn f(
    group: &'static str,
    label: &'static str,
    help: &'static str,
    section: &'static str,
    key: &'static str,
    kind: Kind,
) -> Field {
    Field {
        group,
        label,
        help,
        section,
        key,
        kind,
    }
}

/// Strict safety comes first, as the roadmap asks.
pub const FIELDS: &[Field] = &[
    f(
        "Safety",
        "Strict safety",
        "Device audio only and never read a game's files. On by default for Vanguard games.",
        "safety",
        "strict",
        Kind::Toggle,
    ),
    f(
        "Clips",
        "Clip hotkey",
        "Saves the last seconds as a clip.",
        "clips",
        "hotkey",
        Kind::Text,
    ),
    f(
        "Clips",
        "Whole-buffer hotkey",
        "Saves everything in the buffer.",
        "clips",
        "hotkey_full",
        Kind::Text,
    ),
    f(
        "Clips",
        "Star hotkey",
        "Stars the newest clip for montages.",
        "clips",
        "hotkey_star",
        Kind::Text,
    ),
    f(
        "Clips",
        "Seconds before the hotkey",
        "",
        "clips",
        "pre_roll",
        Kind::Number,
    ),
    f(
        "Clips",
        "Seconds after the hotkey",
        "",
        "clips",
        "post_roll",
        Kind::Number,
    ),
    f(
        "Clips",
        "Sound when a clip is saved",
        "",
        "clips",
        "sound_cue",
        Kind::Toggle,
    ),
    f(
        "Auto-clip",
        "Automatic clips",
        "Clip your kills after each match.",
        "autoclip",
        "enabled",
        Kind::Toggle,
    ),
    f(
        "Auto-clip",
        "Which plays",
        "Every kill, or only multi-kills.",
        "autoclip",
        "clip",
        Kind::Choice(&["\"every\"", "\"2k\"", "\"3k\"", "\"aces\"", "\"off\""]),
    ),
    f(
        "Recording",
        "Capture fps",
        "",
        "capture",
        "fps",
        Kind::Choice(&["\"auto\"", "60", "120", "144", "165", "240"]),
    ),
    f(
        "Recording",
        "Encoder",
        "x264 is for testing on PCs without a hardware encoder.",
        "capture",
        "encoder",
        Kind::Choice(&["\"auto\"", "\"amf\"", "\"nvenc\"", "\"qsv\"", "\"x264\""]),
    ),
    f(
        "Recording",
        "Encoder preset",
        "",
        "capture",
        "preset",
        Kind::Choice(&["\"speed\"", "\"balanced\"", "\"quality\""]),
    ),
    f(
        "Recording",
        "Record any fullscreen app",
        "Treat fullscreen apps (not browsers) as games.",
        "game",
        "any_fullscreen",
        Kind::Toggle,
    ),
    f(
        "Recording",
        "Built-in game list",
        "Record known games with no setup.",
        "game",
        "builtin",
        Kind::Toggle,
    ),
    f("Audio", "Game audio", "", "audio", "game", Kind::Toggle),
    f("Audio", "Microphone", "", "audio", "mic", Kind::Toggle),
    f(
        "Audio",
        "Discord as a third track",
        "",
        "audio",
        "comms",
        Kind::Toggle,
    ),
    f(
        "Grading",
        "Grade clips",
        "",
        "grade",
        "mode",
        Kind::Choice(&["\"on\"", "\"off\""]),
    ),
    f(
        "Grading",
        "Look strength",
        "0 to 1.",
        "grade",
        "look_strength",
        Kind::Number,
    ),
    f(
        "Grading",
        "Motion blur on exports",
        "",
        "motion_blur",
        "enabled",
        Kind::Toggle,
    ),
];

/// A field's current value as shown (TOML text, strings unquoted).
pub fn value(
    doc: &toml_edit::DocumentMut,
    defaults: &toml_edit::DocumentMut,
    fl: &Field,
) -> String {
    let get = |d: &toml_edit::DocumentMut| {
        d.get(fl.section)?
            .get(fl.key)
            .and_then(|v| v.as_value())
            .map(|v| v.to_string().trim().to_owned())
    };
    get(doc)
        .or_else(|| get(defaults))
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

/// Sets a field from its shown value.
pub fn set(doc: &mut toml_edit::DocumentMut, fl: &Field, shown: &str) -> Result<(), String> {
    let raw = match fl.kind {
        Kind::Toggle | Kind::Number => shown.trim().to_owned(),
        Kind::Text => format!("\"{}\"", shown.trim().replace('"', "")),
        Kind::Choice(values) => values
            .iter()
            .find(|v| v.trim_matches('"') == shown)
            .map_or_else(|| shown.to_owned(), |v| (*v).to_owned()),
    };
    let v: toml_edit::Value = raw
        .parse()
        .map_err(|_| format!("{}: \"{shown}\" isn't a valid value", fl.label))?;
    if doc.get(fl.section).is_none() {
        doc[fl.section] = toml_edit::table();
    }
    doc[fl.section][fl.key] = toml_edit::value(v);
    Ok(())
}

/// Per-game switches: automatic clips and strict safety (`[games.<id>]`).
pub fn set_game(doc: &mut toml_edit::DocumentMut, id: &str, key: &str, on: bool) {
    game_table(doc, id)[key] = toml_edit::value(on);
}

/// `[games.<id>]` as a real table (created if missing).
fn game_table<'a>(doc: &'a mut toml_edit::DocumentMut, id: &str) -> &'a mut toml_edit::Item {
    if doc.get("games").is_none() {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc["games"] = toml_edit::Item::Table(t);
    }
    if doc["games"].get(id).is_none() {
        doc["games"][id] = toml_edit::table();
    }
    &mut doc["games"][id]
}

/// Adds a game by exe name (`[games.<id>] name, exes`).
pub fn add_game(doc: &mut toml_edit::DocumentMut, exe: &str) -> Result<String, String> {
    let exe = exe.trim();
    if exe.is_empty() || !exe.to_ascii_lowercase().ends_with(".exe") {
        return Err("Type the game's exe name, e.g. MyGame.exe".into());
    }
    let id = retake_core::games::id_for_exe(exe);
    let name = exe.rsplit_once('.').map_or(exe, |(s, _)| s).to_owned();
    let mut exes = toml_edit::Array::new();
    exes.push(exe);
    let g = game_table(doc, &id);
    g["name"] = toml_edit::value(name.clone());
    g["exes"] = toml_edit::value(exes);
    Ok(name)
}

/// Validates and writes the document.
pub fn save(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), String> {
    let text = doc.to_string();
    config::load_str(&text).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

pub fn defaults() -> toml_edit::DocumentMut {
    config::DEFAULT_CONFIG_TOML
        .parse()
        .expect("the embedded default config parses")
}

/// Fields matching a search (label, help, group or key).
pub fn matches(fl: &Field, query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty()
        || [fl.label, fl.help, fl.group, fl.key]
            .iter()
            .any(|s| s.to_lowercase().contains(&q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_field_exists_in_the_default_config() {
        let d = defaults();
        for fl in FIELDS {
            assert!(
                d.get(fl.section).and_then(|s| s.get(fl.key)).is_some(),
                "{}.{}",
                fl.section,
                fl.key
            );
        }
        assert_eq!(FIELDS[0].key, "strict", "strict safety first");
    }

    #[test]
    fn edits_round_trip_and_validate() {
        let defaults = defaults();
        let mut doc: toml_edit::DocumentMut = "# mine\n[clips]\npre_roll = 20\n".parse().unwrap();
        let fps = FIELDS.iter().find(|f| f.key == "fps").unwrap();
        set(&mut doc, fps, "144").unwrap();
        assert_eq!(value(&doc, &defaults, fps), "144");
        let enc = FIELDS.iter().find(|f| f.key == "encoder").unwrap();
        set(&mut doc, enc, "nvenc").unwrap();
        assert!(doc.to_string().contains("encoder = \"nvenc\""));
        assert!(doc.to_string().starts_with("# mine"), "comments survive");
        set_game(&mut doc, "valorant", "autoclip", true);
        assert_eq!(add_game(&mut doc, "My Game.exe").unwrap(), "My Game");
        let text = doc.to_string();
        config::load_str(&text).unwrap();
        assert!(text.contains("[games.valorant]") && text.contains("[games.my-game]"));
        assert!(add_game(&mut doc, "notepad").is_err());
        let hk = FIELDS.iter().find(|f| f.key == "hotkey").unwrap();
        assert_eq!(value(&doc, &defaults, hk), "F9");
        assert!(matches(fps, "FPS") && matches(fps, "recording") && !matches(fps, "audio"));
    }
}
