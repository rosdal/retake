//! The montage render as one ffmpeg command, built from an arrangement: each piece of each
//! clip's time map is trimmed and retimed, slow motion uses the recording's own high-fps frames
//! (interpolated only where there aren't enough), fast parts get motion blur, kills get a
//! shake and a screen pump, the frame is cropped for the format, the punchy look applied, and
//! the song mixed with the game audio. Pure: the arguments are pinned by tests.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::autoconf::Encoder;
use crate::montage::{Format, MAX_SPEED, Placed};

/// Output frame rate.
pub const FPS: u32 = 60;

#[derive(Debug, Clone)]
pub struct RenderOpts {
    pub format: Format,
    /// The rough preview: 540-px wide, no blur, interpolation or effects.
    pub preview: bool,
    pub encoder: Encoder,
    /// A look LUT for the whole montage.
    pub look: Option<PathBuf>,
    /// Game audio level under the song, dB (e.g. −10).
    pub game_db: f64,
    /// Kill-feed overlay for vertical montages (v1.4): crop `(w, h, x, y)` of the source.
    pub killfeed: Option<(u32, u32, u32, u32)>,
}

/// Facts about a source clip.
#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

fn esc(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/").replace(':', "\\:")
}

/// The ffmpeg arguments that render `placed` with `song` (from `song_start`, `length` s) to
/// `out`. `sources[i]` describes `placed`'s distinct files in first-use order.
pub fn render_args(
    placed: &[Placed],
    sources: &[(PathBuf, Source)],
    song: &Path,
    song_start: f64,
    length: f64,
    o: &RenderOpts,
    out: &Path,
) -> Vec<String> {
    let (w, h) = if o.preview {
        match o.format {
            Format::Tiktok => (540, 960),
            Format::Youtube => (960, 540),
        }
    } else {
        o.format.size()
    };
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "warning", "-nostdin", "-y"]
        .map(String::from)
        .to_vec();
    for (p, _) in sources {
        a.extend(["-i".into(), p.to_string_lossy().into_owned()]);
    }
    let song_idx = sources.len();
    a.extend([
        "-ss".into(),
        format!("{song_start:.3}"),
        "-t".into(),
        format!("{length:.3}"),
        "-i".into(),
        song.to_string_lossy().into_owned(),
    ]);
    let input_of = |f: &Path| sources.iter().position(|(p, _)| p == f).unwrap_or(0);
    let mut g = String::new();
    let mut vlabels = Vec::new();
    let mut alabels = Vec::new();
    let mut n = 0usize;
    for x in placed {
        let i = input_of(&x.file);
        let src = sources.get(i).map(|s| s.1).unwrap_or(Source {
            fps: 60.0,
            width: 1920,
            height: 1080,
            has_audio: false,
        });
        for s in &x.segs {
            let v = s.speed();
            let dur = s.out1 - s.out0;
            let mut chain = format!(
                "[{i}:v:0]trim=start={:.4}:end={:.4},setpts=(PTS-STARTPTS)/{v:.5}",
                s.src0, s.src1
            );
            // Crop to the format's aspect first: everything after works on fewer pixels.
            if o.format == Format::Tiktok {
                chain.push_str(",crop=ih*9/16:ih");
            }
            // Frames the source has per output second at this speed.
            let have = src.fps * v;
            if !o.preview && have < f64::from(FPS) * 0.9 {
                let _ = write!(
                    chain,
                    ",minterpolate=fps={FPS}:mi_mode=mci:mc_mode=obmc:me_mode=bidir"
                );
            } else if !o.preview && v > 1.2 {
                // Blend the frames a fast part skips, then pick the output rate.
                let blend = ((src.fps * v / f64::from(FPS)).round() as usize).clamp(2, 8);
                let _ = write!(chain, ",tmix=frames={blend}");
            }
            let _ = write!(chain, ",fps={FPS}");
            let _ = write!(chain, ",scale={w}:{h}:flags=lanczos,setsar=1");
            if !o.preview {
                // A shake and a pump on each kill inside this piece (t is piece-local).
                // Inclusive: a kill on a cut gets the effect on both sides of it.
                for &(_, at) in x
                    .kills
                    .iter()
                    .filter(|(_, at)| *at >= s.out0 - 1e-9 && *at <= s.out1 + 1e-9)
                {
                    let t = at - s.out0;
                    let _ = write!(
                        chain,
                        ",scale=w='iw*(1+0.07*exp(-pow((t-{t:.3})*9,2)))':h=-2:eval=frame,crop={w}:{h}:'(iw-{w})/2+10*sin((t-{t:.3})*90)*exp(-abs(t-{t:.3})*14)':'(ih-{h})/2+8*cos((t-{t:.3})*80)*exp(-abs(t-{t:.3})*14)'"
                    );
                }
            }
            // The pump's rescale can leave a non-square pixel aspect; concat needs them equal.
            let _ = write!(
                chain,
                ",setsar=1,trim=duration={dur:.4},setpts=PTS-STARTPTS[v{n}];"
            );
            g.push_str(&chain);
            vlabels.push(format!("[v{n}]"));
            // Game audio: retimed where the speed is listenable (0.5–2×), silent otherwise.
            if src.has_audio && (0.5..=MAX_SPEED).contains(&v) {
                let _ = write!(
                    g,
                    "[{i}:a:0]atrim=start={:.4}:end={:.4},asetpts=PTS-STARTPTS,atempo={v:.5},apad,atrim=duration={dur:.4}[a{n}];",
                    s.src0, s.src1
                );
            } else {
                let _ = write!(g, "aevalsrc=0:d={dur:.4}:s=48000:c=stereo[a{n}];");
            }
            alabels.push(format!("[a{n}]"));
            n += 1;
        }
    }
    let _ = write!(g, "{}concat=n={n}:v=1:a=0[vcat];", vlabels.join(""));
    let mut vout = "[vcat]".to_owned();
    if let Some(lut) = &o.look {
        let _ = write!(
            g,
            "[vcat]lut3d=file='{}':interp=tetrahedral[vlook];",
            esc(lut)
        );
        vout = "[vlook]".into();
    }
    let _ = write!(g, "{vout}format=yuv420p[vout];");
    let _ = write!(
        g,
        "{}concat=n={n}:v=0:a=1,volume={:.1}dB[game];",
        alabels.join(""),
        o.game_db
    );
    let _ = write!(
        g,
        "[{song_idx}:a:0]aresample=48000,afade=t=in:d=0.3,afade=t=out:st={:.3}:d=0.8[song];[song][game]amix=inputs=2:normalize=0:duration=first[aout]",
        (length - 0.8).max(0.0)
    );
    a.extend([
        "-filter_complex".into(),
        g,
        "-map".into(),
        "[vout]".into(),
        "-map".into(),
        "[aout]".into(),
    ]);
    let enc = match o.encoder {
        Encoder::Amf => [
            "-c:v", "h264_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", "16M",
            "-maxrate", "24M",
        ],
        Encoder::Nvenc => [
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p5",
            "-rc",
            "vbr",
            "-b:v",
            "16M",
            "-maxrate",
            "24M",
        ],
        Encoder::Qsv => [
            "-c:v",
            "h264_qsv",
            "-preset",
            "medium",
            "-b:v",
            "16M",
            "-maxrate",
            "24M",
            "-look_ahead",
            "0",
        ],
    };
    a.extend(enc.map(String::from));
    a.extend(["-c:a", "aac", "-b:a", "256k", "-r"].map(String::from));
    a.push(FPS.to_string());
    a.extend([
        "-t".into(),
        format!("{length:.3}"),
        "-movflags".into(),
        "+faststart".into(),
    ]);
    a.push(out.to_string_lossy().into_owned());
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::montage::{Role, Seg};

    fn placed() -> Vec<Placed> {
        vec![
            Placed {
                file: PathBuf::from(r"D:\c\a.mkv"),
                role: Role::Hook,
                segs: vec![Seg {
                    out0: 0.0,
                    out1: 0.9375,
                    src0: 14.6,
                    src1: 15.2,
                }],
                kills: vec![(15.2, 0.9375)],
            },
            Placed {
                file: PathBuf::from(r"D:\c\ace.mkv"),
                role: Role::Drop,
                segs: vec![
                    Seg {
                        out0: 0.9375,
                        out1: 2.8125,
                        src0: 8.0,
                        src1: 10.0,
                    },
                    Seg {
                        out0: 2.8125,
                        out1: 3.28125,
                        src0: 10.0,
                        src1: 10.164,
                    },
                ],
                kills: vec![(10.0, 2.8125)],
            },
        ]
    }

    fn sources(fps: f64) -> Vec<(PathBuf, Source)> {
        ["a", "ace"]
            .map(|n| {
                (
                    PathBuf::from(format!(r"D:\c\{n}.mkv")),
                    Source {
                        fps,
                        width: 2560,
                        height: 1440,
                        has_audio: true,
                    },
                )
            })
            .to_vec()
    }

    fn opts(preview: bool) -> RenderOpts {
        RenderOpts {
            format: Format::Tiktok,
            preview,
            encoder: Encoder::Amf,
            look: Some(PathBuf::from(r"D:\c\look.cube")),
            game_db: -10.0,
            killfeed: None,
        }
    }

    fn graph(a: &[String]) -> &str {
        &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1]
    }

    #[test]
    fn pieces_are_trimmed_retimed_cropped_and_joined() {
        let a = render_args(
            &placed(),
            &sources(240.0),
            Path::new(r"D:\s\song.mp3"),
            30.0,
            25.0,
            &opts(false),
            Path::new("o.mp4"),
        );
        let g = graph(&a);
        assert!(
            g.contains("[0:v:0]trim=start=14.6000:end=15.2000,setpts=(PTS-STARTPTS)/0.64000"),
            "{g}"
        );
        assert!(g.contains("setpts=(PTS-STARTPTS)/0.64000,crop=ih*9/16:ih"));
        assert!(g.contains("scale=1080:1920"));
        assert!(g.contains("concat=n=3:v=1:a=0[vcat]"));
        assert!(g.contains("lut3d=file='D\\:/c/look.cube'"));
        // 240 fps at 0.35× still has 686 frames/s: no interpolation needed.
        assert!(!g.contains("minterpolate"));
        // The kills get a shake and pump.
        assert!(g.matches("eval=frame").count() >= 2);
        // The deep slow-mo piece has silent game audio; the others are retimed.
        assert!(g.contains("aevalsrc=0:d=0.4688"));
        assert!(g.contains("atempo=0.64000"));
        assert!(a.windows(2).any(|w| w == ["-ss", "30.000"]));
    }

    #[test]
    fn low_fps_sources_are_interpolated_and_previews_stay_plain() {
        let a = render_args(
            &placed(),
            &sources(60.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &opts(false),
            Path::new("o.mp4"),
        );
        assert!(graph(&a).contains("minterpolate=fps=60"));
        let p = render_args(
            &placed(),
            &sources(60.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &opts(true),
            Path::new("o.mp4"),
        );
        let g = graph(&p);
        assert!(g.contains("scale=540:960"));
        assert!(!g.contains("minterpolate") && !g.contains("eval=frame") && !g.contains("tmix"));
    }
}
