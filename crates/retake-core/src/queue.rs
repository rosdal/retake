//! The grading and export queue, persisted as `clips/queue.json` so jobs survive a restart.
//! A clip's exports wait for its grade; a failed job is retried once, then marked failed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "preset")]
pub enum Task {
    Grade,
    Export(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// The raw clip the job is about.
    pub clip: PathBuf,
    pub task: Task,
    pub status: Status,
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Attempts before a job is marked failed.
pub const MAX_ATTEMPTS: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Queue {
    pub jobs: Vec<Job>,
}

impl Queue {
    pub fn from_json(text: &str) -> Result<Queue, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{\"jobs\":[]}".into())
    }

    fn push(&mut self, clip: &Path, task: Task) {
        let exists = self
            .jobs
            .iter()
            .any(|j| j.clip == clip && j.task == task && j.status == Status::Pending);
        if !exists {
            self.jobs.push(Job {
                clip: clip.to_path_buf(),
                task,
                status: Status::Pending,
                attempts: 0,
                error: None,
            });
        }
    }

    /// Queues a newly saved clip: its grade (if grading is on), then each export preset.
    pub fn add_clip(&mut self, clip: &Path, grade: bool, presets: &[String]) {
        if grade {
            self.push(clip, Task::Grade);
        }
        for p in presets {
            self.push(clip, Task::Export(p.clone()));
        }
    }

    /// Queues a regrade of `clip` even if it was graded before.
    pub fn add_regrade(&mut self, clip: &Path) {
        self.push(clip, Task::Grade);
    }

    /// The next job to run: the first pending one whose clip has no grade still pending ahead
    /// of an export. Exports are skipped unless `exports` (`export.when = "manual"` until the
    /// user asks).
    pub fn next(&self, exports: bool) -> Option<usize> {
        self.jobs.iter().enumerate().position(|(i, j)| {
            j.status == Status::Pending
                && match j.task {
                    Task::Grade => true,
                    Task::Export(_) if !exports => false,
                    Task::Export(_) => !self.jobs.iter().take(i).any(|g| {
                        g.clip == j.clip && g.task == Task::Grade && g.status == Status::Pending
                    }),
                }
        })
    }

    /// Records a run of job `i`. A failure stays pending for one retry, then is marked failed.
    pub fn finish(&mut self, i: usize, result: Result<(), String>) {
        let Some(j) = self.jobs.get_mut(i) else {
            return;
        };
        j.attempts += 1;
        match result {
            Ok(()) => {
                j.status = Status::Done;
                j.error = None;
            }
            Err(e) => {
                j.error = Some(e);
                if j.attempts >= MAX_ATTEMPTS {
                    j.status = Status::Failed;
                }
            }
        }
    }

    pub fn pending(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.status == Status::Pending)
            .count()
    }

    pub fn failed(&self) -> usize {
        self.jobs
            .iter()
            .filter(|j| j.status == Status::Failed)
            .count()
    }

    /// Drops finished jobs, keeping failed ones so the tray can report them.
    pub fn prune_done(&mut self) {
        self.jobs.retain(|j| j.status != Status::Done);
    }

    /// Forgets failed jobs (after the user has seen them, or to retry them by re-adding).
    pub fn clear_failed(&mut self) {
        self.jobs.retain(|j| j.status != Status::Failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presets(p: &[&str]) -> Vec<String> {
        p.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn exports_wait_for_the_grade() {
        let mut q = Queue::default();
        q.add_clip(Path::new("a.mkv"), true, &presets(&["edit", "share"]));
        assert_eq!(q.next(true), Some(0));
        q.finish(0, Ok(()));
        assert_eq!(q.next(true), Some(1));
        q.finish(1, Ok(()));
        assert_eq!(q.next(true), Some(2));
        q.finish(2, Ok(()));
        assert_eq!(q.next(true), None);
    }

    #[test]
    fn retry_once_then_failed_and_exports_still_run() {
        let mut q = Queue::default();
        q.add_clip(Path::new("a.mkv"), true, &presets(&["edit"]));
        q.finish(0, Err("boom".into()));
        assert_eq!(q.next(true), Some(0), "one retry");
        q.finish(0, Err("boom again".into()));
        assert_eq!(q.jobs[0].status, Status::Failed);
        assert_eq!(q.failed(), 1);
        // With the grade given up, the export runs (from the raw clip).
        assert_eq!(q.next(true), Some(1));
    }

    #[test]
    fn manual_exports_wait_until_allowed() {
        let mut q = Queue::default();
        q.add_clip(Path::new("a.mkv"), true, &presets(&["edit"]));
        assert_eq!(q.next(false), Some(0));
        q.finish(0, Ok(()));
        assert_eq!(q.next(false), None);
        assert_eq!(q.next(true), Some(1));
    }

    #[test]
    fn json_round_trip_and_no_duplicates() {
        let mut q = Queue::default();
        q.add_clip(Path::new("a.mkv"), true, &presets(&["share"]));
        q.add_clip(Path::new("a.mkv"), true, &presets(&["share"]));
        assert_eq!(q.jobs.len(), 2);
        q.finish(0, Err("x".into()));
        let back = Queue::from_json(&q.to_json()).unwrap();
        assert_eq!(back, q);
        assert!(q.to_json().contains("\"kind\": \"export\""));
    }

    #[test]
    fn grade_off_queues_only_exports_and_prune_keeps_failures() {
        let mut q = Queue::default();
        q.add_clip(Path::new("b.mkv"), false, &presets(&["edit"]));
        assert_eq!(q.jobs.len(), 1);
        assert_eq!(q.jobs[0].task, Task::Export("edit".into()));
        q.finish(0, Ok(()));
        q.add_regrade(Path::new("c.mkv"));
        q.finish(1, Err("a".into()));
        q.finish(1, Err("b".into()));
        q.prune_done();
        assert_eq!(q.jobs.len(), 1);
        assert_eq!(q.pending(), 0);
        q.clear_failed();
        assert!(q.jobs.is_empty());
    }
}
