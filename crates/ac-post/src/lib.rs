//! Post-stream pipeline (specs/11-post-pipeline.md): turns a finished session into an
//! AI-ready dataset under `derived/`. Stages run in order and are skipped when their inputs
//! are unchanged; `derived/.stages.json` records each run.

mod activity;
mod ctx;
mod frames;
mod index;
mod ingest;
mod media;
mod secrets;
mod segments;
mod sources;
pub mod tool;
mod transcribe;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use ac_core::time::Timestamp;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub use ctx::{Ctx, Master, overlap, read_json, write_json};
pub use segments::{Kind as SegmentKind, Segment, Segments};
pub use sources::keys;
pub use tool::write_helpers;
pub use transcribe::{Word, Words};

pub type Result<T> = std::result::Result<T, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    Ingest,
    Vod,
    Calibrate,
    Audio,
    Transcribe,
    Silence,
    Git,
    Frames,
    Ocr,
    Activity,
    Segments,
    Secrets,
    Index,
    Library,
}

pub const STAGES: [Stage; 14] = [
    Stage::Ingest,
    Stage::Vod,
    Stage::Calibrate,
    Stage::Audio,
    Stage::Transcribe,
    Stage::Silence,
    Stage::Git,
    Stage::Frames,
    Stage::Ocr,
    Stage::Activity,
    Stage::Segments,
    Stage::Secrets,
    Stage::Index,
    Stage::Library,
];

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Vod => "vod",
            Self::Calibrate => "calibrate",
            Self::Audio => "audio",
            Self::Transcribe => "transcribe",
            Self::Silence => "silence",
            Self::Git => "git",
            Self::Frames => "frames",
            Self::Ocr => "ocr",
            Self::Activity => "activity",
            Self::Segments => "segments",
            Self::Secrets => "secrets",
            Self::Index => "index",
            Self::Library => "library",
        }
    }

    pub fn parse(name: &str) -> Result<Self> {
        STAGES
            .into_iter()
            .find(|s| s.name() == name)
            .ok_or_else(|| {
                let all: Vec<&str> = STAGES.iter().map(|s| s.name()).collect();
                format!("unknown stage {name:?} (stages: {})", all.join(", "))
            })
    }

    /// Stages whose output this one reads; their reruns make this one rerun.
    fn deps(self) -> &'static [Stage] {
        use Stage::*;
        match self {
            Ingest | Vod | Git => &[],
            Calibrate => &[Ingest, Vod],
            Audio | Frames => &[Ingest],
            Transcribe | Silence => &[Audio],
            Ocr => &[Frames],
            Activity => &[Ingest, Calibrate, Transcribe, Git, Frames],
            Segments => &[Activity, Transcribe],
            Secrets => &[Segments, Frames, Ocr, Transcribe],
            Index => &[
                Ingest, Calibrate, Transcribe, Silence, Git, Frames, Ocr, Activity, Segments,
                Secrets,
            ],
            Library => &[Index],
        }
    }

    /// `None` when the stage runs; otherwise why it is skipped.
    fn disabled(self, ctx: &Ctx) -> Option<&'static str> {
        match self {
            Self::Vod | Self::Calibrate if !ctx.config.post.use_vod => Some("post.use_vod = false"),
            Self::Ocr if !ctx.config.post.ocr => Some("post.ocr = false"),
            _ => None,
        }
    }

    /// Session files (outside the pipeline's own outputs) and config this stage reads.
    fn inputs(self, ctx: &Ctx) -> (Vec<PathBuf>, String) {
        let p = &ctx.config.post;
        let events = ctx.dir.join("events.jsonl");
        let session = ctx.dir.join("session.toml");
        match self {
            Self::Ingest => {
                let mut files = vec![ctx.session.recordings_file()];
                files.extend(ctx.recordings.iter().map(|r| PathBuf::from(&r.path)));
                (files, String::new())
            }
            Self::Vod => (vec![], ctx.session.meta.video_id.clone()),
            Self::Calibrate => (vec![session], String::new()),
            Self::Audio => (vec![], String::new()),
            Self::Transcribe => (
                vec![events],
                format!(
                    "{:?} {} {} {} {:?} {:?}",
                    p.transcriber,
                    p.whisper_model,
                    p.whisper_compute_type,
                    ctx.language(),
                    std::env::var(transcribe::FIXTURE_ENV).ok(),
                    transcribe::context(ctx),
                ),
            ),
            Self::Silence => (vec![], format!("{} {}", p.silence_db, p.silence_min_s)),
            Self::Git => (
                vec![events, session],
                format!(
                    "{:?} {}",
                    ctx.config.capture.git_roots, ctx.config.capture.git_max_depth
                ),
            ),
            Self::Frames => (
                vec![events],
                format!(
                    "{} {} {} {}",
                    p.frame_interval_s, p.scene_threshold, p.phash_max_distance, p.frame_width
                ),
            ),
            Self::Ocr => (vec![], String::new()),
            Self::Activity => (
                vec![
                    events,
                    session,
                    ctx.dir.join("keys.jsonl.zst"),
                    ctx.dir.join("shell.jsonl"),
                    ctx.dir.join("chat.jsonl"),
                ],
                ctx.config.stream.default_latency_s.to_string(),
            ),
            Self::Segments => (vec![events], p.focus_min_keys_per_10s.to_string()),
            Self::Secrets => (
                vec![events, ctx.dir.join("shell.jsonl")],
                ctx.config
                    .capture
                    .secret_patterns
                    .iter()
                    .map(|p| p.0.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Self::Index => (
                vec![events, session, ctx.dir.join("chat.jsonl")],
                String::new(),
            ),
            Self::Library => (
                vec![session, ctx.dir.join("ai/suggestions.json")],
                String::new(),
            ),
        }
    }

    fn run(self, ctx: &mut Ctx) -> Result<()> {
        match self {
            Self::Ingest => ingest::run(ctx),
            Self::Vod => media::vod(ctx),
            Self::Calibrate => media::calibrate(ctx),
            Self::Audio => media::audio(ctx),
            Self::Transcribe => transcribe::run(ctx),
            Self::Silence => media::silence(ctx),
            Self::Git => sources::git(ctx),
            Self::Frames => frames::run(ctx),
            Self::Ocr => frames::ocr(ctx),
            Self::Activity => activity::run(ctx),
            Self::Segments => segments::run(ctx),
            Self::Secrets => secrets::run(ctx),
            Self::Index => index::run(ctx),
            Self::Library => ac_library::Library::open(&ctx.home)?.upsert(&ctx.dir),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    Failed,
    Skipped,
}

/// One stage's entry in `derived/.stages.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageRecord {
    pub status: Status,
    pub started: Timestamp,
    pub finished: Timestamp,
    pub inputs_hash: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tool_versions: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Default)]
pub struct Options {
    /// Re-run this stage and every later one.
    pub from: Option<Stage>,
    /// Run only this stage.
    pub only: Option<Stage>,
    /// Re-run even when inputs are unchanged.
    pub force: bool,
}

/// What happened to a stage in this run.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Ran,
    UpToDate,
    Disabled(&'static str),
}

/// Runs the pipeline. Stops at the first failure (recorded in `.stages.json`, with the log
/// path in the error) so a rerun resumes there.
pub fn run(ctx: &mut Ctx, opts: &Options) -> Result<Vec<(Stage, Outcome)>> {
    fs::create_dir_all(ctx.derived("logs")).map_err(|e| format!("creating derived/: {e}"))?;
    let path = ctx.derived(".stages.json");
    let mut records: BTreeMap<String, StageRecord> = fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut report = Vec::new();
    for stage in STAGES {
        if opts.only.is_some_and(|only| only != stage) {
            continue;
        }
        let forced =
            opts.force || opts.only == Some(stage) || opts.from.is_some_and(|from| stage >= from);
        let hash = inputs_hash(ctx, stage, &records);
        let started = Timestamp::now();
        if let Some(why) = stage.disabled(ctx) {
            report.push((stage, Outcome::Disabled(why)));
            // Keep an unchanged record, so stages depending on this one stay up to date.
            if records
                .get(stage.name())
                .is_some_and(|r| r.status == Status::Skipped && r.inputs_hash == hash)
            {
                continue;
            }
            records.insert(
                stage.name().into(),
                StageRecord {
                    status: Status::Skipped,
                    started,
                    finished: started,
                    inputs_hash: hash,
                    tool_versions: BTreeMap::new(),
                    error: None,
                },
            );
            continue;
        }
        let up_to_date = records
            .get(stage.name())
            .is_some_and(|r| r.status == Status::Done && r.inputs_hash == hash);
        if up_to_date && !forced {
            report.push((stage, Outcome::UpToDate));
            continue;
        }
        ctx.begin(stage);
        let result = stage.run(ctx);
        let record = StageRecord {
            status: if result.is_ok() {
                Status::Done
            } else {
                Status::Failed
            },
            started,
            finished: Timestamp::now(),
            inputs_hash: hash,
            tool_versions: ctx.take_tool_versions(),
            error: result.as_ref().err().cloned(),
        };
        ctx.end();
        records.insert(stage.name().into(), record);
        let saved = serde_json::to_string_pretty(&records).expect("records serialize");
        fs::write(&path, saved).map_err(|e| format!("writing {}: {e}", path.display()))?;
        if let Err(e) = result {
            let log = ctx.derived(&format!("logs/{}.log", stage.name()));
            return Err(format!(
                "stage {} failed: {e}\nlog: {}",
                stage.name(),
                log.display()
            ));
        }
        report.push((stage, Outcome::Ran));
    }
    let saved = serde_json::to_string_pretty(&records).expect("records serialize");
    fs::write(&path, saved).map_err(|e| format!("writing {}: {e}", path.display()))?;
    Ok(report)
}

/// Hash of a stage's session files (path, size, modification time), config, and when its
/// dependencies last finished.
fn inputs_hash(ctx: &Ctx, stage: Stage, records: &BTreeMap<String, StageRecord>) -> String {
    let (files, config) = stage.inputs(ctx);
    let mut h = Sha256::new();
    h.update(config.as_bytes());
    for f in files {
        h.update(f.to_string_lossy().as_bytes());
        if let Ok(m) = fs::metadata(&f) {
            h.update(m.len().to_le_bytes());
            if let Ok(t) = m.modified().and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_err(std::io::Error::other)
            }) {
                h.update(t.as_nanos().to_le_bytes());
            }
        }
    }
    for dep in stage.deps() {
        let finished = records
            .get(dep.name())
            .map(|r| format!("{:?} {}", r.status, r.finished.unix_ms()))
            .unwrap_or_default();
        h.update(dep.name().as_bytes());
        h.update(finished.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
