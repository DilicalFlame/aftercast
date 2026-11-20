//! Terminal UI for live sessions (specs/02-cli-and-tui.md).
//!
//! The UI only reads [`View`] snapshots and sends [`Command`]s; the controller owns state.
//! Times are captured when a key is pressed, before any prompt opens.

mod theme;

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::chat::ChatMessage;
use ac_core::command::{Command, QuestionAction};
use ac_core::event::{By, Event, EventKind, OutputState, ServiceStatus, StreamStatus};
use ac_core::session::SessionMeta;
use ac_core::session_state::SessionState;
use ac_core::time::{Clock, Timestamp, chapter_ts, hms, vod_t};
use ac_template::Description;
use ac_template::chapters::Chapter;
use chrono::Local;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{self, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use tokio::sync::{mpsc, watch};

use theme::{
    ACCENT, ACCENT_DIM, FAINT, INFO, MUTED, OK, RAISED, SELECTED, TEXT, UserColors, WARN, hint,
    mix, pulse, spinner,
};

/// YouTube's description limit, for the README budget gauge.
const DESCRIPTION_LIMIT: usize = 5000;
/// How long a status-bar flash (hotkey press) lasts.
const FLASH: Duration = Duration::from_millis(700);
/// New chat lines carry a marker this long.
const FRESH_S: f64 = 4.0;
/// Opens the README on `r`, run with `--clean` (no user config).
const EDITOR: &str = "nvim";

/// Snapshot of a live session published by the controller after every change.
#[derive(Debug, Clone)]
pub struct View {
    /// Session folder name, e.g. `2026-11-19_ep042`.
    pub session: String,
    /// The session's `README.md` (`r` opens it in Neovim).
    pub readme: std::path::PathBuf,
    pub meta: SessionMeta,
    pub state: SessionState,
    pub chapters: Vec<Chapter>,
    /// Last successful render (kept while the README has errors).
    pub description: Option<Description>,
    pub render_error: Option<String>,
    pub quota_remaining: i64,
    /// One-line feedback for the last action, e.g. "description unchanged".
    pub notice: Option<String>,
    pub preflight: Vec<Check>,
    /// Last chat messages, oldest first.
    pub chat: Arc<VecDeque<ChatMessage>>,
    /// A `!title` from chat waiting for `y`/`n`.
    pub pending_title: Option<String>,
    pub latency_running: bool,
    /// Measured latency, else the configured default.
    pub latency_s: f64,
    pub keys: KeysStatus,
    /// Stored keys per minute, last minutes, oldest first.
    pub keys_per_min: Vec<u32>,
}

/// Key capture as shown in the status bar (privacy is shown from the session state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeysStatus {
    /// `capture.keys = false`.
    Off,
    /// Waiting for the keyboard hook (or no hook: headless).
    Starting,
    On,
    Error,
}

/// One pre-flight checklist item.
#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub label: String,
    pub state: CheckState,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Ok,
    Fail,
    /// Not known yet (e.g. OBS not connected).
    Pending,
}

/// Input that arrives from outside the terminal, e.g. global hotkeys.
#[derive(Debug, Clone, PartialEq)]
pub enum External {
    /// Checkpoint hotkey: time captured at the press, title asked in the TUI.
    Checkpoint(Timestamp),
    Message(String),
}

/// A hotkey checkpoint without a title after this long is saved as "Checkpoint N".
const HOTKEY_TITLE_TIMEOUT_S: f64 = 600.0;

/// Runs the TUI until the user quits or finalizes. Blocking: call from a dedicated thread.
pub fn run(
    clock: Arc<Clock>,
    view: watch::Receiver<View>,
    commands: mpsc::UnboundedSender<Command>,
    external: std::sync::mpsc::Receiver<External>,
) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new(clock, view, commands);
    app.external = Some(external);
    let result = app.run(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Debug, Clone, PartialEq)]
enum Prompt {
    Checkpoint { by: By },
    Mark,
    Note,
    Edit { cid: String },
    Palette,
}

#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Normal,
    /// Text input; `pressed_at` is when the key that opened it was pressed.
    Input {
        prompt: Prompt,
        text: String,
        pressed_at: Timestamp,
    },
    Preview {
        scroll: u16,
    },
    Help,
    Preflight,
    ConfirmQuit,
    ConfirmDelete {
        cid: String,
        title: String,
    },
    /// The stream ended: finalize, keep open, or post-process.
    Ended,
    /// A `!title` from the owner's chat waits for confirmation.
    ConfirmTitle(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Chat,
    Questions,
    Chapters,
    Log,
}

impl Pane {
    const ORDER: [Pane; 4] = [Pane::Chat, Pane::Questions, Pane::Chapters, Pane::Log];

    fn step(self, delta: isize) -> Self {
        let i = Self::ORDER.iter().position(|p| *p == self).unwrap_or(0) as isize;
        Self::ORDER[(i + delta).rem_euclid(Self::ORDER.len() as isize) as usize]
    }

    /// The pane's number: pressing it focuses the pane.
    fn number(self) -> usize {
        Self::ORDER.iter().position(|p| *p == self).unwrap_or(0) + 1
    }

    /// Lazygit-style layout: the focused pane's column widens and the pane takes most
    /// of its column; the other panes shrink but stay visible.
    fn split(self) -> Split {
        match self {
            Pane::Chat => [0.58, 0.72, 0.5],
            Pane::Questions => [0.58, 0.28, 0.5],
            Pane::Chapters => [0.40, 0.6, 0.72],
            Pane::Log => [0.40, 0.6, 0.28],
        }
    }

    fn from_key(c: char) -> Option<Self> {
        let n = c.to_digit(10)? as usize;
        Self::ORDER.get(n.checked_sub(1)?).copied()
    }
}

/// Left column width, then the top pane's share of the left and of the right column.
type Split = [f64; 3];

/// Chat, Questions, Chapters and Log areas for `split`; every pane keeps at least one
/// line of content.
fn pane_areas(body: Rect, split: Split) -> [Rect; 4] {
    const MIN_W: u16 = 20;
    const MIN_H: u16 = 3;
    let share = |total: u16, frac: f64, min: u16| {
        let n = (total as f64 * frac).round() as u16;
        n.clamp(min.min(total / 2), total.saturating_sub(min).max(total / 2))
    };
    let w = share(body.width, split[0], MIN_W);
    let [left, right] = Layout::horizontal([Constraint::Length(w), Constraint::Min(0)]).areas(body);
    let column = |area: Rect, frac: f64| -> [Rect; 2] {
        let h = share(area.height, frac, MIN_H);
        Layout::vertical([Constraint::Length(h), Constraint::Min(0)]).areas(area)
    };
    let [chat, questions] = column(left, split[1]);
    let [chapters, log] = column(right, split[2]);
    [chat, questions, chapters, log]
}

struct App {
    clock: Arc<Clock>,
    view: watch::Receiver<View>,
    commands: mpsc::UnboundedSender<Command>,
    mode: Mode,
    focus: Pane,
    chapters: ListState,
    questions: ListState,
    log_scroll: usize,
    chat_scroll: usize,
    /// The `!title` last answered, so a stale snapshot does not ask again.
    title_answered: Option<String>,
    message: Option<String>,
    end_prompted: bool,
    exit: bool,
    external: Option<std::sync::mpsc::Receiver<External>>,
    /// Hotkey checkpoints waiting for their title prompt.
    pending: VecDeque<Timestamp>,
    /// `r` was pressed: open this README in Neovim before the next frame.
    edit_readme: Option<std::path::PathBuf>,
    /// Animation clock.
    started: Instant,
    /// The status bar flashes until then (hotkeys pressed in another app).
    flash_until: Option<Instant>,
    /// The notice on screen and when it appeared, so it can fade.
    notice: (String, Instant),
    users: UserColors,
    /// Pane proportions as drawn: they glide towards [`Pane::split`] of the focus.
    split: Split,
    last_frame: Instant,
}

impl App {
    fn new(
        clock: Arc<Clock>,
        view: watch::Receiver<View>,
        commands: mpsc::UnboundedSender<Command>,
    ) -> Self {
        Self {
            clock,
            view,
            commands,
            mode: Mode::Preflight,
            focus: Pane::Chapters,
            chapters: ListState::default(),
            questions: ListState::default(),
            log_scroll: 0,
            chat_scroll: 0,
            title_answered: None,
            message: None,
            end_prompted: false,
            exit: false,
            external: None,
            pending: VecDeque::new(),
            edit_readme: None,
            started: Instant::now(),
            flash_until: None,
            notice: (String::new(), Instant::now()),
            users: UserColors::default(),
            split: Pane::Chapters.split(),
            last_frame: Instant::now(),
        }
    }

    /// Time since start, the input of every animation.
    fn tick(&self) -> Duration {
        self.started.elapsed()
    }

    /// Moves the drawn split a step towards the focused pane's split (eases out in
    /// about 150 ms, whatever the frame rate).
    fn animate_split(&mut self) {
        let dt = self.last_frame.elapsed().as_secs_f64();
        self.last_frame = Instant::now();
        let k = (dt * 20.0).min(1.0);
        let target = self.focus.split();
        for (cur, goal) in self.split.iter_mut().zip(target) {
            *cur += (goal - *cur) * k;
            if (goal - *cur).abs() < 0.002 {
                *cur = goal;
            }
        }
    }

    fn resizing(&self) -> bool {
        self.split != self.focus.split()
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.exit {
            let view = self.view.borrow_and_update().clone();
            self.poll_external(&view);
            if view.state.ended && !self.end_prompted && self.mode == Mode::Normal {
                self.end_prompted = true;
                self.mode = Mode::Ended;
            } else if !view.state.ended && self.end_prompted {
                // The stream restarted: drop the end modal, prompt again on the next end.
                self.end_prompted = false;
                if self.mode == Mode::Ended {
                    self.mode = Mode::Normal;
                }
            }
            if self.mode == Mode::Normal
                && let Some(title) = &view.pending_title
                && self.title_answered.as_ref() != Some(title)
            {
                self.mode = Mode::ConfirmTitle(title.clone());
            }
            if let Some(path) = self.edit_readme.take() {
                self.edit(terminal, &path);
            }
            terminal.draw(|frame| self.draw(frame, &view))?;
            // 10 frames a second keeps the spinners and pulses smooth; ratatui only
            // writes the cells that changed.
            // While panes resize, draw at 60 frames a second.
            let frame = if self.resizing() { 16 } else { 100 };
            if event::poll(Duration::from_millis(frame))?
                && let event::Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.key(key, &view);
            }
        }
        Ok(())
    }

    /// Hands the terminal to Neovim (`--clean`: none of the user's config) on the README and
    /// takes it back when Neovim exits. Saving re-renders the description (the README is
    /// watched); nothing is pushed until `s`.
    fn edit(&mut self, terminal: &mut DefaultTerminal, path: &std::path::Path) {
        ratatui::restore();
        let status = std::process::Command::new(EDITOR)
            .arg("--clean")
            .arg(path)
            .status();
        *terminal = ratatui::init();
        let _ = terminal.clear();
        self.message = Some(match status {
            Ok(s) if s.success() => "README closed; s previews and pushes the description".into(),
            Ok(s) => format!("{EDITOR} exited with {s}"),
            Err(e) => format!("could not start {EDITOR}: {e} (is Neovim installed and on PATH?)"),
        });
    }

    /// Queues hotkey checkpoints, opens their title prompt when the UI is free, and saves
    /// untitled ones after [`HOTKEY_TITLE_TIMEOUT_S`].
    fn poll_external(&mut self, view: &View) {
        while let Some(msg) = self.external.as_ref().and_then(|rx| rx.try_recv().ok()) {
            match msg {
                External::Checkpoint(t) => self.pending.push_back(t),
                External::Message(m) => self.message = Some(m),
            }
            self.flash_until = Some(Instant::now() + FLASH);
        }
        let hotkey = Prompt::Checkpoint { by: By::Hotkey };
        let now = self.clock.now_ts();
        let expired = |t: Timestamp| now.secs_since(t) >= HOTKEY_TITLE_TIMEOUT_S;
        if let Mode::Input {
            prompt,
            text,
            pressed_at,
        } = &self.mode
            && *prompt == hotkey
            && expired(*pressed_at)
        {
            let (text, pressed_at) = (text.trim().to_owned(), *pressed_at);
            self.mode = Mode::Normal;
            self.submit(hotkey.clone(), text, pressed_at, view);
        }
        while let Some(&t) = self.pending.front()
            && expired(t)
        {
            self.pending.pop_front();
            self.submit(hotkey.clone(), String::new(), t, view);
        }
        if self.mode == Mode::Normal
            && let Some(pressed_at) = self.pending.pop_front()
        {
            self.mode = Mode::Input {
                prompt: hotkey,
                text: String::new(),
                pressed_at,
            };
        }
    }

    fn send(&mut self, cmd: Command) {
        if self.commands.send(cmd).is_err() {
            self.exit = true; // the session has ended
        }
    }

    fn key(&mut self, key: KeyEvent, view: &View) {
        let ctrl_c =
            key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c');
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal if ctrl_c => self.mode = Mode::ConfirmQuit,
            Mode::Normal => self.normal_key(key.code, view),
            Mode::Input {
                prompt,
                mut text,
                pressed_at,
            } => match key.code {
                KeyCode::Esc => {}
                KeyCode::Enter => self.submit(prompt, text.trim().to_owned(), pressed_at, view),
                KeyCode::Tab if prompt == Prompt::Palette => {
                    if let Some(rest) = completion(&text) {
                        text.push_str(rest);
                        text.push(' ');
                    }
                    self.mode = Mode::Input {
                        prompt,
                        text,
                        pressed_at,
                    };
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.mode = Mode::Input {
                        prompt,
                        text,
                        pressed_at,
                    };
                }
                KeyCode::Char(c) if !ctrl_c => {
                    text.push(c);
                    self.mode = Mode::Input {
                        prompt,
                        text,
                        pressed_at,
                    };
                }
                _ => {
                    self.mode = Mode::Input {
                        prompt,
                        text,
                        pressed_at,
                    }
                }
            },
            Mode::Preview { scroll } => match key.code {
                KeyCode::Enter => self.send(Command::Sync),
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('j') | KeyCode::Down => {
                    self.mode = Mode::Preview { scroll: scroll + 1 }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.mode = Mode::Preview {
                        scroll: scroll.saturating_sub(1),
                    }
                }
                _ => self.mode = Mode::Preview { scroll },
            },
            Mode::Help | Mode::Preflight => {}
            Mode::ConfirmQuit => match key.code {
                KeyCode::Char('y') => self.finish(Command::Quit),
                KeyCode::Char('f') => self.finish(Command::Finalize),
                KeyCode::Char('n') | KeyCode::Esc => {}
                _ => self.mode = Mode::ConfirmQuit,
            },
            Mode::ConfirmDelete { cid, title } => {
                if key.code == KeyCode::Char('y') {
                    self.send(Command::DeleteCheckpoint { cid });
                    self.message = Some(format!("deleted checkpoint {title:?}"));
                }
            }
            Mode::ConfirmTitle(title) => match key.code {
                KeyCode::Char('y') | KeyCode::Char('n') | KeyCode::Esc => {
                    let ok = key.code == KeyCode::Char('y');
                    self.send(Command::ConfirmTitle { ok });
                    self.title_answered = Some(title);
                }
                _ => self.mode = Mode::ConfirmTitle(title),
            },
            Mode::Ended => match key.code {
                KeyCode::Char('f') => self.finish(Command::Finalize),
                KeyCode::Char('k') | KeyCode::Esc => {}
                KeyCode::Char('p') => {
                    self.message =
                        Some("post-processing arrives with `aftercast post` (phase 5)".into())
                }
                _ => self.mode = Mode::Ended,
            },
        }
    }

    fn normal_key(&mut self, code: KeyCode, view: &View) {
        self.message = None;
        let pressed_at = self.clock.now_ts();
        let input = |prompt| Mode::Input {
            prompt,
            text: String::new(),
            pressed_at,
        };
        match code {
            KeyCode::Char('c') => self.mode = input(Prompt::Checkpoint { by: By::Tui }),
            KeyCode::Char('m') => self.mode = input(Prompt::Mark),
            KeyCode::Char('n') => self.mode = input(Prompt::Note),
            KeyCode::Char(':') => self.mode = input(Prompt::Palette),
            KeyCode::Char('s') => self.mode = Mode::Preview { scroll: 0 },
            KeyCode::Char('r') => self.edit_readme = Some(view.readme.clone()),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('q') => self.mode = Mode::ConfirmQuit,
            KeyCode::Char('b') => self.send(Command::Brb {
                on: None,
                by: By::Tui,
            }),
            KeyCode::Char('p') => self.send(Command::Privacy {
                on: None,
                by: By::Tui,
            }),
            KeyCode::Tab => self.focus = self.focus.step(1),
            KeyCode::BackTab => self.focus = self.focus.step(-1),
            KeyCode::Char(c) if Pane::from_key(c).is_some() => {
                self.focus = Pane::from_key(c).unwrap_or(self.focus)
            }
            // Back to the newest line (chat, log) or the last chapter.
            KeyCode::End | KeyCode::Char('G') => match self.focus {
                Pane::Chat => self.chat_scroll = 0,
                Pane::Log => self.log_scroll = 0,
                _ => self.move_selection(isize::MAX / 2, view),
            },
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(-isize::MAX / 2, view),
            KeyCode::Enter if self.focus == Pane::Questions => {
                self.question(view, QuestionAction::Show)
            }
            KeyCode::Char('x') if self.focus == Pane::Questions => {
                self.question(view, QuestionAction::Dismiss)
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1, view),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1, view),
            KeyCode::Char('e') if self.focus == Pane::Chapters => {
                if let Some(c) = self.selected_checkpoint(view) {
                    self.mode = Mode::Input {
                        prompt: Prompt::Edit { cid: c.cid.clone() },
                        text: c.title.clone(),
                        pressed_at,
                    };
                }
            }
            KeyCode::Char('d') if self.focus == Pane::Chapters => {
                if let Some(c) = self.selected_checkpoint(view) {
                    self.mode = Mode::ConfirmDelete {
                        cid: c.cid.clone(),
                        title: c.title.clone(),
                    };
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize, view: &View) {
        match self.focus {
            Pane::Chapters => {
                let len = view.state.checkpoints.len();
                if len > 0 {
                    let current = self.chapters.selected().unwrap_or(0) as isize;
                    self.chapters
                        .select(Some((current + delta).clamp(0, len as isize - 1) as usize));
                }
            }
            Pane::Questions => {
                let len = queue(&view.state).len();
                if len > 0 {
                    let current = self.questions.selected().unwrap_or(0) as isize;
                    self.questions
                        .select(Some((current + delta).clamp(0, len as isize - 1) as usize));
                }
            }
            // The log and chat show newest at the bottom; "down" moves towards newer lines.
            Pane::Log => {
                let max = view.state.log.len().saturating_sub(1);
                self.log_scroll = self.log_scroll.saturating_add_signed(-delta).min(max)
            }
            Pane::Chat => {
                let max = view.chat.len().saturating_sub(1);
                self.chat_scroll = self.chat_scroll.saturating_add_signed(-delta).min(max)
            }
        }
    }

    /// Sends `action` for the selected question.
    fn question(&mut self, view: &View, action: QuestionAction) {
        let selected = self.questions.selected();
        if let Some(q) = selected.and_then(|i| queue(&view.state).get(i).copied()) {
            let qid = q.qid.clone();
            self.send(Command::Question { qid, action });
        }
    }

    fn selected_checkpoint<'v>(
        &self,
        view: &'v View,
    ) -> Option<&'v ac_core::session_state::Checkpoint> {
        sorted_checkpoints(&view.state)
            .get(self.chapters.selected()?)
            .copied()
    }

    fn submit(&mut self, prompt: Prompt, text: String, pressed_at: Timestamp, view: &View) {
        match prompt {
            Prompt::Checkpoint { by } => {
                let title = if text.is_empty() {
                    format!("Checkpoint {}", view.state.checkpoints.len() + 1)
                } else {
                    text
                };
                self.send(Command::Checkpoint {
                    title,
                    pressed_at,
                    by,
                });
            }
            Prompt::Mark => self.send(Command::Mark {
                label: Some(text).filter(|t| !t.is_empty()),
                pressed_at,
                by: By::Tui,
            }),
            Prompt::Note if text.is_empty() => {}
            Prompt::Note => self.send(Command::Note { text, pressed_at }),
            Prompt::Edit { cid } if !text.is_empty() => {
                self.send(Command::EditCheckpoint { cid, title: text })
            }
            Prompt::Edit { .. } => {}
            // Palette commands take their time when Enter is pressed.
            Prompt::Palette => match palette(&text, self.clock.now_ts()) {
                Ok(Palette::Command(cmd)) => {
                    let ends = matches!(cmd, Command::Quit | Command::Finalize);
                    self.send(cmd);
                    self.exit |= ends;
                }
                Ok(Palette::Help) => self.mode = Mode::Help,
                Ok(Palette::Preflight) => self.mode = Mode::Preflight,
                Err(message) => self.message = Some(message),
            },
        }
    }

    fn finish(&mut self, cmd: Command) {
        self.send(cmd);
        self.exit = true;
    }

    fn draw(&mut self, frame: &mut Frame, view: &View) {
        let [status, body, info, keys] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        self.animate_split();
        let [chat, questions, chapters, log] = pane_areas(body, self.split);

        // Left side wins when the status bar is too narrow; the session name gives way.
        let status_left = self.status_line(view);
        let session = Line::styled(format!("{} ", view.session), Style::new().fg(FAINT));
        let [a, b] = Layout::horizontal([
            Constraint::Length(status_left.width() as u16),
            Constraint::Min(0),
        ])
        .areas(status);
        frame.render_widget(status_left, a);
        if b.width as usize >= session.width() {
            frame.render_widget(session.alignment(Alignment::Right), b);
        }

        self.draw_chat(frame, chat, view);
        self.draw_questions(frame, questions, view);
        self.draw_chapters(frame, chapters, view);
        self.draw_log(frame, log, view);

        // The notice wins on the info line; the README summary gives way.
        let notice = self.notice_line(view);
        let [a, b] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(notice.width() as u16),
        ])
        .areas(info);
        frame.render_widget(readme_line(view), a);
        frame.render_widget(notice, b);
        frame.render_widget(Line::from(self.keys_line()), keys);
        self.draw_modal(frame, view);
    }

    /// The last action's feedback, bright when new, fading to gray after a few seconds.
    fn notice_line(&mut self, view: &View) -> Line<'static> {
        let text = self.message.as_ref().or(view.notice.as_ref()).cloned();
        let text = text.unwrap_or_default();
        if text != self.notice.0 {
            self.notice = (text.clone(), Instant::now());
        }
        if text.is_empty() {
            return Line::default();
        }
        let age = self.notice.1.elapsed().as_secs_f64();
        let color = mix(ACCENT, MUTED, (age - 2.0) / 3.0);
        Line::from(vec![
            Span::styled("› ", Style::new().fg(color)),
            Span::styled(format!("{text} "), Style::new().fg(color)),
        ])
    }

    fn status_line(&self, view: &View) -> Line<'static> {
        let t = self.tick();
        let now = self.clock.now_ts();
        let anchors = &view.meta.anchors;
        let state = &view.state;
        let breathing = mix(ACCENT, ACCENT_DIM, pulse(t, Duration::from_secs(2)));
        let (dot, label, color) = match state.stream {
            Some(StreamStatus::Live) if !state.ended => ("●", "LIVE", breathing),
            Some(StreamStatus::Upcoming) => ("○", "UPCOMING", WARN),
            Some(_) | None if state.ended => ("■", "ENDED", MUTED),
            _ => ("○", "OFFLINE", MUTED),
        };
        let elapsed = anchors
            .yt_actual_start
            .map(|start| hms(anchors.yt_actual_end.unwrap_or(now).secs_since(start)))
            .unwrap_or_default();
        let service = |name: &str| match state.services.get(name) {
            Some(ServiceStatus::Ok) => Span::styled("●", Style::new().fg(OK)),
            Some(ServiceStatus::Retrying) => Span::styled(spinner(t), Style::new().fg(WARN)),
            Some(ServiceStatus::Down) => Span::styled("✖", Style::new().fg(ACCENT).bold()),
            None => Span::styled(spinner(t), Style::new().fg(MUTED)),
        };
        let label_span = |text: &str| Span::styled(format!("{text} "), Style::new().fg(MUTED));
        let dot_sep = || Span::styled(" · ", Style::new().fg(FAINT));

        // A hotkey pressed in another app flashes the episode chip.
        let flash = self
            .flash_until
            .and_then(|until| until.checked_duration_since(Instant::now()))
            .map_or(0.0, |left| left.as_secs_f64() / FLASH.as_secs_f64());
        let chip_bg = mix(ACCENT, TEXT, flash);
        let mut spans = vec![
            Span::styled(
                format!(" ◉ #{:03} ", view.meta.episode),
                Style::new().fg(Color::Black).bg(chip_bg).bold(),
            ),
            Span::styled(format!(" {dot} "), Style::new().fg(color).bold()),
            Span::styled(
                format!("{label} {elapsed} "),
                Style::new().fg(ACCENT).bold(),
            ),
        ];
        for (on, text) in [(state.brb, "BRB"), (state.privacy, "PRIVACY")] {
            if on {
                spans.push(Span::styled(
                    format!(" {text} "),
                    Style::new().fg(Color::Black).bg(WARN).bold(),
                ));
                spans.push(Span::raw(" "));
            }
        }

        spans.push(label_span(" OBS"));
        if state.services.get("obs") == Some(&ServiceStatus::Ok) {
            spans.push(Span::styled(
                state.scene.clone().unwrap_or_else(|| "?".into()),
                Style::new().fg(TEXT),
            ));
            spans.push(match state.obs_record {
                Some(OutputState::Started | OutputState::Resumed) => {
                    Span::styled(" ● REC", Style::new().fg(breathing).bold())
                }
                Some(OutputState::Paused) => Span::styled(" ‖ REC", Style::new().fg(WARN)),
                // The local recording is the editing source: say so loudly.
                _ => Span::styled(" ○ not recording", Style::new().fg(ACCENT).bold()),
            });
        } else {
            spans.push(service("obs"));
        }
        spans.extend([
            dot_sep(),
            label_span("YouTube"),
            service("youtube"),
            dot_sep(),
            label_span("chat"),
            service("chat"),
            dot_sep(),
            label_span("keys"),
        ]);
        spans.push(match view.keys {
            KeysStatus::Off => Span::styled("off", Style::new().fg(FAINT)),
            KeysStatus::Error => Span::styled("✖", Style::new().fg(ACCENT).bold()),
            _ if state.privacy => Span::styled("✖ privacy", Style::new().fg(WARN)),
            KeysStatus::Starting => Span::styled(spinner(t), Style::new().fg(MUTED)),
            KeysStatus::On => Span::styled(sparkline(&view.keys_per_min), Style::new().fg(INFO)),
        });
        spans.extend([
            dot_sep(),
            label_span("quota"),
            Span::styled(thousands(view.quota_remaining), Style::new().fg(TEXT)),
            dot_sep(),
        ]);
        if view.latency_running {
            spans.push(Span::styled(
                format!("lat {} measuring", spinner(t)),
                Style::new().fg(WARN),
            ));
        } else {
            spans.push(label_span("lat"));
            spans.push(Span::styled(
                format!("{:.1}s", view.latency_s),
                Style::new().fg(TEXT),
            ));
        }
        let failing = view
            .preflight
            .iter()
            .filter(|c| c.state == CheckState::Fail)
            .count();
        if failing > 0 {
            spans.push(dot_sep());
            spans.push(Span::styled(
                format!("✖ {failing} pre-flight"),
                Style::new().fg(ACCENT).bold(),
            ));
        }
        spans.push(Span::raw(" "));
        Line::from(spans)
    }

    fn draw_chapters(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        let anchors = &view.meta.anchors;
        // Same format rule as the description: H:MM:SS once the VOD reaches an hour.
        let now = self.clock.now_ts();
        let long = anchors.yt_actual_start.is_some_and(|start| {
            let end = anchors.yt_actual_end.unwrap_or(now);
            vod_t(end, start, anchors.vod_offset_ms) >= 3600.0
        });
        let checkpoints = sorted_checkpoints(&view.state);
        let items: Vec<ListItem> = checkpoints
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let ts = anchors
                    .yt_actual_start
                    .map(|start| {
                        chapter_ts(vod_t(c.pressed_at, start, anchors.vod_offset_ms), long)
                    })
                    .unwrap_or_else(|| "--:--".into());
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:>2} ", i + 1), Style::new().fg(FAINT)),
                    Span::styled(format!("{ts:>8}  "), Style::new().fg(INFO)),
                    Span::styled(c.title.clone(), Style::new().fg(TEXT)),
                ]))
            })
            .collect();
        let published = if ac_template::chapters::block(&view.chapters).is_empty() {
            0
        } else {
            view.chapters.len()
        };
        let mut extra = vec![Span::styled(
            format!("· {published} published "),
            Style::new().fg(MUTED),
        )];
        if !self.pending.is_empty() {
            extra.push(Span::styled(
                format!("· {} waiting for a title ", self.pending.len()),
                Style::new().fg(WARN).bold(),
            ));
        }
        let focused = self.focus == Pane::Chapters;
        let block = pane_block(Pane::Chapters, "Chapters", extra, focused);
        if items.is_empty() {
            let hint = empty_state(&[
                "No chapters yet.",
                "Press c to add one at this moment, or Ctrl+Alt+C from any app.",
            ]);
            frame.render_widget(hint.block(block), area);
            return;
        }
        if self.chapters.selected().is_none() {
            self.chapters.select(Some(items.len() - 1));
        }
        let list = List::new(items)
            .block(block)
            .highlight_symbol("▍")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
            .highlight_style(selected_style(focused));
        frame.render_stateful_widget(list, area, &mut self.chapters);
    }

    fn draw_chat(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        // Colors are handed out in the order people first speak.
        for m in view.chat.iter() {
            self.users.get(&m.author_channel_id);
        }
        let height = area.height.saturating_sub(2) as usize;
        let len = view.chat.len();
        let scroll = self.chat_scroll.min(len);
        let end = len - scroll;
        let now = self.clock.now_ts();
        let lines: Vec<Line> = view
            .chat
            .range(end.saturating_sub(height)..end)
            .map(|m| {
                let fresh = now.secs_since(m.t_received) < FRESH_S;
                chat_line(m, self.users.get(&m.author_channel_id), fresh)
            })
            .collect();
        let extra = if scroll > 0 {
            vec![Span::styled(
                format!("↓ {scroll} newer · G jumps back "),
                Style::new().fg(WARN),
            )]
        } else {
            Vec::new()
        };
        let block = pane_block(Pane::Chat, "Chat", extra, self.focus == Pane::Chat);
        if lines.is_empty() {
            let connected = view.state.services.get("chat") == Some(&ServiceStatus::Ok);
            let text = if connected {
                "No messages yet.".to_owned()
            } else {
                format!("{} waiting for chat", spinner(self.tick()))
            };
            frame.render_widget(empty_state(&[&text]).block(block), area);
            return;
        }
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_questions(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        let queue = queue(&view.state);
        let live = mix(
            ACCENT,
            ACCENT_DIM,
            pulse(self.tick(), Duration::from_secs(2)),
        );
        let items: Vec<ListItem> = queue
            .iter()
            .map(|q| {
                let shown = q.state == ac_core::session_state::QuestionState::Shown;
                let mark = if shown {
                    Span::styled("● ", Style::new().fg(live).bold())
                } else {
                    Span::raw("  ")
                };
                let color = self.users.get(&q.author_id);
                ListItem::new(Line::from(vec![
                    mark,
                    Span::styled(q.author.clone(), Style::new().fg(color).bold()),
                    Span::styled(": ", Style::new().fg(FAINT)),
                    Span::styled(q.text.clone(), Style::new().fg(TEXT)),
                ]))
            })
            .collect();
        let focused = self.focus == Pane::Questions;
        let extra = vec![Span::styled(
            format!("({}) ", queue.len()),
            Style::new().fg(MUTED),
        )];
        let block = pane_block(Pane::Questions, "Questions", extra, focused);
        if items.is_empty() {
            let hint = empty_state(&["No questions yet. Viewers ask with !q <question>."]);
            frame.render_widget(hint.block(block), area);
            return;
        }
        let last = items.len().checked_sub(1);
        match self.questions.selected() {
            None if last.is_some() => self.questions.select(Some(0)),
            Some(i) if last.is_none_or(|l| i > l) => self.questions.select(last),
            _ => {}
        }
        let list = List::new(items)
            .block(block)
            .highlight_symbol("▍")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
            .highlight_style(selected_style(focused));
        frame.render_stateful_widget(list, area, &mut self.questions);
    }

    fn draw_log(&self, frame: &mut Frame, area: Rect, view: &View) {
        let height = area.height.saturating_sub(2) as usize;
        let events: Vec<&Event> = view.state.log.iter().collect();
        let scroll = self.log_scroll.min(events.len());
        let end = events.len() - scroll;
        let start = end.saturating_sub(height);
        let lines: Vec<Line> = events[start..end]
            .iter()
            .map(|e| {
                let time =
                    e.t.datetime()
                        .with_timezone(&Local)
                        .format("%H:%M:%S")
                        .to_string();
                let (text, dim) = summary(e);
                let (icon, color) = event_icon(e);
                let style = Style::new().fg(if dim { MUTED } else { TEXT });
                Line::from(vec![
                    Span::styled(format!("{time} "), Style::new().fg(FAINT)),
                    Span::styled(format!("{icon} "), Style::new().fg(color)),
                    Span::styled(text, style),
                ])
            })
            .collect();
        let extra = if scroll > 0 {
            vec![Span::styled(
                format!("↓ {scroll} newer · G jumps back "),
                Style::new().fg(WARN),
            )]
        } else {
            Vec::new()
        };
        let block = pane_block(Pane::Log, "Event log", extra, self.focus == Pane::Log);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// Hints for the current mode and pane, most useful first (cut at the right edge).
    fn keys_line(&self) -> Vec<Span<'static>> {
        let pairs: &[(&str, &str)] = match &self.mode {
            Mode::Input {
                prompt: Prompt::Palette,
                ..
            } => &[("Enter", "run"), ("Tab", "complete"), ("Esc", "cancel")],
            Mode::Input { .. } => &[("Enter", "save"), ("Esc", "cancel")],
            Mode::Preview { .. } => &[("Enter", "push"), ("j/k", "scroll"), ("Esc", "close")],
            _ => {
                let pane: &[(&str, &str)] = match self.focus {
                    Pane::Questions => &[("Enter", "show/answer"), ("x", "dismiss")],
                    Pane::Chapters => &[("e", "edit"), ("d", "delete")],
                    Pane::Chat | Pane::Log => &[("j/k", "scroll"), ("G", "newest")],
                };
                let global: &[(&str, &str)] = &[
                    ("c", "checkpoint"),
                    ("m", "mark"),
                    ("b", "brb"),
                    ("p", "privacy"),
                    ("s", "sync"),
                    ("1-4", "pane"),
                    ("?", "help"),
                    ("n", "note"),
                    ("r", "readme"),
                    (":", "command"),
                    ("q", "quit"),
                ];
                let mut spans = vec![Span::raw(" ")];
                for (k, label) in pane.iter().chain(global) {
                    spans.extend(hint(k, label));
                }
                return spans;
            }
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, label) in pairs {
            spans.extend(hint(k, label));
        }
        spans
    }

    fn draw_modal(&self, frame: &mut Frame, view: &View) {
        let t = self.tick();
        let chips = |pairs: &[(&str, &str)]| -> Line<'static> {
            Line::from(
                pairs
                    .iter()
                    .flat_map(|(k, l)| hint(k, l))
                    .collect::<Vec<_>>(),
            )
        };
        let (title, body, width, height): (String, Vec<Line>, u16, u16) = match &self.mode {
            Mode::Normal => return,
            Mode::Input {
                prompt,
                text,
                pressed_at,
            } => {
                let label = match prompt {
                    Prompt::Checkpoint { by: By::Hotkey } => "Checkpoint title (hotkey)",
                    Prompt::Checkpoint { .. } => "Checkpoint title",
                    Prompt::Mark => "Mark label (optional)",
                    Prompt::Note => "Private note",
                    Prompt::Edit { .. } => "Edit chapter title",
                    Prompt::Palette => "Command",
                };
                let at = pressed_at
                    .datetime()
                    .with_timezone(&Local)
                    .format("%H:%M:%S");
                let palette = *prompt == Prompt::Palette;
                let hint = if palette {
                    String::new()
                } else {
                    format!(" (time {at})")
                };
                // A blinking caret, and the palette's Tab completion as gray ghost text.
                let caret = if t.as_millis() % 1000 < 600 {
                    "▏"
                } else {
                    " "
                };
                let mut line = vec![];
                if palette {
                    line.push(Span::styled(":", Style::new().fg(ACCENT).bold()));
                }
                line.push(Span::styled(text.clone(), Style::new().fg(TEXT)));
                line.push(Span::styled(caret, Style::new().fg(ACCENT)));
                if palette && let Some(rest) = completion(text) {
                    line.push(Span::styled(rest, Style::new().fg(FAINT)));
                }
                (format!(" {label}{hint} "), vec![Line::from(line)], 70, 3)
            }
            Mode::Preview { scroll } => {
                let mut lines: Vec<Line> = Vec::new();
                if let Some(error) = &view.render_error {
                    lines.push(Line::styled(
                        format!("Render error: {error}"),
                        Style::new().fg(ACCENT),
                    ));
                    lines.push(Line::styled(
                        "Showing the last good render:",
                        Style::new().fg(MUTED),
                    ));
                    lines.push(Line::raw(""));
                }
                match &view.description {
                    Some(d) => lines.extend(d.text.lines().map(|l| Line::raw(l.to_owned()))),
                    None => lines.push(Line::raw("(nothing rendered yet)")),
                }
                let chars = view.description.as_ref().map_or(0, Description::chars);
                let area = frame.area();
                let title = format!(
                    " Description preview · {} / {} chars · Enter pushes ",
                    thousands(chars as i64),
                    thousands(DESCRIPTION_LIMIT as i64)
                );
                let popup = centered(
                    area,
                    area.width.saturating_sub(8),
                    area.height.saturating_sub(4),
                );
                frame.render_widget(Clear, popup);
                let widget = Paragraph::new(lines)
                    .block(modal_block(title))
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0));
                frame.render_widget(widget, popup);
                return;
            }
            Mode::Help => {
                let lines = help_lines();
                let height = lines.len() as u16 + 2;
                (" Help · any key closes ".into(), lines, 84, height)
            }
            Mode::Preflight => {
                let ok = view
                    .preflight
                    .iter()
                    .filter(|c| c.state == CheckState::Ok)
                    .count();
                let mut lines = vec![
                    Line::from(vec![
                        Span::styled(
                            " ◉ aftercast ",
                            Style::new().fg(Color::Black).bg(ACCENT).bold(),
                        ),
                        Span::styled(
                            format!("  {ok} of {} checks pass", view.preflight.len()),
                            Style::new().fg(MUTED),
                        ),
                    ]),
                    Line::raw(""),
                ];
                lines.extend(view.preflight.iter().map(|c| check_line(c, t)));
                lines.push(Line::raw(""));
                lines.push(Line::styled(
                    "Any key closes; :preflight opens this again.",
                    Style::new().fg(FAINT),
                ));
                let height = lines.len() as u16 + 2;
                (" Pre-flight ".into(), lines, 90, height)
            }
            Mode::ConfirmQuit => (
                " Quit ".into(),
                vec![
                    Line::raw("Quit aftercast? The session can be resumed later."),
                    Line::raw(""),
                    chips(&[
                        ("y", "quit"),
                        ("f", "finalize (final description push) and quit"),
                        ("n", "cancel"),
                    ]),
                ],
                76,
                5,
            ),
            Mode::ConfirmDelete { title, .. } => (
                " Delete chapter ".into(),
                vec![
                    Line::raw(format!("Delete {title:?}?")),
                    Line::raw(""),
                    chips(&[("y", "delete"), ("any key", "cancel")]),
                ],
                70,
                5,
            ),
            Mode::ConfirmTitle(title) => (
                " Title change from chat ".into(),
                vec![
                    Line::raw(format!("Set the YouTube title to {title:?}?")),
                    Line::raw(""),
                    chips(&[("y", "set (51 quota units)"), ("n", "drop")]),
                ],
                84,
                5,
            ),
            Mode::Ended => (
                " Stream ended ".into(),
                vec![
                    Line::raw("The stream has ended. What next?"),
                    Line::raw(""),
                    chips(&[
                        ("f", "finalize (push final description)"),
                        ("k", "keep session open"),
                        ("p", "post-process now"),
                    ]),
                ],
                92,
                5,
            ),
        };
        let popup = centered(frame.area(), width, height);
        frame.render_widget(Clear, popup);
        let widget = Paragraph::new(body)
            .block(modal_block(title))
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, popup);
    }
}

/// A parsed `:` command.
#[derive(Debug, PartialEq)]
enum Palette {
    Command(Command),
    Help,
    Preflight,
}

/// Parses a palette line (specs/02, "Command palette"); `now` is when Enter was pressed.
fn palette(line: &str, now: Timestamp) -> Result<Palette, String> {
    let (name, args) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
    let args = args.trim().to_owned();
    let need = |what: &str| {
        if args.is_empty() {
            Err(format!(":{name} needs {what}"))
        } else {
            Ok(args.clone())
        }
    };
    let cmd = match name {
        "checkpoint" | "cp" => Command::Checkpoint {
            title: need("a title")?,
            pressed_at: now,
            by: By::Palette,
        },
        "mark" => Command::Mark {
            label: Some(args.clone()).filter(|a| !a.is_empty()),
            pressed_at: now,
            by: By::Palette,
        },
        "note" => Command::Note {
            text: need("text")?,
            pressed_at: now,
        },
        "quest" => Command::Quest {
            title: need("a title")?,
        },
        "sync" => Command::Sync,
        "finalize" => Command::Finalize,
        "reload" => Command::Reload,
        "quit" | "q" => Command::Quit,
        "help" | "?" => return Ok(Palette::Help),
        "preflight" => return Ok(Palette::Preflight),
        "break" | "b" | "brb" => Command::Brb {
            on: Some(true),
            by: By::Palette,
        },
        "back" => Command::Brb {
            on: Some(false),
            by: By::Palette,
        },
        "privacy" => Command::Privacy {
            on: match args.as_str() {
                "on" => Some(true),
                "off" => Some(false),
                "" => None,
                _ => return Err(":privacy takes on or off".into()),
            },
            by: By::Palette,
        },
        "scene" => Command::Scene {
            name: need("a scene name")?,
        },
        "say" => Command::Say {
            text: need("text")?,
        },
        "overlay" => {
            let (overlay, state) = args.split_once(' ').unwrap_or((args.as_str(), ""));
            let on = match state.trim() {
                "on" => true,
                "off" => false,
                _ => return Err(":overlay <name> on|off".into()),
            };
            Command::Overlay {
                name: overlay.to_owned(),
                on,
            }
        }
        "latency" => Command::Latency,
        "lat" => Command::LatencySample {
            counter: need("the number shown on stream")?
                .parse()
                .map_err(|_| ":lat takes the number shown on stream".to_owned())?,
            at: now,
        },
        "" => return Err(String::new()),
        other => return Err(format!("unknown command :{other} (try :help)")),
    };
    Ok(Palette::Command(cmd))
}

/// Questions waiting or on screen, in the order they were asked.
fn queue(state: &SessionState) -> Vec<&ac_core::session_state::Question> {
    use ac_core::session_state::QuestionState;
    state
        .questions
        .iter()
        .filter(|q| matches!(q.state, QuestionState::Open | QuestionState::Shown))
        .collect()
}

fn chat_line(m: &ChatMessage, color: Color, fresh: bool) -> Line<'_> {
    use ac_core::chat::ChatKind;
    // A colored bar marks lines that just arrived.
    let mut spans = vec![Span::styled(
        if fresh { "▎" } else { " " },
        Style::new().fg(color),
    )];
    let (badge, badge_color) = if m.is_owner {
        ("★ ", ACCENT)
    } else if m.is_moderator {
        ("⚑ ", INFO)
    } else if m.is_member {
        ("✦ ", OK)
    } else {
        ("", MUTED)
    };
    if !badge.is_empty() {
        spans.push(Span::styled(badge, Style::new().fg(badge_color)));
    }
    let name = if m.is_owner { ACCENT } else { color };
    spans.push(Span::styled(
        m.author_name.as_str(),
        Style::new().fg(name).bold(),
    ));
    spans.push(Span::styled(": ", Style::new().fg(FAINT)));
    if let Some(amount) = &m.amount {
        spans.push(Span::styled(
            format!(" {amount} "),
            Style::new().fg(Color::Black).bg(WARN).bold(),
        ));
        spans.push(Span::raw(" "));
    }
    let text_style = match m.kind {
        ChatKind::Text if m.text.starts_with('!') => Style::new().fg(INFO),
        ChatKind::Text | ChatKind::Superchat | ChatKind::Supersticker => Style::new().fg(TEXT),
        _ => Style::new().fg(MUTED).italic(),
    };
    spans.push(Span::styled(m.text.as_str(), text_style));
    Line::from(spans)
}

fn sorted_checkpoints(state: &SessionState) -> Vec<&ac_core::session_state::Checkpoint> {
    let mut list: Vec<_> = state.checkpoints.iter().collect();
    list.sort_by_key(|c| c.pressed_at);
    list
}

/// A rounded pane frame titled with its number chip (press the number to focus it).
fn pane_block(pane: Pane, label: &str, extra: Vec<Span<'static>>, focused: bool) -> Block<'static> {
    let (number, name, border) = if focused {
        (
            Style::new().fg(Color::Black).bg(ACCENT).bold(),
            Style::new().fg(TEXT).bold(),
            ACCENT,
        )
    } else {
        (
            Style::new().fg(MUTED).bg(RAISED),
            Style::new().fg(MUTED),
            FAINT,
        )
    };
    let mut title = vec![
        Span::styled(format!(" {} ", pane.number()), number),
        Span::styled(format!(" {label} "), name),
    ];
    title.extend(extra);
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(Line::from(title))
}

fn modal_block(title: String) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(title, Style::new().fg(TEXT).bold()))
        .padding(Padding::horizontal(1))
}

fn selected_style(focused: bool) -> Style {
    if focused {
        Style::new().bg(SELECTED).fg(TEXT).bold()
    } else {
        Style::new().bg(RAISED)
    }
}

/// Gray hint text for a pane with nothing in it yet.
fn empty_state<'a>(lines: &[&str]) -> Paragraph<'a> {
    let lines: Vec<Line> = lines
        .iter()
        .map(|l| Line::styled(format!(" {l}"), Style::new().fg(FAINT).italic()))
        .collect();
    Paragraph::new(lines).wrap(Wrap { trim: false })
}

fn readme_line(view: &View) -> Line<'static> {
    let mut parts = vec![Span::styled(" README ", Style::new().fg(MUTED))];
    match (&view.render_error, &view.description) {
        (Some(error), _) => parts.push(Span::styled(
            format!("✖ {error}"),
            Style::new().fg(ACCENT).bold(),
        )),
        (None, Some(d)) => {
            let chars = d.chars();
            let (bar, color) = gauge(chars, DESCRIPTION_LIMIT, 10);
            parts.push(Span::styled(bar, Style::new().fg(color)));
            parts.push(Span::styled(
                format!(
                    " {} / {} chars",
                    thousands(chars as i64),
                    thousands(DESCRIPTION_LIMIT as i64)
                ),
                Style::new().fg(TEXT),
            ));
        }
        (None, None) => parts.push(Span::styled("not rendered", Style::new().fg(FAINT))),
    }
    let published = if ac_template::chapters::block(&view.chapters).is_empty() {
        0
    } else {
        view.chapters.len()
    };
    let sep = || Span::styled(" · ", Style::new().fg(FAINT));
    parts.push(sep());
    parts.push(Span::styled(
        format!("{published} chapters"),
        Style::new().fg(MUTED),
    ));
    if let Some((t, trigger)) = view.state.last_push {
        let at = t.datetime().with_timezone(&Local).format("%H:%M");
        parts.push(sep());
        parts.push(Span::styled(
            format!("last push {at} ({trigger:?})").to_lowercase(),
            Style::new().fg(MUTED),
        ));
    }
    Line::from(parts)
}

/// `▰▰▰▱▱` filled to `used / limit`: green, amber past 80 %, red past the limit.
fn gauge(used: usize, limit: usize, cells: usize) -> (String, Color) {
    let filled = (used * cells).div_ceil(limit.max(1)).min(cells);
    let bar = "▰".repeat(filled) + &"▱".repeat(cells - filled);
    let color = match used * 100 / limit.max(1) {
        0..=80 => OK,
        81..=100 => WARN,
        _ => ACCENT,
    };
    (bar, color)
}

/// `▁▃▅█` style bars scaled to the largest value.
fn sparkline(values: &[u32]) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0).max(1);
    values
        .iter()
        .map(|v| BARS[(*v as usize * (BARS.len() - 1)).div_ceil(max as usize)])
        .collect()
}

/// One-line event description; `true` marks routine events shown dimmed.
fn summary(e: &Event) -> (String, bool) {
    match &e.kind {
        EventKind::Checkpoint { title, .. } => (format!("checkpoint {title:?}"), false),
        EventKind::CheckpointEdit { title, .. } => (format!("chapter renamed to {title:?}"), false),
        EventKind::CheckpointDelete { .. } => ("chapter deleted".into(), false),
        EventKind::Mark { label: Some(l), .. } => (format!("mark {l:?}"), false),
        EventKind::Mark { label: None, .. } => ("mark".into(), false),
        EventKind::Note { .. } => ("note".into(), false),
        EventKind::QuestSet { title, .. } => (format!("quest {title:?}"), false),
        EventKind::DescRendered {
            ok: true,
            chars,
            chapters,
            ..
        } => (
            format!("desc rendered ({chars} chars, {chapters} chapters)"),
            true,
        ),
        EventKind::DescRendered { error, .. } => (
            format!("render error: {}", error.as_deref().unwrap_or("?")),
            false,
        ),
        EventKind::DescPushed { chars, trigger } => {
            (format!("desc pushed ({chars} chars, {trigger:?})"), false)
        }
        EventKind::DescPushFailed { error, .. } => (
            format!("push failed: {}", error.as_deref().unwrap_or("?")),
            false,
        ),
        EventKind::QuotaSpent { method, units, .. } => (format!("quota -{units} {method}"), true),
        EventKind::StreamStatus { status, .. } => {
            (format!("stream {status:?}").to_lowercase(), false)
        }
        EventKind::StreamEnded { .. } => ("stream ended".into(), false),
        EventKind::ObsScene { to, .. } => (format!("scene {to:?}"), true),
        EventKind::ObsStream { state } => (format!("OBS stream {state:?}").to_lowercase(), false),
        EventKind::ObsRecord {
            state,
            path: Some(p),
        } => (format!("recording {state:?} {p}").to_lowercase(), false),
        EventKind::ObsRecord { state, .. } => {
            (format!("recording {state:?}").to_lowercase(), false)
        }
        EventKind::BrbStart { .. } => ("BRB on".into(), false),
        EventKind::BrbEnd {} => ("BRB off".into(), false),
        EventKind::PrivacyStart { .. } => ("privacy on".into(), false),
        EventKind::PrivacyEnd {} => ("privacy off".into(), false),
        EventKind::ServiceState { service, state } => {
            (format!("{service} {state:?}").to_lowercase(), true)
        }
        EventKind::BotCmd {
            cmd, args, author, ..
        } => (
            format!("{author}: !{cmd} {args}").trim_end().to_owned(),
            true,
        ),
        EventKind::BotReply { mode, text, .. } => {
            (format!("bot ({mode:?}): {text}").to_lowercase(), true)
        }
        EventKind::QuestionAdd { author, .. } => (format!("question from {author}"), false),
        EventKind::QuestionShow { .. } => ("question on stream".into(), false),
        EventKind::QuestionAnswered { .. } => ("question answered".into(), false),
        EventKind::QuestionDismiss { .. } => ("question dismissed".into(), true),
        EventKind::LatencyMeasured { latency_s, samples } => (
            format!("latency {latency_s:.1} s ({samples} samples)"),
            false,
        ),
        EventKind::TitlePushed { title } => (format!("title set to {title:?}"), false),
        EventKind::CaptureState { keys, reason } => (
            match reason {
                Some(r) => format!("keys {keys:?}: {r}"),
                None => format!("keys {keys:?}"),
            }
            .to_lowercase(),
            false,
        ),
        EventKind::CaptureSecretDropped { source } => (
            format!("secret dropped from {source:?} capture").to_lowercase(),
            false,
        ),
        EventKind::ShellCmd { cmd, .. } => (format!("$ {cmd}"), true),
        EventKind::ServiceError { service, error, .. } => (
            format!("{service} error: {}", error.as_deref().unwrap_or("?")),
            false,
        ),
        _ => {
            let json = serde_json::to_value(e).unwrap_or_default();
            (json["type"].as_str().unwrap_or("event").to_owned(), true)
        }
    }
}

/// Icon and color for an event log line.
fn event_icon(e: &Event) -> (&'static str, Color) {
    match &e.kind {
        EventKind::Checkpoint { .. }
        | EventKind::CheckpointEdit { .. }
        | EventKind::CheckpointDelete { .. } => ("◆", ACCENT),
        EventKind::Mark { .. } => ("★", WARN),
        EventKind::Note { .. } => ("✎", MUTED),
        EventKind::QuestSet { .. } => ("⚑", INFO),
        EventKind::DescRendered { ok: true, .. } => ("↻", FAINT),
        EventKind::DescPushed { .. } | EventKind::TitlePushed { .. } => ("⇡", OK),
        EventKind::DescRendered { .. }
        | EventKind::DescPushFailed { .. }
        | EventKind::ServiceError { .. } => ("✖", ACCENT),
        EventKind::StreamStatus { .. }
        | EventKind::StreamEnded { .. }
        | EventKind::ObsStream { .. }
        | EventKind::ObsRecord { .. } => ("●", INFO),
        EventKind::BrbStart { .. }
        | EventKind::BrbEnd {}
        | EventKind::PrivacyStart { .. }
        | EventKind::PrivacyEnd {} => ("◐", WARN),
        EventKind::QuestionAdd { .. }
        | EventKind::QuestionShow { .. }
        | EventKind::QuestionAnswered { .. }
        | EventKind::QuestionDismiss { .. } => ("?", INFO),
        EventKind::LatencyMeasured { .. } => ("◷", INFO),
        EventKind::CaptureSecretDropped { .. } => ("⚿", WARN),
        EventKind::BotCmd { .. } | EventKind::BotReply { .. } => ("›", MUTED),
        EventKind::ShellCmd { .. } => ("$", FAINT),
        _ => ("·", FAINT),
    }
}

fn help_lines() -> Vec<Line<'static>> {
    let sections: &[(&str, &[(&str, &str)])] = &[
        (
            "Moments (time is taken when the key is pressed)",
            &[
                ("c", "checkpoint: a chapter, then type its title"),
                ("m", "mark a good moment (optional label)"),
                ("n", "private note (never published)"),
            ],
        ),
        (
            "Stream",
            &[
                ("b", "BRB on/off (OBS BRB scene, then back)"),
                (
                    "p",
                    "privacy on/off (OBS privacy scene, key capture paused)",
                ),
                ("s", "preview the description; Enter pushes it"),
                (
                    "r",
                    "edit the README in Neovim (clean, no config); :wq comes back",
                ),
            ],
        ),
        (
            "Panes",
            &[
                ("1 2 3 4", "focus Chat, Questions, Chapters, Event log"),
                ("Tab", "next pane (Shift+Tab: previous)"),
                ("j/k", "move or scroll; g/G first/newest"),
                ("e / d", "edit / delete the selected chapter"),
                ("Enter", "show the selected question; again: answered"),
                ("x", "dismiss the selected question"),
            ],
        ),
        (
            "Commands (: opens, Tab completes)",
            &[
                (
                    ":cp <title>",
                    "checkpoint at Enter   :mark [label]   :note <text>",
                ),
                (
                    ":sync",
                    "push the description now   :finalize   end the session",
                ),
                (
                    ":brb  :back",
                    ":privacy [on|off]   :scene <name>   :quest <title>",
                ),
                (":say <text>", "chat as the channel (20 quota units)"),
                (
                    ":overlay",
                    "<name> on|off: now, question, toast, brb, latency, chapters",
                ),
                (
                    ":latency",
                    "start/stop measuring; :lat <n> enters the number seen",
                ),
                (":preflight", "the checklist again   :reload   :quit"),
            ],
        ),
        (
            "Global hotkeys (any app, [hotkeys] in config)",
            &[
                ("Ctrl+Alt+C", "checkpoint   Ctrl+Alt+M mark"),
                ("Ctrl+Alt+B", "BRB   Ctrl+Alt+P privacy"),
            ],
        ),
    ];
    let mut lines = Vec::new();
    for (i, (heading, rows)) in sections.iter().enumerate() {
        if i > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(*heading, Style::new().fg(ACCENT).bold()));
        for (k, what) in *rows {
            lines.push(Line::from(vec![
                Span::styled(format!("  {k:<13}"), Style::new().fg(TEXT).bold()),
                Span::styled(*what, Style::new().fg(MUTED)),
            ]));
        }
    }
    lines
}

fn check_line(c: &Check, t: Duration) -> Line<'static> {
    let (mark, style) = match c.state {
        CheckState::Ok => ("✔", Style::new().fg(OK)),
        CheckState::Fail => ("✖", Style::new().fg(ACCENT).bold()),
        CheckState::Pending => (spinner(t), Style::new().fg(MUTED)),
    };
    let mut spans = vec![
        Span::styled(format!(" {mark} "), style),
        Span::styled(c.label.clone(), style),
    ];
    if !c.detail.is_empty() {
        spans.push(Span::styled(
            format!("  {}", c.detail),
            Style::new().fg(FAINT),
        ));
    }
    Line::from(spans)
}

/// Palette command names, in Tab completion order.
const COMMANDS: [&str; 19] = [
    "checkpoint",
    "cp",
    "mark",
    "note",
    "quest",
    "sync",
    "finalize",
    "reload",
    "quit",
    "help",
    "preflight",
    "brb",
    "back",
    "privacy",
    "scene",
    "say",
    "overlay",
    "latency",
    "lat",
];

/// What Tab adds to a half-typed palette command name (`""` when it is complete).
fn completion(text: &str) -> Option<&'static str> {
    if text.is_empty() || text.contains(' ') {
        return None;
    }
    if COMMANDS.contains(&text) {
        return Some("");
    }
    COMMANDS
        .iter()
        .find(|c| c.starts_with(text))
        .map(|c| &c[text.len()..])
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(row);
    cell
}

/// `9310` as `9,310`.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-11-19T10:00:00Z".parse().unwrap()
    }

    #[test]
    fn palette_commands() {
        assert_eq!(
            palette("cp Parser with nom", now()),
            Ok(Palette::Command(Command::Checkpoint {
                title: "Parser with nom".into(),
                pressed_at: now(),
                by: By::Palette
            }))
        );
        assert_eq!(
            palette("mark", now()),
            Ok(Palette::Command(Command::Mark {
                label: None,
                pressed_at: now(),
                by: By::Palette
            }))
        );
        assert_eq!(
            palette(" sync ", now()),
            Ok(Palette::Command(Command::Sync))
        );
        assert_eq!(palette("?", now()), Ok(Palette::Help));
        assert!(
            palette("checkpoint", now())
                .unwrap_err()
                .contains("needs a title")
        );
        assert_eq!(
            palette("brb", now()),
            Ok(Palette::Command(Command::Brb {
                on: Some(true),
                by: By::Palette
            }))
        );
        assert_eq!(
            palette("privacy off", now()),
            Ok(Palette::Command(Command::Privacy {
                on: Some(false),
                by: By::Palette
            }))
        );
        assert_eq!(
            palette("scene Starting Soon", now()),
            Ok(Palette::Command(Command::Scene {
                name: "Starting Soon".into()
            }))
        );
        assert!(palette("privacy maybe", now()).is_err());
        assert_eq!(
            palette("say hello chat", now()),
            Ok(Palette::Command(Command::Say {
                text: "hello chat".into()
            }))
        );
        assert_eq!(
            palette("overlay toast off", now()),
            Ok(Palette::Command(Command::Overlay {
                name: "toast".into(),
                on: false
            }))
        );
        assert!(palette("overlay toast", now()).is_err());
        assert_eq!(
            palette("lat 2350", now()),
            Ok(Palette::Command(Command::LatencySample {
                counter: 2350,
                at: now()
            }))
        );
        assert!(palette("lat soon", now()).is_err());
        assert_eq!(palette("preflight", now()), Ok(Palette::Preflight));
        assert!(
            palette("frobnicate", now())
                .unwrap_err()
                .contains("unknown")
        );
    }

    fn sample_view(clock: &Clock) -> View {
        use ac_core::session::{Anchors, Status};
        let start = clock.now_ts().add_secs(-3725.0);
        let mut state = SessionState::default();
        let kinds = [
            EventKind::StreamStatus {
                status: StreamStatus::Live,
                source: ac_core::event::Source::Youtube,
            },
            EventKind::Checkpoint {
                cid: "1".into(),
                title: "Project setup".into(),
                pressed_at: start.add_secs(760.0),
                by: By::Tui,
                latency_corrected_s: None,
            },
            EventKind::Mark {
                label: Some("borrow checker rant".into()),
                pressed_at: start.add_secs(900.0),
                by: By::Tui,
                replay_path: None,
            },
        ];
        for kind in kinds {
            state.apply(&Event::new(clock, "tui", kind));
        }
        View {
            session: "2026-11-19_ep042".into(),
            readme: "sessions/2026/2026-11-19_ep042/README.md".into(),
            meta: SessionMeta {
                schema_version: 1,
                id: "x".into(),
                episode: 42,
                platform: "youtube".into(),
                video_id: "abcdefghijk".into(),
                live_chat_id: None,
                title_at_start: None,
                created_at: start,
                status: Status::Live,
                anchors: Anchors {
                    yt_actual_start: Some(start),
                    ..Anchors::default()
                },
            },
            state,
            chapters: Vec::new(),
            description: Some(Description {
                markdown: String::new(),
                text: "Line one".into(),
            }),
            render_error: None,
            quota_remaining: 9310,
            notice: Some("description pushed (8 chars)".into()),
            chat: Arc::new(VecDeque::from([ChatMessage {
                id: "m1".into(),
                t_received: start,
                t_published: None,
                author_channel_id: "UCbob".into(),
                author_name: "bob".into(),
                is_owner: false,
                is_moderator: false,
                is_member: false,
                kind: ac_core::chat::ChatKind::Text,
                text: "!q why rust over go?".into(),
                amount: None,
            }])),
            pending_title: None,
            latency_running: false,
            latency_s: 4.2,
            keys: KeysStatus::On,
            keys_per_min: vec![0, 10, 40],
            preflight: vec![
                Check {
                    label: "OBS connected".into(),
                    state: CheckState::Ok,
                    detail: "ws://127.0.0.1:4455".into(),
                },
                Check {
                    label: "OBS recording".into(),
                    state: CheckState::Fail,
                    detail: "the local recording is the editing source".into(),
                },
            ],
        }
    }

    fn screen(app: &mut App, view: &View) -> String {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 16)).unwrap();
        terminal.draw(|f| app.draw(f, view)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            )
    }

    #[test]
    fn draws_status_chapters_log_and_prompt() {
        let clock = Arc::new(Clock::new());
        let view = sample_view(&clock);
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock.clone(), rx, cmd_tx);

        // The pre-flight checklist opens first; any key closes it.
        let text = screen(&mut app, &view);
        assert!(text.contains("Pre-flight"), "{text}");
        assert!(text.contains("✖ OBS recording"), "{text}");
        app.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &view);

        let text = screen(&mut app, &view);
        if std::env::var_os("SHOW_TUI").is_some() {
            println!("{text}");
        }
        assert!(text.contains("#042"), "{text}");
        assert!(text.contains("LIVE 01:02:05"), "{text}");
        assert!(text.contains("quota 9,310"), "{text}");
        assert!(text.contains("0:12:40  Project setup"), "{text}");
        assert!(text.contains("Project setup"), "{text}");
        assert!(text.contains("mark \"borrow checker rant\""), "{text}");
        assert!(text.contains("description pushed"), "{text}");
        assert!(text.contains("✖ 1 pre-flight"), "{text}");
        assert!(text.contains("keys ▁▃█"), "{text}");

        // `c` captures the time first; typing then Enter sends the checkpoint.
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        press(&mut app, KeyCode::Char('c'));
        let Mode::Input { pressed_at, .. } = app.mode.clone() else {
            panic!("prompt open")
        };
        assert!(screen(&mut app, &view).contains("Checkpoint title"));
        clock.advance(Duration::from_secs(30));
        for c in "Parser".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Checkpoint {
                title: "Parser".into(),
                pressed_at,
                by: By::Tui
            }
        );

        // Esc cancels without recording anything.
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('x'));
        press(&mut app, KeyCode::Esc);
        assert!(cmd_rx.try_recv().is_err());

        // `s` previews, Enter pushes.
        press(&mut app, KeyCode::Char('s'));
        assert!(screen(&mut app, &view).contains("Description preview"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Sync);

        // `r` queues the README for Neovim (opened by the run loop); nothing is pushed.
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.edit_readme.as_deref(), Some(view.readme.as_path()));
        assert!(cmd_rx.try_recv().is_err());
    }

    #[test]
    fn hotkey_checkpoints_prompt_in_order_and_save_untitled_after_timeout() {
        let clock = Arc::new(Clock::new());
        let view = sample_view(&clock);
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let (ext_tx, ext_rx) = std::sync::mpsc::channel();
        let mut app = App::new(clock.clone(), rx, cmd_tx);
        app.external = Some(ext_rx);
        app.mode = Mode::Normal;
        let first = clock.now_ts();
        ext_tx.send(External::Checkpoint(first)).unwrap();
        ext_tx
            .send(External::Checkpoint(first.add_secs(1.0)))
            .unwrap();
        app.poll_external(&view);
        assert!(screen(&mut app, &view).contains("Checkpoint title (hotkey)"));

        // Ten minutes later both are saved with default titles at their press times.
        clock.advance(Duration::from_secs(601));
        app.poll_external(&view);
        let saved: Vec<Command> = std::iter::from_fn(|| cmd_rx.try_recv().ok()).collect();
        let times: Vec<_> = saved
            .iter()
            .map(|c| match c {
                Command::Checkpoint {
                    pressed_at,
                    by: By::Hotkey,
                    title,
                } => {
                    assert!(title.starts_with("Checkpoint "));
                    *pressed_at
                }
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(times, [first, first.add_secs(1.0)]);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn questions_pane_shows_and_dismisses_and_title_asks() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        for (qid, author) in [("q1", "bob"), ("q2", "cy")] {
            let kind = EventKind::QuestionAdd {
                qid: qid.into(),
                author: author.into(),
                author_id: format!("UC{author}"),
                text: "why rust?".into(),
            };
            view.state.apply(&Event::new(&clock, "bot", kind));
        }
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        let text = screen(&mut app, &view);
        assert!(text.contains("Questions (2)"), "{text}");
        assert!(
            text.contains("bob: !q why rust over go?"),
            "chat pane: {text}"
        );
        assert!(text.contains(" 2  Questions (2)"), "numbered panes: {text}");
        assert!(text.contains("lat 4.2s"), "{text}");

        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        press(&mut app, KeyCode::BackTab); // Chapters -> Questions
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Question {
                qid: "q2".into(),
                action: QuestionAction::Show
            }
        );
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Question {
                qid: "q2".into(),
                action: QuestionAction::Dismiss
            }
        );

        app.mode = Mode::ConfirmTitle("New title".into());
        assert!(screen(&mut app, &view).contains("Set the YouTube title"));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::ConfirmTitle { ok: true }
        );
        assert_eq!(app.title_answered.as_deref(), Some("New title"));
    }

    #[test]
    fn number_keys_focus_panes_and_tab_completes_commands() {
        let clock = Arc::new(Clock::new());
        let view = sample_view(&clock);
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        for (key, pane) in [('1', Pane::Chat), ('4', Pane::Log), ('2', Pane::Questions)] {
            press(&mut app, KeyCode::Char(key));
            assert_eq!(app.focus, pane);
        }
        press(&mut app, KeyCode::Char('9'));
        assert_eq!(app.focus, Pane::Questions);

        assert_eq!(completion("fin"), Some("alize"));
        assert_eq!(completion("lat"), Some(""));
        assert_eq!(completion("say hi"), None);
        assert_eq!(completion("zzz"), None);
        press(&mut app, KeyCode::Char(':'));
        for c in "sy".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Sync);
    }

    #[test]
    fn focused_pane_grows_and_others_stay_visible() {
        let body = Rect::new(0, 0, 120, 30);
        for pane in Pane::ORDER {
            let areas = pane_areas(body, pane.split());
            let focused = areas[pane.number() - 1];
            for (i, a) in areas.iter().enumerate() {
                assert!(a.height >= 3 && a.width >= 20, "{pane:?} pane {i}: {a:?}");
                assert!(focused.area() >= a.area(), "{pane:?} is the largest");
            }
        }
        // Tiny terminals still split instead of panicking.
        let tiny = pane_areas(Rect::new(0, 0, 10, 4), Pane::Chat.split());
        assert!(tiny.iter().all(|a| a.width > 0));
    }

    #[test]
    fn split_glides_to_the_focused_pane() {
        let clock = Arc::new(Clock::new());
        let view = sample_view(&clock);
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        assert!(!app.resizing());
        app.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE), &view);
        assert!(app.resizing());
        app.last_frame -= Duration::from_secs(1); // one long frame finishes the move
        app.animate_split();
        assert_eq!(app.split, Pane::Chat.split());
    }

    #[test]
    fn gauge_fills_and_turns_red_past_the_limit() {
        assert_eq!(gauge(0, 5000, 5), ("▱▱▱▱▱".into(), OK));
        assert_eq!(gauge(4500, 5000, 5), ("▰▰▰▰▰".into(), WARN));
        assert_eq!(gauge(6000, 5000, 5).1, ACCENT);
    }

    #[test]
    fn sparkline_scales_to_the_peak() {
        assert_eq!(sparkline(&[0, 10, 40, 20]), "▁▃█▅");
        assert_eq!(sparkline(&[0, 0]), "▁▁");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(thousands(9310), "9,310");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(-1500), "-1,500");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
