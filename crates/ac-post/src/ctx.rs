//! What every stage needs: the session, its events and recordings, the master timeline,
//! paths, tool versions and a progress bar.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ac_core::Home;
use ac_core::config::Config;
use ac_core::event::{Event, EventKind, read_events};
use ac_core::recordings::Recordings;
use ac_core::session::Session;
use ac_core::time::{Recording, Timestamp, rec_t};
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};

use crate::{Result, Stage};

/// `media/master.json`: the editing master and how the recordings map onto it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Master {
    /// Absolute path, or relative to the session folder.
    pub path: String,
    pub duration_s: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub files: Vec<MasterFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MasterFile {
    pub path: String,
    /// Where this file starts on the master timeline.
    pub offset_s: f64,
    pub duration_s: f64,
    /// `stop - start - pauses` from the recording anchors.
    pub expected_s: Option<f64>,
}

pub struct Ctx {
    pub home: Home,
    pub config: Config,
    pub session: Session,
    pub dir: PathBuf,
    pub events: Vec<Event>,
    pub recordings: Vec<Recording>,
    /// `--language` for this run.
    pub language_override: Option<String>,
    /// Show progress bars (off in tests).
    pub progress: bool,
    /// Warnings for the user at the end of the run.
    pub notes: Vec<String>,
    bar: Option<ProgressBar>,
    stage: Option<Stage>,
    tool_versions: BTreeMap<String, String>,
}

impl Ctx {
    pub fn open(home: Home, config: Config, dir: &Path) -> Result<Self> {
        let session = Session::open(dir).map_err(|e| e.to_string())?;
        let events = read_events(&session.events_file()).map_err(|e| e.to_string())?;
        let mut recordings = Recordings::load(&session.recordings_file())
            .map_err(|e| e.to_string())?
            .files;
        recordings.sort_by_key(|r| r.start);
        Ok(Self {
            home,
            config,
            dir: dir.to_path_buf(),
            session,
            events,
            recordings,
            language_override: None,
            progress: true,
            notes: Vec::new(),
            bar: None,
            stage: None,
            tool_versions: BTreeMap::new(),
        })
    }

    pub fn derived(&self, rel: &str) -> PathBuf {
        self.dir.join("derived").join(rel)
    }

    pub fn media(&self, rel: &str) -> PathBuf {
        self.dir.join("media").join(rel)
    }

    pub fn language(&self) -> &str {
        self.language_override
            .as_deref()
            .unwrap_or(&self.config.post.language)
    }

    /// The ingested master; fails before `ingest` ran.
    pub fn master(&self) -> Result<Master> {
        let path = self.media("master.json");
        let text = fs::read_to_string(&path)
            .map_err(|e| format!("{}: {e} (run the ingest stage first)", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The master video file.
    pub fn master_video(&self) -> Result<PathBuf> {
        let m = self.master()?;
        Ok(self.dir.join(m.path))
    }

    /// Wall time to master seconds; `None` outside every recording.
    pub fn rec(&self, t: Timestamp) -> Option<f64> {
        rec_t(&self.recordings, t)
    }

    /// Like [`Ctx::rec`], but times before the first recording map to 0 and after the last
    /// to the end, so ranges that start or end off the recording are clamped.
    pub fn rec_clamped(&self, t: Timestamp, duration: f64) -> f64 {
        if let Some(s) = self.rec(t) {
            return s.min(duration);
        }
        match self.recordings.first() {
            Some(first) if t < first.start => 0.0,
            _ => {
                // Between two files: the end of the earlier one.
                let mut best = 0.0;
                for r in &self.recordings {
                    if let Some(stop) = r.stop
                        && stop <= t
                        && let Some(s) = self.rec(stop)
                    {
                        best = s;
                    }
                }
                if self
                    .recordings
                    .last()
                    .is_some_and(|r| r.stop.is_some_and(|s| t > s))
                {
                    duration
                } else {
                    best
                }
            }
        }
    }

    /// Master ranges between start and end events, e.g. privacy or BRB. An open range at the
    /// end runs to the end of the master.
    pub fn ranges(
        &self,
        duration: f64,
        is_start: impl Fn(&EventKind) -> bool,
        is_end: impl Fn(&EventKind) -> bool,
    ) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        let mut open: Option<f64> = None;
        for e in &self.events {
            if is_start(&e.kind) && open.is_none() {
                open = Some(self.rec_clamped(e.t, duration));
            } else if is_end(&e.kind)
                && let Some(start) = open.take()
            {
                let end = self.rec_clamped(e.t, duration);
                if end > start {
                    out.push((start, end));
                }
            }
        }
        if let Some(start) = open
            && duration > start
        {
            out.push((start, duration));
        }
        out
    }

    /// Privacy ranges on the master: nothing from them may appear in outputs.
    pub fn privacy(&self, duration: f64) -> Vec<(f64, f64)> {
        self.ranges(
            duration,
            |k| matches!(k, EventKind::PrivacyStart { .. }),
            |k| matches!(k, EventKind::PrivacyEnd {}),
        )
    }

    pub fn brb(&self, duration: f64) -> Vec<(f64, f64)> {
        self.ranges(
            duration,
            |k| matches!(k, EventKind::BrbStart { .. }),
            |k| matches!(k, EventKind::BrbEnd {}),
        )
    }

    pub(crate) fn begin(&mut self, stage: Stage) {
        self.stage = Some(stage);
        if self.progress {
            let bar = ProgressBar::new(1000);
            bar.set_style(
                ProgressStyle::with_template("{prefix:>10} [{bar:30}] {percent:>3}% {msg}")
                    .expect("valid template")
                    .progress_chars("=> "),
            );
            bar.set_prefix(stage.name());
            bar.enable_steady_tick(Duration::from_millis(200));
            self.bar = Some(bar);
        }
    }

    pub(crate) fn end(&mut self) {
        if let Some(bar) = self.bar.take() {
            bar.finish();
        }
        self.stage = None;
    }

    /// Progress of the running stage, 0.0 to 1.0.
    pub fn set_progress(&self, fraction: f64) {
        if let Some(bar) = &self.bar {
            bar.set_position((fraction.clamp(0.0, 1.0) * 1000.0) as u64);
        }
    }

    pub fn set_message(&self, msg: impl Into<String>) {
        if let Some(bar) = &self.bar {
            bar.set_message(msg.into());
        }
    }

    /// Log file of the running stage.
    pub fn log_file(&self) -> PathBuf {
        let name = self.stage.map_or("post", |s| s.name());
        self.derived(&format!("logs/{name}.log"))
    }

    pub fn has_tool(&self, tool: &str) -> bool {
        self.tool_versions.contains_key(tool)
    }

    pub fn note_tool(&mut self, tool: &str, version: String) {
        self.tool_versions.insert(tool.into(), version);
    }

    pub(crate) fn take_tool_versions(&mut self) -> BTreeMap<String, String> {
        std::mem::take(&mut self.tool_versions)
    }
}

/// Writes JSON with a trailing newline.
pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether `t` lies inside any of `ranges`.
pub fn inside(ranges: &[(f64, f64)], t: f64) -> bool {
    ranges.iter().any(|(a, b)| t >= *a && t < *b)
}

/// Overlap length of `[a, b)` with `ranges`.
pub fn overlap(ranges: &[(f64, f64)], a: f64, b: f64) -> f64 {
    ranges
        .iter()
        .map(|(s, e)| (e.min(b) - s.max(a)).max(0.0))
        .sum()
}
