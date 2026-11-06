//! The ffmpeg command line for the ring buffer, built from a resolved [`CaptureSetup`]. Pure,
//! so the exact arguments are pinned by tests.

use std::path::PathBuf;

use crate::autoconf::Encoder;
use crate::config::Codec;

/// Length of one buffer segment, in seconds.
pub const SEGMENT_SECS: u32 = 2;
/// The CSV segment list (filename, start, end) inside `buffer_dir`.
pub const INDEX_FILE: &str = "index.csv";
/// ffmpeg's output pattern inside `buffer_dir`.
pub const SEGMENT_PATTERN: &str = "seg_%02d.ts";

/// Everything one recording session needs, with every `"auto"` already decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSetup {
    /// `ddagrab` output index.
    pub output: u32,
    pub fps: u32,
    pub encoder: Encoder,
    pub codec: Codec,
    pub qp_i: u32,
    pub qp_p: u32,
    pub buffer_dir: PathBuf,
    pub buffer_secs: u32,
}

/// Segments needed to hold `buffer_secs`, rounded up.
pub fn segment_count(buffer_secs: u32) -> u32 {
    buffer_secs.div_ceil(SEGMENT_SECS)
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

fn ddagrab(c: &CaptureSetup) -> String {
    format!(
        "ddagrab=output_idx={}:framerate={}:draw_mouse=0",
        c.output, c.fps
    )
}

/// Desktop Duplication of monitor `output` at `fps`, without the cursor. AMF and NVENC take the
/// D3D11 frames as they are; QSV needs them mapped onto a QSV device derived from the same
/// D3D11 device.
pub fn input_args(c: &CaptureSetup) -> Vec<String> {
    match c.encoder {
        Encoder::Amf | Encoder::Nvenc => {
            let mut a = strs(&["-init_hw_device", "d3d11va", "-filter_complex"]);
            a.push(ddagrab(c));
            a
        }
        Encoder::Qsv => {
            let mut a = strs(&[
                "-init_hw_device",
                "d3d11va=dx",
                "-init_hw_device",
                "qsv=qs@dx",
                "-filter_hw_device",
                "dx",
                "-filter_complex",
            ]);
            a.push(format!("{},hwmap=derive_device=qsv,format=qsv", ddagrab(c)));
            a
        }
    }
}

/// The encoder at constant QP, a keyframe every segment, no B-frames.
pub fn encoder_args(c: &CaptureSetup) -> Vec<String> {
    let codec = match c.codec {
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    };
    let gop = (c.fps * SEGMENT_SECS).to_string();
    let mut a = vec!["-c:v".to_owned(), format!("{codec}_{}", c.encoder)];
    match c.encoder {
        Encoder::Amf => {
            a.extend(strs(&["-quality", "speed", "-rc", "cqp"]));
            a.extend([
                "-qp_i".into(),
                c.qp_i.to_string(),
                "-qp_p".into(),
                c.qp_p.to_string(),
            ]);
        }
        Encoder::Qsv => {
            // QSV runs CQP by default; its P-frame QP is `-q:v` and its I-frame QP is
            // `q * i_qfactor + i_qoffset`.
            a.extend(strs(&["-preset", "veryfast"]));
            a.extend([
                "-q:v".into(),
                c.qp_p.to_string(),
                "-i_qfactor".into(),
                "1".into(),
                "-i_qoffset".into(),
                (i64::from(c.qp_i) - i64::from(c.qp_p)).to_string(),
            ]);
        }
        Encoder::Nvenc => {
            a.extend(strs(&["-preset", "p1", "-tune", "ll", "-rc", "constqp"]));
            a.extend([
                "-init_qpI".into(),
                c.qp_i.to_string(),
                "-init_qpP".into(),
                c.qp_p.to_string(),
            ]);
        }
    }
    a.extend(["-g".into(), gop, "-bf".into(), "0".into()]);
    a
}

/// Wrapping MPEG-TS segments plus a bounded CSV index, both in `buffer_dir`.
pub fn segment_args(c: &CaptureSetup) -> Vec<String> {
    let n = segment_count(c.buffer_secs).to_string();
    let index = c.buffer_dir.join(INDEX_FILE).to_string_lossy().into_owned();
    let pattern = c
        .buffer_dir
        .join(SEGMENT_PATTERN)
        .to_string_lossy()
        .into_owned();
    let mut a = strs(&["-f", "segment", "-segment_time"]);
    a.push(SEGMENT_SECS.to_string());
    a.extend([
        "-segment_wrap".into(),
        n.clone(),
        "-segment_list_size".into(),
        n,
    ]);
    a.extend(strs(&[
        "-segment_format",
        "mpegts",
        "-reset_timestamps",
        "1",
        "-segment_list",
    ]));
    a.extend([index, "-segment_list_type".into(), "csv".into(), pattern]);
    a
}

/// The full ring-buffer command line (after the exe name).
pub fn capture_args(c: &CaptureSetup) -> Vec<String> {
    let mut a = strs(&[
        "-hide_banner",
        "-loglevel",
        "warning",
        "-stats",
        "-stats_period",
        "1",
    ]);
    a.extend(input_args(c));
    a.extend(encoder_args(c));
    a.extend(segment_args(c));
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Codec;

    fn amf() -> CaptureSetup {
        CaptureSetup {
            output: 0,
            fps: 120,
            encoder: Encoder::Amf,
            codec: Codec::Hevc,
            qp_i: 16,
            qp_p: 18,
            buffer_dir: PathBuf::from(r"C:\buf"),
            buffer_secs: 60,
        }
    }

    #[test]
    fn default_hevc_args() {
        assert_eq!(
            capture_args(&amf()),
            [
                "-hide_banner",
                "-loglevel",
                "warning",
                "-stats",
                "-stats_period",
                "1",
                "-init_hw_device",
                "d3d11va",
                "-filter_complex",
                "ddagrab=output_idx=0:framerate=120:draw_mouse=0",
                "-c:v",
                "hevc_amf",
                "-quality",
                "speed",
                "-rc",
                "cqp",
                "-qp_i",
                "16",
                "-qp_p",
                "18",
                "-g",
                "240",
                "-bf",
                "0",
                "-f",
                "segment",
                "-segment_time",
                "2",
                "-segment_wrap",
                "30",
                "-segment_list_size",
                "30",
                "-segment_format",
                "mpegts",
                "-reset_timestamps",
                "1",
                "-segment_list",
                r"C:\buf\index.csv",
                "-segment_list_type",
                "csv",
                r"C:\buf\seg_%02d.ts",
            ]
        );
    }

    #[test]
    fn av1_swaps_only_the_encoder() {
        for enc in [Encoder::Amf, Encoder::Qsv, Encoder::Nvenc] {
            let hevc = CaptureSetup {
                encoder: enc,
                ..amf()
            };
            let av1 = CaptureSetup {
                codec: Codec::Av1,
                ..hevc.clone()
            };
            let (h, a) = (capture_args(&hevc), capture_args(&av1));
            let diff: Vec<_> = h.iter().zip(&a).filter(|(x, y)| x != y).collect();
            let want = (format!("hevc_{enc}"), format!("av1_{enc}"));
            assert_eq!(diff, [(&want.0, &want.1)], "{enc}");
        }
    }

    #[test]
    fn qsv_maps_frames_and_sets_both_qps() {
        let c = CaptureSetup {
            encoder: Encoder::Qsv,
            fps: 60,
            ..amf()
        };
        assert_eq!(
            input_args(&c),
            [
                "-init_hw_device",
                "d3d11va=dx",
                "-init_hw_device",
                "qsv=qs@dx",
                "-filter_hw_device",
                "dx",
                "-filter_complex",
                "ddagrab=output_idx=0:framerate=60:draw_mouse=0,hwmap=derive_device=qsv,format=qsv",
            ]
        );
        assert_eq!(
            encoder_args(&c),
            [
                "-c:v",
                "hevc_qsv",
                "-preset",
                "veryfast",
                "-q:v",
                "18",
                "-i_qfactor",
                "1",
                "-i_qoffset",
                "-2",
                "-g",
                "120",
                "-bf",
                "0",
            ]
        );
    }

    #[test]
    fn nvenc_uses_constqp() {
        let c = CaptureSetup {
            encoder: Encoder::Nvenc,
            ..amf()
        };
        assert_eq!(input_args(&c), input_args(&amf()));
        assert_eq!(
            encoder_args(&c),
            [
                "-c:v",
                "hevc_nvenc",
                "-preset",
                "p1",
                "-tune",
                "ll",
                "-rc",
                "constqp",
                "-init_qpI",
                "16",
                "-init_qpP",
                "18",
                "-g",
                "240",
                "-bf",
                "0",
            ]
        );
    }

    #[test]
    fn output_fps_and_qp_follow_the_setup() {
        let c = CaptureSetup {
            output: 2,
            fps: 60,
            qp_i: 20,
            qp_p: 22,
            ..amf()
        };
        let a = capture_args(&c);
        assert!(a.contains(&"ddagrab=output_idx=2:framerate=60:draw_mouse=0".to_string()));
        let g = a.iter().position(|s| s == "-g").unwrap();
        assert_eq!(a[g + 1], "120");
        assert!(a.windows(2).any(|w| w == ["-qp_i", "20"]));
        assert!(a.windows(2).any(|w| w == ["-qp_p", "22"]));
    }

    #[test]
    fn segment_count_rounds_up() {
        assert_eq!(segment_count(60), 30);
        assert_eq!(segment_count(61), 31);
        assert_eq!(segment_count(22), 11);
    }

    #[test]
    fn never_sets_nostdin() {
        for encoder in [Encoder::Amf, Encoder::Qsv, Encoder::Nvenc] {
            let c = CaptureSetup { encoder, ..amf() };
            assert!(!capture_args(&c).iter().any(|a| a == "-nostdin"));
        }
    }
}
