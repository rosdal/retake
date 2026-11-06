//! The ffmpeg command line for the ring buffer, built from `[capture]`. Pure, so the exact
//! arguments are pinned by tests.

use crate::config::{Capture, Codec};

/// Length of one buffer segment, in seconds.
pub const SEGMENT_SECS: u32 = 2;
/// The CSV segment list (filename, start, end) inside `buffer_dir`.
pub const INDEX_FILE: &str = "index.csv";
/// ffmpeg's output pattern inside `buffer_dir`.
pub const SEGMENT_PATTERN: &str = "seg_%02d.ts";

/// Segments needed to hold `buffer_secs`, rounded up.
pub fn segment_count(buffer_secs: u32) -> u32 {
    buffer_secs.div_ceil(SEGMENT_SECS)
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// Desktop Duplication of monitor `output` at `fps`, without the cursor.
pub fn input_args(c: &Capture) -> Vec<String> {
    let mut a = strs(&["-init_hw_device", "d3d11va", "-filter_complex"]);
    a.push(format!(
        "ddagrab=output_idx={}:framerate={}:draw_mouse=0",
        c.output, c.fps
    ));
    a
}

/// AMF encoder at constant QP, a keyframe every segment, no B-frames.
pub fn encoder_args(c: &Capture) -> Vec<String> {
    let codec = match c.codec {
        Codec::Hevc => "hevc_amf",
        Codec::Av1 => "av1_amf",
    };
    let mut a = strs(&["-c:v", codec, "-quality", "speed", "-rc", "cqp"]);
    a.extend([
        "-qp_i".into(),
        c.qp_i.to_string(),
        "-qp_p".into(),
        c.qp_p.to_string(),
        "-g".into(),
        (c.fps * SEGMENT_SECS).to_string(),
        "-bf".into(),
        "0".into(),
    ]);
    a
}

/// Wrapping MPEG-TS segments plus a bounded CSV index, both in `buffer_dir`.
pub fn segment_args(c: &Capture) -> Vec<String> {
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
pub fn capture_args(c: &Capture) -> Vec<String> {
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
    use crate::config::{Codec, load_str};
    use std::path::PathBuf;

    fn cap() -> crate::config::Capture {
        let mut c = load_str("").unwrap().capture;
        c.buffer_dir = PathBuf::from(r"C:\buf");
        c
    }

    #[test]
    fn default_hevc_args() {
        assert_eq!(
            capture_args(&cap()),
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
        let mut c = cap();
        c.codec = Codec::Av1;
        let hevc = capture_args(&cap());
        let av1 = capture_args(&c);
        assert_eq!(hevc.len(), av1.len());
        let diff: Vec<_> = hevc.iter().zip(&av1).filter(|(a, b)| a != b).collect();
        assert_eq!(diff, [(&"hevc_amf".to_string(), &"av1_amf".to_string())]);
    }

    #[test]
    fn output_fps_and_qp_follow_config() {
        let mut c = cap();
        (c.output, c.fps, c.qp_i, c.qp_p) = (2, 60, 20, 22);
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
        assert!(!capture_args(&cap()).iter().any(|a| a == "-nostdin"));
    }
}
