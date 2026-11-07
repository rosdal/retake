//! A DaVinci Resolve timeline of a montage, as FCPXML 1.9 (File → Import → Timeline): every
//! piece of every clip on the main track at its place, with its speed, and the song under it,
//! so the automatic montage can be polished by hand.

use std::fmt::Write as _;
use std::path::Path;

use crate::montage::{Format, Placed};

/// Frames per second of the timeline.
const FPS: u32 = 60;

fn frames(secs: f64) -> i64 {
    (secs * f64::from(FPS)).round() as i64
}

/// `n/60s` rational time.
fn t(f: i64) -> String {
    format!("{f}/{FPS}s")
}

fn url(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.replace(' ', "%20");
    format!("file:///{}", s.trim_start_matches('/'))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The FCPXML document. `durations[i]` is the length in seconds of `placed`'s i-th distinct
/// file (first-use order); the song starts at `song_start` and runs `length` seconds.
/// `song_marks` are markers on the song (bars, the drop) at montage times; every kill gets a
/// marker on its clip.
#[allow(clippy::too_many_arguments)]
pub fn timeline(
    name: &str,
    format: Format,
    placed: &[Placed],
    durations: &[(std::path::PathBuf, f64)],
    song: &Path,
    song_start: f64,
    length: f64,
    song_marks: &[(f64, String)],
) -> String {
    let (w, h) = format.size();
    let mut x = String::new();
    let _ = writeln!(x, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(x, r#"<!DOCTYPE fcpxml>"#);
    let _ = writeln!(x, r#"<fcpxml version="1.9">"#);
    let _ = writeln!(x, "  <resources>");
    let _ = writeln!(
        x,
        r#"    <format id="r0" name="FFVideoFormat{h}p{FPS}" frameDuration="1/{FPS}s" width="{w}" height="{h}"/>"#
    );
    for (i, (p, d)) in durations.iter().enumerate() {
        let _ = writeln!(
            x,
            r#"    <asset id="a{i}" name="{}" start="0s" duration="{}" hasVideo="1" hasAudio="1" format="r0"><media-rep kind="original-media" src="{}"/></asset>"#,
            xml_escape(&p.file_name().unwrap_or_default().to_string_lossy()),
            t(frames(*d)),
            xml_escape(&url(p))
        );
    }
    let song_id = durations.len();
    let _ = writeln!(
        x,
        r#"    <asset id="a{song_id}" name="{}" start="0s" duration="{}" hasAudio="1"><media-rep kind="original-media" src="{}"/></asset>"#,
        xml_escape(&song.file_name().unwrap_or_default().to_string_lossy()),
        t(frames(song_start + length + 1.0)),
        xml_escape(&url(song))
    );
    let _ = writeln!(x, "  </resources>");
    let _ = writeln!(
        x,
        "  <library><event name=\"Retake\"><project name=\"{}\">",
        xml_escape(name)
    );
    let _ = writeln!(
        x,
        r#"    <sequence format="r0" duration="{}" tcStart="0s">"#,
        t(frames(length))
    );
    let _ = writeln!(x, "      <spine>");
    for p in placed {
        let a = durations
            .iter()
            .position(|(f, _)| *f == p.file)
            .unwrap_or(0);
        for s in &p.segs {
            let (o0, o1) = (frames(s.out0), frames(s.out1));
            if o1 <= o0 {
                continue;
            }
            let src0 = frames(s.src0);
            let speed = s.speed();
            let _ = writeln!(
                x,
                r#"        <asset-clip ref="a{a}" offset="{}" start="{}" duration="{}" name="{:?} x{speed:.2}">"#,
                t(o0),
                t(src0),
                t(o1 - o0),
                p.role
            );
            if (speed - 1.0).abs() > 0.01 {
                // A constant retime: output 0 → source start, output end → source end.
                let _ = writeln!(
                    x,
                    r#"          <timeMap><timept time="{}" value="{}" interp="linear"/><timept time="{}" value="{}" interp="linear"/></timeMap>"#,
                    t(src0),
                    t(src0),
                    t(src0 + (o1 - o0)),
                    t(frames(s.src1))
                );
            }
            if p.segs.first().is_some_and(|f| f == s) && o0 == 0 {
                // The song rides on the first piece as a connected clip, on lane −1, with
                // its bar and drop markers (in the song's own time).
                let _ = writeln!(
                    x,
                    r#"          <asset-clip ref="a{song_id}" lane="-1" offset="{}" start="{}" duration="{}" name="Song">"#,
                    t(src0),
                    t(frames(song_start)),
                    t(frames(length))
                );
                for (at, label) in song_marks.iter().filter(|(a, _)| *a >= 0.0 && *a <= length) {
                    let _ = writeln!(
                        x,
                        r#"            <marker start="{}" duration="1/{FPS}s" value="{}"/>"#,
                        t(frames(song_start + at)),
                        xml_escape(label)
                    );
                }
                let _ = writeln!(x, "          </asset-clip>");
            }
            // Kills in this piece, at their source time (a kill on a cut belongs to the piece
            // it starts).
            let last = p.segs.last().is_some_and(|l| l == s);
            for &(src, out) in p.kills.iter().filter(|(_, out)| {
                *out >= s.out0 - 1e-9 && (*out < s.out1 - 1e-9 || last && *out <= s.out1 + 1e-9)
            }) {
                let _ = writeln!(
                    x,
                    r#"          <marker start="{}" duration="1/{FPS}s" value="Kill at {:.2} s"/>"#,
                    t(frames(src)),
                    out
                );
            }
            let _ = writeln!(x, "        </asset-clip>");
        }
    }
    let _ = writeln!(x, "      </spine>");
    let _ = writeln!(x, "    </sequence>");
    let _ = writeln!(x, "  </project></event></library>");
    let _ = writeln!(x, "</fcpxml>");
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::montage::{Role, Seg};
    use std::path::PathBuf;

    #[test]
    fn pieces_land_at_their_place_with_their_speed() {
        let placed = vec![Placed {
            file: PathBuf::from(r"D:\c\a b.mkv"),
            role: Role::Drop,
            segs: vec![
                Seg {
                    out0: 0.0,
                    out1: 1.0,
                    src0: 10.0,
                    src1: 11.0,
                },
                Seg {
                    out0: 1.0,
                    out1: 2.0,
                    src0: 11.0,
                    src1: 11.35,
                },
            ],
            kills: vec![(11.0, 1.0)],
        }];
        let x = timeline(
            "test",
            Format::Tiktok,
            &placed,
            &[(PathBuf::from(r"D:\c\a b.mkv"), 20.0)],
            Path::new(r"D:\s\song.mp3"),
            30.0,
            2.0,
            &[
                (0.0, "Bar 1".into()),
                (1.0, "Drop".into()),
                (9.0, "outside".into()),
            ],
        );
        assert!(x.contains(r#"<fcpxml version="1.9">"#));
        assert!(x.contains(r#"width="1080" height="1920""#));
        assert!(x.contains("file:///D:/c/a%20b.mkv"));
        assert!(x.contains(r#"offset="0/60s" start="600/60s" duration="60/60s""#));
        assert!(x.contains(r#"offset="60/60s" start="660/60s" duration="60/60s""#));
        // Only the slowed piece has a time map.
        assert_eq!(x.matches("<timeMap>").count(), 1);
        assert!(x.contains(r#"value="681/60s""#));
        assert!(x.contains(r#"name="Song""#) && x.contains(r#"start="1800/60s""#));
        // Markers: the song's bar and drop (not one outside the montage), and the kill.
        assert!(x.contains(r#"<marker start="1860/60s" duration="1/60s" value="Drop"/>"#));
        assert!(!x.contains("outside"));
        assert_eq!(x.matches("value=\"Kill at").count(), 1, "{x}");
        assert!(x.contains(r#"<marker start="660/60s""#));
        // Well-formed enough: every opened asset-clip closes.
        assert_eq!(
            x.matches("<asset-clip").count(),
            x.matches("</asset-clip>").count()
        );
    }
}
