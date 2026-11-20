//! The settings page (`,`): the settings worth changing from the TUI, grouped in
//! sections, each saved to `aftercast.toml` the moment it changes (comments kept), then
//! reloaded so it applies live where it can.

use std::path::Path;
use std::time::{Duration, Instant};

use ac_core::command::Command;
use ac_core::config::{
    Captions, Config, Encoder, KeysOverlay, OcrEngine, ReplyMode, Transcriber, Value,
};
use ac_core::obs::Media;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use crate::theme::{ACCENT, FAINT, INFO, MUTED, OK, RAISED, SELECTED, TEXT, WARN, hint, mix};

/// What a setting holds and how it is changed.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Bool,
    Int { min: i64, max: i64, step: i64 },
    Float { min: f64, max: f64, step: f64 },
    Text,
    Choice(&'static [&'static str]),
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Now,
    Restart,
    NextSession,
    NextPost,
}

impl When {
    pub fn text(self) -> &'static str {
        match self {
            When::Now => "applies now",
            When::Restart => "applies after restarting aftercast",
            When::NextSession => "applies from the next session",
            When::NextPost => "applies to the next post-processing or render",
        }
    }
}

pub struct Setting {
    pub section: usize,
    /// Dotted key in `aftercast.toml`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub when: When,
    pub get: fn(&Config) -> Value,
}

pub const SECTIONS: [&str; 8] = [
    "Display",
    "Stream",
    "YouTube",
    "Chat bot",
    "OBS",
    "OBS live",
    "Capture",
    "After the stream",
];

/// The section listing what OBS has right now: audio inputs and every scene's sources.
pub const OBS_LIVE: usize = 5;

/// One row of the page: a setting from `aftercast.toml`, or something live in OBS.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Row {
    Setting(usize),
    Audio(usize),
    Item(usize),
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// Every setting on the page, in display order.
pub fn all() -> Vec<Setting> {
    use Kind::*;
    use When::*;
    let s = |section, key, label, help, kind, when, get| Setting {
        section,
        key,
        label,
        help,
        kind,
        when,
        get,
    };
    vec![
        // Display
        s(
            0,
            "tui.wrap_lines",
            "Wrap long lines",
            "Wrap every chat message, Inbox item and log event over several lines instead of cutting it with …. Off keeps one line each; the line under the cursor always opens in full.",
            Bool,
            Now,
            |c| Value::Bool(c.tui.wrap_lines),
        ),
        s(
            0,
            "tui.animations",
            "Animations",
            "Pulsing LIVE and REC dots, spinners, the blinking caret and panes that glide when focus moves. Off keeps the screen still.",
            Bool,
            Now,
            |c| Value::Bool(c.tui.animations),
        ),
        s(
            0,
            "tui.name_colors",
            "Name colors",
            "Give every chat author their own color, in chat and in the Inbox, so you can tell people apart at a glance. Off shows every name in one color.",
            Bool,
            Now,
            |c| Value::Bool(c.tui.name_colors),
        ),
        s(
            0,
            "tui.home_layout",
            "Home layout",
            "columns: what needs you beside audio and the scene on air. page: one full-width page. classic: checks beside audio and sources listed one per line. tabs: Checks and Studio, switched with [ and ]. v on Home cycles them.",
            Choice(&["columns", "page", "classic", "tabs"]),
            Now,
            |c| text(c.tui.home_layout.name()),
        ),
        // Stream
        s(
            1,
            "stream.chapter_on_brb",
            "Chapter on BRB",
            "Going BRB adds a \"Break\" chapter; coming back returns to the chapter you were in.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_brb),
        ),
        s(
            1,
            "stream.chapter_on_question",
            "Chapter on question",
            "Showing a question on stream adds a \"Q: …\" chapter.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_question),
        ),
        s(
            1,
            "stream.chapter_on_quest_change",
            "Chapter on new quest",
            "A new quest (from :quest or the quest file) starts a chapter.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_quest_change),
        ),
        s(
            1,
            "stream.chapter_on_latency",
            "Chapter on latency check",
            ":latency adds a \"Latency check\" chapter, then returns to the previous one.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_latency),
        ),
        s(
            1,
            "stream.default_latency_s",
            "Default latency (s)",
            "Stream delay used until :latency measures it. A mod's !chapter and !mark are moved back by this much.",
            Float {
                min: 0.0,
                max: 600.0,
                step: 0.5,
            },
            Now,
            |c| Value::Float(c.stream.default_latency_s),
        ),
        s(
            1,
            "stream.first_chapter_title",
            "First chapter title",
            "Title of the chapter at 0:00.",
            Text,
            Now,
            |c| text(&c.stream.first_chapter_title),
        ),
        // YouTube
        s(
            2,
            "youtube.push_on_start",
            "Push description at start",
            "Push the description to YouTube when a session starts (50 quota units).",
            Bool,
            NextSession,
            |c| Value::Bool(c.youtube.push_on_start),
        ),
        s(
            2,
            "youtube.push_on_end",
            "Push description at end",
            "Push the final description when the stream ends or you finalize (50 quota units).",
            Bool,
            Now,
            |c| Value::Bool(c.youtube.push_on_end),
        ),
        s(
            2,
            "youtube.daily_quota_reserve",
            "Quota reserve",
            "Units of the daily YouTube quota kept for manual actions; pre-flight warns below it.",
            Int {
                min: 0,
                max: 10_000,
                step: 100,
            },
            Restart,
            |c| Value::Int(c.youtube.daily_quota_reserve.into()),
        ),
        // Chat bot
        s(
            3,
            "bot.enabled",
            "Chat bot",
            "Answer !commands in chat (!q, !links, !uptime, !chapter, …). Off ignores them; chat still shows here.",
            Bool,
            Restart,
            |c| Value::Bool(c.bot.enabled),
        ),
        s(
            3,
            "bot.prefix",
            "Command prefix",
            "What chat commands start with.",
            Text,
            Restart,
            |c| text(&c.bot.prefix),
        ),
        s(
            3,
            "bot.default_reply",
            "Replies go to",
            "Where bot answers appear: the toast overlay, chat (20 quota units each) or both. A command can override it.",
            Choice(&["overlay", "chat", "both"]),
            Restart,
            |c| {
                text(match c.bot.default_reply {
                    ReplyMode::Overlay => "overlay",
                    ReplyMode::Chat => "chat",
                    ReplyMode::Both => "both",
                })
            },
        ),
        s(
            3,
            "bot.global_cooldown_s",
            "Global cooldown (s)",
            "Minimum time between any two bot replies.",
            Int {
                min: 0,
                max: 600,
                step: 1,
            },
            Restart,
            |c| Value::Int(c.bot.global_cooldown_s as i64),
        ),
        s(
            3,
            "bot.user_cooldown_s",
            "Per-viewer cooldown (s)",
            "Minimum time between two commands from the same viewer.",
            Int {
                min: 0,
                max: 3600,
                step: 5,
            },
            Restart,
            |c| Value::Int(c.bot.user_cooldown_s as i64),
        ),
        s(
            3,
            "bot.max_chat_replies_per_hour",
            "Chat replies per hour",
            "Cap on replies sent into chat; beyond it they go to the overlay, saving quota.",
            Int {
                min: 0,
                max: 1000,
                step: 5,
            },
            Restart,
            |c| Value::Int(c.bot.max_chat_replies_per_hour.into()),
        ),
        // OBS
        s(
            4,
            "obs.require_recording",
            "Warn when not recording",
            "Pre-flight and the status bar warn loudly while OBS is not recording: the local recording is the editing source.",
            Bool,
            Now,
            |c| Value::Bool(c.obs.require_recording),
        ),
        s(
            4,
            "obs.record_into_session",
            "Record into the session folder",
            "While aftercast runs, OBS records into the session's media folder, so the master lives with the session and quitting a draft keeps no recording. OBS's own folder is restored on exit.",
            Bool,
            Restart,
            |c| Value::Bool(c.obs.record_into_session),
        ),
        s(
            4,
            "obs.scenes.main",
            "Main scene",
            "The scene BRB and privacy return to.",
            Text,
            Now,
            |c| text(&c.obs.scenes.main),
        ),
        s(
            4,
            "obs.scenes.brb",
            "BRB scene",
            "The scene b switches to.",
            Text,
            Now,
            |c| text(&c.obs.scenes.brb),
        ),
        s(
            4,
            "obs.scenes.privacy",
            "Privacy scene",
            "The scene p switches to (key capture pauses with it).",
            Text,
            Now,
            |c| text(&c.obs.scenes.privacy),
        ),
        // Capture
        s(
            6,
            "capture.keys",
            "Record key presses",
            "Record typed keys and shortcuts for the keys overlay and post-processing. Privacy mode, denied apps and password boxes are always skipped.",
            Bool,
            Restart,
            |c| Value::Bool(c.capture.keys),
        ),
        s(
            6,
            "capture.shell",
            "Record shell commands",
            "Read commands from the shell hook (aftercast shell-hook) into the session.",
            Bool,
            Restart,
            |c| Value::Bool(c.capture.shell),
        ),
        // After the stream
        s(
            7,
            "post.use_vod",
            "Use the YouTube VOD",
            "Download the VOD to post-process instead of the local recording.",
            Bool,
            NextPost,
            |c| Value::Bool(c.post.use_vod),
        ),
        s(
            7,
            "post.transcriber",
            "Transcriber",
            "Speech to text engine. faster-whisper is installed by aftercast setup.",
            Choice(&["faster-whisper", "whisperx", "whisper-cpp"]),
            NextPost,
            |c| {
                text(match c.post.transcriber {
                    Transcriber::FasterWhisper => "faster-whisper",
                    Transcriber::Whisperx => "whisperx",
                    Transcriber::WhisperCpp => "whisper-cpp",
                })
            },
        ),
        s(
            7,
            "post.whisper_model",
            "Whisper model",
            "Model name (large-v3-turbo did best on your speech) or, for whisper-cpp, a ggml .bin path.",
            Text,
            NextPost,
            |c| text(&c.post.whisper_model),
        ),
        s(
            7,
            "post.language",
            "Language",
            "auto, or a language code such as en or hi.",
            Text,
            NextPost,
            |c| text(&c.post.language),
        ),
        s(
            7,
            "post.ocr",
            "Read screen text (OCR)",
            "Read the text in sampled frames so clips and search know what was on screen.",
            Bool,
            NextPost,
            |c| Value::Bool(c.post.ocr),
        ),
        s(
            7,
            "post.ocr_engine",
            "OCR engine",
            "glm-ocr: a small vision model on the GPU, far better on code and terminals. tesseract: CPU only.",
            Choice(&["glm-ocr", "tesseract"]),
            NextPost,
            |c| {
                text(match c.post.ocr_engine {
                    OcrEngine::GlmOcr => "glm-ocr",
                    OcrEngine::Tesseract => "tesseract",
                })
            },
        ),
        s(
            7,
            "post.correct",
            "Fix misheard terms",
            "A small local model fixes words that sound like this stream's terms (\"rest\" to \"Rust\"); every change is listed in derived/corrections.json.",
            Bool,
            NextPost,
            |c| Value::Bool(c.post.correct),
        ),
        s(
            7,
            "render.encoder",
            "Video encoder",
            "h264_nvenc uses the NVIDIA GPU; libx264 works everywhere, slower.",
            Choice(&["h264_nvenc", "libx264"]),
            NextPost,
            |c| {
                text(match c.render.encoder {
                    Encoder::H264Nvenc => "h264_nvenc",
                    Encoder::Libx264 => "libx264",
                })
            },
        ),
        s(
            7,
            "render.captions",
            "Captions",
            "Burn subtitles into clips, write an .srt next to them, or none.",
            Choice(&["burn", "srt", "none"]),
            NextPost,
            |c| {
                text(match c.render.captions {
                    Captions::Burn => "burn",
                    Captions::Srt => "srt",
                    Captions::None => "none",
                })
            },
        ),
        s(
            7,
            "render.keys_overlay",
            "Keys overlay in clips",
            "Burn the key presses into clips, render them as a separate alpha layer, or none.",
            Choice(&["burn", "alpha", "none"]),
            NextPost,
            |c| {
                text(match c.render.keys_overlay {
                    KeysOverlay::Burn => "burn",
                    KeysOverlay::Alpha => "alpha",
                    KeysOverlay::None => "none",
                })
            },
        ),
    ]
}

pub fn show(v: &Value) -> String {
    match v {
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Text(t) if t.is_empty() => "(empty)".into(),
        Value::Text(t) => t.clone(),
    }
}

/// What a key press asks of the TUI.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    /// The file changed: reload it.
    Reload,
    /// Send this to the controller (an OBS change).
    Command(Command),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    section: usize,
    row: usize,
    /// The search filter; `typing` while the search box has the keyboard.
    search: String,
    typing: bool,
    /// Inline editor for a number or text.
    edit: Option<String>,
    /// The last change: which key, when (for the switch animation and the status line).
    changed: Option<(&'static str, Instant)>,
    status: Option<(String, bool)>,
}

impl Page {
    /// The rows listed right now: the section's, or every match of the search (settings,
    /// OBS audio inputs and sources alike).
    fn visible(&self, all: &[Setting], obs: &Media) -> Vec<Row> {
        let q = self.search.to_lowercase();
        let hit = |texts: &[&str]| texts.iter().any(|t| t.to_lowercase().contains(&q));
        let mut rows: Vec<Row> = (0..all.len())
            .filter(|&i| {
                let s = &all[i];
                if q.is_empty() {
                    s.section == self.section
                } else {
                    hit(&[s.label, s.key, s.help, SECTIONS[s.section]])
                }
            })
            .map(Row::Setting)
            .collect();
        let live = q.is_empty() && self.section == OBS_LIVE;
        if live || !q.is_empty() {
            for (i, a) in obs.audio.iter().enumerate() {
                if live || hit(&[&a.name, "mute volume audio mic", SECTIONS[OBS_LIVE]]) {
                    rows.push(Row::Audio(i));
                }
            }
            // Sources grouped by scene, scenes in OBS's order.
            for scene in &obs.scenes {
                for (i, item) in obs.items.iter().enumerate() {
                    if item.scene == *scene
                        && (live || hit(&[&item.source, scene, "show hide source"]))
                    {
                        rows.push(Row::Item(i));
                    }
                }
            }
        }
        rows
    }

    fn selected(&self, all: &[Setting], obs: &Media) -> Option<Row> {
        self.visible(all, obs).get(self.row).copied()
    }

    /// The selected row when it is a setting.
    fn setting<'a>(&self, all: &'a [Setting], obs: &Media) -> Option<&'a Setting> {
        match self.selected(all, obs)? {
            Row::Setting(i) => Some(&all[i]),
            _ => None,
        }
    }

    /// A key on an OBS row: what to ask OBS.
    fn obs_key(
        &mut self,
        code: ratatui::crossterm::event::KeyCode,
        row: Row,
        obs: &Media,
    ) -> Option<Command> {
        use ratatui::crossterm::event::KeyCode as K;
        let cmd = match row {
            Row::Audio(i) => {
                let a = obs.audio.get(i)?;
                let volume = |db: f64| Command::Volume {
                    input: a.name.clone(),
                    db,
                };
                match code {
                    K::Enter | K::Char(' ') => Command::Mute {
                        input: a.name.clone(),
                        on: Some(!a.muted),
                    },
                    K::Char('l') | K::Right | K::Char('+') => volume((a.volume_db + 1.0).round()),
                    K::Char('h') | K::Left | K::Char('-') => volume((a.volume_db - 1.0).round()),
                    K::Char('r') => volume(0.0),
                    _ => return None,
                }
            }
            Row::Item(i) => {
                let item = obs.items.get(i)?;
                let visible = match code {
                    K::Enter | K::Char(' ') => !item.enabled,
                    K::Char('l') | K::Right => true,
                    K::Char('h') | K::Left => false,
                    _ => return None,
                };
                Command::Source {
                    source: item.source.clone(),
                    scene: Some(item.scene.clone()),
                    visible: Some(visible),
                }
            }
            Row::Setting(_) => return None,
        };
        self.changed = Some(("obs", Instant::now()));
        self.status = Some(("Sent to OBS".into(), true));
        Some(cmd)
    }

    pub fn key(
        &mut self,
        code: ratatui::crossterm::event::KeyCode,
        config: &Config,
        path: &Path,
        obs: &Media,
    ) -> Outcome {
        use ratatui::crossterm::event::KeyCode as K;
        let all = all();
        if let Some(buffer) = &mut self.edit {
            match code {
                K::Esc => self.edit = None,
                K::Backspace => {
                    buffer.pop();
                }
                K::Char(c) => buffer.push(c),
                K::Enter => {
                    let buffer = self.edit.take().unwrap_or_default();
                    let Some(s) = self.setting(&all, obs) else {
                        return Outcome::Stay;
                    };
                    let value = match s.kind {
                        Kind::Int { min, max, .. } => match buffer.trim().parse::<i64>() {
                            Ok(n) => Value::Int(n.clamp(min, max)),
                            Err(_) => {
                                return self.fail(format!("{:?} is not a whole number", buffer));
                            }
                        },
                        Kind::Float { min, max, .. } => match buffer.trim().parse::<f64>() {
                            Ok(n) if n.is_finite() => Value::Float(n.clamp(min, max)),
                            _ => return self.fail(format!("{:?} is not a number", buffer)),
                        },
                        _ => Value::Text(buffer.trim().to_owned()),
                    };
                    return self.save(s, Some(value), path);
                }
                _ => {}
            }
            return Outcome::Stay;
        }
        if self.typing {
            match code {
                K::Esc => {
                    self.search.clear();
                    self.typing = false;
                }
                K::Enter | K::Down | K::Tab => self.typing = false,
                K::Backspace => {
                    self.search.pop();
                }
                K::Char(c) => self.search.push(c),
                _ => {}
            }
            self.row = 0;
            return Outcome::Stay;
        }
        let len = self.visible(&all, obs).len();
        if let Some(row @ (Row::Audio(_) | Row::Item(_))) = self.selected(&all, obs)
            && let Some(cmd) = self.obs_key(code, row, obs)
        {
            return Outcome::Command(cmd);
        }
        match code {
            // Esc clears a search first, then closes.
            K::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.row = 0;
            }
            K::Esc | K::Char('q') | K::Char(',') => return Outcome::Close,
            K::Char('/') => {
                self.typing = true;
                self.row = 0;
            }
            K::Char('j') | K::Down => self.row = (self.row + 1).min(len.saturating_sub(1)),
            K::Char('k') | K::Up => self.row = self.row.saturating_sub(1),
            K::Char('g') | K::Home => self.row = 0,
            K::Char('G') | K::End => self.row = len.saturating_sub(1),
            K::Tab | K::Char(']') => self.go(self.section as isize + 1),
            K::BackTab | K::Char('[') => self.go(self.section as isize - 1),
            K::Char(c @ '1'..='9') => {
                let n = c as usize - '1' as usize;
                if n < SECTIONS.len() {
                    self.go(n as isize);
                }
            }
            K::Char('r') => {
                if let Some(s) = self.setting(&all, obs) {
                    return self.save(s, None, path);
                }
            }
            K::Enter | K::Char(' ') => {
                let Some(s) = self.setting(&all, obs) else {
                    return Outcome::Stay;
                };
                let current = (s.get)(config);
                return match (s.kind, current) {
                    (Kind::Bool, Value::Bool(b)) => self.save(s, Some(Value::Bool(!b)), path),
                    (Kind::Choice(_), _) => self.step(s, 1, config, path),
                    (_, v) => {
                        self.edit = Some(match v {
                            Value::Text(t) => t,
                            v => show(&v),
                        });
                        Outcome::Stay
                    }
                };
            }
            K::Char('l') | K::Right | K::Char('+') => {
                if let Some(s) = self.setting(&all, obs) {
                    return self.step(s, 1, config, path);
                }
            }
            K::Char('h') | K::Left | K::Char('-') => {
                if let Some(s) = self.setting(&all, obs) {
                    return self.step(s, -1, config, path);
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn go(&mut self, section: isize) {
        self.search.clear();
        self.section = section.rem_euclid(SECTIONS.len() as isize) as usize;
        self.row = 0;
    }

    /// ←/→: a switch goes off/on, a choice moves, a number steps.
    fn step(&mut self, s: &Setting, dir: i64, config: &Config, path: &Path) -> Outcome {
        let value = match (s.kind, (s.get)(config)) {
            (Kind::Bool, Value::Bool(b)) => {
                if b == (dir > 0) {
                    return Outcome::Stay;
                }
                Value::Bool(dir > 0)
            }
            (Kind::Choice(options), Value::Text(t)) => {
                let i = options.iter().position(|o| *o == t).unwrap_or(0) as i64;
                let n = options.len() as i64;
                Value::Text(options[(i + dir).rem_euclid(n) as usize].to_owned())
            }
            (Kind::Int { min, max, step }, Value::Int(v)) => {
                Value::Int((v + dir * step).clamp(min, max))
            }
            (Kind::Float { min, max, step }, Value::Float(v)) => {
                Value::Float((v + dir as f64 * step).clamp(min, max))
            }
            _ => return Outcome::Stay,
        };
        self.save(s, Some(value), path)
    }

    fn save(&mut self, s: &Setting, value: Option<Value>, path: &Path) -> Outcome {
        let reset = value.is_none();
        match ac_core::config::set_value(path, s.key, value) {
            Ok(()) => {
                self.changed = Some((s.key, Instant::now()));
                let what = if reset { "Reset to default" } else { "Saved" };
                self.status = Some((format!("{what} · {}", s.when.text()), true));
                Outcome::Reload
            }
            Err(e) => self.fail(e.to_string()),
        }
    }

    fn fail(&mut self, message: String) -> Outcome {
        self.status = Some((message, false));
        Outcome::Stay
    }

    pub fn hints(&self) -> Vec<Span<'static>> {
        let pairs: &[(&str, &str)] = if self.edit.is_some() {
            &[("Enter", "save"), ("Esc", "cancel")]
        } else if self.typing {
            &[
                ("type", "to search"),
                ("Enter", "results"),
                ("Esc", "clear"),
            ]
        } else {
            &[
                ("j/k", "move"),
                ("Space", "toggle / edit"),
                ("h/l", "change"),
                ("Tab", "section"),
                ("/", "search"),
                ("r", "reset"),
                ("Esc", "close"),
            ]
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, l) in pairs {
            spans.extend(hint(k, l));
        }
        spans
    }

    pub fn draw(
        &self,
        frame: &mut Frame,
        config: &Config,
        path: &Path,
        obs: &Media,
        scene: Option<&str>,
        motion: bool,
    ) {
        let all = all();
        let defaults = Config::default();
        // A whole page between the status bar and the key hints.
        let area = frame.area();
        let outer = ratatui::layout::Rect {
            y: area.y + 1,
            height: area.height.saturating_sub(2),
            ..area
        };
        frame.render_widget(Clear, outer);
        let file = Span::styled(format!(" {} ", path.display()), Style::new().fg(FAINT));
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(ACCENT))
            .title(Line::from(vec![
                Span::styled(" ◉ ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
                Span::styled(" Settings ", Style::new().fg(TEXT).bold()),
            ]))
            .title(file.into_right_aligned_line());
        let inner = block.inner(outer);
        frame.render_widget(block, outer);

        let [top, body, status] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .areas(inner.inner(Margin::new(1, 0)));
        let [side, main] =
            Layout::horizontal([Constraint::Length(24), Constraint::Min(20)]).areas(body);

        // Search box.
        let search = if self.typing || !self.search.is_empty() {
            let caret = if self.typing { "▏" } else { "" };
            Line::from(vec![
                Span::styled(" / ", Style::new().fg(Color::Black).bg(INFO).bold()),
                Span::styled(format!(" {}{caret}", self.search), Style::new().fg(TEXT)),
                Span::styled(
                    format!("   {} found", self.visible(&all, obs).len()),
                    Style::new().fg(FAINT),
                ),
            ])
        } else {
            Line::from(vec![
                Span::styled(" / ", Style::new().fg(MUTED).bg(RAISED)),
                Span::styled(" search all settings", Style::new().fg(FAINT)),
            ])
        };
        frame.render_widget(Paragraph::new(search), top);

        // Sections, each with how many settings differ from the default.
        let mut lines = Vec::new();
        for (i, name) in SECTIONS.iter().enumerate() {
            let changed = all
                .iter()
                .filter(|s| s.section == i && (s.get)(config) != (s.get)(&defaults))
                .count();
            let active = self.search.is_empty() && i == self.section;
            let (chip, label) = if active {
                (
                    Style::new().fg(Color::Black).bg(ACCENT).bold(),
                    Style::new().fg(TEXT).bold(),
                )
            } else {
                (Style::new().fg(MUTED).bg(RAISED), Style::new().fg(MUTED))
            };
            let mut spans = vec![
                Span::styled(format!(" {} ", i + 1), chip),
                Span::styled(format!(" {name}"), label),
            ];
            // OBS live: offline, or a muted mic.
            if i == OBS_LIVE {
                if obs.scenes.is_empty() {
                    spans.push(Span::styled("  offline", Style::new().fg(FAINT)));
                } else if obs.audio.iter().any(|a| a.is_mic() && a.muted) {
                    spans.push(Span::styled("  mic off", Style::new().fg(ACCENT)));
                }
            }
            if changed > 0 {
                spans.push(Span::styled(
                    format!("  •{changed}"),
                    Style::new().fg(ACCENT),
                ));
            }
            lines.push(Line::from(spans));
            lines.push(Line::raw(""));
        }
        frame.render_widget(Paragraph::new(lines), side);

        // Settings list and the selected one's description.
        let [list, about] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(6)]).areas(main);
        let visible = self.visible(&all, obs);
        let width = list.width as usize;
        let mut rows = Vec::new();
        if visible.is_empty() {
            let empty = if self.search.is_empty() && self.section == OBS_LIVE {
                "  OBS is not connected, or has no audio inputs or sources.".to_owned()
            } else {
                format!("  Nothing matches {:?}.", self.search)
            };
            rows.push(Line::styled(empty, Style::new().fg(FAINT).italic()));
        }
        for (r, &row) in visible.iter().enumerate() {
            let i = match row {
                Row::Setting(i) => i,
                other => {
                    let prefix = !self.search.is_empty();
                    rows.push(obs_row(other, obs, scene, r == self.row, prefix, width));
                    continue;
                }
            };
            let s = &all[i];
            let value = (s.get)(config);
            let modified = value != (s.get)(&defaults);
            let selected = r == self.row;
            let mut label = String::new();
            if !self.search.is_empty() {
                label.push_str(&format!("{} › ", SECTIONS[s.section]));
            }
            label.push_str(s.label);
            let mut left = vec![
                Span::styled(if selected { "▍" } else { " " }, Style::new().fg(ACCENT)),
                Span::styled(if modified { "• " } else { "  " }, Style::new().fg(ACCENT)),
                Span::styled(
                    label,
                    if selected {
                        Style::new().fg(TEXT).bold()
                    } else {
                        Style::new().fg(TEXT)
                    },
                ),
            ];
            if s.when == When::Restart {
                left.push(Span::styled(" ↻", Style::new().fg(FAINT)));
            }
            let just = self
                .changed
                .filter(|(k, at)| *k == s.key && at.elapsed() < Duration::from_millis(400))
                .map(|(_, at)| at.elapsed().as_secs_f64() / 0.4);
            let right = if selected && self.edit.is_some() {
                let buffer = self.edit.clone().unwrap_or_default();
                vec![Span::styled(
                    format!(" {buffer}▏ "),
                    Style::new().fg(TEXT).bg(RAISED),
                )]
            } else {
                widget(&s.kind, &value, selected, if motion { just } else { None })
            };
            let used: usize = left.iter().chain(&right).map(Span::width).sum();
            let mut spans = left;
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
            spans.extend(right);
            let mut line = Line::from(spans);
            if selected {
                line = line.style(Style::new().bg(SELECTED));
            }
            rows.push(line);
        }
        // Keep the selected row in view.
        let height = list.height as usize;
        let skip = (self.row + 1).saturating_sub(height);
        let rows: Vec<Line> = rows.into_iter().skip(skip).collect();
        frame.render_widget(Paragraph::new(rows), list);

        let about_block = Block::new()
            .borders(ratatui::widgets::Borders::TOP)
            .border_style(Style::new().fg(FAINT))
            .padding(Padding::horizontal(1));
        if let Some(text) = self.selected(&all, obs).and_then(|row| obs_about(row, obs)) {
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .block(about_block.clone()),
                about,
            );
        }
        if let Some(s) = self.setting(&all, obs) {
            let default = show(&(s.get)(&defaults));
            let when_color = if s.when == When::Now { OK } else { WARN };
            let text = vec![
                Line::styled(s.label, Style::new().fg(TEXT).bold()),
                Line::styled(s.help, Style::new().fg(MUTED)),
                Line::from(vec![
                    Span::styled(s.key, Style::new().fg(FAINT)),
                    Span::styled(format!(" · default {default} · "), Style::new().fg(FAINT)),
                    Span::styled(s.when.text(), Style::new().fg(when_color)),
                ]),
            ];
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .block(about_block),
                about,
            );
        }

        // Result of the last change, fading after a few seconds.
        if let Some((message, ok)) = &self.status {
            let age = self
                .changed
                .map_or(0.0, |(_, at)| at.elapsed().as_secs_f64());
            let base = if *ok { OK } else { ACCENT };
            let color = if *ok {
                mix(base, FAINT, (age - 3.0) / 2.0)
            } else {
                base
            };
            let mark = if *ok { "✔" } else { "✖" };
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("{mark} {message}"),
                    Style::new().fg(color),
                )),
                status,
            );
        }
    }
}

/// An OBS row: an audio input with its volume and mute switch, or a source with its
/// visibility switch.
fn obs_row(
    row: Row,
    obs: &Media,
    program: Option<&str>,
    selected: bool,
    prefix: bool,
    width: usize,
) -> Line<'static> {
    let arrows = Style::new().fg(if selected { MUTED } else { FAINT });
    let name_style = if selected {
        Style::new().fg(TEXT).bold()
    } else {
        Style::new().fg(TEXT)
    };
    let mut left = vec![
        Span::styled(if selected { "▍" } else { " " }, Style::new().fg(ACCENT)),
        Span::raw("  "),
    ];
    if prefix {
        left.push(Span::styled(
            format!("{} › ", SECTIONS[OBS_LIVE]),
            Style::new().fg(TEXT),
        ));
    }
    let right = match row {
        Row::Audio(i) => {
            let a = &obs.audio[i];
            left.push(Span::styled(a.name.clone(), name_style));
            let kind = if a.is_mic() {
                "mic"
            } else if a.kind.contains("output_capture") {
                "desktop"
            } else {
                "audio"
            };
            left.push(Span::styled(format!("  {kind}"), Style::new().fg(FAINT)));
            let (track, label, color) = if a.muted {
                ("●━━", " muted ", ACCENT)
            } else {
                ("━━●", " live  ", OK)
            };
            vec![
                Span::styled("‹ ", arrows),
                Span::styled(format!("{:.1} dB", a.volume_db), Style::new().fg(TEXT)),
                Span::styled(" ›  ", arrows),
                Span::styled(track, Style::new().fg(color)),
                Span::styled(label, Style::new().fg(if a.muted { ACCENT } else { TEXT })),
            ]
        }
        Row::Item(i) => {
            let item = &obs.items[i];
            let on_air = program == Some(item.scene.as_str());
            left.push(Span::styled(
                if on_air { "● " } else { "  " },
                Style::new().fg(ACCENT),
            ));
            left.push(Span::styled(
                format!("{} › ", item.scene),
                Style::new().fg(MUTED),
            ));
            left.push(Span::styled(item.source.clone(), name_style));
            let (track, label, color) = if item.enabled {
                ("━━●", " shown ", OK)
            } else {
                ("●━━", " hidden", FAINT)
            };
            vec![
                Span::styled(track, Style::new().fg(color)),
                Span::styled(
                    label,
                    Style::new().fg(if item.enabled { TEXT } else { MUTED }),
                ),
            ]
        }
        Row::Setting(_) => Vec::new(),
    };
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
    spans.extend(right);
    let line = Line::from(spans);
    if selected {
        line.style(Style::new().bg(SELECTED))
    } else {
        line
    }
}

/// The description under the list for an OBS row.
fn obs_about(row: Row, obs: &Media) -> Option<Vec<Line<'static>>> {
    let (title, help, meta) = match row {
        Row::Audio(i) => {
            let a = obs.audio.get(i)?;
            (
                a.name.clone(),
                format!(
                    "Space mutes or unmutes; h/l change the volume by 1 dB; r sets 0 dB. Also :mute {} and :volume {} <dB>.",
                    a.name, a.name
                ),
                format!("OBS audio input · {}", a.kind),
            )
        }
        Row::Item(i) => {
            let item = obs.items.get(i)?;
            (
                format!("{} in {}", item.source, item.scene),
                format!(
                    "Space shows or hides it; l shows, h hides. In the program scene also :show {} and :hide {}.",
                    item.source, item.source
                ),
                format!("OBS scene item {}", item.id),
            )
        }
        Row::Setting(_) => return None,
    };
    Some(vec![
        Line::styled(title, Style::new().fg(TEXT).bold()),
        Line::styled(help, Style::new().fg(MUTED)),
        Line::from(vec![
            Span::styled(meta, Style::new().fg(FAINT)),
            Span::styled(" · ", Style::new().fg(FAINT)),
            Span::styled("applies now, in OBS", Style::new().fg(OK)),
        ]),
    ])
}

/// The value column: a switch, `‹ choice ›`, a number or a text value. `just` is the
/// progress (0..1) of a switch that was just flipped, for its knob to slide.
fn widget(kind: &Kind, value: &Value, selected: bool, just: Option<f64>) -> Vec<Span<'static>> {
    let arrows = Style::new().fg(if selected { MUTED } else { FAINT });
    match (kind, value) {
        (Kind::Bool, Value::Bool(on)) => {
            let track = match just {
                Some(p) if p < 0.5 => "━●━",
                _ if *on => "━━●",
                _ => "●━━",
            };
            let color = if *on { OK } else { FAINT };
            vec![
                Span::styled(track, Style::new().fg(color)),
                Span::styled(
                    if *on { " on  " } else { " off " },
                    Style::new().fg(if *on { TEXT } else { MUTED }),
                ),
            ]
        }
        (Kind::Choice(_) | Kind::Int { .. } | Kind::Float { .. }, v) => vec![
            Span::styled("‹ ", arrows),
            Span::styled(show(v), Style::new().fg(TEXT).bold()),
            Span::styled(" › ", arrows),
        ],
        (_, v) => {
            let empty = matches!(v, Value::Text(t) if t.is_empty());
            vec![
                Span::styled(show(v), Style::new().fg(if empty { FAINT } else { TEXT })),
                Span::styled(if selected { " ✎ " } else { "   " }, Style::new().fg(MUTED)),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode as K;

    #[test]
    fn every_setting_reads_its_default_and_saves_back_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftercast.toml");
        std::fs::write(&path, "# mine\n").unwrap();
        let defaults = Config::default();
        for s in all() {
            // Writing the default back must give a valid config with the same value.
            let value = (s.get)(&defaults);
            ac_core::config::set_value(&path, s.key, Some(value.clone()))
                .unwrap_or_else(|e| panic!("{}: {e}", s.key));
            let config = Config::load(&path).unwrap();
            assert_eq!((s.get)(&config), value, "{}", s.key);
            if let Kind::Choice(options) = s.kind {
                for o in options {
                    ac_core::config::set_value(&path, s.key, Some(Value::Text((*o).into())))
                        .unwrap_or_else(|e| panic!("{} = {o}: {e}", s.key));
                }
            }
        }
    }

    #[test]
    fn keys_toggle_step_edit_search_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftercast.toml");
        std::fs::write(&path, "").unwrap();
        let load = || Config::load(&path).unwrap();
        let mut page = Page::default();

        // Display › Wrap long lines: Space flips it and asks for a reload.
        assert_eq!(
            page.key(K::Char(' '), &load(), &path, &Media::default()),
            Outcome::Reload
        );
        assert!(load().tui.wrap_lines);
        // l on an "on" switch does nothing; h turns it off.
        assert_eq!(
            page.key(K::Char('l'), &load(), &path, &Media::default()),
            Outcome::Stay
        );
        page.key(K::Char('h'), &load(), &path, &Media::default());
        assert!(!load().tui.wrap_lines);

        // Section 2, last row: typed text.
        page.key(K::Char('2'), &load(), &path, &Media::default());
        page.key(K::Char('G'), &load(), &path, &Media::default());
        page.key(K::Enter, &load(), &path, &Media::default());
        for _ in 0..5 {
            page.key(K::Backspace, &load(), &path, &Media::default());
        }
        for c in "Intro".chars() {
            page.key(K::Char(c), &load(), &path, &Media::default());
        }
        assert_eq!(
            page.key(K::Enter, &load(), &path, &Media::default()),
            Outcome::Reload
        );
        assert_eq!(load().stream.first_chapter_title, "Intro");

        // A number refuses garbage and keeps the file.
        page.key(K::Char('k'), &load(), &path, &Media::default());
        page.key(K::Enter, &load(), &path, &Media::default());
        page.key(K::Char('x'), &load(), &path, &Media::default());
        assert_eq!(
            page.key(K::Enter, &load(), &path, &Media::default()),
            Outcome::Stay
        );
        assert!(page.status.as_ref().is_some_and(|(_, ok)| !ok));
        // ...and steps with l.
        page.key(K::Char('l'), &load(), &path, &Media::default());
        assert_eq!(load().stream.default_latency_s, 5.5);
        // r resets it.
        page.key(K::Char('r'), &load(), &path, &Media::default());
        assert_eq!(load().stream.default_latency_s, 5.0);

        // Search finds settings in any section; choices cycle.
        page.key(K::Char('/'), &load(), &path, &Media::default());
        for c in "replies go".chars() {
            page.key(K::Char(c), &load(), &path, &Media::default());
        }
        page.key(K::Enter, &load(), &path, &Media::default());
        page.key(K::Char(' '), &load(), &path, &Media::default());
        assert_eq!(load().bot.default_reply, ReplyMode::Chat);
        // Esc clears the search, a second Esc closes the page.
        assert_eq!(
            page.key(K::Esc, &load(), &path, &Media::default()),
            Outcome::Stay
        );
        assert_eq!(
            page.key(K::Esc, &load(), &path, &Media::default()),
            Outcome::Close
        );
    }
}
