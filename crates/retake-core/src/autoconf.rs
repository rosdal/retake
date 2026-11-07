//! Zero-setup choices: which encoder, frame rate, monitor and drive to use when the config
//! says `"auto"`. Pure: the binary gathers the facts (GPU vendor, monitors, Siege's window,
//! drives) and these functions decide.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, de};

use crate::config::ALLOWED_FPS;
use crate::display::{OutputInfo, check_output};

/// A config value that is either `"auto"` or given explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auto<T> {
    Auto,
    Value(T),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Auto<T> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw<T> {
            Text(String),
            Value(T),
        }
        match Raw::<T>::deserialize(d)? {
            Raw::Text(s) if s == "auto" => Ok(Auto::Auto),
            Raw::Text(s) => Err(de::Error::custom(format!(
                "expected \"auto\" or a number, got \"{s}\""
            ))),
            Raw::Value(v) => Ok(Auto::Value(v)),
        }
    }
}

impl<T: fmt::Display> fmt::Display for Auto<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Auto::Auto => f.write_str("auto"),
            Auto::Value(v) => v.fmt(f),
        }
    }
}

/// `capture.encoder` as written in the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EncoderSetting {
    Auto,
    Amf,
    Qsv,
    Nvenc,
    /// CPU H.264, for testing on a PC without a hardware encoder.
    X264,
}

/// The hardware encoder family ffmpeg uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    /// AMD Advanced Media Framework.
    Amf,
    /// Intel Quick Sync Video.
    Qsv,
    /// NVIDIA NVENC.
    Nvenc,
    /// CPU libx264 `ultrafast` at up to 60 fps: testing only, it costs game performance.
    X264,
}

impl fmt::Display for Encoder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Encoder::Amf => "amf",
            Encoder::Qsv => "qsv",
            Encoder::Nvenc => "nvenc",
            Encoder::X264 => "x264",
        })
    }
}

/// A GPU maker, from the DXGI adapter's PCI vendor id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    Amd,
    Intel,
    Nvidia,
    Other(u32),
}

impl Vendor {
    pub fn from_id(id: u32) -> Vendor {
        match id {
            0x1002 | 0x1022 => Vendor::Amd,
            0x8086 => Vendor::Intel,
            0x10DE => Vendor::Nvidia,
            other => Vendor::Other(other),
        }
    }
}

impl fmt::Display for Vendor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Vendor::Amd => f.write_str("AMD"),
            Vendor::Intel => f.write_str("Intel"),
            Vendor::Nvidia => f.write_str("NVIDIA"),
            Vendor::Other(id) => write!(f, "vendor 0x{id:04X}"),
        }
    }
}

/// The encoder for `setting`: an explicit choice wins; `auto` follows the GPU maker.
pub fn pick_encoder(setting: EncoderSetting, vendor: Vendor) -> Result<Encoder, String> {
    Ok(match (setting, vendor) {
        (EncoderSetting::Amf, _) => Encoder::Amf,
        (EncoderSetting::Qsv, _) => Encoder::Qsv,
        (EncoderSetting::Nvenc, _) => Encoder::Nvenc,
        (EncoderSetting::X264, _) => Encoder::X264,
        (EncoderSetting::Auto, Vendor::Amd) => Encoder::Amf,
        (EncoderSetting::Auto, Vendor::Intel) => Encoder::Qsv,
        (EncoderSetting::Auto, Vendor::Nvidia) => Encoder::Nvenc,
        (EncoderSetting::Auto, Vendor::Other(_)) => {
            return Err(format!(
                "capture.encoder = auto: the GPU ({vendor}) has no supported hardware encoder; \
                 set capture.encoder to amf, qsv or nvenc, or to x264 to test on the CPU \
                 (60 fps at most; it costs game performance)"
            ));
        }
    })
}

/// The monitor and rate one recording uses, and what [`check_output`] says about the monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorChoice {
    pub output: u32,
    pub fps: u32,
    /// `Err` if the monitor can't be recorded; otherwise its warnings.
    pub check: Result<Vec<String>, String>,
}

/// Picks the monitor ([`pick_output`]) and rate ([`pick_fps`] from its refresh rate) and
/// checks the monitor. `None` if there are no monitors.
pub fn pick_monitor(
    output: Auto<u32>,
    fps: Auto<u32>,
    outputs: &[OutputInfo],
    game_monitor: Option<isize>,
) -> Option<MonitorChoice> {
    let output = pick_output(output, outputs, game_monitor)?;
    let refresh = outputs
        .iter()
        .find(|o| o.index == output)
        .map_or(0, |o| o.refresh_hz);
    let fps = pick_fps(fps, refresh);
    Some(MonitorChoice {
        output,
        fps,
        check: check_output(outputs, output, fps),
    })
}

/// The capture rate for `setting`: an explicit value as is; `auto` is the fastest allowed rate
/// the monitor shows (1 Hz of slack, since Windows reports 143.98 Hz as 143), at least 60.
pub fn pick_fps(setting: Auto<u32>, refresh_hz: u32) -> u32 {
    match setting {
        Auto::Value(v) => v,
        Auto::Auto => ALLOWED_FPS
            .iter()
            .copied()
            .filter(|&f| f <= refresh_hz + 1)
            .max()
            .unwrap_or(ALLOWED_FPS[0]),
    }
}

/// The `ddagrab` output for `setting`: an explicit index as is; `auto` is the output showing
/// `game_monitor` (Siege's window), else the primary monitor, else the first. `None` if there
/// are no outputs at all.
pub fn pick_output(
    setting: Auto<u32>,
    outputs: &[OutputInfo],
    game_monitor: Option<isize>,
) -> Option<u32> {
    if let Auto::Value(v) = setting {
        return Some(v);
    }
    game_monitor
        .and_then(|m| outputs.iter().find(|o| o.monitor != 0 && o.monitor == m))
        .or_else(|| outputs.iter().find(|o| o.primary))
        .or_else(|| outputs.first())
        .map(|o| o.index)
}

/// While recording on `current` with `output = auto`: the output to move to because Siege's
/// window is now on another known monitor, if it is.
pub fn follow_game(
    current: u32,
    setting: Auto<u32>,
    outputs: &[OutputInfo],
    game_monitor: Option<isize>,
) -> Option<u32> {
    if setting != Auto::Auto {
        return None;
    }
    let m = game_monitor?;
    let target = outputs.iter().find(|o| o.monitor != 0 && o.monitor == m)?;
    (target.index != current).then_some(target.index)
}

/// How fast a drive is, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Speed {
    Nvme,
    Ssd,
    Hdd,
    Unknown,
}

impl fmt::Display for Speed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Speed::Nvme => "NVMe",
            Speed::Ssd => "SSD",
            Speed::Hdd => "HDD",
            Speed::Unknown => "unknown type",
        })
    }
}

/// A fixed local drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveInfo {
    /// e.g. `D:\`.
    pub root: PathBuf,
    /// Bytes free to this user.
    pub free: u64,
    pub speed: Speed,
    /// It already holds a `Retake` folder from an earlier run.
    pub used: bool,
}

/// Free space a drive needs to be preferred for the buffer and clips. The 60 s buffer takes
/// 1–2 GB at 1440p240 near-lossless and a 20 s clip about 0.5 GB, so this holds the buffer
/// and a few dozen clips. Below it a drive can still be picked; it just isn't preferred.
pub const AUTO_DRIVE_MIN_FREE: u64 = 20 * 1024 * 1024 * 1024;

/// The drive for `"auto"` folders. A drive Retake already uses keeps being used, so clips
/// don't move to another drive as free space changes. Otherwise: among drives with at least
/// [`AUTO_DRIVE_MIN_FREE`], the fastest, then the one with most free space; if none has that
/// much, the one with most free.
pub fn pick_drive(drives: &[DriveInfo]) -> Option<&DriveInfo> {
    let fastest = |a: &&DriveInfo, b: &&DriveInfo| a.speed.cmp(&b.speed).then(b.free.cmp(&a.free));
    if let Some(d) = drives.iter().filter(|d| d.used).min_by(fastest) {
        return Some(d);
    }
    let roomy = drives
        .iter()
        .filter(|d| d.free >= AUTO_DRIVE_MIN_FREE)
        .min_by(fastest);
    roomy.or_else(|| drives.iter().max_by_key(|d| d.free))
}

/// `(buffer_dir, out_dir)` under `root` for `"auto"`.
pub fn auto_dirs(root: &Path) -> (PathBuf, PathBuf) {
    let base = root.join("Retake");
    (base.join("buffer"), base.join("clips"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    fn out(index: u32, primary: bool, monitor: isize) -> OutputInfo {
        OutputInfo {
            index,
            name: format!(r"\\.\DISPLAY{}", index + 1),
            width: 2560,
            height: 1440,
            refresh_hz: 144,
            hdr: false,
            primary,
            monitor,
        }
    }

    fn drive(root: &str, free_gb: u64, speed: Speed) -> DriveInfo {
        DriveInfo {
            root: PathBuf::from(root),
            free: free_gb * GB,
            speed,
            used: false,
        }
    }

    #[test]
    fn auto_parses_from_text_or_number() {
        #[derive(Deserialize)]
        struct T {
            a: Auto<u32>,
            b: Auto<u32>,
        }
        let t: T = toml::from_str("a = \"auto\"\nb = 144").unwrap();
        assert_eq!((t.a, t.b), (Auto::Auto, Auto::Value(144)));
        let e = toml::from_str::<T>("a = \"fast\"\nb = 1")
            .err()
            .unwrap()
            .to_string();
        assert!(e.contains("expected \"auto\" or a number"), "{e}");
        assert_eq!(Auto::<u32>::Auto.to_string(), "auto");
        assert_eq!(Auto::Value(60).to_string(), "60");
    }

    #[test]
    fn encoder_follows_the_gpu_unless_set() {
        let a = EncoderSetting::Auto;
        assert_eq!(pick_encoder(a, Vendor::from_id(0x1002)), Ok(Encoder::Amf));
        assert_eq!(pick_encoder(a, Vendor::from_id(0x8086)), Ok(Encoder::Qsv));
        assert_eq!(pick_encoder(a, Vendor::from_id(0x10DE)), Ok(Encoder::Nvenc));
        let e = pick_encoder(a, Vendor::from_id(0x1414)).unwrap_err();
        assert!(
            e.contains("vendor 0x1414") && e.contains("capture.encoder"),
            "{e}"
        );
        assert_eq!(
            pick_encoder(EncoderSetting::Amf, Vendor::Intel),
            Ok(Encoder::Amf)
        );
        assert_eq!(
            pick_encoder(EncoderSetting::Nvenc, Vendor::Other(1)),
            Ok(Encoder::Nvenc)
        );
    }

    #[test]
    fn fps_follows_the_refresh_rate() {
        for (hz, fps) in [
            (60, 60),
            (75, 60),
            (119, 120),
            (143, 144),
            (144, 144),
            (165, 165),
            (200, 165),
            (360, 240),
            (0, 60),
        ] {
            assert_eq!(pick_fps(Auto::Auto, hz), fps, "{hz} Hz");
        }
        assert_eq!(pick_fps(Auto::Value(120), 60), 120);
    }

    #[test]
    fn output_follows_the_game_then_primary() {
        let outs = [out(0, false, 11), out(1, true, 22), out(2, false, 33)];
        assert_eq!(pick_output(Auto::Value(2), &outs, Some(11)), Some(2));
        assert_eq!(pick_output(Auto::Auto, &outs, Some(33)), Some(2));
        assert_eq!(pick_output(Auto::Auto, &outs, Some(99)), Some(1));
        assert_eq!(pick_output(Auto::Auto, &outs, None), Some(1));
        let no_primary = [out(4, false, 1), out(5, false, 2)];
        assert_eq!(pick_output(Auto::Auto, &no_primary, None), Some(4));
        assert_eq!(pick_output(Auto::Auto, &[], None), None);
    }

    #[test]
    fn follow_moves_only_when_auto_and_known_and_different() {
        let outs = [out(0, true, 11), out(1, false, 22)];
        assert_eq!(follow_game(0, Auto::Auto, &outs, Some(22)), Some(1));
        assert_eq!(follow_game(1, Auto::Auto, &outs, Some(22)), None);
        assert_eq!(follow_game(0, Auto::Auto, &outs, Some(99)), None);
        assert_eq!(follow_game(0, Auto::Auto, &outs, None), None);
        assert_eq!(follow_game(0, Auto::Value(0), &outs, Some(22)), None);
    }

    #[test]
    fn drive_prefers_fast_with_room() {
        let d = [
            drive("C:\\", 500, Speed::Hdd),
            drive("D:\\", 150, Speed::Nvme),
        ];
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("D:\\"));
        // 50 GB on NVMe is plenty: speed wins over a roomier HDD.
        let d = [
            drive("C:\\", 50, Speed::Nvme),
            drive("E:\\", 400, Speed::Hdd),
        ];
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("C:\\"));
        let d = [
            drive("C:\\", 15, Speed::Nvme),
            drive("E:\\", 400, Speed::Hdd),
        ];
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("E:\\"));
        let d = [
            drive("C:\\", 120, Speed::Ssd),
            drive("D:\\", 300, Speed::Ssd),
        ];
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("D:\\"));
        let d = [
            drive("C:\\", 10, Speed::Nvme),
            drive("D:\\", 15, Speed::Hdd),
        ];
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("D:\\"));
        assert_eq!(pick_drive(&[]), None);
    }

    #[test]
    fn drive_already_used_stays_picked() {
        let mut d = [
            drive("C:\\", 15, Speed::Nvme),
            drive("D:\\", 900, Speed::Hdd),
        ];
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("D:\\"));
        d[0].used = true;
        assert_eq!(pick_drive(&d).unwrap().root, PathBuf::from("C:\\"));
    }

    #[test]
    fn auto_dirs_live_under_retake() {
        let (b, c) = auto_dirs(Path::new("D:\\"));
        assert_eq!(b, PathBuf::from(r"D:\Retake\buffer"));
        assert_eq!(c, PathBuf::from(r"D:\Retake\clips"));
    }
}
