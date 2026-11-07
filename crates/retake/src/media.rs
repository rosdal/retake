//! The grading and export queue at run time: newly saved clips are queued, the queue is kept in
//! `clips/queue.json`, and one job at a time runs on the background runner, so only while the
//! game is closed (quiet mode). If the game starts mid-job, that job finishes and the rest
//! waits.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use retake_core::config::{ExportWhen, GradeMode};
use retake_core::export::Preset;
use retake_core::queue::{Queue, Status, Task};

use crate::background::Submitter;
use crate::grader::Grader;

pub struct Media {
    queue: Mutex<Queue>,
    file: PathBuf,
    grader: Grader,
    jobs: Submitter,
    when: ExportWhen,
    presets: Vec<String>,
    max_raw_gb: f64,
    /// Exports may run now: always with `after_game`; after "Export pending now" with `manual`.
    exports_allowed: AtomicBool,
    /// A step is queued on the background runner.
    scheduled: AtomicBool,
    on_failed: Box<dyn Fn(&str) + Send + Sync>,
}

impl Media {
    /// Loads `clips/queue.json` (a broken or missing file starts empty) and resumes pending
    /// jobs.
    pub fn start(
        grader: Grader,
        jobs: Submitter,
        when: ExportWhen,
        presets: Vec<String>,
        max_raw_gb: f64,
        on_failed: impl Fn(&str) + Send + Sync + 'static,
    ) -> Arc<Media> {
        let file = grader.out_dir.join("queue.json");
        let mut queue = match fs::read_to_string(&file) {
            Ok(text) => Queue::from_json(&text).unwrap_or_else(|e| {
                tracing::warn!("{}: {e}; starting with an empty queue", file.display());
                Queue::default()
            }),
            Err(_) => Queue::default(),
        };
        queue.prune_done();
        let pending = queue.pending();
        let m = Arc::new(Media {
            queue: Mutex::new(queue),
            file,
            grader,
            jobs,
            when,
            presets,
            max_raw_gb,
            exports_allowed: AtomicBool::new(when == ExportWhen::AfterGame),
            scheduled: AtomicBool::new(false),
            on_failed: Box::new(on_failed),
        });
        if pending > 0 {
            tracing::info!(pending, "resuming the grading and export queue");
            m.kick();
        }
        m
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save(&self, q: &Queue) {
        let tmp = self.file.with_extension("json.part");
        let ok = fs::write(&tmp, q.to_json()).and_then(|()| fs::rename(&tmp, &self.file));
        if let Err(e) = ok {
            tracing::warn!("saving {}: {e}", self.file.display());
        }
    }

    /// A clip was saved: queue its grade and exports.
    pub fn clip_saved(self: &Arc<Self>, clip: &Path) {
        let grade = self.grader.grade.mode == GradeMode::On;
        if !grade && self.presets.is_empty() {
            return;
        }
        {
            let mut q = self.lock();
            q.add_clip(clip, grade, &self.presets);
            self.save(&q);
        }
        self.kick();
    }

    /// "Export pending now".
    pub fn export_now(self: &Arc<Self>) {
        self.exports_allowed.store(true, Ordering::Relaxed);
        tracing::info!("exporting pending clips");
        self.kick();
    }

    /// Export `clip` with `preset` now (from the window), whatever `export.when` says.
    pub fn export(self: &Arc<Self>, clip: &Path, preset: &str) {
        {
            let mut q = self.lock();
            q.add_clip(clip, false, &[preset.to_owned()]);
            self.save(&q);
        }
        self.export_now();
    }

    /// "Regrade last clip".
    pub fn regrade(self: &Arc<Self>, clip: &Path) {
        {
            let mut q = self.lock();
            q.add_regrade(clip);
            self.save(&q);
        }
        self.kick();
    }

    fn kick(self: &Arc<Self>) {
        if self.scheduled.swap(true, Ordering::AcqRel) {
            return;
        }
        let me = self.clone();
        self.jobs.submit("grade/export", move || me.step());
    }

    /// Runs one queue job, then queues the next step if there is more to do.
    fn step(self: &Arc<Self>) {
        self.scheduled.store(false, Ordering::Release);
        let exports = self.exports_allowed.load(Ordering::Relaxed);
        let Some((i, job)) = ({
            let q = self.lock();
            q.next(exports).map(|i| (i, q.jobs[i].clone()))
        }) else {
            let pending = self.lock().pending();
            crate::status::update(|b| {
                b.queued = pending;
                b.exporting = false;
            });
            if self.when == ExportWhen::Manual {
                self.exports_allowed.store(false, Ordering::Relaxed);
            }
            return;
        };
        let pending = self.lock().pending();
        crate::status::update(|b| {
            b.queued = pending;
            b.exporting = true;
        });
        let started = std::time::Instant::now();
        let result = if !job.clip.is_file() {
            Err(format!("{} no longer exists", job.clip.display()))
        } else {
            match &job.task {
                Task::Grade => self.grader.grade(&job.clip).map(|out| {
                    tracing::info!(clip = %job.clip.display(), graded = %out.display(), secs = started.elapsed().as_secs_f32(), "clip graded");
                    crate::status::event(format!(
                        "Graded  {}",
                        out.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    self.enforce_raw_limit();
                }),
                Task::Export(p) => match Preset::parse(p) {
                    Some(preset) => self.grader.export(&job.clip, preset).map(|out| {
                        tracing::info!(clip = %job.clip.display(), export = %out.display(), secs = started.elapsed().as_secs_f32(), "clip exported");
                    }),
                    None => Err(anyhow::anyhow!("unknown preset {p}")),
                },
            }
            .map_err(|e| format!("{e:#}"))
        };
        let failed_now = {
            let mut q = self.lock();
            if let Err(e) = &result {
                tracing::warn!(clip = %job.clip.display(), task = ?job.task, "{e}");
            }
            q.finish(i, result);
            let failed = q.jobs.get(i).is_some_and(|j| j.status == Status::Failed);
            q.prune_done();
            self.save(&q);
            failed.then(|| q.failed())
        };
        if let Some(n) = failed_now {
            let what = match job.task {
                Task::Grade => "grade",
                Task::Export(_) => "export",
            };
            (self.on_failed)(&format!(
                "A clip {what} failed twice ({n} failed in total); the raw clip is kept. See the log."
            ));
        }
        self.kick();
    }

    /// With `clips.max_raw_gb` set: delete the oldest raw clips that already have a graded
    /// version until raw clips fit. Never an ungraded clip.
    fn enforce_raw_limit(&self) {
        if self.max_raw_gb <= 0.0 {
            return;
        }
        let raw = self.grader.out_dir.join("raw");
        let suffix = &self.grader.grade.suffix;
        let Ok(entries) = fs::read_dir(&raw) else {
            return;
        };
        let mut clips: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let name = p.file_stem()?.to_string_lossy().into_owned();
                let is_raw =
                    p.extension().is_some_and(|x| x == "mkv") && !name.ends_with(suffix.as_str());
                let meta = e.metadata().ok()?;
                let modified = meta.modified().ok()?;
                is_raw.then_some((modified, meta.len(), p))
            })
            .collect();
        let limit = (self.max_raw_gb * 1024.0 * 1024.0 * 1024.0) as u64;
        let mut total: u64 = clips.iter().map(|c| c.1).sum();
        clips.sort();
        for (_, len, p) in clips {
            if total <= limit {
                break;
            }
            let graded = self.grader.source_for_export(&p) != p;
            if graded && fs::remove_file(&p).is_ok() {
                tracing::info!(clip = %p.display(), "raw clip removed (clips.max_raw_gb); its graded version stays");
                total = total.saturating_sub(len);
            }
        }
    }
}
