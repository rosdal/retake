//! Trimming a clip from the window: the kept range is stream-copied into a new clip next to
//! it (`<stem>_trim.mkv`), with its sidecar's kills moved to the new start. The original
//! stays untouched.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::child::ChildJob;

/// The trimmed clip's sidecar: the original's, with kills inside the range moved by `from`.
pub fn shifted_sidecar(
    original: Option<serde_json::Value>,
    from: f64,
    to: f64,
) -> serde_json::Value {
    let mut v = original.unwrap_or_else(|| serde_json::json!({}));
    if let Some(kills) = v
        .pointer_mut("/autoclip/kills")
        .and_then(|k| k.as_array_mut())
    {
        kills.retain(|k| {
            k.get("at")
                .and_then(serde_json::Value::as_f64)
                .is_some_and(|t| t >= from && t <= to)
        });
        for k in kills.iter_mut() {
            if let Some(t) = k.get("at").and_then(serde_json::Value::as_f64) {
                k["at"] = serde_json::json!(t - from);
            }
        }
    }
    v
}

/// Cuts `from..to` (seconds) of `clip` into a new clip and returns it.
pub fn trim(ffmpeg: &Path, clip: &Path, from: f64, to: f64) -> Result<PathBuf> {
    if to - from < 0.5 {
        bail!("the trimmed clip would be shorter than half a second");
    }
    let stem = clip
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut out = clip.with_file_name(format!("{stem}_trim.mkv"));
    let mut n = 2;
    while out.exists() {
        out = clip.with_file_name(format!("{stem}_trim{n}.mkv"));
        n += 1;
    }
    let part = out.with_extension("mkv.part");
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-y".into(),
        "-ss".into(),
        format!("{from:.3}"),
        "-to".into(),
        format!("{to:.3}"),
        "-i".into(),
        clip.to_string_lossy().into_owned(),
        "-map".into(),
        "0".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "matroska".into(),
        part.to_string_lossy().into_owned(),
    ];
    let o = ChildJob::new()?.run_output(ffmpeg, &args)?;
    if !o.status.success() {
        let _ = std::fs::remove_file(&part);
        bail!("ffmpeg exited with {}", o.status);
    }
    std::fs::rename(&part, &out)?;
    let original = std::fs::read_to_string(clip.with_extension("json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let side = shifted_sidecar(original, from, to);
    if side.as_object().is_some_and(|o| !o.is_empty()) {
        std::fs::write(
            out.with_extension("json"),
            serde_json::to_string_pretty(&side)?,
        )?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kills_outside_the_range_go_and_the_rest_move() {
        let v = serde_json::json!({"game": "siege", "autoclip": {"play": "3k", "kills": [{"at": 5.0}, {"at": 12.0}, {"at": 19.0}]}});
        let s = shifted_sidecar(Some(v), 10.0, 18.0);
        assert_eq!(s["game"], "siege");
        assert_eq!(s["autoclip"]["kills"], serde_json::json!([{"at": 2.0}]));
        assert_eq!(shifted_sidecar(None, 0.0, 1.0), serde_json::json!({}));
    }
}
