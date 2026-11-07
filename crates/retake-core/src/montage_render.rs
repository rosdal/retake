//! The montage render as one ffmpeg command, built from an arrangement: each piece of each
//! clip's time map is trimmed and retimed, slow motion uses the recording's own high-fps frames
//! (interpolated only where there aren't enough), fast parts get motion blur, kills get a
//! shake and a screen pump, the frame is cropped for the format, the punchy look applied, and
//! the song mixed with the game audio. Pure: the arguments are pinned by tests.

/// Seconds the kill-feed overlay stays up after a kill.
const FEED_HOLD: f64 = 2.5;

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::autoconf::Encoder;
use crate::montage::{Format, Interpolation, MAX_SPEED, Placed, Role, Style, Transitions};

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
    /// Per-clip looks (a mixed montage: each clip's game look), applied to the clip before
    /// the montage's look.
    pub clip_looks: Vec<(PathBuf, PathBuf)>,
    /// Game audio level under the song, dB (e.g. −10).
    pub game_db: f64,
    /// Vertical montages: show the source's kill feed (which the 9:16 crop cuts off) near
    /// the top of the frame.
    pub killfeed: bool,
    /// Speeds, blur, shakes and transitions.
    pub style: Style,
}

/// How long a zoom whip lasts on each side of the cut into the drop.
const WHIP: f64 = 0.12;
/// How much a zoom whip zooms in.
const WHIP_ZOOM: f64 = 0.35;
/// How long a flash cut lasts.
const FLASH: f64 = 0.07;
/// Fade at both ends of every game-audio piece, so cuts don't click.
const XFADE: f64 = 0.008;

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
    let style = o.style.clamped();
    for (pi, x) in placed.iter().enumerate() {
        let i = input_of(&x.file);
        // The zoom whip runs from the end of the clip before the drop into the drop.
        let whip = style.transitions != Transitions::Cuts;
        let whip_out = whip && placed.get(pi + 1).is_some_and(|nx| nx.role == Role::Drop);
        let whip_in = whip && x.role == Role::Drop && pi > 0;
        let flash = style.transitions == Transitions::Flashy && pi > 0;
        let src = sources.get(i).map(|s| s.1).unwrap_or(Source {
            fps: 60.0,
            width: 1920,
            height: 1080,
            has_audio: false,
        });
        let last_seg = x.segs.len().saturating_sub(1);
        for (si, s) in x.segs.iter().enumerate() {
            let v = s.speed();
            let dur = s.out1 - s.out0;
            // The feed region is see-through when empty, so it shows only while a kill's row
            // is up: from just before each kill to 2.5 s after (piece-local times).
            let shows: Vec<String> = x
                .kills
                .iter()
                .map(|(_, at)| (at - s.out0 - 0.15, at - s.out0 + FEED_HOLD))
                .filter(|(a, b)| *b > 0.0 && *a < dur)
                .map(|(a, b)| format!("between(t,{a:.3},{b:.3})"))
                .collect();
            let feed = o.killfeed
                && style.killfeed
                && o.format == Format::Tiktok
                && !o.preview
                && !shows.is_empty();
            let mut chain = format!(
                "[{i}:v:0]trim=start={:.4}:end={:.4},setpts=(PTS-STARTPTS)/{v:.5}",
                s.src0, s.src1
            );
            if feed {
                // Keep a copy of the full frame for the kill-feed overlay.
                let (fw, fh, fx, fy) = crate::autoclip::feed_crop(src.width, src.height);
                let ow = (w as f64 * 0.8) as u32 / 2 * 2;
                let _ = write!(
                    g,
                    "[{i}:v:0]trim=start={:.4}:end={:.4},setpts=(PTS-STARTPTS)/{v:.5},fps={FPS},crop={fw}:{fh}:{fx}:{fy},scale={ow}:-2,format=rgba,colorchannelmixer=aa=0.92[kf{n}];",
                    s.src0, s.src1
                );
            }
            // Crop to the format's aspect first: everything after works on fewer pixels.
            if o.format == Format::Tiktok {
                chain.push_str(",crop=ih*9/16:ih");
            }
            if !o.preview
                && let Some((_, lut)) = o.clip_looks.iter().find(|(f, _)| *f == x.file)
            {
                let _ = write!(chain, ",lut3d=file='{}':interp=tetrahedral", esc(lut));
            }
            // Frames the source has per output second at this speed.
            let have = src.fps * v;
            let interpolate = match style.interpolation {
                Interpolation::Auto => have < f64::from(FPS) * 0.9,
                Interpolation::Always => v < 1.0 - 1e-9,
                Interpolation::Off => false,
            };
            if !o.preview && interpolate {
                let _ = write!(
                    chain,
                    ",minterpolate=fps={FPS}:mi_mode=mci:mc_mode=obmc:me_mode=bidir"
                );
            } else if !o.preview && v > 1.2 && style.blur > 0.0 {
                // Blend the frames a fast part skips (more with more blur), then pick the
                // output rate.
                let blend =
                    ((src.fps * v / f64::from(FPS) * style.blur).round() as usize).clamp(2, 8);
                let _ = write!(chain, ",tmix=frames={blend}");
            }
            let _ = write!(chain, ",fps={FPS}");
            let _ = write!(chain, ",scale={w}:{h}:flags=lanczos,setsar=1");
            if !o.preview && (style.shake > 0.0 || style.punch > 0.0) {
                // A shake and a pump on each kill inside this piece (t is piece-local).
                // Inclusive: a kill on a cut gets the effect on both sides of it.
                let (sx, sy) = (16.0 * style.shake, 13.0 * style.shake);
                let pump = style.punch;
                for &(_, at) in x
                    .kills
                    .iter()
                    .filter(|(_, at)| *at >= s.out0 - 1e-9 && *at <= s.out1 + 1e-9)
                {
                    let t = at - s.out0;
                    let _ = write!(
                        chain,
                        ",scale=w='iw*(1+{pump:.3}*exp(-pow((t-{t:.3})*9,2)))':h=-2:eval=frame,crop={w}:{h}:'(iw-{w})/2+{sx:.1}*sin((t-{t:.3})*90)*exp(-abs(t-{t:.3})*14)':'(ih-{h})/2+{sy:.1}*cos((t-{t:.3})*80)*exp(-abs(t-{t:.3})*14)'"
                    );
                }
            }
            if !o.preview {
                // Transitions. Out of the clip before the drop: zoom in fast over its last
                // frames; into the drop: start zoomed and settle.
                if whip_out && si == last_seg && dur > WHIP {
                    let a = dur - WHIP;
                    let _ = write!(
                        chain,
                        ",scale=w='iw*(1+{WHIP_ZOOM}*pow(clip((t-{a:.3})/{WHIP},0,1),2))':h=-2:eval=frame,crop={w}:{h}"
                    );
                }
                if whip_in && si == 0 && dur > WHIP {
                    let _ = write!(
                        chain,
                        ",scale=w='iw*(1+{WHIP_ZOOM}*pow(clip(1-t/{WHIP},0,1),2))':h=-2:eval=frame,crop={w}:{h}"
                    );
                }
                // Flashy: a short brightness spike at the start of every new clip.
                if flash && si == 0 {
                    let _ = write!(
                        chain,
                        ",eq=brightness='0.35*clip(1-t/{FLASH},0,1)':eval=frame"
                    );
                }
            }
            if feed {
                let _ = write!(
                    chain,
                    ",setsar=1[base{n}];[base{n}][kf{n}]overlay=x=(W-w)/2:y=H*0.10:shortest=1:enable='{}'",
                    shows.join("+")
                );
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
                // The shot itself comes up through the song: +8 dB around each kill.
                let lifts: Vec<String> = x
                    .kills
                    .iter()
                    .filter(|(_, at)| *at >= s.out0 - 1e-9 && *at <= s.out1 + 1e-9)
                    .map(|(_, at)| {
                        let t = at - s.out0;
                        format!("between(t,{:.3},{:.3})", t - 0.12, t + 0.5)
                    })
                    .collect();
                let lift = if lifts.is_empty() || o.preview {
                    String::new()
                } else {
                    format!(",volume='if({},2.5,1)':eval=frame", lifts.join("+"))
                };
                let _ = write!(
                    g,
                    "[{i}:a:0]atrim=start={:.4}:end={:.4},asetpts=PTS-STARTPTS,atempo={v:.5}{lift},apad,atrim=duration={dur:.4},afade=t=in:d={XFADE},afade=t=out:st={:.4}:d={XFADE}[a{n}];",
                    s.src0,
                    s.src1,
                    (dur - XFADE).max(0.0)
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
    // Under the song the game sits at `game_db`; without the song it is the soundtrack.
    let game_db = if style.music_in_video { o.game_db } else { 0.0 };
    let _ = write!(
        g,
        "{}concat=n={n}:v=0:a=1,volume={game_db:.1}dB[game];",
        alabels.join("")
    );
    // Loudness to about -14 LUFS (what TikTok and YouTube play at), back at 48 kHz.
    let finish = if o.preview {
        String::new()
    } else {
        ",loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000".to_owned()
    };
    if style.music_in_video {
        let _ = write!(
            g,
            "[{song_idx}:a:0]aresample=48000,afade=t=in:d=0.3,afade=t=out:st={:.3}:d=0.8[song];[song][game]amix=inputs=2:normalize=0:duration=first{finish}[aout]",
            (length - 0.8).max(0.0)
        );
    } else {
        let _ = write!(g, "[game]anull{finish}[aout]");
    }
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
            "-c:v", "h264_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", "25M",
            "-maxrate", "35M",
        ],
        Encoder::Nvenc => [
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p5",
            "-rc",
            "vbr",
            "-b:v",
            "25M",
            "-maxrate",
            "35M",
        ],
        Encoder::Qsv => [
            "-c:v",
            "h264_qsv",
            "-preset",
            "medium",
            "-b:v",
            "25M",
            "-maxrate",
            "35M",
            "-look_ahead",
            "0",
        ],
        Encoder::X264 => [
            "-c:v", "libx264", "-preset", "medium", "-b:v", "25M", "-maxrate", "35M", "-bufsize",
            "50M",
        ],
    };
    // YouTube: HEVC at 1440p60 (what YouTube gives its better encodes for); TikTok: H.264.
    let hevc: Option<[&str; 10]> =
        (o.format == Format::Youtube && !o.preview).then_some(match o.encoder {
            Encoder::Amf => [
                "-c:v", "hevc_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", "40M",
                "-maxrate", "60M",
            ],
            Encoder::Nvenc => [
                "-c:v",
                "hevc_nvenc",
                "-preset",
                "p5",
                "-rc",
                "vbr",
                "-b:v",
                "40M",
                "-maxrate",
                "60M",
            ],
            Encoder::Qsv => [
                "-c:v",
                "hevc_qsv",
                "-preset",
                "medium",
                "-b:v",
                "40M",
                "-maxrate",
                "60M",
                "-look_ahead",
                "0",
            ],
            Encoder::X264 => [
                "-c:v", "libx265", "-preset", "medium", "-b:v", "40M", "-maxrate", "60M", "-tag:v",
                "hvc1",
            ],
        });
    a.extend(hevc.unwrap_or(enc).map(String::from));
    if hevc.is_some() && o.encoder != Encoder::X264 {
        a.extend(["-tag:v", "hvc1"].map(String::from));
    }
    a.extend(["-c:a", "aac", "-b:a", "320k", "-r"].map(String::from));
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
            clip_looks: Vec::new(),
            style: Style::default(),
            game_db: -10.0,
            killfeed: true,
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
    fn the_style_drives_effects_and_transitions() {
        let base = |style: Style| {
            let mut o = opts(false);
            o.style = style;
            let args = render_args(
                &placed(),
                &sources(60.0),
                Path::new("s.mp3"),
                0.0,
                25.0,
                &o,
                Path::new("o.mp4"),
            );
            graph(&args).to_owned()
        };
        let def = base(Style::default());
        // Mostly cuts: a zoom whip into the drop (out of the clip before, into the drop).
        assert_eq!(def.matches("pow(clip((t-").count(), 1, "{def}");
        assert_eq!(def.matches("pow(clip(1-t/").count(), 1, "{def}");
        assert!(!def.contains("eq=brightness"));
        let cuts = base(Style {
            transitions: Transitions::Cuts,
            ..Style::default()
        });
        assert!(!cuts.contains("pow(clip("));
        let flashy = base(Style {
            transitions: Transitions::Flashy,
            ..Style::default()
        });
        assert_eq!(flashy.matches("eq=brightness").count(), placed().len() - 1);
        // No shake or punch: no per-kill rescale; no interpolation when it's off.
        let calm = base(Style {
            shake: 0.0,
            punch: 0.0,
            interpolation: Interpolation::Off,
            ..Style::default()
        });
        assert!(!calm.contains("exp(-pow") && !calm.contains("minterpolate"));
        assert!(
            def.contains("minterpolate"),
            "60 fps sources slowed down are interpolated"
        );
    }

    #[test]
    fn audio_has_no_clicks_hits_minus_14_lufs_and_can_leave_the_song_out() {
        let mut o = opts(false);
        let g = graph(&render_args(
            &placed(),
            &sources(240.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &o,
            Path::new("o.mp4"),
        ))
        .to_owned();
        assert!(g.contains("afade=t=in:d=0.008"), "{g}");
        assert!(g.contains("loudnorm=I=-14") && g.contains("amix=inputs=2"));
        assert!(g.contains("volume=-10.0dB[game]"));
        o.style.music_in_video = false;
        let a = render_args(
            &placed(),
            &sources(240.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &o,
            Path::new("o.mp4"),
        );
        let g = graph(&a);
        assert!(!g.contains("amix") && g.contains("[game]anull,loudnorm"));
        assert!(
            g.contains("volume=0.0dB[game]"),
            "the game is the soundtrack"
        );
        assert!(a.windows(2).any(|w| w == ["-b:a", "320k"]));
        o.style.killfeed = false;
        let g = graph(&render_args(
            &placed(),
            &sources(240.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &o,
            Path::new("o.mp4"),
        ))
        .to_owned();
        assert!(!g.contains("overlay="), "Centre layout: no kill feed");
    }

    #[test]
    fn each_clip_gets_its_games_look_before_the_montage_look() {
        let mut o = opts(false);
        o.clip_looks = vec![(
            PathBuf::from(r"D:\c\a.mkv"),
            PathBuf::from(r"D:\l\cs2.cube"),
        )];
        let a = render_args(
            &placed(),
            &sources(240.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &o,
            Path::new("o.mp4"),
        );
        let g = graph(&a);
        assert_eq!(g.matches(r"lut3d=file='D\:/l/cs2.cube'").count(), 1, "{g}");
        let clip = g.find("cs2.cube").unwrap();
        let montage = g.find("look.cube").unwrap();
        assert!(clip < montage);
    }

    #[test]
    fn vertical_montages_overlay_the_kill_feed_and_lift_kills() {
        let a = render_args(
            &placed(),
            &sources(240.0),
            Path::new("s.mp3"),
            0.0,
            25.0,
            &opts(false),
            Path::new("o.mp4"),
        );
        let g = graph(&a);
        assert!(g.contains("crop=664:374:1896:316"), "{g}");
        assert!(g.contains("overlay=x=(W-w)/2:y=H*0.10:shortest=1:enable='between(t,"));
        assert!(g.contains(",volume='if(between(t,"));
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
