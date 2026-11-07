//! ffmpeg command lines for grading renders, export presets and motion blur. Pure, so every
//! argument is pinned by tests.

use std::path::Path;

use crate::autoconf::Encoder;

/// An export preset (`export.presets`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    /// Native resolution and fps, the graded HEVC remuxed to MP4 with every audio track.
    Edit,
    /// DNxHR HQ with PCM audio, for editors that scrub HEVC badly.
    EditSmooth,
    /// 1080p60 H.264 with game and mic mixed, for Discord.
    Share,
    /// 1080×1920 centre crop, 60 fps H.264, for TikTok and Shorts.
    Vertical,
}

impl Preset {
    pub fn parse(s: &str) -> Option<Preset> {
        Some(match s {
            "edit" => Preset::Edit,
            "edit-smooth" => Preset::EditSmooth,
            "share" => Preset::Share,
            "vertical" => Preset::Vertical,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Preset::Edit => "edit",
            Preset::EditSmooth => "edit-smooth",
            Preset::Share => "share",
            Preset::Vertical => "vertical",
        }
    }

    /// Output extension.
    pub fn ext(self) -> &'static str {
        match self {
            Preset::EditSmooth => "mov",
            _ => "mp4",
        }
    }

    /// Whether motion blur may be added: never to `edit` exports, which keep the frames slow-mo
    /// needs.
    pub fn blurrable(self) -> bool {
        matches!(self, Preset::Share | Preset::Vertical)
    }
}

/// How frames are weighted when blended into one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weighting {
    Equal,
    GaussianSym,
    Pyramid,
}

impl Weighting {
    pub fn parse(s: &str) -> Option<Weighting> {
        Some(match s {
            "equal" => Weighting::Equal,
            "gaussian_sym" => Weighting::GaussianSym,
            "pyramid" => Weighting::Pyramid,
            _ => return None,
        })
    }
}

/// Frames blended into each output frame: `round(source / output × amount)`, made odd so the
/// blend is centred, at least 1.
pub fn blend_frames(source_fps: f64, output_fps: f64, amount: f64) -> usize {
    let n = (source_fps / output_fps * amount).round().max(1.0) as usize;
    if n.is_multiple_of(2) { n + 1 } else { n }
}

/// Weights for `n` blended frames, normalised to sum to 1, printed for `tmix`.
pub fn blend_weights(n: usize, w: Weighting) -> Vec<f64> {
    let mid = (n as f64 - 1.0) / 2.0;
    let raw: Vec<f64> = (0..n)
        .map(|i| {
            let d = i as f64 - mid;
            match w {
                Weighting::Equal => 1.0,
                Weighting::Pyramid => mid + 1.0 - d.abs(),
                Weighting::GaussianSym => {
                    let sigma = (n as f64 / 4.0).max(0.5);
                    (-d * d / (2.0 * sigma * sigma)).exp()
                }
            }
        })
        .collect();
    let sum: f64 = raw.iter().sum();
    raw.iter().map(|v| v / sum).collect()
}

/// `tmix` + `fps` for motion blur from `source_fps` to `output_fps`, or nothing when the blend
/// would be a single frame.
pub fn blur_filter(source_fps: f64, output_fps: u32, amount: f64, w: Weighting) -> Option<String> {
    let n = blend_frames(source_fps, f64::from(output_fps), amount);
    if n < 2 {
        return None;
    }
    let weights: Vec<String> = blend_weights(n, w)
        .iter()
        .map(|v| format!("{v:.4}"))
        .collect();
    Some(format!(
        "tmix=frames={n}:weights='{}',fps={output_fps}",
        weights.join(" ")
    ))
}

fn s(a: &[&str]) -> Vec<String> {
    a.iter().map(|x| (*x).to_owned()).collect()
}

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Colour tags: limited-range BT.709, so editors and players show what Retake recorded.
pub const BT709_TAGS: [&str; 8] = [
    "-colorspace",
    "bt709",
    "-color_primaries",
    "bt709",
    "-color_trc",
    "bt709",
    "-color_range",
    "tv",
];

/// The 10-bit HEVC encoder for graded files, at constant QP 14.
fn hevc10(enc: Encoder) -> Vec<String> {
    match enc {
        Encoder::Amf => s(&[
            "-c:v",
            "hevc_amf",
            "-profile:v",
            "main10",
            "-quality",
            "quality",
            "-rc",
            "cqp",
            "-qp_i",
            "14",
            "-qp_p",
            "14",
        ]),
        Encoder::Nvenc => s(&[
            "-c:v",
            "hevc_nvenc",
            "-profile:v",
            "main10",
            "-preset",
            "p5",
            "-rc",
            "constqp",
            "-qp",
            "14",
        ]),
        Encoder::Qsv => s(&["-c:v", "hevc_qsv", "-profile:v", "main10", "-q:v", "14"]),
    }
}

/// The 8-bit HEVC encoder, for `bit_depth = 8`.
fn hevc8(enc: Encoder) -> Vec<String> {
    let mut a = hevc10(enc);
    if let Some(i) = a.iter().position(|x| x == "main10") {
        a[i] = "main".into();
    }
    a
}

/// H.264 at about 20 Mb/s for shares.
fn h264(enc: Encoder) -> Vec<String> {
    match enc {
        Encoder::Amf => s(&[
            "-c:v", "h264_amf", "-quality", "quality", "-rc", "vbr_peak", "-b:v", "20M",
            "-maxrate", "30M",
        ]),
        Encoder::Nvenc => s(&[
            "-c:v",
            "h264_nvenc",
            "-preset",
            "p5",
            "-rc",
            "vbr",
            "-b:v",
            "20M",
            "-maxrate",
            "30M",
        ]),
        Encoder::Qsv => s(&["-c:v", "h264_qsv", "-b:v", "20M", "-maxrate", "30M"]),
    }
}

/// The grading render: decode with the BT.709 limited-range matrix, apply the per-clip LUT in
/// 16-bit RGB, sharpen, and encode HEVC Main10 (or Main) with every audio track copied. Each
/// `protect` rectangle (the HUD) gets half of the ungraded image blended back in, so kill feed
/// and UI colours stay true.
pub fn grade_args(
    input: &Path,
    lut: &Path,
    sharpen: f32,
    bit_depth: u32,
    enc: Encoder,
    protect: &[crate::grade::Rect],
    output: &Path,
) -> Vec<String> {
    let mut a = s(&[
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-i",
    ]);
    a.push(path(input));
    // lut3d reads the path itself: escape it for the filter graph.
    let lut = path(lut).replace('\\', "/").replace(':', "\\:");
    let out_fmt = if bit_depth == 8 { "yuv420p" } else { "p010le" };
    let grade = format!("lut3d=file='{lut}':interp=tetrahedral");
    // Planar 16-bit RGB is ~20% faster than packed here. Sharpening runs on the luma plane
    // after the conversion back to YUV: the same look at a third of the cost (it was the
    // slowest filter on three 16-bit RGB planes).
    let to_rgb = "scale=in_color_matrix=bt709:in_range=tv,format=gbrp16le";
    let mut to_yuv = format!("scale=out_color_matrix=bt709:out_range=tv,format={out_fmt}");
    if sharpen > 0.0 {
        to_yuv.push_str(&format!(",cas=strength={sharpen}:planes=1"));
    }
    let n = protect.len();
    let graph = if n == 0 {
        format!("[0:v:0]{to_rgb},{grade},{to_yuv}[v]")
    } else {
        let outs: String = (0..n).map(|k| format!("[o{k}]")).collect();
        let mut g =
            format!("[0:v:0]{to_rgb},split=2[src][orig];[src]{grade}[g0];[orig]split={n}{outs}");
        for (k, r) in protect.iter().enumerate() {
            g.push_str(&format!(
                ";[o{k}]crop=w=iw*{w}:h=ih*{h}:x=iw*{x}:y=ih*{y},format=rgba64le,colorchannelmixer=aa=0.5[h{k}]\
                 ;[g{k}][h{k}]overlay=x=main_w*{x}:y=main_h*{y}[g{n1}]",
                w = r.w,
                h = r.h,
                x = r.x,
                y = r.y,
                n1 = k + 1
            ));
        }
        g.push_str(&format!(";[g{n}]{to_yuv}[v]"));
        g
    };
    a.extend(s(&["-filter_complex"]));
    a.push(graph);
    a.extend(s(&["-map", "[v]", "-map", "0:a?"]));
    a.extend(if bit_depth == 8 {
        hevc8(enc)
    } else {
        hevc10(enc)
    });
    a.extend(s(&BT709_TAGS));
    a.extend(s(&["-c:a", "copy", "-map_metadata", "0", "-f", "matroska"]));
    a.push(path(output));
    a
}

/// Settings an export needs besides the preset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportSetup {
    pub encoder: Encoder,
    /// The source's frame rate.
    pub source_fps: f64,
    /// Audio tracks in the source (Game, Mic, Comms).
    pub audio_tracks: usize,
    /// Motion blur, if enabled for this preset: (output fps, amount, weighting).
    pub blur: Option<(u32, f64, Weighting)>,
    pub sharpen: f32,
}

/// The export command line for `preset` from `input` (usually the graded clip).
pub fn export_args(preset: Preset, input: &Path, x: &ExportSetup, output: &Path) -> Vec<String> {
    let mut a = s(&[
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-i",
    ]);
    a.push(path(input));
    match preset {
        Preset::Edit => {
            a.extend(s(&[
                "-map",
                "0:v:0",
                "-map",
                "0:a?",
                "-c",
                "copy",
                "-tag:v",
                "hvc1",
                "-movflags",
                "+faststart",
            ]));
        }
        Preset::EditSmooth => {
            a.extend(s(&[
                "-map",
                "0:v:0",
                "-map",
                "0:a?",
                "-c:v",
                "dnxhd",
                "-profile:v",
                "dnxhr_hq",
                "-pix_fmt",
                "yuv422p",
                "-c:a",
                "pcm_s16le",
            ]));
            a.extend(s(&BT709_TAGS));
        }
        Preset::Share | Preset::Vertical => {
            // Blur at the source rate, then scale, then sharpen at the output size.
            let mut v = String::from("[0:v:0]");
            match x
                .blur
                .and_then(|(fps, amount, w)| blur_filter(x.source_fps, fps, amount, w))
            {
                Some(b) => v.push_str(&b),
                None => v.push_str("fps=60"),
            }
            if preset == Preset::Vertical {
                v.push_str(",crop=ih*9/16:ih,scale=1080:1920:flags=lanczos");
            } else {
                v.push_str(",scale=-2:1080:flags=lanczos");
            }
            if x.sharpen > 0.0 {
                v.push_str(&format!(",cas=strength={}", x.sharpen));
            }
            v.push_str(",format=yuv420p[v]");
            let mut graph = v;
            let audio = match x.audio_tracks {
                0 => None,
                1 => Some("0:a:0".to_owned()),
                n => {
                    let ins: String = (0..n).map(|k| format!("[0:a:{k}]")).collect();
                    graph.push_str(&format!(";{ins}amix=inputs={n}:normalize=0[a]"));
                    Some("[a]".to_owned())
                }
            };
            a.push("-filter_complex".into());
            a.push(graph);
            a.extend(s(&["-map", "[v]"]));
            if let Some(m) = audio {
                a.extend([
                    "-map".into(),
                    m,
                    "-c:a".into(),
                    "aac".into(),
                    "-b:a".into(),
                    "192k".into(),
                ]);
            }
            a.extend(h264(x.encoder));
            a.extend(s(&BT709_TAGS));
            a.extend(s(&["-movflags", "+faststart"]));
        }
    }
    a.push(path(output));
    a
}

/// `clips/graded/<clip>_<preset>.<ext>`.
pub fn export_name(clip_stem: &str, preset: Preset) -> String {
    format!("{clip_stem}_{}.{}", preset.name(), preset.ext())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn blend_frames_is_odd_and_scales() {
        assert_eq!(blend_frames(240.0, 60.0, 1.0), 5);
        assert_eq!(blend_frames(120.0, 60.0, 1.0), 3);
        assert_eq!(blend_frames(240.0, 60.0, 0.25), 1);
        assert_eq!(blend_frames(60.0, 60.0, 1.0), 1);
        assert_eq!(blend_frames(240.0, 60.0, 2.0), 9);
    }

    #[test]
    fn weights_sum_to_one_and_are_symmetric() {
        for w in [Weighting::Equal, Weighting::GaussianSym, Weighting::Pyramid] {
            let v = blend_weights(5, w);
            assert!((v.iter().sum::<f64>() - 1.0).abs() < 1e-9);
            assert!((v[0] - v[4]).abs() < 1e-12 && (v[1] - v[3]).abs() < 1e-12);
            if w != Weighting::Equal {
                assert!(v[2] > v[0]);
            }
        }
        assert_eq!(blend_weights(3, Weighting::Pyramid), [0.25, 0.5, 0.25]);
    }

    #[test]
    fn no_blur_when_one_frame() {
        assert_eq!(blur_filter(60.0, 60, 1.0, Weighting::Equal), None);
        assert_eq!(
            blur_filter(120.0, 60, 1.0, Weighting::Pyramid).unwrap(),
            "tmix=frames=3:weights='0.2500 0.5000 0.2500',fps=60"
        );
    }

    fn setup() -> ExportSetup {
        ExportSetup {
            encoder: Encoder::Amf,
            source_fps: 240.0,
            audio_tracks: 2,
            blur: None,
            sharpen: 0.35,
        }
    }

    #[test]
    fn grade_render_args() {
        let a = grade_args(
            &PathBuf::from(r"D:\c\raw\a.mkv"),
            &PathBuf::from(r"D:\c\tmp\a.cube"),
            0.35,
            10,
            Encoder::Amf,
            &[],
            &PathBuf::from(r"D:\c\raw\a_graded.mkv"),
        );
        let vf = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(
            vf,
            "[0:v:0]scale=in_color_matrix=bt709:in_range=tv,format=gbrp16le,\
             lut3d=file='D\\:/c/tmp/a.cube':interp=tetrahedral,\
             scale=out_color_matrix=bt709:out_range=tv,format=p010le,cas=strength=0.35:planes=1[v]"
        );
        assert!(a.windows(2).any(|w| w == ["-profile:v", "main10"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "copy"]));
        assert!(a.windows(2).any(|w| w == ["-color_range", "tv"]));
        assert_eq!(a.last().unwrap(), r"D:\c\raw\a_graded.mkv");
        let a8 = grade_args(
            &PathBuf::from("a"),
            &PathBuf::from("l"),
            0.0,
            8,
            Encoder::Amf,
            &[],
            &PathBuf::from("o"),
        );
        assert!(a8.windows(2).any(|w| w == ["-profile:v", "main"]));
        assert!(!a8.iter().any(|x| x.contains("cas=")));
    }

    #[test]
    fn hud_protect_blends_each_rectangle_back() {
        let rects = crate::grade::SIEGE_HUD;
        let a = grade_args(
            &PathBuf::from("a"),
            &PathBuf::from("l"),
            0.0,
            10,
            Encoder::Amf,
            &rects,
            &PathBuf::from("o"),
        );
        let g = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(g.contains("split=4[o0][o1][o2][o3]"), "{g}");
        assert_eq!(g.matches("overlay=").count(), 4);
        assert!(g.ends_with("[g4]scale=out_color_matrix=bt709:out_range=tv,format=p010le[v]"));
    }

    #[test]
    fn share_mixes_audio_and_scales_then_sharpens() {
        let a = export_args(
            Preset::Share,
            &PathBuf::from("in.mkv"),
            &setup(),
            &PathBuf::from("out.mp4"),
        );
        let g = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert_eq!(
            g,
            "[0:v:0]fps=60,scale=-2:1080:flags=lanczos,cas=strength=0.35,format=yuv420p[v];\
             [0:a:0][0:a:1]amix=inputs=2:normalize=0[a]"
        );
        assert!(a.windows(2).any(|w| w == ["-map", "[a]"]));
        assert!(a.windows(2).any(|w| w == ["-c:v", "h264_amf"]));
    }

    #[test]
    fn vertical_with_blur_crops_centre() {
        let x = ExportSetup {
            blur: Some((60, 1.0, Weighting::Equal)),
            audio_tracks: 1,
            ..setup()
        };
        let a = export_args(
            Preset::Vertical,
            &PathBuf::from("in.mkv"),
            &x,
            &PathBuf::from("o.mp4"),
        );
        let g = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(g.starts_with("[0:v:0]tmix=frames=5:weights='0.2000 0.2000 0.2000 0.2000 0.2000',fps=60,crop=ih*9/16:ih,scale=1080:1920"), "{g}");
        assert!(a.windows(2).any(|w| w == ["-map", "0:a:0"]));
    }

    #[test]
    fn edit_is_a_stream_copy_and_smooth_is_dnxhr() {
        let a = export_args(
            Preset::Edit,
            &PathBuf::from("g.mkv"),
            &setup(),
            &PathBuf::from("g_edit.mp4"),
        );
        assert!(a.windows(2).any(|w| w == ["-c", "copy"]));
        let b = export_args(
            Preset::EditSmooth,
            &PathBuf::from("g.mkv"),
            &setup(),
            &PathBuf::from("g.mov"),
        );
        assert!(b.windows(2).any(|w| w == ["-profile:v", "dnxhr_hq"]));
        assert!(b.windows(2).any(|w| w == ["-c:a", "pcm_s16le"]));
    }

    #[test]
    fn names_and_parse() {
        assert_eq!(
            export_name("2026-11-07_201500", Preset::Share),
            "2026-11-07_201500_share.mp4"
        );
        assert_eq!(export_name("x", Preset::EditSmooth), "x_edit-smooth.mov");
        for p in [
            Preset::Edit,
            Preset::EditSmooth,
            Preset::Share,
            Preset::Vertical,
        ] {
            assert_eq!(Preset::parse(p.name()), Some(p));
        }
        assert!(Preset::Share.blurrable() && !Preset::Edit.blurrable());
    }
}
