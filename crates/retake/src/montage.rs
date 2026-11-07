//! Montages at run time: creating a project from the library, analysing its song, arranging
//! and rendering it. A project is a folder `clips/montages/<date>_<name>/` with `montage.toml`
//! (the project), `song.json` (the analysis) and the renders. Clips are only read.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use retake_core::autoconf::Encoder;
use retake_core::grade::{Cube, builtin_look, contrast_curve, vibrance};
use retake_core::montage::{Format, Pick, Placed, Project, arrange, candidates};
use retake_core::montage_render::{RenderOpts, Source, render_args};
use retake_core::song::{self, Analysis};

use crate::child::ChildJob;
use crate::grader::probe;
use crate::library::Library;

fn job() -> &'static ChildJob {
    static JOB: std::sync::OnceLock<ChildJob> = std::sync::OnceLock::new();
    JOB.get_or_init(|| ChildJob::new().expect("creating the montage job object"))
}

/// The song as mono f32 at the analysis rate.
fn decode_song(ffmpeg: &Path, song_path: &Path) -> Result<Vec<f32>> {
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-i".into(),
        song_path.to_string_lossy().into_owned(),
        "-ac".into(),
        "1".into(),
        "-ar".into(),
        song::RATE.to_string(),
        "-f".into(),
        "f32le".into(),
        "-".into(),
    ];
    let o = job().run_output(ffmpeg, &args)?;
    if !o.status.success() {
        bail!(
            "decoding {}: ffmpeg exited with {}",
            song_path.display(),
            o.status
        );
    }
    Ok(o.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect())
}

/// The song's analysis, cached as `song.json` in the project.
pub fn analysis(ffmpeg: &Path, dir: &Path, p: &Project) -> Result<Analysis> {
    let cache = dir.join("song.json");
    if let Ok(text) = std::fs::read_to_string(&cache)
        && let Ok(a) = serde_json::from_str::<Analysis>(&text)
    {
        return Ok(a);
    }
    let samples = decode_song(ffmpeg, &p.song)?;
    let a = song::analyse(&samples).context("the song is too short or has no beat")?;
    std::fs::write(&cache, serde_json::to_string_pretty(&a)?)?;
    Ok(a)
}

pub fn load(dir: &Path) -> Result<Project> {
    let text = std::fs::read_to_string(dir.join("montage.toml")).context("reading montage.toml")?;
    Project::from_toml(&text).map_err(|e| anyhow::anyhow!("montage.toml: {e}"))
}

pub fn save(dir: &Path, p: &Project) -> Result<()> {
    std::fs::write(dir.join("montage.toml"), p.to_toml()).context("writing montage.toml")
}

/// Creates a project for `song` from the library and returns its folder.
pub fn create(
    lib: &Library,
    montages: &Path,
    song_path: &Path,
    format: Format,
    pick: &Pick,
    name: &str,
) -> Result<PathBuf> {
    let clips = candidates(&lib.list(), pick, format);
    if clips.is_empty() {
        bail!("no clips with kills to pick from ({pick:?})");
    }
    let stamp = crate::clock::local_stamp();
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = montages.join(format!(
        "{:04}-{:02}-{:02}_{safe}",
        stamp.year, stamp.month, stamp.day
    ));
    std::fs::create_dir_all(&dir)?;
    save(&dir, &Project::new(format, song_path, &clips))?;
    Ok(dir)
}

/// A punchier version of the built-in look for montages.
fn montage_look() -> Cube {
    let base = builtin_look(33);
    Cube::from_fn(33, |rgb| {
        let x = base.sample(rgb);
        vibrance(x.map(|v| contrast_curve(v, 1.08)), 0.1)
    })
}

/// Arranges and renders the project in `dir`: `preview.mp4` (rough, small) or `montage.mp4`.
/// Writes the arrangement back into the project and returns the output and the placement.
pub fn render(
    ffmpeg: &Path,
    encoder: Encoder,
    lib: Option<&Library>,
    dir: &Path,
    preview: bool,
) -> Result<(PathBuf, Vec<Placed>)> {
    let mut p = load(dir)?;
    let mut a = analysis(ffmpeg, dir, &p)?;
    if let Some(bpm) = p.bpm {
        // A BPM override re-spaces the beats from the first detected one.
        let first = a.beats.first().copied().unwrap_or(0.0);
        let period = 60.0 / bpm;
        let n = ((a.duration - first) / period) as usize;
        a.beats = (0..n).map(|i| first + i as f64 * period).collect();
        a.bars = (0..a.beats.len()).step_by(4).collect();
        a.bpm = bpm;
        a.drop_bar = a.drop_bar.min(a.bars.len().saturating_sub(1));
    }
    let placed = arrange(&p, &a);
    if placed.is_empty() {
        bail!("nothing could be arranged: no included clip has kills that fit the song");
    }
    for c in p.clips.iter_mut().filter(|c| !c.manual) {
        if let Some(x) = placed.iter().find(|x| x.file == c.file) {
            c.role = Some(x.role);
            c.r#in = x.segs.first().map(|s| s.src0);
            c.out = x.segs.last().map(|s| s.src1);
        }
    }
    save(dir, &p)?;
    let mut sources: Vec<(PathBuf, Source)> = Vec::new();
    let mut durations: Vec<(PathBuf, f64)> = Vec::new();
    for x in &placed {
        if !sources.iter().any(|(f, _)| *f == x.file) {
            let pr = probe(ffmpeg, &x.file)?;
            durations.push((x.file.clone(), pr.duration));
            sources.push((
                x.file.clone(),
                Source {
                    fps: pr.fps,
                    width: pr.width,
                    height: pr.height,
                    has_audio: pr.audio_tracks > 0,
                },
            ));
        }
    }
    let look = dir.join("look.cube");
    std::fs::write(&look, montage_look().write("Retake montage look"))?;
    let start = p.song_start.unwrap_or_else(|| a.window_start(p.length));
    let out = dir.join(if preview {
        "preview.mp4"
    } else {
        "montage.mp4"
    });
    let opts = RenderOpts {
        format: p.format,
        preview,
        encoder,
        look: Some(look),
        game_db: -10.0,
        killfeed: true,
    };
    let args = render_args(&placed, &sources, &p.song, start, p.length, &opts, &out);
    let o = job().run_output(ffmpeg, &args)?;
    if !o.status.success() {
        let err = String::from_utf8_lossy(&o.stderr);
        bail!(
            "rendering the montage: ffmpeg exited with {}: {}",
            o.status,
            err.lines()
                .filter(|l| !l.trim().is_empty())
                .take(6)
                .collect::<Vec<_>>()
                .join(" / ")
        );
    }
    // A Resolve timeline of the same arrangement, for polishing by hand.
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let xml = retake_core::fcpxml::timeline(
        &name, p.format, &placed, &durations, &p.song, start, p.length,
    );
    std::fs::write(dir.join("timeline.fcpxml"), xml)?;
    if !preview && let Some(lib) = lib {
        let used: Vec<PathBuf> = placed.iter().map(|x| x.file.clone()).collect();
        let _ = lib.mark_used(&used);
    }
    Ok((out, placed))
}

/// `retake.exe montage new <song> [--format tiktok|youtube] [--pick starred|auto|<folder>]
/// [--name n]` and `retake.exe montage render <dir> [--preview]`.
pub fn cli(ffmpeg: &Path, encoder: Encoder, clips: &Path, words: &[String]) -> Result<()> {
    let lib = Library::open(clips)?;
    match words.first().map(String::as_str) {
        Some("new") => {
            let song_path = PathBuf::from(words.get(1).context("montage new <song>")?);
            let mut format = Format::Tiktok;
            let mut pick = Pick::Auto;
            let mut name = song_path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let mut it = words[2..].iter();
            while let Some(w) = it.next() {
                match w.as_str() {
                    "--format" => {
                        format = if it.next().is_some_and(|f| f == "youtube") {
                            Format::Youtube
                        } else {
                            Format::Tiktok
                        }
                    }
                    "--pick" => {
                        pick = match it.next().map(String::as_str) {
                            Some("starred") => Pick::Starred,
                            Some("auto") | None => Pick::Auto,
                            Some(folder) => Pick::Folder(PathBuf::from(folder)),
                        }
                    }
                    "--name" => name = it.next().cloned().unwrap_or(name),
                    other => bail!("unknown option {other}"),
                }
            }
            if lib.list().is_empty() {
                let n = lib.rescan(ffmpeg)?;
                println!("library built: {n} clips");
            }
            let dir = create(
                &lib,
                &clips.join("montages"),
                &song_path,
                format,
                &pick,
                &name,
            )?;
            println!("{}", dir.display());
        }
        Some("render") => {
            let dir = PathBuf::from(words.get(1).context("montage render <dir>")?);
            let preview = words.iter().any(|w| w == "--preview");
            let t = std::time::Instant::now();
            let (out, placed) = render(ffmpeg, encoder, Some(&lib), &dir, preview)?;
            for x in &placed {
                for s in &x.segs {
                    println!(
                        "    out {:.2}-{:.2}  src {:.2}-{:.2}  x{:.2}",
                        s.out0,
                        s.out1,
                        s.src0,
                        s.src1,
                        s.speed()
                    );
                }
                println!(
                    "{:?}  {}  kills on {:?}",
                    x.role,
                    x.file.file_name().unwrap_or_default().to_string_lossy(),
                    x.kills
                        .iter()
                        .map(|k| (k.1 * 100.0).round() / 100.0)
                        .collect::<Vec<_>>()
                );
            }
            println!("{} in {:.1} s", out.display(), t.elapsed().as_secs_f32());
        }
        Some("rescan") => println!("{} clips", lib.rescan(ffmpeg)?),
        _ => bail!("usage: retake montage new <song> [...] | render <dir> [--preview] | rescan"),
    }
    Ok(())
}
