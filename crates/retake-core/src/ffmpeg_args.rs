//! The ffmpeg command line for the ring buffer, built from a resolved [`CaptureSetup`]. Pure,
//! so the exact arguments are pinned by tests.

use std::path::PathBuf;

use crate::autoconf::Encoder;
use crate::config::{Codec, Preset};

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
    pub preset: Preset,
    pub buffer_dir: PathBuf,
    pub buffer_secs: u32,
    /// Audio pipes, in track order. Empty = video only.
    pub audio: Vec<AudioInput>,
    /// The run's origin in Unix microseconds: video and audio timestamps both count from it.
    /// Only used with audio.
    pub origin_us: i64,
}

/// One audio track: a named pipe carrying 48 kHz s16le stereo, and its title in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioInput {
    pub pipe: String,
    pub title: &'static str,
}

/// AAC bitrate per audio track.
pub const AUDIO_BITRATE: &str = "256k";

/// Segments needed to hold `buffer_secs`, rounded up.
pub fn segment_count(buffer_secs: u32) -> u32 {
    buffer_secs.div_ceil(SEGMENT_SECS)
}

fn strs(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// The capture source. With audio, frames are restamped with ffmpeg's wall clock counted from
/// the run's origin: ddagrab counts from its own first frame, a moment nobody outside ffmpeg
/// knows, while the audio pump places samples by their capture time from that same origin.
fn ddagrab(c: &CaptureSetup) -> String {
    let mut s = format!(
        "ddagrab=output_idx={}:framerate={}:draw_mouse=0",
        c.output, c.fps
    );
    if !c.audio.is_empty() {
        s.push_str(&format!(",setpts=(RTCTIME-{})/(TB*1000000)", c.origin_us));
    }
    s
}

/// The filter graph's video output label, needed for `-map` once there are audio inputs.
fn video_label(c: &CaptureSetup) -> &'static str {
    if c.audio.is_empty() { "" } else { "[v]" }
}

/// Desktop Duplication of monitor `output` at `fps`, without the cursor. AMF and NVENC take the
/// D3D11 frames as they are; QSV needs them mapped onto a QSV device derived from the same
/// D3D11 device.
pub fn input_args(c: &CaptureSetup) -> Vec<String> {
    match c.encoder {
        Encoder::Amf | Encoder::Nvenc => {
            let mut a = strs(&["-init_hw_device", "d3d11va", "-filter_complex"]);
            a.push(format!("{}{}", ddagrab(c), video_label(c)));
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
            a.push(format!(
                "{},hwmap=derive_device=qsv,format=qsv{}",
                ddagrab(c),
                video_label(c)
            ));
            a
        }
        Encoder::X264 => {
            // CPU encoding: the frames come down from the GPU.
            let mut a = strs(&["-init_hw_device", "d3d11va", "-filter_complex"]);
            a.push(format!(
                "{},hwdownload,format=bgra,format=yuv420p{}",
                ddagrab(c),
                video_label(c)
            ));
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
    let mut a = vec![
        "-c:v".to_owned(),
        if c.encoder == Encoder::X264 {
            "libx264".to_owned()
        } else {
            format!("{codec}_{}", c.encoder)
        },
    ];
    match c.encoder {
        Encoder::Amf => {
            let quality = match c.preset {
                Preset::Speed => "speed",
                Preset::Balanced => "balanced",
                Preset::Quality => "quality",
            };
            a.extend(strs(&["-quality", quality, "-rc", "cqp"]));
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
            let preset = match c.preset {
                Preset::Speed => "veryfast",
                Preset::Balanced => "medium",
                Preset::Quality => "veryslow",
            };
            a.extend(strs(&["-preset", preset]));
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
            let preset = match c.preset {
                Preset::Speed => "p1",
                Preset::Balanced => "p4",
                Preset::Quality => "p7",
            };
            a.extend(strs(&["-preset", preset, "-tune", "ll", "-rc", "constqp"]));
            a.extend([
                "-init_qpI".into(),
                c.qp_i.to_string(),
                "-init_qpP".into(),
                c.qp_p.to_string(),
            ]);
        }
        Encoder::X264 => {
            // The fastest CPU preset whatever the setting: anything slower takes frames from
            // the game.
            a.extend(strs(&["-preset", "ultrafast", "-tune", "zerolatency"]));
            a.extend(["-qp".into(), c.qp_p.to_string()]);
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
    // Timestamps stay continuous across segments (no -reset_timestamps): the video and audio
    // rings count from the same origin, and the saver lines them up by timestamp.
    a.extend(strs(&["-segment_format", "mpegts", "-segment_list"]));
    a.extend([index, "-segment_list_type".into(), "csv".into(), pattern]);
    a
}

/// One raw PCM input per audio pipe (inputs 0..n, timestamped by sample count).
pub fn audio_input_args(c: &CaptureSetup) -> Vec<String> {
    let mut a = Vec::new();
    for t in &c.audio {
        a.extend(strs(&[
            "-f",
            "s16le",
            "-ar",
            "48000",
            "-ch_layout",
            "stereo",
            "-thread_queue_size",
            "1024",
            "-i",
        ]));
        a.push(t.pipe.clone());
    }
    a
}

/// Maps the video with its wall-clock timestamps kept (`passthrough`) when there is audio to
/// line up with: re-gridding to a constant frame rate drifted the picture by about 100 ms over
/// 20 s. Empty without audio: ffmpeg then maps the filter graph's only output by itself.
pub fn map_args(c: &CaptureSetup) -> Vec<String> {
    if c.audio.is_empty() {
        return Vec::new();
    }
    // A 90 kHz encoder time base (the MPEG-TS clock): with the default 1/fps, two frames
    // stamped within one frame period would collide.
    strs(&[
        "-map",
        "[v]",
        "-fps_mode:v",
        "passthrough",
        "-enc_time_base:v",
        "1/90000",
    ])
}

/// The audio ring buffer folder inside `buffer_dir`.
pub const AUDIO_DIR: &str = "audio";

/// The audio recorder's command line: every pipe encoded as an AAC track (titled Game, Mic,
/// Comms) into its own ring of 2 s segments in `buffer_dir/audio`, with timestamps counted
/// from the run's origin like the video's.
///
/// Audio runs in its own ffmpeg because one ffmpeg keeps all its outputs within 100 ms of each
/// other: audio, which arrives a few tens of ms late by nature, made it pause the video, and
/// ddagrab then captured 70–140 fps instead of 240. The saver joins the two by timestamp.
pub fn audio_capture_args(c: &CaptureSetup) -> Vec<String> {
    // "error": the segment muxer warns "frame size not set" for AAC on every segment.
    let mut a = strs(&["-hide_banner", "-loglevel", "error", "-nostats"]);
    a.extend(audio_input_args(c));
    for k in 0..c.audio.len() {
        a.extend(["-map".into(), format!("{k}:a")]);
    }
    a.extend(strs(&["-c:a", "aac", "-b:a", AUDIO_BITRATE]));
    for (k, t) in c.audio.iter().enumerate() {
        a.extend([format!("-metadata:s:a:{k}"), format!("title={}", t.title)]);
    }
    let in_audio = CaptureSetup {
        buffer_dir: c.buffer_dir.join(AUDIO_DIR),
        ..c.clone()
    };
    a.extend(segment_args(&in_audio));
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
    a.extend(map_args(c));
    a.extend(encoder_args(c));
    a.extend(segment_args(c));
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Codec, Preset};

    fn amf() -> CaptureSetup {
        CaptureSetup {
            output: 0,
            fps: 120,
            encoder: Encoder::Amf,
            codec: Codec::Hevc,
            qp_i: 16,
            qp_p: 18,
            preset: Preset::Speed,
            buffer_dir: PathBuf::from(r"C:\buf"),
            buffer_secs: 60,
            audio: Vec::new(),
            origin_us: 0,
        }
    }

    fn with_audio(n: usize) -> CaptureSetup {
        let titles = ["Game", "Mic", "Comms"];
        CaptureSetup {
            audio: (0..n)
                .map(|k| AudioInput {
                    pipe: format!(r"\\.\pipe\retake_{k}"),
                    title: titles[k],
                })
                .collect(),
            origin_us: 1_700_000_000_000_000,
            ..amf()
        }
    }

    #[test]
    fn video_with_audio_keeps_wall_clock_timestamps() {
        let a = capture_args(&with_audio(2));
        let v = a.iter().position(|s| s == "-c:v").unwrap();
        assert_eq!(
            a[6..v],
            [
                "-init_hw_device",
                "d3d11va",
                "-filter_complex",
                "ddagrab=output_idx=0:framerate=120:draw_mouse=0,\
                 setpts=(RTCTIME-1700000000000000)/(TB*1000000)[v]",
                "-map",
                "[v]",
                "-fps_mode:v",
                "passthrough",
                "-enc_time_base:v",
                "1/90000",
            ]
        );
        // No audio in the video recorder; everything from the encoder on is unchanged.
        assert!(!a.iter().any(|s| s.contains("pipe")));
        let plain = capture_args(&amf());
        let pv = plain.iter().position(|s| s == "-c:v").unwrap();
        assert_eq!(a[v..], plain[pv..]);
    }

    #[test]
    fn audio_recorder_writes_its_own_ring() {
        let a = audio_capture_args(&with_audio(2));
        let seg = a.iter().position(|s| s == "-f").unwrap();
        assert_eq!(
            a[..seg + 16],
            [
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostats",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ch_layout",
                "stereo",
                "-thread_queue_size",
                "1024",
                "-i",
                r"\\.\pipe\retake_0",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ch_layout",
                "stereo",
            ]
        );
        assert!(a.windows(2).any(|w| w == ["-map", "1:a"]));
        assert!(a.windows(2).any(|w| w == ["-metadata:s:a:1", "title=Mic"]));
        assert!(a.windows(2).any(|w| w == ["-b:a", "256k"]));
        assert!(a.contains(&r"C:\buf\audio\index.csv".to_string()));
        assert_eq!(a.last().unwrap(), r"C:\buf\audio\seg_%02d.ts");
    }

    #[test]
    fn comms_is_a_third_track() {
        let a = audio_capture_args(&with_audio(3));
        assert!(a.windows(2).any(|w| w == ["-map", "2:a"]));
        assert!(
            a.windows(2)
                .any(|w| w == ["-metadata:s:a:2", "title=Comms"])
        );
        assert_eq!(a.iter().filter(|s| *s == "-i").count(), 3);
    }
    #[test]
    fn qsv_with_audio_labels_after_hwmap() {
        let c = CaptureSetup {
            encoder: Encoder::Qsv,
            ..with_audio(1)
        };
        let i = input_args(&c);
        assert!(
            i.last()
                .unwrap()
                .ends_with(",hwmap=derive_device=qsv,format=qsv[v]")
        );
        assert!(i.last().unwrap().contains(",setpts=(RTCTIME-"));
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
    fn x264_downloads_frames_and_uses_a_fixed_qp() {
        let c = CaptureSetup {
            encoder: Encoder::X264,
            ..amf()
        };
        assert_eq!(
            input_args(&c),
            [
                "-init_hw_device",
                "d3d11va",
                "-filter_complex",
                "ddagrab=output_idx=0:framerate=120:draw_mouse=0,hwdownload,format=bgra,format=yuv420p",
            ]
        );
        assert_eq!(
            encoder_args(&c),
            [
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-qp",
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
    fn preset_maps_per_encoder() {
        let want = [
            (
                Encoder::Amf,
                ["-quality", "speed"],
                ["-quality", "balanced"],
                ["-quality", "quality"],
            ),
            (
                Encoder::Qsv,
                ["-preset", "veryfast"],
                ["-preset", "medium"],
                ["-preset", "veryslow"],
            ),
            (
                Encoder::Nvenc,
                ["-preset", "p1"],
                ["-preset", "p4"],
                ["-preset", "p7"],
            ),
        ];
        for (encoder, s, b, q) in want {
            for (preset, pair) in [
                (Preset::Speed, s),
                (Preset::Balanced, b),
                (Preset::Quality, q),
            ] {
                let a = encoder_args(&CaptureSetup {
                    encoder,
                    preset,
                    ..amf()
                });
                assert!(
                    a.windows(2).any(|w| w == pair),
                    "{encoder} {preset:?}: {a:?}"
                );
            }
        }
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
