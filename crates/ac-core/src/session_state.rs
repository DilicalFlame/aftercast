//! `SessionState`: derived purely by folding events (specs/01-architecture.md).

use std::collections::{BTreeMap, VecDeque};

use crate::event::{
    By, Event, EventKind, OutputState, PushTrigger, QuestSource, ServiceStatus, StreamStatus,
};
use serde::Serialize;

use crate::time::Timestamp;

/// Events kept for the TUI event log.
const LOG_LEN: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Checkpoint {
    pub cid: String,
    pub title: String,
    pub pressed_at: Timestamp,
    pub by: By,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Quest {
    pub t: Timestamp,
    pub id: Option<String>,
    pub title: String,
    pub source: QuestSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionState {
    Open,
    Shown,
    Answered,
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Question {
    pub qid: String,
    pub author: String,
    pub author_id: String,
    pub text: String,
    /// Super Chat, Super Sticker or membership; `None` for a `!q` question.
    pub support: Option<crate::chat::Support>,
    pub state: QuestionState,
    pub shown_at: Option<Timestamp>,
    /// When it was answered, thanked or dismissed; `None` while it waits.
    pub done_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SessionState {
    pub episode: u32,
    pub video_id: String,
    pub stream: Option<StreamStatus>,
    pub ended: bool,
    /// Live checkpoints in creation order (edits applied, deletes removed).
    pub checkpoints: Vec<Checkpoint>,
    pub marks: Vec<(Timestamp, Option<String>)>,
    pub notes: Vec<(Timestamp, String)>,
    /// Quest changes in order; the last one is current.
    pub quests: Vec<Quest>,
    pub questions: Vec<Question>,
    pub brb_starts: Vec<Timestamp>,
    pub brb_ends: Vec<Timestamp>,
    pub brb: bool,
    /// Scene to return to when BRB ends (from `brb.start`).
    pub brb_return: Option<String>,
    pub privacy: bool,
    /// Current OBS program scene, as last reported.
    pub scene: Option<String>,
    /// Last OBS stream / record output state.
    pub obs_stream: Option<OutputState>,
    pub obs_record: Option<OutputState>,
    /// Last render: (chars, chapters, error).
    pub last_render: Option<(usize, usize, Option<String>)>,
    pub last_push: Option<(Timestamp, PushTrigger)>,
    pub quota_remaining: Option<i64>,
    pub latency_s: Option<f64>,
    pub services: BTreeMap<String, ServiceStatus>,
    #[serde(skip)]
    pub log: VecDeque<Event>,
}

impl SessionState {
    pub fn from_events(events: &[Event]) -> Self {
        let mut state = Self::default();
        events.iter().for_each(|e| state.apply(e));
        state
    }

    pub fn apply(&mut self, e: &Event) {
        match &e.kind {
            EventKind::SessionStart {
                episode, video_id, ..
            } => {
                self.episode = *episode;
                self.video_id.clone_from(video_id);
            }
            EventKind::StreamStatus { status, .. } => {
                self.stream = Some(*status);
                // `live` after an end means OBS started streaming again (a restart).
                self.ended = *status == StreamStatus::Ended;
            }
            EventKind::StreamEnded { .. } => {
                self.stream = Some(StreamStatus::Ended);
                self.ended = true;
            }
            EventKind::Checkpoint {
                cid,
                title,
                pressed_at,
                by,
                ..
            } => {
                self.checkpoints.push(Checkpoint {
                    cid: cid.clone(),
                    title: title.clone(),
                    pressed_at: *pressed_at,
                    by: *by,
                });
            }
            EventKind::CheckpointEdit { cid, title } => {
                if let Some(c) = self.checkpoints.iter_mut().find(|c| &c.cid == cid) {
                    c.title.clone_from(title);
                }
            }
            EventKind::CheckpointDelete { cid } => self.checkpoints.retain(|c| &c.cid != cid),
            EventKind::Mark {
                label, pressed_at, ..
            } => self.marks.push((*pressed_at, label.clone())),
            EventKind::Note { text, pressed_at } => self.notes.push((*pressed_at, text.clone())),
            EventKind::QuestSet {
                id, title, source, ..
            } => self.quests.push(Quest {
                t: e.t,
                id: id.clone(),
                title: title.clone(),
                source: *source,
            }),
            EventKind::QuestionAdd {
                qid,
                author,
                author_id,
                text,
                support,
            } => self.questions.push(Question {
                qid: qid.clone(),
                author: author.clone(),
                author_id: author_id.clone(),
                text: text.clone(),
                support: support.clone(),
                state: QuestionState::Open,
                shown_at: None,
                done_at: None,
            }),
            EventKind::QuestionShow { qid } => {
                self.question(qid, QuestionState::Shown, Some(e.t));
                self.question_done(qid, None);
            }
            EventKind::QuestionAnswered { qid } => {
                self.question(qid, QuestionState::Answered, None);
                self.question_done(qid, Some(e.t));
            }
            EventKind::QuestionDismiss { qid } => {
                self.question(qid, QuestionState::Dismissed, None);
                self.question_done(qid, Some(e.t));
            }
            EventKind::QuestionReopen { qid } => {
                self.question(qid, QuestionState::Open, None);
                self.question_done(qid, None);
            }
            EventKind::BrbStart { return_scene, .. } => {
                self.brb = true;
                self.brb_return.clone_from(return_scene);
                self.brb_starts.push(e.t);
            }
            EventKind::BrbEnd {} => {
                self.brb = false;
                self.brb_ends.push(e.t);
            }
            EventKind::ObsScene { to, .. } => self.scene = Some(to.clone()),
            EventKind::ObsStream { state } => self.obs_stream = Some(*state),
            EventKind::ObsRecord { state, .. } => {
                // A file split does not change whether OBS is recording.
                if *state != OutputState::FileChanged {
                    self.obs_record = Some(*state);
                }
            }
            EventKind::PrivacyStart { .. } => self.privacy = true,
            EventKind::PrivacyEnd {} => self.privacy = false,
            EventKind::DescRendered {
                chars,
                chapters,
                error,
                ..
            } => {
                self.last_render = Some((*chars, *chapters, error.clone()));
            }
            EventKind::DescPushed { trigger, .. } => self.last_push = Some((e.t, *trigger)),
            EventKind::QuotaSpent { remaining, .. } => self.quota_remaining = Some(*remaining),
            EventKind::LatencyMeasured { latency_s, .. } => self.latency_s = Some(*latency_s),
            EventKind::ServiceState { service, state } => {
                self.services.insert(service.clone(), *state);
            }
            EventKind::SessionEnd { .. }
            | EventKind::BotCmd { .. }
            | EventKind::BotReply { .. }
            | EventKind::ModAction { .. }
            | EventKind::DescPushFailed { .. }
            | EventKind::TitlePushed { .. }
            | EventKind::CaptureState { .. }
            | EventKind::CaptureSecretDropped { .. }
            | EventKind::ShellCmd { .. }
            | EventKind::ServiceError { .. } => {}
        }
        if self.log.len() == LOG_LEN {
            self.log.pop_front();
        }
        self.log.push_back(e.clone());
    }

    fn question_done(&mut self, qid: &str, at: Option<Timestamp>) {
        if let Some(q) = self.questions.iter_mut().find(|q| q.qid == qid) {
            q.done_at = at;
        }
    }

    fn question(&mut self, qid: &str, state: QuestionState, shown_at: Option<Timestamp>) {
        if let Some(q) = self.questions.iter_mut().find(|q| q.qid == qid) {
            q.state = state;
            q.shown_at = shown_at.or(q.shown_at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Event, EventLog, read_events};
    use crate::time::Clock;

    #[test]
    fn handled_questions_record_when_and_reopen_clears_it() {
        let clock = Clock::new();
        let mut state = SessionState::default();
        let add = EventKind::QuestionAdd {
            qid: "q1".into(),
            author: "bob".into(),
            author_id: "UCbob".into(),
            text: "why?".into(),
            support: None,
        };
        state.apply(&Event::new(&clock, "bot", add));
        let answered = Event::new(
            &clock,
            "tui",
            EventKind::QuestionAnswered { qid: "q1".into() },
        );
        state.apply(&answered);
        assert_eq!(state.questions[0].state, QuestionState::Answered);
        assert_eq!(state.questions[0].done_at, Some(answered.t));
        state.apply(&Event::new(
            &clock,
            "tui",
            EventKind::QuestionReopen { qid: "q1".into() },
        ));
        assert_eq!(state.questions[0].state, QuestionState::Open);
        assert_eq!(state.questions[0].done_at, None);
    }

    #[test]
    fn replay_restores_checkpoints_with_edits_and_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let clock = Clock::new();
        let cp = |cid: &str, title: &str| EventKind::Checkpoint {
            cid: cid.into(),
            title: title.into(),
            pressed_at: clock.now_ts(),
            by: By::Tui,
            latency_corrected_s: None,
        };
        let kinds = [
            cp("1", "Setup"),
            cp("2", "Parser"),
            cp("3", "Oops"),
            EventKind::CheckpointEdit {
                cid: "2".into(),
                title: "Parser with nom".into(),
            },
            EventKind::CheckpointDelete { cid: "3".into() },
        ];
        let mut live = SessionState::default();
        {
            let mut log = EventLog::open(&path).unwrap();
            for kind in kinds {
                let e = Event::new(&clock, "tui", kind);
                log.append(&e).unwrap();
                live.apply(&e);
            }
        }
        let resumed = SessionState::from_events(&read_events(&path).unwrap());
        assert_eq!(resumed, live);
        let titles: Vec<_> = resumed
            .checkpoints
            .iter()
            .map(|c| c.title.as_str())
            .collect();
        assert_eq!(titles, ["Setup", "Parser with nom"]);
    }
}
