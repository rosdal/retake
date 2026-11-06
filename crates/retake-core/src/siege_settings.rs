//! The recording-setup check: reads Siege's `GameSettings.ini` (only while Siege is closed) and
//! warns about settings that spoil footage for montage editing.
//!
//! `LensEffects` and `DOF` are the key names a Siege settings guide confirms
//! (steamcommunity.com/discussions/forum/29/3821910756550676421). `WindowMode=0` is exclusive
//! fullscreen, confirmed in Siege's menu. Motion blur and HDR key names aren't confirmed (a
//! real 2026 `GameSettings.ini` had neither), so every key mentioning `blur` or `hdr` is
//! reported as unconfirmed for a human to look at.

/// What the check found in one settings file.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SetupReport {
    /// One line per setting that should be off but isn't.
    pub warnings: Vec<String>,
    /// `(key, value)` of keys that look like motion blur or HDR but aren't confirmed names.
    pub unconfirmed: Vec<(String, String)>,
}

/// Keys that must be 0, with why.
const MUST_BE_OFF: [(&str, &str); 2] = [
    (
        "LensEffects",
        "lens effects are on: bloom and lens flare add glare that can't be graded out",
    ),
    (
        "DOF",
        "depth of field is on: it softens the image while aiming",
    ),
];

/// Keys with a value that spoils recording, with why.
const BAD_VALUE: [(&str, f64, &str); 1] = [(
    "WindowMode",
    0.0,
    "Siege runs in exclusive fullscreen: switch to borderless, which screen capture needs",
)];

/// Checks the text of a `GameSettings.ini`. Keys match in any section, ignoring case.
pub fn check(ini: &str) -> SetupReport {
    let mut report = SetupReport::default();
    for line in ini.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['[', ';', '#']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if let Some((_, why)) = MUST_BE_OFF
            .iter()
            .find(|(k, _)| key.eq_ignore_ascii_case(k))
        {
            if value.parse::<f64>().is_ok_and(|v| v != 0.0) {
                report
                    .warnings
                    .push(format!("Siege: {why} ({key}={value})"));
            }
            continue;
        }
        if let Some((_, bad, why)) = BAD_VALUE
            .iter()
            .find(|(k, _, _)| key.eq_ignore_ascii_case(k))
        {
            if value.parse::<f64>().is_ok_and(|v| v == *bad) {
                report
                    .warnings
                    .push(format!("Siege: {why} ({key}={value})"));
            }
            continue;
        }
        let lower = key.to_ascii_lowercase();
        if lower.contains("blur") || lower.contains("hdr") {
            report.unconfirmed.push((key.to_owned(), value.to_owned()));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warns_about_lens_effects_and_depth_of_field() {
        let r = check("[CUSTOM_QUALITY]\nLensEffects=1\nDOF=2\nShadow=3\n");
        assert_eq!(r.warnings.len(), 2);
        assert!(
            r.warnings[0].contains("lens effects") && r.warnings[0].ends_with("(LensEffects=1)")
        );
        assert!(r.warnings[1].contains("depth of field"));
    }

    #[test]
    fn off_settings_give_no_warning() {
        assert_eq!(check("LensEffects=0\nDOF = 0\n"), SetupReport::default());
    }

    #[test]
    fn keys_ignore_case_and_section() {
        let r = check("[DISPLAY]\nlenseffects = 2\n");
        assert_eq!(r.warnings.len(), 1);
    }

    #[test]
    fn blur_and_hdr_keys_are_reported_unconfirmed() {
        let r = check("MotionBlurQuality=1\nEnableHDR=0\nBrightness=55\n");
        assert_eq!(
            r.unconfirmed,
            [
                ("MotionBlurQuality".to_owned(), "1".to_owned()),
                ("EnableHDR".to_owned(), "0".to_owned())
            ]
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn warns_about_exclusive_fullscreen_only() {
        let r = check("[DISPLAY_SETTINGS]\nWindowMode=0\n");
        assert_eq!(r.warnings.len(), 1);
        assert!(
            r.warnings[0].contains("exclusive fullscreen"),
            "{:?}",
            r.warnings
        );
        assert!(check("WindowMode=2\n").warnings.is_empty());
    }

    #[test]
    fn garbage_is_ignored() {
        assert_eq!(
            check("; comment\nno equals sign\n=\nLensEffects=on\n"),
            SetupReport::default()
        );
    }
}
