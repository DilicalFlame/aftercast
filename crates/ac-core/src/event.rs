//! Events: immutable facts appended to `events.jsonl` (schemas/events.schema.json).

use serde::{Deserialize, Serialize};

use crate::time::{Clock, Timestamp};

/// One line of `events.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub v: u8,
    pub id: String,
    pub t: Timestamp,
    pub mono_ms: u64,
    /// Emitting service, e.g. `core`, `tui`, `youtube`.
    pub src: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    pub fn new(clock: &Clock, src: &str, kind: EventKind) -> Self {
        let (t, mono_ms) = clock.now();
        Self {
            v: 1,
            id: ulid::Ulid::generate().to_string(),
            t,
            mono_ms,
            src: src.to_owned(),
            kind,
        }
    }
}

/// Event type and its `data` object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum EventKind {
    #[serde(rename = "session.start")]
    SessionStart {
        episode: u32,
        video_id: String,
        resumed: bool,
    },
    #[serde(rename = "session.end")]
    SessionEnd {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<EndReason>,
    },
    #[serde(rename = "stream.status")]
    StreamStatus {
        status: StreamStatus,
        source: Source,
    },
    #[serde(rename = "stream.ended")]
    StreamEnded { source: Source },
    #[serde(rename = "obs.scene")]
    ObsScene { from: Option<String>, to: String },
    #[serde(rename = "obs.stream")]
    ObsStream { state: OutputState },
    #[serde(rename = "obs.record")]
    ObsRecord {
        state: OutputState,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "checkpoint")]
    Checkpoint {
        cid: String,
        title: String,
        pressed_at: Timestamp,
        by: By,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        latency_corrected_s: Option<f64>,
    },
    #[serde(rename = "checkpoint.edit")]
    CheckpointEdit { cid: String, title: String },
    #[serde(rename = "checkpoint.delete")]
    CheckpointDelete { cid: String },
    #[serde(rename = "mark")]
    Mark {
        #[serde(default)]
        label: Option<String>,
        pressed_at: Timestamp,
        by: By,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay_path: Option<String>,
    },
    #[serde(rename = "note")]
    Note { text: String, pressed_at: Timestamp },
    #[serde(rename = "brb.start")]
    BrbStart {
        #[serde(default)]
        return_scene: Option<String>,
        #[serde(default)]
        countdown_s: Option<f64>,
    },
    #[serde(rename = "brb.end")]
    BrbEnd {},
    #[serde(rename = "privacy.start")]
    PrivacyStart { by: By },
    #[serde(rename = "privacy.end")]
    PrivacyEnd {},
    #[serde(rename = "quest.set")]
    QuestSet {
        #[serde(default)]
        id: Option<String>,
        title: String,
        source: QuestSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extra: Option<serde_json::Map<String, serde_json::Value>>,
    },
    #[serde(rename = "question.add")]
    QuestionAdd {
        qid: String,
        author: String,
        author_id: String,
        text: String,
    },
    #[serde(rename = "question.show")]
    QuestionShow { qid: String },
    #[serde(rename = "question.answered")]
    QuestionAnswered { qid: String },
    #[serde(rename = "question.dismiss")]
    QuestionDismiss { qid: String },
    #[serde(rename = "bot.cmd")]
    BotCmd {
        cmd: String,
        args: String,
        author: String,
        level: Level,
    },
    #[serde(rename = "bot.reply")]
    BotReply {
        cmd: String,
        mode: BotReplyMode,
        text: String,
    },
    #[serde(rename = "mod.action")]
    ModAction {
        action: ModActionKind,
        author_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    #[serde(rename = "desc.rendered")]
    DescRendered {
        chars: usize,
        chapters: usize,
        ok: bool,
        #[serde(default)]
        error: Option<String>,
    },
    #[serde(rename = "desc.pushed")]
    DescPushed { chars: usize, trigger: PushTrigger },
    #[serde(rename = "desc.push_failed")]
    DescPushFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        trigger: PushTrigger,
    },
    #[serde(rename = "title.pushed")]
    TitlePushed { title: String },
    #[serde(rename = "quota.spent")]
    QuotaSpent {
        method: String,
        units: u32,
        remaining: i64,
    },
    #[serde(rename = "latency.measured")]
    LatencyMeasured { latency_s: f64, samples: u32 },
    #[serde(rename = "capture.state")]
    CaptureState {
        keys: CaptureKeys,
        #[serde(default)]
        reason: Option<String>,
    },
    #[serde(rename = "capture.secret_dropped")]
    CaptureSecretDropped { source: CaptureSource },
    #[serde(rename = "service.error")]
    ServiceError {
        service: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        retry_in_s: f64,
    },
    #[serde(rename = "service.state")]
    ServiceState {
        service: String,
        state: ServiceStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndReason {
    Quit,
    Finalized,
    CrashRecovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamStatus {
    Upcoming,
    Live,
    Ended,
}

/// Which service observed a stream status change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Youtube,
    Obs,
    Chat,
}

/// OBS stream/record output state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputState {
    Started,
    Stopped,
    Reconnecting,
    Paused,
    Resumed,
    FileChanged,
}

/// Who triggered an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum By {
    Tui,
    Hotkey,
    Palette,
    Mod,
    OwnerChat,
    ObsScene,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestSource {
    Manual,
    External,
}

/// Chat permission level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Viewer,
    Member,
    Mod,
    Owner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotReplyMode {
    Overlay,
    Chat,
    Both,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModActionKind {
    Delete,
    Ban,
    Timeout,
}

/// What caused a description push.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushTrigger {
    Start,
    End,
    Manual,
    Autosync,
    Cli,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKeys {
    On,
    Paused,
    Off,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSource {
    Keys,
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceStatus {
    Ok,
    Retrying,
    Down,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../tests/fixtures/events.sample.jsonl");

    fn validator() -> jsonschema::Validator {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../schemas/events.schema.json"
        );
        let schema = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        jsonschema::options()
            .should_validate_formats(true)
            .build(&schema)
            .unwrap()
    }

    fn assert_valid(v: &jsonschema::Validator, json: &serde_json::Value) {
        let errors: Vec<String> = v.iter_errors(json).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{json}: {errors:?}");
    }

    #[test]
    fn sample_events_roundtrip_and_validate() {
        let v = validator();
        for line in SAMPLE.lines() {
            let event: Event = serde_json::from_str(line).unwrap();
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(
                serde_json::from_value::<Event>(json.clone()).unwrap(),
                event
            );
            assert_valid(&v, &json);
        }
    }

    #[test]
    fn constructed_events_validate() {
        let v = validator();
        let clock = Clock::new();
        let t = clock.now_ts();
        let kinds = [
            EventKind::SessionStart {
                episode: 1,
                video_id: "abcdefghijk".into(),
                resumed: true,
            },
            EventKind::SessionEnd { reason: None },
            EventKind::StreamStatus {
                status: StreamStatus::Live,
                source: Source::Youtube,
            },
            EventKind::Checkpoint {
                cid: "c".into(),
                title: "Parser".into(),
                pressed_at: t,
                by: By::Tui,
                latency_corrected_s: None,
            },
            EventKind::Mark {
                label: None,
                pressed_at: t,
                by: By::Palette,
                replay_path: None,
            },
            EventKind::BrbEnd {},
            EventKind::DescRendered {
                chars: 10,
                chapters: 0,
                ok: false,
                error: Some("x".into()),
            },
            EventKind::DescPushFailed {
                error: None,
                trigger: PushTrigger::Manual,
            },
            EventKind::QuotaSpent {
                method: "videos.update".into(),
                units: 50,
                remaining: 9950,
            },
            EventKind::ServiceError {
                service: "youtube".into(),
                error: Some("timeout".into()),
                retry_in_s: 2.0,
            },
        ];
        for kind in kinds {
            let event = Event::new(&clock, "core", kind);
            assert_eq!(event.id.len(), 26);
            assert_valid(&v, &serde_json::to_value(&event).unwrap());
        }
    }
}
