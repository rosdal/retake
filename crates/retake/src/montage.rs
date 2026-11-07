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

/// Game looks for montages: profile id → `.cube`, from `[games.<id>] look`.
pub type Looks = std::collections::BTreeMap<String, PathBuf>;

/// The looks set in the config (only files that exist).
pub fn looks_of(config: &retake_core::config::Config) -> Looks {
    config
        .games
        .iter()
        .filter_map(|(id, o)| Some((id.clone(), PathBuf::from(o.look.as_ref()?))))
        .filter(|(_, p)| p.is_file())
        .collect()
}

/// Creates a project for `song` from the library (only `game`'s clips if given; mixed
/// otherwise) and returns its folder.
pub fn create(
    lib: &Library,
    montages: &Path,
    song_path: &Path,
    format: Format,
    pick: &Pick,
    name: &str,
    game: Option<&str>,
) -> Result<PathBuf> {
    let all: Vec<_> = lib
        .list()
        .into_iter()
        .filter(|c| game.is_none_or(|g| c.game == g))
        .collect();
    let clips = candidates(&all, pick, format);
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

static CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Render progress in thousandths (u32::MAX = no render running).
static PROGRESS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(u32::MAX);

/// How far the running render is, 0–1.
pub fn progress() -> Option<f32> {
    let p = PROGRESS.load(std::sync::atomic::Ordering::Relaxed);
    (p != u32::MAX).then(|| p as f32 / 1000.0)
}

/// Stops the running render (it fails with "cancelled").
pub fn cancel() {
    if progress().is_some() {
        CANCEL.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Runs the render, following ffmpeg's `-progress` output (`out_time_us=` against the
/// montage length), and stops it when cancelled.
fn run_with_progress(ffmpeg: &Path, args: &[String], length: f64) -> Result<()> {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;
    use std::sync::atomic::Ordering;
    let mut full: Vec<String> = vec!["-progress".into(), "pipe:1".into(), "-nostats".into()];
    full.extend(args.iter().cloned());
    let mut child = std::process::Command::new(ffmpeg)
        .args(&full)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS
        .creation_flags(0x0800_0000 | 0x0000_4000)
        .spawn()
        .context("starting ffmpeg")?;
    job().adopt(&child);
    CANCEL.store(false, Ordering::Release);
    PROGRESS.store(0, Ordering::Relaxed);
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut last: Vec<String> = Vec::new();
        if let Some(e) = stderr {
            for l in std::io::BufReader::new(e).lines().map_while(Result::ok) {
                if !l.trim().is_empty() {
                    last.push(l);
                    if last.len() > 6 {
                        last.remove(0);
                    }
                }
            }
        }
        last
    });
    let mut cancelled = false;
    if let Some(out) = child.stdout.take() {
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            if let Some(us) = line
                .strip_prefix("out_time_us=")
                .and_then(|v| v.trim().parse::<f64>().ok())
            {
                let p = (us / 1e6 / length.max(0.1)).clamp(0.0, 1.0);
                PROGRESS.store((p * 1000.0) as u32, Ordering::Relaxed);
            }
            if CANCEL.load(Ordering::Acquire) {
                let _ = child.kill();
                cancelled = true;
                break;
            }
        }
    }
    let status = child.wait()?;
    PROGRESS.store(u32::MAX, Ordering::Relaxed);
    let last = errors.join().unwrap_or_default();
    if cancelled {
        bail!("cancelled");
    }
    if !status.success() {
        bail!(
            "rendering the montage: ffmpeg exited with {status}: {}",
            last.join(" / ")
        );
    }
    Ok(())
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
    looks: &Looks,
    dir: &Path,
    preview: bool,
) -> Result<(PathBuf, Vec<Placed>)> {
    let mut p = load(dir)?;
    let mut a = analysis(ffmpeg, dir, &p)?;
    if let Some(bpm) = p.bpm {
        // A BPM override re-spaces the beats from the first detected one.
        a = a.respaced(bpm);
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
        // Each clip in its own game's look first (mixed montages).
        clip_looks: p
            .clips
            .iter()
            .filter_map(|c| Some((c.file.clone(), looks.get(&c.game)?.clone())))
            .collect(),
        game_db: -10.0,
        killfeed: true,
        style: p.style,
    };
    let args = render_args(&placed, &sources, &p.song, start, p.length, &opts, &out);
    run_with_progress(ffmpeg, &args, p.length)?;
    // A Resolve timeline of the same arrangement, for polishing by hand.
    let name = dir
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    // Bars and the drop as markers on the song.
    let mut marks: Vec<(f64, String)> = a
        .bars
        .iter()
        .enumerate()
        .filter_map(|(n, &b)| Some((a.beats.get(b)? - start, format!("Bar {}", n + 1))))
        .collect();
    marks.push((a.drop_time() - start, "Drop".into()));
    let xml = retake_core::fcpxml::timeline(
        &name, p.format, &placed, &durations, &p.song, start, p.length, &marks,
    );
    std::fs::write(dir.join("timeline.fcpxml"), xml)?;
    if !preview && let Some(lib) = lib {
        let used: Vec<PathBuf> = placed.iter().map(|x| x.file.clone()).collect();
        let _ = lib.mark_used(&used);
    }
    Ok((out, placed))
}

/// `retake.exe montage new <song> [--format tiktok|youtube] [--pick starred|auto|<folder>]
/// [--game <id>] [--name n]` and `retake.exe montage render <dir> [--preview]`.
pub fn cli(
    ffmpeg: &Path,
    encoder: Encoder,
    clips: &Path,
    looks: &Looks,
    words: &[String],
) -> Result<()> {
    let lib = Library::open(clips)?;
    match words.first().map(String::as_str) {
        Some("new") => {
            let song_path = PathBuf::from(words.get(1).context("montage new <song>")?);
            let mut format = Format::Tiktok;
            let mut pick = Pick::Auto;
            let mut game: Option<String> = None;
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
                    "--game" => game = it.next().cloned(),
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
                game.as_deref(),
            )?;
            println!("{}", dir.display());
        }
        Some("render") => {
            let dir = PathBuf::from(words.get(1).context("montage render <dir>")?);
            let preview = words.iter().any(|w| w == "--preview");
            let t = std::time::Instant::now();
            let (out, placed) = render(ffmpeg, encoder, Some(&lib), looks, &dir, preview)?;
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

/// What the window asks for; runs on a background thread, one at a time.
#[derive(Clone, Debug, PartialEq)]
pub enum Job {
    /// A new project for this song (then a preview of it).
    New {
        song: PathBuf,
        format: Format,
        starred: bool,
        length: Option<f64>,
        /// Only this game's clips; `None` = mixed.
        game: Option<String>,
    },
    /// Render a project: a preview, or the full montage.
    Render { dir: PathBuf, preview: bool },
    /// Rebuild the clip library from the clips folder.
    Rescan,
}

static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a montage job is running.
pub fn busy() -> bool {
    BUSY.load(std::sync::atomic::Ordering::Acquire)
}

/// Starts `job` in the background; reports through the status board and a toast.
pub fn run_in_background(
    ffmpeg: PathBuf,
    encoder: Encoder,
    lib: Option<std::sync::Arc<Library>>,
    clips: PathBuf,
    looks: Looks,
    job: Job,
) {
    use std::sync::atomic::Ordering;
    if BUSY.swap(true, Ordering::AcqRel) {
        crate::notify::toast("A montage is already being made; try again when it's done.");
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("montage".into())
        .spawn(move || {
            let r = run_job(&ffmpeg, encoder, lib.as_deref(), &clips, &looks, &job);
            BUSY.store(false, Ordering::Release);
            match r {
                Ok(msg) => {
                    crate::status::event(msg.clone());
                    crate::notify::toast(&msg);
                }
                Err(e) if e.to_string() == "cancelled" => {
                    crate::status::event("Montage cancelled");
                    crate::notify::toast("Montage cancelled.");
                }
                Err(e) => {
                    tracing::warn!("montage: {e:#}");
                    let msg = format!("Montage failed: {e:#}");
                    crate::status::event(msg.clone());
                    crate::notify::toast(&msg);
                }
            }
        });
    if let Err(e) = spawned {
        BUSY.store(false, Ordering::Release);
        tracing::warn!("starting the montage thread: {e}");
    }
}

fn run_job(
    ffmpeg: &Path,
    encoder: Encoder,
    lib: Option<&Library>,
    clips: &Path,
    looks: &Looks,
    job: &Job,
) -> Result<String> {
    let lib = lib.context("the clip library is unavailable")?;
    match job {
        Job::New {
            song,
            format,
            starred,
            length,
            game,
        } => {
            if lib.list().is_empty() {
                lib.rescan(ffmpeg)?;
            }
            let pick = if *starred { Pick::Starred } else { Pick::Auto };
            let name = song
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            crate::status::event(format!("Montage: analysing {name}"));
            let dir = create(
                lib,
                &clips.join("montages"),
                song,
                *format,
                &pick,
                &name,
                game.as_deref(),
            )?;
            if let Some(l) = length {
                let mut p = load(&dir)?;
                p.length = l.clamp(5.0, 600.0);
                save(&dir, &p)?;
            }
            let (out, _) = render(ffmpeg, encoder, Some(lib), looks, &dir, true)?;
            Ok(format!("Montage preview ready: {}", out.display()))
        }
        Job::Render { dir, preview } => {
            crate::status::event(format!(
                "Montage: rendering {}{}",
                dir.file_name().unwrap_or_default().to_string_lossy(),
                if *preview { " (preview)" } else { "" }
            ));
            let t = std::time::Instant::now();
            let (out, _) = render(ffmpeg, encoder, Some(lib), looks, dir, *preview)?;
            Ok(format!(
                "Montage ready in {:.0} s: {}",
                t.elapsed().as_secs_f32(),
                out.display()
            ))
        }
        Job::Rescan => Ok(format!("Library rescanned: {} clips", lib.rescan(ffmpeg)?)),
    }
}

/// Projects in `clips/montages`, newest first.
pub fn projects(clips: &Path) -> Vec<(PathBuf, Project)> {
    let Ok(rd) = std::fs::read_dir(clips.join("montages")) else {
        return Vec::new();
    };
    let mut v: Vec<(PathBuf, Project)> = rd
        .flatten()
        .filter_map(|e| {
            let d = e.path();
            load(&d).ok().map(|p| (d, p))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Beat detection on real songs of published tempo: files named `<bpm>_<title>.mp3` in
    /// `RETAKE_BPM_DIR` (e.g. Kevin MacLeod tracks from incompetech, which lists each BPM).
    /// `RETAKE_BPM_DIR=... cargo test -p retake --release -- --ignored known_bpm`
    #[test]
    #[ignore]
    fn known_bpm_songs() {
        let dir = PathBuf::from(std::env::var("RETAKE_BPM_DIR").expect("RETAKE_BPM_DIR"));
        let ffmpeg =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe");
        let mut results = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "mp3") {
                continue;
            }
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let Some(bpm) = name.split('_').next().and_then(|b| b.parse::<f64>().ok()) else {
                continue;
            };
            let samples = decode_song(&ffmpeg, &p).unwrap();
            let a = song::analyse(&samples).expect("analysis");
            results.push((name, bpm, a.bpm));
        }
        for (n, want, got) in &results {
            println!("{n}: published {want} BPM, detected {got:.1}");
        }
        assert!(results.len() >= 5, "need at least 5 songs");
        let near = |got: f64, want: f64| (got - want).abs() / want < 0.02;
        let exact = results.iter().filter(|r| near(r.2, r.1)).count();
        // MIREX "accuracy 2": half or double tempo counts too.
        let octave = results
            .iter()
            .filter(|r| [1.0, 2.0, 0.5].iter().any(|f| near(r.2, r.1 * f)))
            .count();
        println!(
            "exact {exact}/{n}, octave-tolerant {octave}/{n}",
            n = results.len()
        );
        // Where the tempo is right it is right to within 2%; keep at least today's level.
        assert!(octave * 3 >= results.len() * 2, "octave-tolerant below 2/3");
    }
}

#[cfg(test)]
mod golden {
    use super::*;
    use retake_core::montage::{Placed, Role, Seg, Style};

    const W: u32 = 90;
    const H: u32 = 160;

    fn ffmpeg() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe")
    }

    fn run(args: &[String]) {
        let o = std::process::Command::new(ffmpeg())
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    /// A frame of `video` at `at` seconds as tiny RGB.
    fn frame(video: &Path, at: f64) -> Vec<u8> {
        let o = std::process::Command::new(ffmpeg())
            .args(["-v", "error", "-ss", &format!("{at:.3}"), "-i"])
            .arg(video)
            .args([
                "-frames:v",
                "1",
                "-vf",
                &format!("scale={W}:{H}:flags=area"),
                "-pix_fmt",
                "rgb24",
                "-f",
                "rawvideo",
                "-",
            ])
            .output()
            .unwrap();
        assert_eq!(o.stdout.len(), (W * H * 3) as usize);
        o.stdout
    }

    /// Renders a fixed montage from synthetic footage (a moving test pattern and a tone) and
    /// compares three frames with the stored ones in `tests/golden/` (mean difference under
    /// 4/255; hardware encoders differ slightly). `RETAKE_BLESS=1` rewrites them.
    /// `cargo test -p retake --release -- --ignored golden`
    #[test]
    #[ignore]
    fn montage_frames_match_the_golden_ones() {
        let d = tempfile::tempdir().unwrap();
        let clip = d.path().join("clip.mkv");
        let song = d.path().join("song.wav");
        run(&s(&[
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1280x720:r=60:d=6",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=6",
            "-c:v",
            "libx264",
            "-qp",
            "0",
            "-c:a",
            "pcm_s16le",
        ])
        .into_iter()
        .chain([clip.to_string_lossy().into_owned()])
        .collect::<Vec<_>>());
        run(
            &s(&["-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=220:d=8"])
                .into_iter()
                .chain([song.to_string_lossy().into_owned()])
                .collect::<Vec<_>>(),
        );
        let placed = vec![
            Placed {
                file: clip.clone(),
                role: Role::Build,
                segs: vec![Seg {
                    out0: 0.0,
                    out1: 1.5,
                    src0: 0.5,
                    src1: 2.0,
                }],
                kills: vec![(1.8, 1.3)],
            },
            Placed {
                file: clip.clone(),
                role: Role::Drop,
                segs: vec![
                    Seg {
                        out0: 1.5,
                        out1: 2.0,
                        src0: 3.0,
                        src1: 3.5,
                    },
                    Seg {
                        out0: 2.0,
                        out1: 3.0,
                        src0: 3.5,
                        src1: 3.85,
                    },
                ],
                kills: vec![(3.5, 2.0)],
            },
        ];
        let sources = vec![(
            clip.clone(),
            retake_core::montage_render::Source {
                fps: 60.0,
                width: 1280,
                height: 720,
                has_audio: true,
            },
        )];
        let opts = RenderOpts {
            format: Format::Tiktok,
            preview: false,
            encoder: Encoder::X264,
            look: None,
            clip_looks: Vec::new(),
            game_db: -10.0,
            killfeed: false,
            style: Style {
                interpolation: retake_core::montage::Interpolation::Off,
                ..Style::default()
            },
        };
        let out = d.path().join("out.mp4");
        let args = render_args(&placed, &sources, &song, 0.0, 3.0, &opts, &out);
        run(&args);
        let golden = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden");
        let bless = std::env::var_os("RETAKE_BLESS").is_some();
        for (i, at) in [0.4, 1.3, 2.5].iter().enumerate() {
            let got = frame(&out, *at);
            let file = golden.join(format!("montage-{i}.rgb"));
            if bless || !file.exists() {
                std::fs::create_dir_all(&golden).unwrap();
                std::fs::write(&file, &got).unwrap();
                continue;
            }
            let want = std::fs::read(&file).unwrap();
            let diff: f64 = got
                .iter()
                .zip(&want)
                .map(|(a, b)| f64::from(a.abs_diff(*b)))
                .sum::<f64>()
                / got.len() as f64;
            assert!(
                diff < 4.0,
                "frame {i} at {at} s differs by {diff:.2}/255 on average"
            );
        }
    }
}
