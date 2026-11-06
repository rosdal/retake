//! Hotkey strings from `[clips]` (`F9`, `Ctrl+F9`, `Alt+Shift+K`) parsed into the modifier bits
//! and virtual-key code `RegisterHotKey` takes. Pure: the Win32 values are constants here.

pub const MOD_ALT: u32 = 0x1;
pub const MOD_CONTROL: u32 = 0x2;
pub const MOD_SHIFT: u32 = 0x4;
pub const MOD_WIN: u32 = 0x8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub modifiers: u32,
    pub vk: u32,
}

/// Parses `Mod+Mod+Key`, ignoring case and spaces around each part.
pub fn parse(s: &str) -> Result<Hotkey, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).collect();
    let (key, mods) = parts.split_last().expect("split yields at least one part");
    if mods.is_empty() && key.is_empty() {
        return Err("empty".into());
    }
    let mut modifiers = 0;
    for m in mods {
        let bit = modifier(m)
            .ok_or_else(|| format!("\"{m}\" is not a modifier (Ctrl, Alt, Shift, Win)"))?;
        if modifiers & bit != 0 {
            return Err(format!("\"{m}\" given twice"));
        }
        modifiers |= bit;
    }
    if key.is_empty() || modifier(key).is_some() {
        return Err("no key after the modifiers".into());
    }
    let vk = vk(key).ok_or_else(|| format!("unknown key \"{key}\""))?;
    Ok(Hotkey { modifiers, vk })
}

fn modifier(m: &str) -> Option<u32> {
    match m.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => Some(MOD_CONTROL),
        "alt" => Some(MOD_ALT),
        "shift" => Some(MOD_SHIFT),
        "win" => Some(MOD_WIN),
        _ => None,
    }
}

fn vk(key: &str) -> Option<u32> {
    let k = key.to_ascii_lowercase();
    if let [c] = k.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        return Some(u32::from(c.to_ascii_uppercase()));
    }
    let number = |prefix: &str| {
        k.strip_prefix(prefix)
            .filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| d.parse::<u32>().ok())
    };
    if let Some(n) = number("f") {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    if let Some(n) = number("numpad") {
        return (n <= 9).then_some(0x60 + n);
    }
    Some(match k.as_str() {
        "space" => 0x20,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "end" => 0x23,
        "home" => 0x24,
        "insert" => 0x2D,
        "delete" => 0x2E,
        "pause" => 0x13,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hk(modifiers: u32, vk: u32) -> Result<Hotkey, String> {
        Ok(Hotkey { modifiers, vk })
    }

    #[test]
    fn plain_and_modified_keys() {
        assert_eq!(parse("F9"), hk(0, 0x78));
        assert_eq!(parse("Ctrl+F9"), hk(MOD_CONTROL, 0x78));
        assert_eq!(parse("Alt+Shift+K"), hk(MOD_ALT | MOD_SHIFT, 0x4B));
        assert_eq!(parse("Win+Control+F24"), hk(MOD_WIN | MOD_CONTROL, 0x87));
    }

    #[test]
    fn case_and_spaces_ignored() {
        assert_eq!(parse(" ctrl + f9 "), parse("Ctrl+F9"));
        assert_eq!(parse("z"), hk(0, 0x5A));
    }

    #[test]
    fn named_keys() {
        for (s, vk) in [
            ("F1", 0x70),
            ("0", 0x30),
            ("9", 0x39),
            ("Space", 0x20),
            ("PageUp", 0x21),
            ("PageDown", 0x22),
            ("End", 0x23),
            ("Home", 0x24),
            ("Insert", 0x2D),
            ("Delete", 0x2E),
            ("Pause", 0x13),
            ("Numpad0", 0x60),
            ("Numpad9", 0x69),
        ] {
            assert_eq!(parse(s), hk(0, vk), "{s}");
        }
    }

    #[test]
    fn errors_say_what_is_wrong() {
        assert_eq!(parse(""), Err("empty".into()));
        assert_eq!(parse("F25"), Err("unknown key \"F25\"".into()));
        assert_eq!(parse("Numpad10"), Err("unknown key \"Numpad10\"".into()));
        assert_eq!(parse("Ctrl+"), Err("no key after the modifiers".into()));
        assert_eq!(parse("Ctrl"), Err("no key after the modifiers".into()));
        assert_eq!(parse("Ctrl+Ctrl+F9"), Err("\"Ctrl\" given twice".into()));
        assert_eq!(
            parse("F9+K"),
            Err("\"F9\" is not a modifier (Ctrl, Alt, Shift, Win)".into())
        );
    }
}
