//! Chat, bot, questions, latency, current quest and overlays in the controller
//! (specs/09-streambot.md, specs/05-time-sync.md "Latency measurement").

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;

use ac_bot::{Builtin, Cmd, Parsed};
use ac_core::chat::{ChatKind, ChatMessage, Support};
use ac_core::command::QuestionAction;
use ac_core::config::ReplyMode;
use ac_core::event::{
    BotReplyMode, By, EventKind, Level, QuestSource, ServiceStatus, Source, StreamStatus,
};
use ac_core::session::Status;
use ac_core::session_state::QuestionState;
use ac_core::time::{Timestamp, hms};
use ac_server::{Action, OverlayMsg};
use ac_youtube::chat::ChatUpdate;
use serde_json::{Value, json};

use super::Controller;

/// Messages kept for the TUI chat pane.
const CHAT_PANE: usize = 500;
/// Chat reaches aftercast about this long after it was typed (specs/05-time-sync.md).
const CHAT_DELIVERY_S: f64 = 0.5;
const QUESTION_MAX: usize = 300;

/// When a viewer sent `msg`: YouTube's publish time, which quiet-chat reconnect waits do not
/// delay; else arrival minus the usual delivery time. Never later than arrival (clock skew).
fn sent_at(msg: &ChatMessage) -> Timestamp {
    match msg.t_published {
        Some(p) if p <= msg.t_received => p,
        Some(_) => msg.t_received,
        None => msg.t_received.add_secs(-CHAT_DELIVERY_S),
    }
}
/// A latency measurement ends after this many samples.
const LATENCY_SAMPLES: usize = 3;
/// `!chapters` replies are cut to this many characters.
const CHAPTERS_REPLY: usize = 200;

/// A running latency measurement: the overlay counter started at `start` and counts
/// tenths of a second.
pub(super) struct LatencyRun {
    start: Timestamp,
    samples: Vec<f64>,
    /// Chapter to return to when the check ends; `None` when no chapter was added.
    resume: Option<String>,
}

/// Chapter title while a latency measurement runs (`stream.chapter_on_latency`).
const LATENCY_CHAPTER: &str = "Latency check";

impl Controller {
    pub(super) fn on_chat(&mut self, update: ChatUpdate) {
        match update {
            ChatUpdate::Connected => self.service_state("chat", ServiceStatus::Ok),
            ChatUpdate::Down { error, retry_in } => {
                if self.state.services.get("chat") != Some(&ServiceStatus::Retrying) {
                    self.emit(
                        "chat",
                        EventKind::ServiceError {
                            service: "chat".into(),
                            error: Some(error),
                            retry_in_s: retry_in.as_secs_f64(),
                        },
                    );
                    self.service_state("chat", ServiceStatus::Retrying);
                }
            }
            ChatUpdate::Stopped(error) => {
                self.notice = Some(format!("chat stopped: {error}"));
                self.emit(
                    "chat",
                    EventKind::ServiceError {
                        service: "chat".into(),
                        error: Some(error),
                        retry_in_s: 0.0,
                    },
                );
                self.service_state("chat", ServiceStatus::Down);
            }
            ChatUpdate::Offline => {
                self.service_state("chat", ServiceStatus::Down);
                if !self.state.ended {
                    self.emit(
                        "chat",
                        EventKind::StreamEnded {
                            source: Source::Chat,
                        },
                    );
                    self.set_status(Status::Ended);
                }
            }
            ChatUpdate::Message(msg) => self.chat_message(msg),
        }
    }

    fn chat_message(&mut self, msg: ChatMessage) {
        if let Some(log) = &mut self.chat_log {
            let line = serde_json::to_string(&msg).expect("chat serializes");
            if let Err(e) = writeln!(log, "{line}") {
                tracing::error!("writing chat.jsonl: {e}");
            }
        }
        let chat = Arc::make_mut(&mut self.chat);
        if chat.len() == CHAT_PANE {
            chat.pop_front();
        }
        chat.push_back(msg.clone());
        // History YouTube sends on connect is shown, never executed.
        let fresh = msg.t_published.unwrap_or(msg.t_received) >= self.started;
        if fresh && let Some(support) = msg.support() {
            return self.support_add(support, &msg);
        }
        if msg.kind != ChatKind::Text || !fresh {
            return;
        }
        // Cooldowns count from when each message arrived, not when it is handled.
        let at = msg.t_received;
        if let Some(parsed) = self.bot.as_mut().and_then(|bot| bot.parse(&msg, at)) {
            self.run_bot(parsed, &msg);
        }
    }

    fn run_bot(&mut self, p: Parsed, msg: &ChatMessage) {
        self.emit(
            "bot",
            EventKind::BotCmd {
                cmd: p.name.clone(),
                args: p.args.clone(),
                author: msg.author_name.clone(),
                level: p.level,
            },
        );
        let by = if p.level == Level::Owner {
            By::OwnerChat
        } else {
            By::Mod
        };
        // What a mod points at happened `latency` earlier on their screen; the owner
        // acts on the present (specs/05-time-sync.md, "Viewer-perceived time"). The
        // owner's other accounts (`bot.owner_accounts`) watch the delayed stream.
        let correction = if msg.is_owner { 0.0 } else { self.latency() };
        let moment = sent_at(msg).add_secs(-correction);
        let builtin = match p.cmd {
            Cmd::Custom { text, reply } => {
                let text = self.render_reply(&p.name, &text);
                return self.reply(&p.name, text, reply);
            }
            Cmd::Builtin(b) => b,
        };
        match builtin {
            Builtin::Help => {
                let help = self.bot.as_ref().map(|b| b.help()).unwrap_or_default();
                self.reply(&p.name, help, ReplyMode::Chat);
            }
            Builtin::Q => self.question_add(&p.args, msg),
            Builtin::Quest => {
                let text = match self.state.quests.last() {
                    Some(q) => {
                        let mins = self.clock.now_ts().secs_since(q.t) / 60.0;
                        format!("Now: {} (for {mins:.0} min)", q.title)
                    }
                    None => "No quest set right now".into(),
                };
                self.reply(&p.name, text, ReplyMode::Overlay);
            }
            Builtin::Uptime => {
                let text = match self.live_since() {
                    Some(start) => {
                        format!("Live for {}", hms(self.clock.now_ts().secs_since(start)))
                    }
                    None => "Not live yet".into(),
                };
                self.reply(&p.name, text, ReplyMode::Overlay);
            }
            Builtin::Chapters => {
                let text = chapters_reply(&self.chapters);
                self.reply(&p.name, text, ReplyMode::Chat);
            }
            Builtin::Lurk => {
                let text = format!("{} is lurking", msg.author_name);
                self.reply(&p.name, text, ReplyMode::Overlay);
            }
            Builtin::Brb => self.brb(Some(true), by),
            Builtin::Back => self.brb(Some(false), by),
            Builtin::Chapter if p.args.is_empty() => {}
            Builtin::Chapter => {
                let title: String = p.args.chars().take(100).collect();
                self.emit_render(
                    "bot",
                    EventKind::Checkpoint {
                        cid: ulid::Ulid::generate().to_string(),
                        title: title.clone(),
                        pressed_at: moment,
                        by,
                        latency_corrected_s: (correction > 0.0).then_some(correction),
                    },
                );
                self.reply(&p.name, format!("chapter: {title}"), ReplyMode::Overlay);
            }
            Builtin::Mark => self.emit(
                "bot",
                EventKind::Mark {
                    label: Some(p.args).filter(|a| !a.is_empty()),
                    pressed_at: moment,
                    by,
                    replay_path: None,
                },
            ),
            Builtin::Lat => match p.args.parse() {
                Ok(counter) => {
                    self.latency_sample(counter, sent_at(msg));
                }
                Err(_) => self.notice = Some(format!("!lat needs a number, got {:?}", p.args)),
            },
            Builtin::Title if p.args.is_empty() => {}
            Builtin::Title => {
                self.notice = Some(format!("set the YouTube title to {:?}? (y/n)", p.args));
                self.pending_title = Some(p.args);
            }
        }
    }

    /// Renders a custom command's template; a broken template replies with nothing.
    fn render_reply(&mut self, name: &str, src: &str) -> String {
        let (ctx, _) = ac_template::context(&ac_template::ContextInput {
            config: &self.config,
            meta: &self.session.meta,
            state: &self.state,
            now: self.clock.now_ts(),
        });
        self.renderer
            .render_str(&format!("bot.commands.{name}"), src, &ctx)
            .unwrap_or_else(|e| {
                self.notice = Some(format!("!{name}: {e}"));
                String::new()
            })
    }

    /// Sends a bot reply where the chat cap allows; beyond it, to the toast overlay.
    fn reply(&mut self, cmd: &str, text: String, mode: ReplyMode) {
        if text.trim().is_empty() {
            return;
        }
        let now = self.clock.now_ts();
        let Some(route) = self.bot.as_mut().map(|bot| bot.route(mode, now)) else {
            return;
        };
        if route.chat {
            self.send_chat(text.clone());
        }
        if route.overlay {
            self.toast(&text);
        }
        let mode = match (route.overlay, route.chat) {
            (true, true) => BotReplyMode::Both,
            (false, true) => BotReplyMode::Chat,
            (true, false) => BotReplyMode::Overlay,
            (false, false) => BotReplyMode::None,
        };
        self.emit(
            "bot",
            EventKind::BotReply {
                cmd: cmd.into(),
                mode,
                text,
            },
        );
    }

    /// Posts to chat in the background (a network round trip must not stall the session).
    pub(super) fn send_chat(&mut self, text: String) {
        let Some(chat_id) = self.session.meta.live_chat_id.clone() else {
            self.notice = Some("no live chat for this broadcast".into());
            return;
        };
        let yt = self.yt.clone();
        tokio::spawn(async move {
            if let Err(e) = yt.send_chat(&chat_id, &text).await {
                tracing::warn!("sending chat: {e}");
            }
        });
    }

    pub(super) fn toast(&self, text: &str) {
        if !self.overlays_off.contains("toast") {
            self.overlays.send(OverlayMsg {
                overlay: "toast".into(),
                action: Action::Update,
                data: json!({ "text": text }),
            });
        }
    }

    /// `!q`: one open question per viewer, near-duplicates dropped, at most 300 chars.
    fn question_add(&mut self, text: &str, msg: &ChatMessage) {
        let text: String = text.trim().chars().take(QUESTION_MAX).collect();
        if text.is_empty() {
            return self.toast("usage: !q <your question>");
        }
        let open =
            |state: QuestionState| matches!(state, QuestionState::Open | QuestionState::Shown);
        let key = normalise(&text);
        let questions = &self.state.questions;
        if questions
            .iter()
            .any(|q| open(q.state) && q.author_id == msg.author_channel_id)
        {
            return self.toast(&format!(
                "{}: you already have an open question",
                msg.author_name
            ));
        }
        if questions
            .iter()
            .any(|q| open(q.state) && normalise(&q.text) == key)
        {
            return self.toast("that question is already in the queue");
        }
        self.emit(
            "bot",
            EventKind::QuestionAdd {
                qid: ulid::Ulid::generate().to_string(),
                author: msg.author_name.clone(),
                author_id: msg.author_channel_id.clone(),
                text,
                support: None,
            },
        );
        self.toast(&format!("question received from {}", msg.author_name));
    }

    /// A Super Chat, Super Sticker or membership joins the inbox, pinned above the
    /// questions, so it can be read and thanked long after chat has scrolled past it.
    fn support_add(&mut self, support: Support, msg: &ChatMessage) {
        let text = match msg.text.trim() {
            "" if msg.kind == ChatKind::Supersticker => "(sticker)".to_owned(),
            "" => "(no message)".to_owned(),
            t => t.chars().take(QUESTION_MAX).collect(),
        };
        let what = match support.kind {
            ChatKind::Membership => "membership".to_owned(),
            _ => format!("{} Super Chat", support.label),
        };
        self.emit(
            "chat",
            EventKind::QuestionAdd {
                qid: ulid::Ulid::generate().to_string(),
                author: msg.author_name.clone(),
                author_id: msg.author_channel_id.clone(),
                text,
                support: Some(support),
            },
        );
        self.notice = Some(format!(
            "{what} from {}: pinned in the inbox",
            msg.author_name
        ));
    }

    pub(super) fn question(&mut self, qid: String, action: QuestionAction) {
        let Some(q) = self.state.questions.iter().find(|q| q.qid == qid) else {
            self.notice = Some("no such question".into());
            return;
        };
        let kind = match (action, q.state) {
            // Supporters are thanked in person (their alert overlay plays on arrival):
            // Enter marks them done without a question card.
            (QuestionAction::Show, QuestionState::Open) if q.support.is_some() => {
                EventKind::QuestionAnswered { qid }
            }
            (QuestionAction::Show, QuestionState::Open) => EventKind::QuestionShow { qid },
            (QuestionAction::Show | QuestionAction::Answer, QuestionState::Shown) => {
                EventKind::QuestionAnswered { qid }
            }
            (QuestionAction::Answer, QuestionState::Open) => EventKind::QuestionAnswered { qid },
            (QuestionAction::Dismiss, QuestionState::Open | QuestionState::Shown) => {
                EventKind::QuestionDismiss { qid }
            }
            _ => return,
        };
        // Answered questions can become chapters (stream.chapter_on_question).
        self.emit_render("tui", kind);
    }

    /// `:latency`: shows the counter overlay, or stops a running measurement.
    pub(super) fn latency_toggle(&mut self) {
        if let Some(run) = self.latency_run.take() {
            self.latency_end(run);
            self.notice = Some("latency measurement stopped".into());
            return;
        }
        let resume = self.config.stream.chapter_on_latency.then(|| {
            let current = self.current_chapter();
            self.latency_chapter(LATENCY_CHAPTER.into());
            current
        });
        self.latency_run = Some(LatencyRun {
            start: self.clock.now_ts(),
            samples: Vec::new(),
            resume,
        });
        self.notice = Some(
            "latency: watch the stream, type the number you see as !lat <n> in chat (or :lat <n>)"
                .into(),
        );
    }

    /// One reading: the stream showed `counter` at time `at`.
    pub(super) fn latency_sample(&mut self, counter: u64, at: Timestamp) {
        let Some(run) = &mut self.latency_run else {
            self.notice = Some("no latency measurement running (start it with :latency)".into());
            return;
        };
        let shown = run.start.add_secs(counter as f64 / 10.0);
        let latency = at.secs_since(shown);
        if latency < 0.0 {
            self.notice = Some(format!(
                "LAT {counter} has not been shown yet; check the number"
            ));
            return;
        }
        run.samples.push(latency);
        let samples = run.samples.len();
        let latency_s = run.samples.iter().sum::<f64>() / samples as f64;
        let latency_s = (latency_s * 10.0).round() / 10.0;
        if samples >= LATENCY_SAMPLES
            && let Some(run) = self.latency_run.take()
        {
            self.latency_end(run);
        }
        self.emit(
            "core",
            EventKind::LatencyMeasured {
                latency_s,
                samples: samples as u32,
            },
        );
        self.session.meta.anchors.latency_s = Some(latency_s);
        self.save_session();
        self.notice = Some(format!(
            "latency {latency_s:.1} s ({samples} of {LATENCY_SAMPLES} samples)"
        ));
    }

    /// Ends the "Latency check" chapter by returning to the one before it.
    fn latency_end(&mut self, run: LatencyRun) {
        if let Some(title) = run.resume {
            self.latency_chapter(title);
        }
    }

    /// Title of the chapter the stream is in now (last checkpoint, quest, ... so far).
    fn current_chapter(&self) -> String {
        let now = self.clock.now_ts();
        ac_template::chapters::candidates(&self.state, &self.config.stream)
            .into_iter()
            .filter(|c| c.0 <= now)
            .max_by_key(|c| c.0)
            .map_or_else(|| self.config.stream.first_chapter_title.clone(), |c| c.1)
    }

    /// A chapter starting now, set by the streamer.
    fn latency_chapter(&mut self, title: String) {
        let pressed_at = self.clock.now_ts();
        self.emit_render(
            "tui",
            EventKind::Checkpoint {
                cid: ulid::Ulid::generate().to_string(),
                title,
                pressed_at,
                by: By::Tui,
                latency_corrected_s: None,
            },
        );
    }

    /// Seconds viewers are behind: measured, else `stream.default_latency_s`.
    pub(super) fn latency(&self) -> f64 {
        self.state
            .latency_s
            .or(self.session.meta.anchors.latency_s)
            .unwrap_or(self.config.stream.default_latency_s)
    }

    fn live_since(&self) -> Option<Timestamp> {
        let anchors = &self.session.meta.anchors;
        anchors.yt_actual_start.or(anchors.obs_stream_start)
    }

    /// `!title`, confirmed in the TUI.
    pub(super) async fn confirm_title(&mut self, ok: bool) {
        let Some(title) = self.pending_title.take() else {
            return;
        };
        if !ok {
            self.notice = Some("title change dropped".into());
            return;
        }
        let id = self.session.meta.video_id.clone();
        match self.yt.update_title(&id, &title).await {
            Ok(()) => {
                self.notice = Some(format!("title set to {title:?}"));
                self.title = title.clone();
                self.emit("youtube", EventKind::TitlePushed { title });
            }
            Err(e) => self.notice = Some(format!("title not set: {e}")),
        }
    }

    /// The external quest file changed: `{ "id", "title", ... }` (specs/09, "External context").
    pub(super) fn quest_file(&mut self) {
        let path = self.home.path(&self.config.external.current_quest_file);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return;
        };
        let mut obj = match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(obj)) => obj,
            _ => {
                tracing::warn!("{}: not a JSON object", path.display());
                return;
            }
        };
        let Some(title) = obj
            .remove("title")
            .and_then(|t| t.as_str().map(str::to_owned))
        else {
            tracing::warn!("{}: no \"title\"", path.display());
            return;
        };
        let id = obj.remove("id").and_then(|t| t.as_str().map(str::to_owned));
        let current = self.state.quests.last();
        if current.is_some_and(|q| q.title == title && q.id == id) {
            return;
        }
        self.emit_render(
            "external",
            EventKind::QuestSet {
                id,
                title,
                source: QuestSource::External,
                extra: Some(obj).filter(|o| !o.is_empty()),
            },
        );
    }

    /// Sends each stateful overlay's content when it changed: now, question, brb,
    /// latency, chapters. Overlays turned off with `:overlay <name> off` are hidden.
    pub(super) fn sync_overlays(&mut self) {
        let hide = || (Action::Hide, Value::Null);
        let live = self.state.stream == Some(StreamStatus::Live) && !self.state.ended;
        let now = match self.state.quests.last() {
            Some(q) if live => (
                Action::Show,
                json!({
                    "quest": q.title,
                    "live_since_ms": self.live_since().map(Timestamp::unix_ms),
                }),
            ),
            _ => hide(),
        };
        let question = self
            .state
            .questions
            .iter()
            .filter(|q| q.state == QuestionState::Shown)
            .max_by_key(|q| q.shown_at)
            .map_or_else(hide, |q| {
                (Action::Show, json!({ "author": q.author, "text": q.text }))
            });
        let brb = if self.state.brb {
            let since = self.state.brb_starts.last().map(|t| t.unix_ms());
            (Action::Show, json!({ "since_ms": since }))
        } else {
            hide()
        };
        let latency = self.latency_run.as_ref().map_or_else(hide, |run| {
            (Action::Show, json!({ "start_ms": run.start.unix_ms() }))
        });
        let chapters = self
            .state
            .checkpoints
            .iter()
            .max_by_key(|c| c.pressed_at)
            .filter(|_| live)
            .map_or_else(hide, |c| (Action::Show, json!({ "title": c.title })));
        for (name, (action, data)) in [
            ("now", now),
            ("question", question),
            ("brb", brb),
            ("latency", latency),
            ("chapters", chapters),
        ] {
            let (action, data) = if self.overlays_off.contains(name) {
                hide()
            } else {
                (action, data)
            };
            let msg = OverlayMsg {
                overlay: name.into(),
                action,
                data,
            };
            if self.overlay_sent.get(name) != Some(&msg) {
                self.overlay_sent.insert(name, msg.clone());
                self.overlays.send(msg);
            }
        }
    }

    pub(super) fn chat_pane(&self) -> Arc<VecDeque<ChatMessage>> {
        self.chat.clone()
    }
}

/// Lowercase letters and digits only, for near-duplicate questions.
fn normalise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// `ts title` per chapter, joined; latest first when it would not fit.
fn chapters_reply(chapters: &[ac_template::chapters::Chapter]) -> String {
    let line = |c: &ac_template::chapters::Chapter| format!("{} {}", c.ts, c.title);
    let all: Vec<String> = chapters.iter().map(line).collect();
    let text = all.join(" · ");
    if text.chars().count() <= CHAPTERS_REPLY {
        return if text.is_empty() {
            "No chapters yet".into()
        } else {
            text
        };
    }
    let mut out = String::new();
    for l in all.iter().rev() {
        let next = if out.is_empty() {
            l.clone()
        } else {
            format!("{out} · {l}")
        };
        if next.chars().count() > CHAPTERS_REPLY {
            break;
        }
        out = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_template::chapters::{Chapter, ChapterKind};

    #[test]
    fn near_duplicates_normalise_equal() {
        assert_eq!(normalise("Why Rust?"), normalise("why rust"));
        assert_ne!(normalise("why rust"), normalise("why go"));
    }

    #[test]
    fn chapters_reply_fits_latest_first() {
        let ch = |n: usize| Chapter {
            ts: format!("{n}:00"),
            title: "x".repeat(40),
            vod_t: n as f64 * 60.0,
            kind: ChapterKind::Checkpoint,
        };
        assert_eq!(chapters_reply(&[]), "No chapters yet");
        assert!(chapters_reply(&[ch(0), ch(1)]).starts_with("0:00"));
        let long = chapters_reply(&(0..10).map(ch).collect::<Vec<_>>());
        assert!(long.starts_with("9:00"), "{long}");
        assert!(long.chars().count() <= CHAPTERS_REPLY);
    }
}
