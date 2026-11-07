//! Grading and exporting saved clips. Runs only from the background queue, so never while the
//! game runs (quiet mode). A failed grade never touches the raw clip: everything is written to
//! `clips/tmp/` first and only renamed into place once ffprobe has checked it.

use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use retake_core::autoconf::Encoder;
use retake_core::config::{BUILTIN_LOOK, Grade, GradeOutput, MotionBlur};
use retake_core::export::{ExportSetup, Preset, Weighting, export_args, export_name, grade_args};
use retake_core::grade::{
    Cube, Histogram, Params, SIEGE_HUD, Stats, builtin_look, compose, correction,
};
use windows::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW};

/// Analysis frames are keyframes, scaled to 640 px wide.
const ANALYSIS_WIDTH: u32 = 640;
/// Grid points of the per-clip LUT.
const LUT_SIZE: usize = 33;

pub struct Grader {
    pub ffmpeg: PathBuf,
    pub encoder: Encoder,
    pub grade: Grade,
    pub blur: MotionBlur,
    /// `clips/` (holds `raw/`, `tmp/`, `graded/`).
    pub out_dir: PathBuf,
}

/// Facts about a clip from ffprobe.
#[derive(Debug, Clone, Copy)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64,
    pub audio_tracks: usize,
}

fn ffprobe(ffmpeg: &Path) -> PathBuf {
    ffmpeg.with_file_name("ffprobe.exe")
}

/// Runs a tool to completion at below-normal priority with no window; fails on a non-zero exit.
fn run(exe: &Path, args: &[String]) -> Result<Vec<u8>> {
    let out = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0 | BELOW_NORMAL_PRIORITY_CLASS.0)
        .output()
        .with_context(|| format!("running {}", exe.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let last: Vec<&str> = err.lines().rev().take(3).collect();
        bail!(
            "{} exited with {}: {}",
            exe.file_name().unwrap_or_default().to_string_lossy(),
            out.status,
            last.into_iter().rev().collect::<Vec<_>>().join(" / ")
        );
    }
    Ok(out.stdout)
}

pub fn probe(ffmpeg: &Path, clip: &Path) -> Result<Probe> {
    let p = |args: &[&str]| -> Result<String> {
        let mut a: Vec<String> = ["-v", "error"].map(String::from).to_vec();
        a.extend(args.iter().map(|s| (*s).to_owned()));
        a.extend([
            "-of".into(),
            "csv=p=0".into(),
            clip.to_string_lossy().into_owned(),
        ]);
        Ok(String::from_utf8_lossy(&run(&ffprobe(ffmpeg), &a)?)
            .trim()
            .to_owned())
    };
    let v = p(&[
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height,avg_frame_rate",
    ])?;
    let mut f = v.split(',');
    let width = f
        .next()
        .and_then(|s| s.parse().ok())
        .context("no video width")?;
    let height = f
        .next()
        .and_then(|s| s.parse().ok())
        .context("no video height")?;
    let fps = f
        .next()
        .and_then(|r| {
            let (n, d) = r.split_once('/')?;
            let (n, d): (f64, f64) = (n.parse().ok()?, d.parse().ok()?);
            (d > 0.0).then(|| n / d)
        })
        .unwrap_or(60.0);
    let duration = p(&["-show_entries", "format=duration"])?
        .parse()
        .unwrap_or(0.0);
    let audio_tracks = p(&["-select_streams", "a", "-show_entries", "stream=index"])?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    Ok(Probe {
        width,
        height,
        fps,
        duration,
        audio_tracks,
    })
}

impl Grader {
    fn params(&self) -> Params {
        let g = &self.grade;
        Params {
            auto_exposure: g.auto_exposure,
            exposure_amount: g.exposure_amount,
            auto_white_bal: g.auto_white_bal,
            wb_amount: g.wb_amount,
            contrast: g.contrast,
            vibrance: g.vibrance,
            look_strength: g.look_strength,
        }
    }

    /// The look LUT: the built-in one, or the configured `.cube` file.
    pub fn look(&self) -> Result<Cube> {
        if self.grade.look == Path::new(BUILTIN_LOOK) {
            return Ok(builtin_look(LUT_SIZE));
        }
        let text = fs::read_to_string(&self.grade.look)
            .with_context(|| format!("reading {}", self.grade.look.display()))?;
        Cube::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", self.grade.look.display()))
    }

    /// Luma percentiles and midtone cast of `clip`, from its keyframes at 640 px wide.
    pub fn analyse(&self, clip: &Path, p: &Probe) -> Result<Stats> {
        let w = ANALYSIS_WIDTH;
        let h = ((u64::from(p.height) * u64::from(w) / u64::from(p.width.max(1))) as u32 / 2 * 2)
            .max(2);
        let args: Vec<String> = vec![
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-hwaccel".into(),
            "d3d11va".into(),
            // Keyframes only (one per 2 s segment in Retake's clips): enough for one
            // constant grade per clip, without decoding every frame.
            "-skip_frame".into(),
            "nokey".into(),
            "-i".into(),
            clip.to_string_lossy().into_owned(),
            "-map".into(),
            "0:v:0".into(),
            "-fps_mode".into(),
            "passthrough".into(),
            "-vf".into(),
            format!("scale={w}:{h}:in_color_matrix=bt709:in_range=tv,format=rgb24"),
            "-f".into(),
            "rawvideo".into(),
            "-".into(),
        ];
        let raw = run(&self.ffmpeg, &args)?;
        let frame = (w * h * 3) as usize;
        let mask: &[_] = if self.grade.hud_mask { &SIEGE_HUD } else { &[] };
        let mut hist = Histogram::default();
        for f in raw.chunks_exact(frame) {
            hist.add_rgb24(f, w as usize, h as usize, mask);
        }
        hist.stats().context("the clip has no frames to measure")
    }

    /// Grades `clip` (a raw clip): measure, compose the per-clip LUT, render, check, publish.
    /// Returns the graded file. The raw clip is only replaced with `output = "overwrite"`, and
    /// only after the graded file passed the check.
    pub fn grade(&self, clip: &Path) -> Result<PathBuf> {
        let p = probe(&self.ffmpeg, clip)?;
        let stats = self.analyse(clip, &p)?;
        let params = self.params();
        let corr = correction(&stats, &params);
        let look = self.look()?;
        let lut = compose(&corr, &params, Some(&look), LUT_SIZE);
        let stem = clip
            .file_stem()
            .context("clip has no name")?
            .to_string_lossy()
            .into_owned();
        let tmp = self.out_dir.join("tmp");
        fs::create_dir_all(&tmp)?;
        let cube = tmp.join(format!("{stem}.cube"));
        fs::write(&cube, lut.write(&stem)).context("writing the clip LUT")?;
        // Next to the clip, so publishing is a rename on the same drive (atomic).
        let work = clip.with_file_name(format!("{stem}.grading.mkv"));
        let protect: &[_] = if self.grade.hud_protect {
            &SIEGE_HUD
        } else {
            &[]
        };
        let args = grade_args(
            clip,
            &cube,
            self.grade.sharpen,
            self.grade.bit_depth,
            self.encoder,
            protect,
            &work,
        );
        let rendered = run(&self.ffmpeg, &args);
        let _ = fs::remove_file(&cube);
        if let Err(e) = rendered {
            let _ = fs::remove_file(&work);
            return Err(e.context("rendering the graded clip"));
        }
        let check = probe(&self.ffmpeg, &work);
        let ok = matches!(&check, Ok(g) if (g.duration - p.duration).abs() <= 0.1 && g.audio_tracks == p.audio_tracks);
        if !ok {
            let _ = fs::remove_file(&work);
            bail!(
                "the graded file failed its check (raw {:.2} s / {} audio, graded {:?}); raw clip kept",
                p.duration,
                p.audio_tracks,
                check.map(|g| (g.duration, g.audio_tracks)).ok()
            );
        }
        let dest = match self.grade.output {
            GradeOutput::SaveAs => clip.with_file_name(format!("{stem}{}.mkv", self.grade.suffix)),
            GradeOutput::Overwrite => clip.to_path_buf(),
        };
        fs::rename(&work, &dest).with_context(|| format!("publishing {}", dest.display()))?;
        self.write_sidecar(clip, &stats, &corr)?;
        Ok(dest)
    }

    fn write_sidecar(
        &self,
        clip: &Path,
        s: &Stats,
        c: &retake_core::grade::Correction,
    ) -> Result<()> {
        let json = serde_json::json!({
            "grade": {
                "black": s.black, "median": s.median, "white": s.white,
                "mid_mean": s.mid_mean, "gamma": c.gamma, "gains": c.gains,
                "look": self.grade.look.to_string_lossy(), "look_strength": self.grade.look_strength,
            }
        });
        let side = clip.with_extension("json");
        // Keep anything else already in the sidecar (later versions add more).
        let mut doc: serde_json::Value = fs::read_to_string(&side)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if let (Some(o), Some(g)) = (doc.as_object_mut(), json.get("grade")) {
            o.insert("grade".into(), g.clone());
        }
        fs::write(&side, serde_json::to_string_pretty(&doc)?).context("writing the sidecar")?;
        Ok(())
    }

    /// The graded version of a raw clip if there is one, else the raw clip itself.
    pub fn source_for_export(&self, clip: &Path) -> PathBuf {
        if self.grade.output == GradeOutput::SaveAs
            && let Some(stem) = clip.file_stem()
        {
            let g = clip.with_file_name(format!(
                "{}{}.mkv",
                stem.to_string_lossy(),
                self.grade.suffix
            ));
            if g.is_file() {
                return g;
            }
        }
        clip.to_path_buf()
    }

    /// Exports `clip` with `preset` into `clips/graded/`.
    pub fn export(&self, clip: &Path, preset: Preset) -> Result<PathBuf> {
        let src = self.source_for_export(clip);
        let p = probe(&self.ffmpeg, &src)?;
        let stem = clip
            .file_stem()
            .context("clip has no name")?
            .to_string_lossy()
            .into_owned();
        let dir = self.out_dir.join("graded");
        fs::create_dir_all(&dir)?;
        let out = dir.join(export_name(&stem, preset));
        let part = out.with_extension(format!("part.{}", preset.ext()));
        let b = &self.blur;
        let blur = (b.enabled && b.apply_to.iter().any(|x| x == preset.name()))
            .then(|| Weighting::parse(&b.weighting).map(|w| (b.output_fps, b.amount, w)))
            .flatten();
        let setup = ExportSetup {
            encoder: self.encoder,
            source_fps: p.fps,
            audio_tracks: p.audio_tracks,
            blur,
            sharpen: self.grade.sharpen,
        };
        let args = export_args(preset, &src, &setup, &part);
        if let Err(e) = run(&self.ffmpeg, &args) {
            let _ = fs::remove_file(&part);
            return Err(e.context(format!("exporting {}", preset.name())));
        }
        fs::rename(&part, &out).with_context(|| format!("publishing {}", out.display()))?;
        Ok(out)
    }
}
