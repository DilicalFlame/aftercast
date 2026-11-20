//! Terminal UI for live sessions (specs/02-cli-and-tui.md).
//!
//! The UI only reads [`View`] snapshots and sends [`Command`]s; the controller owns state.
//! Times are captured when a key is pressed, before any prompt opens.

mod home;
mod palette;
mod settings;
mod theme;

use std::collections::VecDeque;
use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ac_core::chat::{ChatKind, ChatMessage};
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
    /// The config in effect; the settings page (`,`) shows and edits it.
    pub config: Arc<ac_core::config::Config>,
    /// `aftercast.toml`, written by the settings page, then reloaded.
    pub config_path: std::path::PathBuf,
    /// The session was started from Home. Before that nothing lasting or public happens.
    pub begun: bool,
    /// A new session (a draft until started; quitting before that keeps nothing).
    pub fresh: bool,
    /// OBS scenes, audio inputs and scene sources (empty while OBS is down).
    pub obs: Arc<ac_core::obs::Media>,
    /// Live audio peaks per input, filled while a page with meters is open.
    pub levels: ac_core::obs::Levels,
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
    /// How to fix it, while it is not fine (Home shows it under the check).
    pub fix: String,
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
    /// The settings page (`,`).
    Settings(settings::Page),
    /// `:stream stop` asks first: ending the broadcast cannot be undone.
    ConfirmStreamStop,
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

    fn from_key(c: char) -> Option<Self> {
        let n = c.to_digit(10)? as usize;
        Self::ORDER.get(n.checked_sub(1)?).copied()
    }
}

/// Inbox filter tabs (`[` / `]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Questions,
    Supers,
    Members,
    /// Answered, thanked or dismissed this session, newest first.
    Done,
}

impl Filter {
    const ORDER: [Filter; 5] = [
        Filter::All,
        Filter::Questions,
        Filter::Supers,
        Filter::Members,
        Filter::Done,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Questions => "Questions",
            Filter::Supers => "Super Chats",
            Filter::Members => "Members",
            Filter::Done => "Done",
        }
    }

    fn keeps(self, q: &ac_core::session_state::Question) -> bool {
        use ac_core::session_state::QuestionState;
        let waiting = matches!(q.state, QuestionState::Open | QuestionState::Shown);
        match (self, q.support.as_ref().map(|s| s.kind)) {
            (Filter::Done, _) => !waiting,
            _ if !waiting => false,
            (Filter::All, _) | (Filter::Questions, None) => true,
            (Filter::Supers, Some(kind)) => kind != ChatKind::Membership,
            (Filter::Members, Some(kind)) => kind == ChatKind::Membership,
            _ => false,
        }
    }

    fn step(self, delta: isize) -> Self {
        let i = Self::ORDER.iter().position(|f| *f == self).unwrap_or(0) as isize;
        Self::ORDER[(i + delta).rem_euclid(Self::ORDER.len() as isize) as usize]
    }
}

/// Left column width, then the top pane's share of the left and of the right column.
type Split = [f64; 3];

/// Lazygit-style layout. The focused pane's column widens and the pane takes what its
/// neighbour does not need: the neighbour shrinks to its content, at most 28 % of the
/// column. In the other column the pane with less content gets just enough rows (at most
/// half) and the busier one the rest. `want` is each pane's content height with borders.
fn target_split(focus: Pane, body: Rect, want: [u16; 4]) -> Split {
    let width = match focus {
        Pane::Chat | Pane::Questions => 0.58,
        Pane::Chapters | Pane::Log => 0.40,
    };
    let h = f64::from(body.height.max(1));
    let need = |p: Pane| f64::from(want[p.number() - 1]) / h;
    let column = |top: Pane, bottom: Pane| {
        if focus == top {
            1.0 - need(bottom).min(0.28)
        } else if focus == bottom {
            need(top).min(0.28)
        } else if need(top) <= need(bottom) {
            need(top).min(0.5)
        } else {
            1.0 - need(bottom).min(0.5)
        }
    };
    [
        width,
        column(Pane::Chat, Pane::Questions),
        column(Pane::Chapters, Pane::Log),
    ]
}

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
    chat_feed: Feed,
    log_feed: Feed,
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
    /// `tui.animations` as of the last frame.
    motion: bool,
    /// The highlighted palette suggestion, and whether ↑/↓ chose it.
    pick: usize,
    picked: bool,
    /// Audio meters were asked of OBS (a page showing them is open).
    meters: bool,
    /// The focused item on Home.
    home_pick: usize,
    /// Palette lines run before, oldest first (`<home>/state/history`), read on first use.
    history: Option<Vec<String>>,
    filter: Filter,
    /// Pane proportions as drawn: they glide towards `split_target`.
    split: Split,
    split_target: Split,
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
            chat_feed: Feed::default(),
            log_feed: Feed::default(),
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
            motion: true,
            pick: 0,
            picked: false,
            meters: false,
            home_pick: 0,
            history: None,
            filter: Filter::All,
            split: [0.45, 0.6, 0.5],
            split_target: [0.45, 0.6, 0.5],
            last_frame: Instant::now(),
        }
    }

    /// The Inbox as shown: [`queue`] through the current filter tab.
    fn inbox<'v>(&self, state: &'v SessionState) -> Vec<&'v ac_core::session_state::Question> {
        if self.filter == Filter::Done {
            let mut done: Vec<_> = state
                .questions
                .iter()
                .filter(|q| self.filter.keeps(q))
                .collect();
            done.sort_by_key(|q| std::cmp::Reverse(q.done_at));
            return done;
        }
        queue(state)
            .into_iter()
            .filter(|q| self.filter.keeps(q))
            .collect()
    }

    /// `u`: the selected item back to waiting in Done, else the last one handled.
    fn undo(&mut self, view: &View) {
        let target = match self.filter {
            Filter::Done => self.selected_item(view),
            _ => view
                .state
                .questions
                .iter()
                .filter(|q| q.done_at.is_some())
                .max_by_key(|q| q.done_at),
        };
        let Some(q) = target else {
            self.message = Some("nothing to undo".into());
            return;
        };
        let what = match q.support.as_ref().map(|s| s.kind) {
            None => "question",
            Some(ChatKind::Membership) => "membership",
            Some(_) => "Super Chat",
        };
        self.message = Some(format!("{}'s {what} is back in the Inbox", q.author));
        let qid = q.qid.clone();
        self.send(Command::Question {
            qid,
            action: QuestionAction::Reopen,
        });
    }

    /// A breathing 0..1..0 over `period`; still (1) with animations off.
    fn pulse(&self, period: Duration) -> f64 {
        if self.motion {
            pulse(self.tick(), period)
        } else {
            0.0
        }
    }

    /// A spinner frame; `…` with animations off.
    fn spin(&self) -> &'static str {
        if self.motion {
            spinner(self.tick())
        } else {
            "…"
        }
    }

    /// Time since start, the input of every animation.
    fn tick(&self) -> Duration {
        self.started.elapsed()
    }

    /// Moves the drawn split a step towards the target (eases out in about 150 ms,
    /// whatever the frame rate).
    fn animate_split(&mut self) {
        let dt = self.last_frame.elapsed().as_secs_f64();
        self.last_frame = Instant::now();
        let k = if self.motion {
            (dt * 20.0).min(1.0)
        } else {
            1.0
        };
        for (cur, goal) in self.split.iter_mut().zip(self.split_target) {
            *cur += (goal - *cur) * k;
            if (goal - *cur).abs() < 0.002 {
                *cur = goal;
            }
        }
    }

    fn resizing(&self) -> bool {
        self.split != self.split_target
    }

    fn run(mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        while !self.exit {
            let view = self.view.borrow_and_update().clone();
            self.poll_external(&view);
            // Until the session starts, Home is where everything returns to.
            if !view.begun && self.mode == Mode::Normal {
                self.mode = Mode::Preflight;
            }
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
            // OBS sends audio levels only while the Home page shows them.
            let want = self.mode == Mode::Preflight;
            if want != self.meters {
                self.meters = want;
                self.send(Command::Meters { on: want });
            }
            let frame = if self.resizing() {
                16
            } else if self.mode == Mode::Preflight {
                home::FRAME.as_millis() as u64
            } else {
                100
            };
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

    fn key(&mut self, mut key: KeyEvent, view: &View) {
        // In the palette, Ctrl+J/K (and Ctrl+N/P) move through the suggestions like ↓/↑;
        // plain letters type.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl
            && matches!(
                self.mode,
                Mode::Input {
                    prompt: Prompt::Palette,
                    ..
                }
            )
        {
            match key.code {
                KeyCode::Char('j' | 'n') => key = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                KeyCode::Char('k' | 'p') => key = KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
                // Other Ctrl+letters never type into the line.
                KeyCode::Char(c) if c != 'c' => return,
                _ => {}
            }
        }
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
                // In the palette, ↑/↓ choose a suggestion, Tab takes it, and Enter takes it
                // too when it was chosen or the line is not a command yet.
                KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Enter
                    if prompt == Prompt::Palette =>
                {
                    let suggestions = self.suggestions(&text, view);
                    let n = suggestions.len();
                    let take = match key.code {
                        KeyCode::Up | KeyCode::Down => {
                            if n > 0 {
                                let step = if key.code == KeyCode::Up { n - 1 } else { 1 };
                                self.pick = (self.pick + step) % n;
                                self.picked = true;
                            }
                            false
                        }
                        KeyCode::Tab => true,
                        // Enter takes the suggestion when one was chosen, when the line is
                        // not a command yet, or when its last word is half typed (no
                        // suggestion matches it exactly and no space ends it); otherwise
                        // it runs the line as typed.
                        _ => {
                            let exact = suggestions
                                .iter()
                                .any(|s| s.line.trim().eq_ignore_ascii_case(text.trim()));
                            let half_typed = text.contains(' ') && !text.ends_with(' ') && !exact;
                            n > 0
                                && !exact
                                && (self.picked
                                    || half_typed
                                    || palette(text.trim(), self.clock.now_ts()).is_err())
                        }
                    };
                    if key.code == KeyCode::Enter && !take {
                        self.submit(prompt, text.trim().to_owned(), pressed_at, view);
                        return;
                    }
                    if take && let Some(s) = suggestions.get(self.pick.min(n.saturating_sub(1))) {
                        text = s.line.clone();
                        self.pick = 0;
                        self.picked = false;
                    }
                    self.mode = Mode::Input {
                        prompt,
                        text,
                        pressed_at,
                    };
                }
                KeyCode::Enter => self.submit(prompt, text.trim().to_owned(), pressed_at, view),
                KeyCode::Backspace => {
                    self.pick = 0;
                    self.picked = false;
                    text.pop();
                    self.mode = Mode::Input {
                        prompt,
                        text,
                        pressed_at,
                    };
                }
                KeyCode::Char(c) if !ctrl_c => {
                    self.pick = 0;
                    self.picked = false;
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
            Mode::Help => {}
            // Home: j/k move through everything on it, Enter acts on what is focused.
            Mode::Preflight => {
                self.mode = Mode::Preflight;
                self.home_input(key.code, view);
            }
            Mode::ConfirmStreamStop => {
                if key.code == KeyCode::Char('y') {
                    self.send(Command::Stream { on: false });
                }
            }
            Mode::Settings(mut page) => {
                match page.key(key.code, &view.config, &view.config_path, &view.obs) {
                    settings::Outcome::Close => return,
                    settings::Outcome::Reload => self.send(Command::Reload),
                    settings::Outcome::Command(cmd) => self.send(cmd),
                    settings::Outcome::Stay => {}
                }
                self.mode = Mode::Settings(page);
            }
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
            KeyCode::Char(':') => {
                self.pick = 0;
                self.picked = false;
                self.history(view);
                self.mode = input(Prompt::Palette)
            }
            KeyCode::Char('s') => self.mode = Mode::Preview { scroll: 0 },
            KeyCode::Char('r') => self.edit_readme = Some(view.readme.clone()),
            KeyCode::Char('0') => self.mode = Mode::Preflight,
            KeyCode::Char(',') => self.mode = Mode::Settings(settings::Page::default()),
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
            // Back to the newest line (chat, log, following again) or the last chapter.
            KeyCode::End | KeyCode::Char('G') => match self.focus {
                Pane::Chat => self.chat_feed = Feed::default(),
                Pane::Log => self.log_feed = Feed::default(),
                _ => self.move_selection(isize::MAX / 2, view),
            },
            KeyCode::Home | KeyCode::Char('g') => self.move_selection(-isize::MAX / 2, view),
            // In Done, Enter (like u) puts the item back to waiting.
            KeyCode::Enter if self.focus == Pane::Questions && self.filter == Filter::Done => {
                self.undo(view)
            }
            KeyCode::Enter if self.focus == Pane::Questions => {
                self.question(view, QuestionAction::Show)
            }
            // A question can be dismissed; a supporter is thanked, never dismissed.
            KeyCode::Char('x') if self.focus == Pane::Questions => {
                match self.selected_item(view).map(|q| q.support.is_some()) {
                    Some(false) => self.question(view, QuestionAction::Dismiss),
                    Some(true) => {
                        self.message = Some("supporters are thanked with t, not dismissed".into())
                    }
                    None => {}
                }
            }
            KeyCode::Char('u') if self.focus == Pane::Questions => self.undo(view),
            KeyCode::Char('x' | 't')
                if self.focus == Pane::Questions && self.filter == Filter::Done =>
            {
                self.message = Some("already handled; u puts it back in the Inbox".into())
            }
            KeyCode::Char('t') if self.focus == Pane::Questions => {
                match self.selected_item(view).map(|q| q.support.is_some()) {
                    Some(true) => self.question(view, QuestionAction::Answer),
                    Some(false) => {
                        self.message =
                            Some("questions: Enter shows then answers, x dismisses".into())
                    }
                    None => {}
                }
            }
            KeyCode::Char(c @ ('[' | ']')) if self.focus == Pane::Questions => {
                self.filter = self.filter.step(if c == ']' { 1 } else { -1 });
                self.questions.select(None);
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
                let len = self.inbox(&view.state).len();
                if len > 0 {
                    let current = self.questions.selected().unwrap_or(0) as isize;
                    self.questions
                        .select(Some((current + delta).clamp(0, len as isize - 1) as usize));
                }
            }
            // The log and chat show newest at the bottom; "down" moves towards newer lines.
            Pane::Log => {
                let ids: Vec<&str> = view.state.log.iter().map(|e| e.id.as_str()).collect();
                self.log_feed.step(delta, &ids)
            }
            Pane::Chat => {
                let ids: Vec<&str> = view.chat.iter().map(|m| m.id.as_str()).collect();
                self.chat_feed.step(delta, &ids)
            }
        }
    }

    /// A key on Home. Before the session starts, Esc and 0 stay (Home is the only way in).
    fn home_input(&mut self, code: KeyCode, view: &View) {
        let items = home::items(view);
        let last = items.len().saturating_sub(1);
        self.home_pick = self.home_pick.min(last);
        let focused = items.get(self.home_pick).cloned();
        match code {
            // j/k wrap around: past the last item to the first, and back.
            KeyCode::Char('j') | KeyCode::Down => {
                self.home_pick = if self.home_pick >= last {
                    0
                } else {
                    self.home_pick + 1
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.home_pick = if self.home_pick == 0 {
                    last
                } else {
                    self.home_pick - 1
                }
            }
            KeyCode::Char('g') | KeyCode::Home => self.home_pick = 0,
            KeyCode::Char('G') | KeyCode::End => self.home_pick = last,
            KeyCode::Tab => self.home_pick = home::next_group(&items, self.home_pick),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(item) = focused {
                    self.home_act(item, view);
                }
            }
            // h/l: an input's volume by 1 dB, a source hidden or shown.
            KeyCode::Char(c @ ('h' | 'l')) => {
                let up = c == 'l';
                match focused {
                    Some(home::Item::Audio(name)) => {
                        if let Some(a) = view.obs.audio(&name) {
                            let db = (a.volume_db + if up { 1.0 } else { -1.0 }).round();
                            self.send(Command::Volume { input: name, db });
                        }
                    }
                    Some(home::Item::Source(source)) => self.send(Command::Source {
                        source,
                        scene: None,
                        visible: Some(up),
                    }),
                    _ => {}
                }
            }
            KeyCode::Esc | KeyCode::Char('0') if view.begun => self.mode = Mode::Normal,
            KeyCode::Char('q') if view.begun => self.mode = Mode::ConfirmQuit,
            KeyCode::Char('q') => self.finish(Command::Quit),
            KeyCode::Char(c @ ('R' | 'r' | 'S' | 's' | 'M' | 'm' | ',' | ':')) => {
                self.home_key(c, view)
            }
            _ => {}
        }
    }

    /// Enter on a Home item.
    fn home_act(&mut self, item: home::Item, view: &View) {
        match item {
            home::Item::Dashboard => self.mode = Mode::Normal,
            home::Item::Quit => self.finish(Command::Quit),
            // Started: on to the dashboard.
            home::Item::Start => {
                self.send(Command::Begin);
                self.mode = Mode::Normal;
            }
            other => {
                if let Some(cmd) = home::command(&other, view) {
                    self.send(cmd);
                }
            }
        }
    }

    /// Home's fix keys (R, S, M) and its way out to settings (,) and the palette (:).
    fn home_key(&mut self, c: char, view: &View) {
        match c.to_ascii_uppercase() {
            'R' => self.send(Command::Record {
                action: ac_core::command::OutputAction::Start,
            }),
            'S' => self.send(Command::Stream { on: true }),
            'M' => {
                if let Some(mic) = home::mic(&view.obs) {
                    let input = mic.name.clone();
                    self.send(Command::Mute { input, on: None });
                }
            }
            ',' => self.mode = Mode::Settings(settings::Page::default()),
            ':' => {
                self.pick = 0;
                self.picked = false;
                self.history(view);
                self.mode = Mode::Input {
                    prompt: Prompt::Palette,
                    text: String::new(),
                    pressed_at: self.clock.now_ts(),
                };
            }
            _ => {}
        }
    }

    /// Palette suggestions for `text`, from OBS's lists and the settings.
    fn suggestions(&self, text: &str, view: &View) -> Vec<palette::Suggestion> {
        let world = palette::World {
            obs: &view.obs,
            scene: view.state.scene.as_deref(),
            config: &view.config,
            recent: self.history.as_deref().unwrap_or_default(),
        };
        palette::suggest(text, &world).1
    }

    /// `:set <key> [value]` and `:reset <key>`: written to aftercast.toml, then reloaded.
    fn set_setting(&mut self, key: &str, value: Option<&str>, reset: bool, view: &View) {
        let all = settings::all();
        let Some(s) = all.iter().find(|s| s.key.eq_ignore_ascii_case(key)) else {
            self.message = Some(format!("no setting {key:?} (Tab lists them)"));
            return;
        };
        let current = (s.get)(&view.config);
        let value = match (reset, value, &current) {
            (true, ..) => None,
            (false, Some(text), _) => match palette::parse_value(s, text) {
                Ok(v) => Some(v),
                Err(e) => {
                    self.message = Some(e);
                    return;
                }
            },
            (false, None, ac_core::config::Value::Bool(b)) => {
                Some(ac_core::config::Value::Bool(!b))
            }
            (false, None, v) => {
                self.message = Some(format!("{} is {} ({})", s.key, settings::show(v), s.label));
                return;
            }
        };
        let shown = value.as_ref().map_or_else(
            || {
                format!(
                    "default ({})",
                    settings::show(&(s.get)(&Default::default()))
                )
            },
            settings::show,
        );
        match ac_core::config::set_value(&view.config_path, s.key, value) {
            Ok(()) => {
                self.message = Some(format!("{} = {shown} · {}", s.key, s.when.text()));
                self.send(Command::Reload);
            }
            Err(e) => self.message = Some(e.to_string()),
        }
    }

    /// The selected Inbox item.
    fn selected_item<'v>(&self, view: &'v View) -> Option<&'v ac_core::session_state::Question> {
        self.inbox(&view.state)
            .get(self.questions.selected()?)
            .copied()
    }

    /// Sends `action` for the selected question.
    fn question(&mut self, view: &View, action: QuestionAction) {
        let selected = self.questions.selected();
        if let Some(q) = selected.and_then(|i| self.inbox(&view.state).get(i).copied()) {
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
                Ok(parsed) if !text.is_empty() => {
                    self.remember(&text, view);
                    self.run_palette(Ok(parsed), view);
                }
                other => self.run_palette(other, view),
            },
        }
    }

    /// Carries out a parsed palette line.
    fn run_palette(&mut self, parsed: Result<Palette, String>, view: &View) {
        {
            match parsed {
                Ok(Palette::Command(cmd)) => {
                    let ends = matches!(cmd, Command::Quit | Command::Finalize);
                    self.send(cmd);
                    self.exit |= ends;
                }
                Ok(Palette::Help) => self.mode = Mode::Help,
                Ok(Palette::Preflight) => self.mode = Mode::Preflight,
                Ok(Palette::StopStream) => self.mode = Mode::ConfirmStreamStop,
                Ok(Palette::Settings) => self.mode = Mode::Settings(settings::Page::default()),
                Ok(Palette::Set { key, value }) => {
                    self.set_setting(&key, value.as_deref(), false, view)
                }
                Ok(Palette::Reset { key }) => self.set_setting(&key, None, true, view),
                Err(message) => self.message = Some(message),
            }
        }
    }

    /// `<home>/state/history`; `None` (kept in memory only) without a home folder.
    fn history_path(view: &View) -> Option<std::path::PathBuf> {
        let home = view
            .config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())?;
        Some(home.join("state").join("history"))
    }

    /// Palette lines run before, oldest first.
    fn history(&mut self, view: &View) -> &[String] {
        self.history.get_or_insert_with(|| {
            Self::history_path(view)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .unwrap_or_default()
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect()
        })
    }

    /// Adds a line to the history (once, as the newest) and saves the last 100.
    fn remember(&mut self, line: &str, view: &View) {
        let line = line.trim().to_owned();
        self.history(view);
        let history = self.history.get_or_insert_with(Vec::new);
        history.retain(|l| *l != line);
        history.push(line);
        let keep = history.len().saturating_sub(100);
        history.drain(..keep);
        let Some(path) = Self::history_path(view) else {
            return;
        };
        let text = history.join("\n") + "\n";
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&path, text) {
            self.message = Some(format!("palette history: {e}"));
        }
    }

    fn finish(&mut self, cmd: Command) {
        self.send(cmd);
        self.exit = true;
    }

    fn draw(&mut self, frame: &mut Frame, view: &View) {
        self.motion = view.config.tui.animations;
        let [status, body, info, keys] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let rows = |n: usize| (n.max(2) + 2).min(usize::from(u16::MAX)) as u16;
        let want = [
            rows(view.chat.len()),
            rows(self.inbox(&view.state).len()),
            rows(view.state.checkpoints.len()),
            rows(view.state.log.len()),
        ];
        self.split_target = target_split(self.focus, body, want);
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
        frame.render_widget(Line::from(self.keys_line(view)), keys);
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
        let now = self.clock.now_ts();
        let anchors = &view.meta.anchors;
        let state = &view.state;
        let breathing = mix(ACCENT, ACCENT_DIM, self.pulse(Duration::from_secs(2)));
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
            Some(ServiceStatus::Retrying) => Span::styled(self.spin(), Style::new().fg(WARN)),
            Some(ServiceStatus::Down) => Span::styled("✖", Style::new().fg(ACCENT).bold()),
            None => Span::styled(self.spin(), Style::new().fg(MUTED)),
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
        // A muted mic is easy to miss and ruins a stream: say so loudly.
        if view.obs.audio.iter().any(|a| a.is_mic() && a.muted) {
            spans.push(Span::styled(
                " MIC MUTED ",
                Style::new().fg(Color::Black).bg(ACCENT).bold(),
            ));
            spans.push(Span::raw(" "));
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
            KeysStatus::Starting => Span::styled(self.spin(), Style::new().fg(MUTED)),
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
                format!("lat {} measuring", self.spin()),
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
        let block = match self.pane_keys(Pane::Chapters, view) {
            Some(keys) => block.title_bottom(keys),
            None => block,
        };
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
        let (width, height) = inner(area);
        let focused = self.focus == Pane::Chat;
        let now = self.clock.now_ts();
        let ids: Vec<&str> = view.chat.iter().map(|m| m.id.as_str()).collect();
        let users = &self.users;
        let colors = view.config.tui.name_colors;
        let make = |i: usize| {
            let m = &view.chat[i];
            let fresh = now.secs_since(m.t_received) < FRESH_S;
            let color = if colors {
                users.peek(&m.author_channel_id)
            } else {
                TEXT
            };
            let line = chat_line(m, color, fresh);
            let indent = line
                .width()
                .saturating_sub(Span::raw(m.text.as_str()).width());
            (line, indent)
        };
        let (rows, newer) = self.chat_feed.layout(
            &ids,
            make,
            focused,
            view.config.tui.wrap_lines,
            (width, height),
        );
        let block = pane_block(Pane::Chat, "Chat", scrolled_hint(newer), focused);
        let block = match self.pane_keys(Pane::Chat, view) {
            Some(keys) => block.title_bottom(keys),
            None => block,
        };
        if rows.is_empty() {
            let connected = view.state.services.get("chat") == Some(&ServiceStatus::Ok);
            let text = if connected {
                "No messages yet.".to_owned()
            } else {
                format!("{} waiting for chat", self.spin())
            };
            frame.render_widget(empty_state(&[&text]).block(block), area);
            return;
        }
        frame.render_widget(Paragraph::new(rows).block(block), area);
    }

    fn draw_questions(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        let all = queue(&view.state);
        let queue = self.inbox(&view.state);
        let live = mix(ACCENT, ACCENT_DIM, self.pulse(Duration::from_secs(2)));
        let items: Vec<Line> = queue
            .iter()
            .map(|q| {
                use ac_core::session_state::QuestionState;
                let mark = match (q.state, q.support.is_some()) {
                    (QuestionState::Shown, _) => Span::styled("● ", Style::new().fg(live).bold()),
                    (QuestionState::Answered, false) => Span::styled("✔ ", Style::new().fg(OK)),
                    (QuestionState::Answered, true) => Span::styled("♥ ", Style::new().fg(WARN)),
                    (QuestionState::Dismissed, _) => Span::styled("✕ ", Style::new().fg(FAINT)),
                    _ => Span::raw("  "),
                };
                let when = q.done_at.map(|t| {
                    let at = t.datetime().with_timezone(&Local).format("%H:%M ");
                    Span::styled(at.to_string(), Style::new().fg(FAINT))
                });
                let color = if view.config.tui.name_colors {
                    self.users.get(&q.author_id)
                } else {
                    TEXT
                };
                let mut spans = vec![mark];
                spans.extend(when);
                if let Some(support) = &q.support {
                    let bg = if support.kind == ChatKind::Membership {
                        OK
                    } else {
                        WARN
                    };
                    spans.push(Span::styled(
                        format!(" {} ", support.label),
                        Style::new().fg(Color::Black).bg(bg).bold(),
                    ));
                    spans.push(Span::raw(" "));
                }
                spans.extend([
                    Span::styled(q.author.clone(), Style::new().fg(color).bold()),
                    Span::styled(": ", Style::new().fg(FAINT)),
                    Span::styled(q.text.clone(), Style::new().fg(TEXT)),
                ]);
                Line::from(spans)
            })
            .collect();
        let focused = self.focus == Pane::Questions;
        // Keep a valid selection before anything reads it (the border hints do).
        let last = items.len().checked_sub(1);
        match self.questions.selected() {
            None if last.is_some() => self.questions.select(Some(0)),
            Some(i) if last.is_none_or(|l| i > l) => self.questions.select(last),
            _ => {}
        }
        // The selected question opens in full while the pane has focus; the rest stay
        // on one line (the highlight symbol takes a cell).
        let width = inner(area).0.saturating_sub(1);
        let items: Vec<ListItem> = items
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                if view.config.tui.wrap_lines || (focused && self.questions.selected() == Some(i)) {
                    let text = line.spans.last().map_or(0, Span::width);
                    let indent = line.width() - text;
                    ListItem::new(wrap(line, width, indent))
                } else {
                    ListItem::new(truncate(line, width))
                }
            })
            .collect();
        // Focused: filter tabs with their counts, the active one underlined. Unfocused
        // (narrow): the active tab and how many supporters wait.
        let mut extra = Vec::new();
        let tabs = if focused {
            &Filter::ORDER[..]
        } else {
            std::slice::from_ref(&self.filter)
        };
        let count = |f: Filter| view.state.questions.iter().filter(|q| f.keeps(q)).count();
        // When every tab does not fit, the empty ones (but the active one) give way.
        let full: usize = tabs
            .iter()
            .map(|f| f.label().len() + count(*f).to_string().len() + 4)
            .sum();
        let room = (area.width as usize).saturating_sub(14);
        let tabs: Vec<Filter> = tabs
            .iter()
            .copied()
            .filter(|&f| full <= room || f == self.filter || count(f) > 0)
            .collect();
        for (i, &f) in tabs.iter().enumerate() {
            if i > 0 {
                extra.push(Span::styled("· ", Style::new().fg(FAINT)));
            }
            let n = count(f);
            let style = if f == self.filter {
                Style::new().fg(TEXT).bold().underlined()
            } else if n == 0 {
                Style::new().fg(FAINT)
            } else {
                Style::new().fg(MUTED)
            };
            extra.push(Span::styled(format!("{} {n}", f.label()), style));
            extra.push(Span::raw(" "));
        }
        let to_thank = all.iter().filter(|q| q.support.is_some()).count();
        if !focused && to_thank > 0 {
            extra.push(Span::styled(
                format!("· {to_thank} to thank "),
                Style::new().fg(WARN).bold(),
            ));
        }
        let block = pane_block(Pane::Questions, "Inbox", extra, focused);
        let block = match self.pane_keys(Pane::Questions, view) {
            Some(keys) => block.title_bottom(keys),
            None => block,
        };
        if items.is_empty() {
            let other = format!(
                "Nothing under {}; ] shows the next filter.",
                self.filter.label()
            );
            let hint = if self.filter == Filter::Done {
                empty_state(&["Nothing handled yet. What you answer, thank or dismiss lands here."])
            } else if all.is_empty() {
                empty_state(&[
                    "Nothing waiting. Questions (!q), Super Chats and memberships land here",
                    "and leave once handled: Enter shows on stream, x dismisses a question,",
                    "t thanks a supporter, u undoes the last one.",
                ])
            } else {
                empty_state(&[&other])
            };
            frame.render_widget(hint.block(block), area);
            return;
        }
        let list = List::new(items)
            .block(block)
            .highlight_symbol("▍")
            .highlight_spacing(ratatui::widgets::HighlightSpacing::Always)
            .highlight_style(selected_style(focused));
        frame.render_stateful_widget(list, area, &mut self.questions);
    }

    fn draw_log(&mut self, frame: &mut Frame, area: Rect, view: &View) {
        let (width, height) = inner(area);
        let focused = self.focus == Pane::Log;
        let events: Vec<&Event> = view.state.log.iter().collect();
        let ids: Vec<&str> = events.iter().map(|e| e.id.as_str()).collect();
        let make = |i: usize| {
            let e = events[i];
            let time =
                e.t.datetime()
                    .with_timezone(&Local)
                    .format("%H:%M:%S")
                    .to_string();
            let (text, dim) = summary(e);
            let (icon, color) = event_icon(e);
            let style = Style::new().fg(if dim { MUTED } else { TEXT });
            let line = Line::from(vec![
                Span::styled(format!("{time} "), Style::new().fg(FAINT)),
                Span::styled(format!("{icon} "), Style::new().fg(color)),
                Span::styled(text, style),
            ]);
            (line, 11)
        };
        let (rows, newer) = self.log_feed.layout(
            &ids,
            make,
            focused,
            view.config.tui.wrap_lines,
            (width, height),
        );
        let block = pane_block(Pane::Log, "Event log", scrolled_hint(newer), focused);
        let block = match self.pane_keys(Pane::Log, view) {
            Some(keys) => block.title_bottom(keys),
            None => block,
        };
        frame.render_widget(Paragraph::new(rows).block(block), area);
    }

    /// Hints for the current mode and pane, most useful first (cut at the right edge).
    /// The focused pane's own keys, drawn faded in its bottom border.
    fn pane_keys(&self, pane: Pane, view: &View) -> Option<Line<'static>> {
        if self.focus != pane {
            return None;
        }
        use ac_core::session_state::QuestionState;
        let mut live = false;
        let pairs: Vec<(&str, String)> = match pane {
            // Questions are answered or dismissed; supporters are thanked. The hint follows
            // what is on stream right now.
            Pane::Questions => {
                let can_undo = view.state.questions.iter().any(|q| q.done_at.is_some());
                let mut pairs: Vec<(&str, String)> = match self.selected_item(view) {
                    None => Vec::new(),
                    // Done: Enter and u both put it back to waiting.
                    Some(_) if self.filter == Filter::Done => {
                        vec![("Enter/u", "back to waiting".into())]
                    }
                    Some(q) => {
                        let supporter = q.support.is_some();
                        let enter = if q.state == QuestionState::Shown {
                            live = true;
                            if supporter {
                                "mark thanked"
                            } else {
                                "mark answered"
                            }
                            .to_owned()
                        } else {
                            // The overlay shows one card: the one shown last.
                            let on_screen = view
                                .state
                                .questions
                                .iter()
                                .filter(|o| o.state == QuestionState::Shown)
                                .max_by_key(|o| o.shown_at);
                            match on_screen {
                                Some(o) => format!("show instead of {}", o.author),
                                None => "show on stream".into(),
                            }
                        };
                        let other = if supporter {
                            ("t", "thank".into())
                        } else {
                            ("x", "dismiss".into())
                        };
                        let mut pairs = vec![("Enter", enter), other];
                        if can_undo {
                            pairs.push(("u", "undo".into()));
                        }
                        pairs
                    }
                };
                pairs.push(("[ ]", "filter".into()));
                pairs
            }
            Pane::Chapters => vec![
                ("e", "edit".into()),
                ("d", "delete".into()),
                ("j/k", "move".into()),
            ],
            Pane::Chat | Pane::Log => vec![("j/k", "read".into()), ("G", "newest".into())],
        };
        let mut spans = vec![Span::raw(" ")];
        if live {
            let dot = mix(ACCENT, ACCENT_DIM, self.pulse(Duration::from_secs(2)));
            spans.push(Span::styled("● ", Style::new().fg(dot)));
            spans.push(Span::styled("ON STREAM", Style::new().fg(ACCENT).bold()));
            spans.push(Span::styled(" · ", Style::new().fg(FAINT)));
        }
        for (i, (k, label)) in pairs.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", Style::new().fg(FAINT)));
            }
            spans.push(Span::styled(*k, Style::new().fg(MUTED)));
            spans.push(Span::styled(format!(" {label}"), Style::new().fg(FAINT)));
        }
        spans.push(Span::raw(" "));
        Some(Line::from(spans))
    }

    fn keys_line(&self, view: &View) -> Vec<Span<'static>> {
        if self.mode == Mode::Preflight {
            let mut spans = vec![Span::raw(" ")];
            for (k, label) in home::hints(view) {
                spans.extend(hint(&k, &label));
            }
            return spans;
        }
        let pairs: &[(&str, &str)] = match &self.mode {
            Mode::Settings(page) => return page.hints(),
            Mode::Input {
                prompt: Prompt::Palette,
                ..
            } => &[
                ("Ctrl+j/k", "choose"),
                ("Tab", "complete"),
                ("Enter", "run"),
                ("Esc", "cancel"),
            ],
            Mode::Input { .. } => &[("Enter", "save"), ("Esc", "cancel")],
            Mode::Preview { .. } => &[("Enter", "push"), ("j/k", "scroll"), ("Esc", "close")],
            // Global keys only, so this line never shifts; each pane shows its own keys
            // in its bottom border.
            _ => {
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
                    (",", "settings"),
                    (":", "command"),
                    ("q", "quit"),
                ];
                let mut spans = vec![Span::raw(" ")];
                for (k, label) in global {
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

    /// The command palette: the line being typed, what the command does, and a list of
    /// suggestions (command names, then OBS scenes, inputs, sources, settings and values).
    fn draw_palette(&self, frame: &mut Frame, text: &str, view: &View) {
        let world = palette::World {
            obs: &view.obs,
            scene: view.state.scene.as_deref(),
            config: &view.config,
            recent: self.history.as_deref().unwrap_or_default(),
        };
        let (spec, suggestions) = palette::suggest(text, &world);
        const ROWS: usize = 9;
        let area = frame.area();
        let width = area.width.saturating_sub(8).min(96);
        let rows = suggestions.len().min(ROWS);
        let height = (4 + rows + usize::from(rows > 0)) as u16;
        let popup = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height / 5).min(6),
            width,
            height: height.min(area.height),
        };
        frame.render_widget(Clear, popup);
        let block = modal_block(" Command ".into());
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        let caret = if !self.motion || self.tick().as_millis() % 1000 < 600 {
            "▏"
        } else {
            " "
        };
        let pick = self.pick.min(suggestions.len().saturating_sub(1));
        // Ghost text: the rest of the highlighted suggestion, when it extends the line.
        let ghost = suggestions
            .get(pick)
            .and_then(|s| s.line.strip_prefix(text))
            .unwrap_or("")
            .to_owned();
        let mut lines = vec![Line::from(vec![
            Span::styled(":", Style::new().fg(ACCENT).bold()),
            Span::styled(text.to_owned(), Style::new().fg(TEXT)),
            Span::styled(caret, Style::new().fg(ACCENT)),
            Span::styled(ghost, Style::new().fg(FAINT)),
        ])];
        lines.push(match spec {
            Some(s) => Line::from(vec![
                Span::styled(
                    format!("{} {}", s.name, s.usage).trim_end().to_owned(),
                    Style::new().fg(INFO),
                ),
                Span::styled(format!("  {}", s.about), Style::new().fg(MUTED)),
            ]),
            None if text.contains(' ') => {
                Line::styled("unknown command (Esc, then :help)", Style::new().fg(ACCENT))
            }
            None => Line::styled(
                "type a command; Ctrl+j/k or ↑↓ choose, Tab completes, Enter runs",
                Style::new().fg(FAINT),
            ),
        });
        if rows > 0 {
            lines.push(Line::styled(
                "─".repeat(inner.width as usize),
                Style::new().fg(RAISED),
            ));
        }
        // Keep the highlighted row in view.
        let skip = (pick + 1).saturating_sub(ROWS);
        let label_width = suggestions
            .iter()
            .map(|s| s.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(inner.width as usize / 2);
        for (i, s) in suggestions.iter().enumerate().skip(skip).take(ROWS) {
            let selected = i == pick;
            let style = if selected {
                Style::new().fg(TEXT).bg(SELECTED).bold()
            } else {
                Style::new().fg(TEXT)
            };
            let mut row = Line::from(vec![
                Span::styled(if selected { "▍" } else { " " }, Style::new().fg(ACCENT)),
                Span::styled(format!("{:<label_width$}  ", s.label), style),
                Span::styled(s.detail.clone(), Style::new().fg(MUTED)),
            ]);
            if selected {
                row = row.style(Style::new().bg(SELECTED));
            }
            lines.push(row);
        }
        frame.render_widget(Paragraph::new(lines), inner);
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
                prompt: Prompt::Palette,
                text,
                ..
            } => {
                self.draw_palette(frame, text, view);
                return;
            }
            Mode::Settings(page) => {
                page.draw(
                    frame,
                    &view.config,
                    &view.config_path,
                    &view.obs,
                    view.state.scene.as_deref(),
                    self.motion,
                );
                return;
            }
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
                let caret = if !self.motion || t.as_millis() % 1000 < 600 {
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
                // The status bar's live part, without the episode chip the page already shows.
                let mut status = self.status_line(view);
                status.spans.remove(0);
                home::draw(frame, view, status, self.spin(), self.home_pick);
                return;
            }
            Mode::ConfirmStreamStop => (
                " Stop the stream ".into(),
                vec![
                    Line::raw("End the live stream on YouTube? Viewers see it end at once."),
                    Line::raw(""),
                    chips(&[("y", "stop streaming"), ("any key", "keep streaming")]),
                ],
                76,
                5,
            ),
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
    Settings,
    /// `:stream stop`: confirmed first.
    StopStream,
    Set {
        key: String,
        value: Option<String>,
    },
    Reset {
        key: String,
    },
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
        "preflight" | "home" => return Ok(Palette::Preflight),
        "record" | "rec" => Command::Record {
            action: match args.as_str() {
                "start" | "" => ac_core::command::OutputAction::Start,
                "stop" => ac_core::command::OutputAction::Stop,
                "pause" => ac_core::command::OutputAction::Pause,
                "resume" => ac_core::command::OutputAction::Resume,
                _ => return Err(":record start|stop|pause|resume".into()),
            },
        },
        "stream" => match args.as_str() {
            "start" => Command::Stream { on: true },
            "stop" => return Ok(Palette::StopStream),
            _ => return Err(":stream start|stop".into()),
        },
        "settings" => return Ok(Palette::Settings),
        "set" if args.is_empty() => return Ok(Palette::Settings),
        "set" => {
            let (key, value) = args.split_once(' ').unwrap_or((args.as_str(), ""));
            let value = Some(value.trim().to_owned()).filter(|v| !v.is_empty());
            return Ok(Palette::Set {
                key: key.to_owned(),
                value,
            });
        }
        "reset" => {
            return Ok(Palette::Reset {
                key: need("a setting")?,
            });
        }
        "mute" => {
            let (input, on) = palette::trailing_switch(&need("an audio input")?);
            Command::Mute { input, on }
        }
        "unmute" => Command::Mute {
            input: need("an audio input")?,
            on: Some(false),
        },
        "volume" | "vol" => {
            let usage = || ":volume <input> <dB>, e.g. :volume Mic/Aux -6".to_owned();
            let (input, db) = need("an audio input and dB")?
                .rsplit_once(' ')
                .map(|(i, d)| (i.trim().to_owned(), d.to_lowercase()))
                .ok_or_else(usage)?;
            let db = db.trim_end_matches("db").parse().map_err(|_| usage())?;
            Command::Volume { input, db }
        }
        "hide" | "show" => Command::Source {
            source: need("a source")?,
            scene: None,
            visible: Some(name == "show"),
        },
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

/// The inbox: supporters (Super Chats, stickers, memberships), then questions.
fn queue(state: &SessionState) -> Vec<&ac_core::session_state::Question> {
    use ac_core::session_state::QuestionState;
    let mut list: Vec<_> = state
        .questions
        .iter()
        .filter(|q| matches!(q.state, QuestionState::Open | QuestionState::Shown))
        .collect();
    // Supporters are pinned above the questions, each group in arrival order.
    list.sort_by_key(|q| q.support.is_none());
    list
}

fn chat_line(m: &ChatMessage, color: Color, fresh: bool) -> Line<'static> {
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
        m.author_name.clone(),
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
    spans.push(Span::styled(m.text.clone(), text_style));
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

/// Scroll state of a feed that grows at the bottom (chat, event log), kept by entry id
/// so new entries arriving while you read back do not move the cursor or the view.
#[derive(Debug, Default)]
struct Feed {
    /// The entry under the cursor; `None` follows the newest.
    cursor: Option<String>,
    /// The last entry on screen while reading back.
    last: Option<String>,
}

impl Feed {
    fn position(id: &Option<String>, ids: &[&str]) -> Option<usize> {
        let id = id.as_deref()?;
        ids.iter().position(|i| *i == id)
    }

    /// Moves the cursor; reaching the newest entry follows the feed again.
    fn step(&mut self, delta: isize, ids: &[&str]) {
        let Some(newest) = ids.len().checked_sub(1) else {
            return;
        };
        let current = Self::position(&self.cursor, ids).unwrap_or(newest);
        let next = current.saturating_add_signed(delta).min(newest);
        self.cursor = (next < newest).then(|| ids[next].to_owned());
    }

    /// Rows to draw and how many entries are newer than the cursor. The view only moves
    /// when the cursor would leave it, like an editor: `j/k` move the highlight, and the
    /// lines scroll once it reaches an edge. The cursor line opens in full when focused.
    fn layout(
        &mut self,
        ids: &[&str],
        make: impl Fn(usize) -> (Line<'static>, usize),
        focused: bool,
        wrap_all: bool,
        (width, height): (usize, usize),
    ) -> (Vec<Line<'static>>, usize) {
        let len = ids.len();
        if len == 0 || height == 0 {
            return (Vec::new(), 0);
        }
        // A cursor whose entry left the buffer falls back to the oldest one kept.
        let cursor = match &self.cursor {
            None => len - 1,
            Some(_) => Self::position(&self.cursor, ids).unwrap_or(0),
        };
        let open = focused.then_some(cursor);
        let rows_of = |i: usize| {
            if wrap_all || open == Some(i) {
                let (line, indent) = make(i);
                wrap(line, width, indent).len()
            } else {
                1
            }
        };
        let mut end = match &self.cursor {
            None => len,
            Some(_) => Self::position(&self.last, ids).map_or(len, |p| p + 1),
        };
        end = end.clamp(cursor + 1, len);
        // Scroll up just enough to keep the cursor's rows on screen.
        while end > cursor + 1 && (cursor..end).map(rows_of).sum::<usize>() > height {
            end -= 1;
        }
        self.last = self.cursor.as_ref().map(|_| ids[end - 1].to_owned());
        let start = end.saturating_sub(height);
        let entries = (start..end).map(make).collect();
        let rows = feed_rows(entries, open.map(|o| o - start), wrap_all, width, height);
        (rows, len - 1 - cursor)
    }
}

/// Content width and height inside a pane's border.
fn inner(area: Rect) -> (usize, usize) {
    (
        area.width.saturating_sub(2) as usize,
        area.height.saturating_sub(2) as usize,
    )
}

/// `↓ 5 newer` in a pane title while it is scrolled back.
fn scrolled_hint(scroll: usize) -> Vec<Span<'static>> {
    if scroll == 0 {
        return Vec::new();
    }
    vec![Span::styled(
        format!("↓ {scroll} newer · G jumps back "),
        Style::new().fg(WARN),
    )]
}

/// Rows for a feed that grows at the bottom (chat, event log). Every entry keeps to one
/// line, ending in `…` when cut, except `open`, the reading cursor: it is highlighted and
/// wrapped in full, continuation rows indented to where its text starts, so a long
/// message reads as a block under its author.
fn feed_rows(
    entries: Vec<(Line<'static>, usize)>,
    open: Option<usize>,
    wrap_all: bool,
    width: usize,
    height: usize,
) -> Vec<Line<'static>> {
    let mut rows = VecDeque::new();
    for (i, (line, indent)) in entries.into_iter().enumerate().rev() {
        if rows.len() >= height {
            break;
        }
        if Some(i) == open {
            for mut row in wrap(line, width, indent).into_iter().rev() {
                let pad = width.saturating_sub(row.width());
                row.spans.push(Span::raw(" ".repeat(pad)));
                rows.push_front(row.style(Style::new().bg(SELECTED)));
            }
        } else if wrap_all {
            for row in wrap(line, width, indent).into_iter().rev() {
                rows.push_front(row);
            }
        } else {
            rows.push_front(truncate(line, width));
        }
    }
    let excess = rows.len().saturating_sub(height);
    rows.into_iter().skip(excess).collect()
}

/// A line as one styled cell per character.
fn cells(line: Line<'static>) -> Vec<(char, Style)> {
    let base = line.style;
    line.spans
        .into_iter()
        .flat_map(|s| {
            let style = base.patch(s.style);
            s.content
                .chars()
                .map(move |c| (c, style))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn line_of(cells: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span> = Vec::new();
    for &(c, style) in cells {
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(c),
            _ => spans.push(Span::styled(c.to_string(), style)),
        }
    }
    Line::from(spans)
}

fn char_width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
}

/// `line` cut to `width` cells, ending in `…` when anything was cut.
fn truncate(line: Line<'static>, width: usize) -> Line<'static> {
    if line.width() <= width || width == 0 {
        return line;
    }
    let mut kept = Vec::new();
    let mut used = 0;
    for cell in cells(line) {
        let w = char_width(cell.0);
        if used + w > width - 1 {
            break;
        }
        used += w;
        kept.push(cell);
    }
    let style = kept.last().map_or(Style::new(), |c| c.1);
    kept.push(('…', style.fg(MUTED)));
    line_of(&kept)
}

/// `line` wrapped into rows of `width` cells, breaking after a space where one is in
/// reach; continuation rows start with `indent` spaces.
fn wrap(line: Line<'static>, width: usize, indent: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line];
    }
    // A hanging indent wider than half the row would leave too little room for text.
    let indent = if indent * 2 > width { 0 } else { indent };
    let mut rows = Vec::new();
    let mut row: Vec<(char, Style)> = Vec::new();
    let mut used = 0;
    let mut start = 0; // where this row's text begins (after the indent)
    for cell in cells(line) {
        let w = char_width(cell.0);
        if used + w > width && row.len() > start {
            // Break at the space that overflows, else after the last space in the row.
            let carry = match row[start..].iter().rposition(|c| c.0 == ' ') {
                Some(i) if i > 0 && cell.0 != ' ' => row.split_off(start + i + 1),
                _ => Vec::new(),
            };
            rows.push(line_of(&row));
            row = vec![(' ', Style::new()); indent];
            start = indent;
            used = indent + carry.iter().map(|c| char_width(c.0)).sum::<usize>();
            row.extend(carry);
        }
        if cell.0 == ' ' && row.len() == start && !rows.is_empty() {
            continue; // no leading space on a continuation row
        }
        used += w;
        row.push(cell);
    }
    rows.push(line_of(&row));
    rows
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
        EventKind::QuestionAdd {
            author,
            support: Some(s),
            ..
        } => match s.kind {
            ChatKind::Membership => (format!("membership: {author}"), false),
            _ => (format!("Super Chat {} from {author}", s.label), false),
        },
        EventKind::QuestionAdd { author, .. } => (format!("question from {author}"), false),
        EventKind::QuestionShow { .. } => ("question on stream".into(), false),
        EventKind::QuestionAnswered { .. } => ("question answered".into(), false),
        EventKind::QuestionDismiss { .. } => ("question dismissed".into(), true),
        EventKind::QuestionReopen { .. } => ("back in the Inbox".into(), false),
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
        EventKind::QuestionAdd {
            support: Some(_), ..
        } => ("◈", WARN),
        EventKind::QuestionAdd { .. }
        | EventKind::QuestionShow { .. }
        | EventKind::QuestionAnswered { .. }
        | EventKind::QuestionDismiss { .. } => ("?", INFO),
        EventKind::QuestionReopen { .. } => ("↺", INFO),
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
                (",", "settings: display, chapters, bot, OBS, capture, post"),
            ],
        ),
        (
            "Panes",
            &[
                ("0", "Home: checks, audio meters, what is on air"),
                ("1 2 3 4", "focus Chat, Inbox, Chapters, Event log"),
                ("Tab", "next pane (Shift+Tab: previous)"),
                ("j/k", "move or scroll; g/G first/newest"),
                ("e / d", "edit / delete the selected chapter"),
                ("Enter", "Inbox: show on stream; again: answered / done"),
                ("t", "Inbox: thank a supporter without showing"),
                ("x", "Inbox: dismiss a question"),
                (
                    "u",
                    "Inbox: undo the last answer, thanks or dismissal (Done: Enter too)",
                ),
                (
                    "[ ]",
                    "Inbox: filter All, Questions, Super Chats, Members, Done",
                ),
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
                    ":mute <input>",
                    "mute/unmute OBS audio   :volume <input> <dB>   :unmute <input>",
                ),
                (
                    ":hide <source>",
                    "hide a source in the program scene   :show <source>",
                ),
                (
                    ":record start",
                    "stop, pause, resume   :stream start|stop (stop asks first)",
                ),
                (
                    ":set <key> [v]",
                    "change a setting (no value flips a switch)   :reset <key>",
                ),
                (
                    ":overlay",
                    "<name> on|off: now, question, toast, brb, latency, chapters",
                ),
                (
                    ":latency",
                    "start/stop measuring; :lat <n> enters the number seen",
                ),
                (":preflight", "the checklist again   :reload   :quit"),
                (
                    ":settings",
                    "the settings page (also ,): config and OBS live",
                ),
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
            config: Arc::default(),
            config_path: "aftercast.toml".into(),
            begun: true,
            fresh: false,
            levels: ac_core::obs::Levels::default(),
            obs: Arc::new(ac_core::obs::Media {
                scenes: vec!["Code".into(), "BRB".into()],
                audio: vec![ac_core::obs::Audio {
                    name: "Mic/Aux".into(),
                    kind: "wasapi_input_capture".into(),
                    muted: false,
                    volume_db: -3.0,
                }],
                items: vec![ac_core::obs::Item {
                    scene: "Code".into(),
                    source: "Camera".into(),
                    id: 1,
                    enabled: true,
                }],
            }),
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
                    fix: String::new(),
                },
                Check {
                    label: "OBS recording".into(),
                    state: CheckState::Fail,
                    detail: "the local recording is the editing source".into(),
                    fix: "start recording in OBS (also :record start)".into(),
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

        // Home opens first (this session has started); Esc goes to the dashboard.
        let text = screen(&mut app, &view);
        assert!(
            text.contains("STARTED") && text.contains("1 thing needs you"),
            "{text}"
        );
        assert!(text.contains("▶ Dashboard"), "{text}");
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
        // The Super Chat arrives last but is pinned first.
        let asks = [
            ("q1", "bob", None),
            ("q2", "cy", None),
            ("s1", "eve", Some("₹100.00")),
        ];
        for (qid, author, amount) in asks {
            let kind = EventKind::QuestionAdd {
                qid: qid.into(),
                author: author.into(),
                author_id: format!("UC{author}"),
                text: "why rust?".into(),
                support: amount.map(|label: &str| ac_core::chat::Support {
                    kind: ChatKind::Superchat,
                    label: label.into(),
                }),
            };
            view.state.apply(&Event::new(&clock, "bot", kind));
        }
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        let text = screen(&mut app, &view);
        assert!(text.contains(" 2  Inbox All 3 · 1 to thank"), "{text}");
        assert!(
            text.contains(" ₹100.00  eve: why rust?"),
            "pinned first: {text}"
        );
        assert!(
            text.contains("bob: !q why rust over go?"),
            "chat pane: {text}"
        );
        assert!(text.contains("lat 4.2s"), "{text}");

        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        press(&mut app, KeyCode::Char('2'));
        app.last_frame -= Duration::from_secs(1); // let the pane finish growing
        let text = screen(&mut app, &view);
        assert!(
            text.contains("All 3 · Questions 2 · Super Chats 1 ─"),
            "tabs when focused: {text}"
        );
        // `]` filters to questions: the Super Chat is hidden, q1 is first.
        press(&mut app, KeyCode::Char(']'));
        let text = screen(&mut app, &view);
        assert!(!text.contains("₹100.00  eve"), "{text}");
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
        // While the selected question is on stream, its border says so.
        let mut shown = view.clone();
        shown.state.apply(&Event::new(
            &Clock::new(),
            "tui",
            EventKind::QuestionShow { qid: "q2".into() },
        ));
        app.last_frame -= Duration::from_secs(1);
        press(&mut app, KeyCode::Char('j'));
        let text = screen(&mut app, &shown);
        assert!(
            text.contains("● ON STREAM · Enter mark answered · x dismiss"),
            "{text}"
        );
        press(&mut app, KeyCode::Char('k'));
        let text = screen(&mut app, &shown);
        assert!(
            text.contains("Enter show instead of cy · x dismiss"),
            "{text}"
        );
        press(&mut app, KeyCode::Char('j'));
        // `]` again: Super Chats only. A supporter is not dismissed; `t` thanks them.
        press(&mut app, KeyCode::Char(']'));
        screen(&mut app, &view);
        press(&mut app, KeyCode::Char('x'));
        assert!(cmd_rx.try_recv().is_err());
        assert!(
            app.message
                .as_deref()
                .unwrap_or("")
                .contains("thanked with t")
        );
        press(&mut app, KeyCode::Char('t'));
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Question {
                qid: "s1".into(),
                action: QuestionAction::Answer
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

        press(&mut app, KeyCode::Char(':'));
        for c in "sy".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Sync);
    }

    #[test]
    fn focused_pane_grows_and_quiet_panes_shrink_to_their_content() {
        let body = Rect::new(0, 0, 120, 40);
        let busy = [40, 40, 40, 40];
        for pane in Pane::ORDER {
            let areas = pane_areas(body, target_split(pane, body, busy));
            let focused = areas[pane.number() - 1];
            for (i, a) in areas.iter().enumerate() {
                assert!(a.height >= 3 && a.width >= 20, "{pane:?} pane {i}: {a:?}");
                assert!(focused.area() >= a.area(), "{pane:?} is the largest");
            }
        }
        // One chapter needs 4 rows (2 for content, 2 border): the log gets the rest.
        let split = target_split(Pane::Chat, body, [40, 4, 4, 40]);
        let [_, _, chapters, log] = pane_areas(body, split);
        assert_eq!((chapters.height, log.height), (4, 36));
        // Tiny terminals still split instead of panicking.
        let tiny = Rect::new(0, 0, 10, 4);
        let areas = pane_areas(tiny, target_split(Pane::Chat, tiny, busy));
        assert!(areas.iter().all(|a| a.width > 0));
    }

    #[test]
    fn split_glides_to_the_focused_pane() {
        let clock = Arc::new(Clock::new());
        let view = sample_view(&clock);
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        screen(&mut app, &view);
        app.last_frame -= Duration::from_secs(1); // one long frame finishes a move
        screen(&mut app, &view);
        assert!(!app.resizing());
        app.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE), &view);
        screen(&mut app, &view);
        assert!(app.resizing());
        app.last_frame -= Duration::from_secs(1);
        screen(&mut app, &view);
        assert!(!app.resizing());
    }

    #[test]
    fn long_lines_end_in_an_ellipsis_and_the_cursor_line_wraps_in_full() {
        let line = || {
            Line::from(vec![
                Span::raw("bob: "),
                Span::raw("why rust over go for this project"),
            ])
        };
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        assert_eq!(text(&truncate(line(), 16)), "bob: why rust o…");
        assert_eq!(text(&truncate(line(), 80)), text(&line()));
        let rows: Vec<String> = wrap(line(), 16, 5).iter().map(text).collect();
        assert_eq!(
            rows,
            [
                "bob: why rust ",
                "     over go for",
                "     this ",
                "     project"
            ]
        );

        let entries = vec![(line(), 5), (line(), 5)];
        let rows = feed_rows(entries.clone(), None, false, 16, 10);
        assert_eq!(rows.len(), 2, "one row per entry when nothing is open");
        assert_eq!(feed_rows(entries.clone(), None, true, 16, 10).len(), 8);
        let rows = feed_rows(entries, Some(1), false, 16, 3);
        assert_eq!(
            rows.len(),
            3,
            "the open entry pushes older ones off the top"
        );
        assert_eq!(text(&rows[2]).trim_end(), "     project");
    }

    #[test]
    fn feed_cursor_moves_inside_a_still_view_and_holds_while_new_lines_arrive() {
        let ids: Vec<String> = (0..10).map(|i| format!("m{i}")).collect();
        let make = |i: usize| (Line::raw(format!("message {i}")), 0);
        let text = |l: &Line| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        };
        let shown = |feed: &mut Feed, ids: &[String]| {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            let (rows, newer) = feed.layout(&ids, make, true, false, (20, 4));
            let rows: Vec<String> = rows.iter().map(|r| text(r).trim_end().to_owned()).collect();
            (rows, newer)
        };
        let mut feed = Feed::default();
        let (rows, newer) = shown(&mut feed, &ids);
        assert_eq!(rows, ["message 6", "message 7", "message 8", "message 9"]);
        assert_eq!(newer, 0);

        // k moves the cursor up; the lines stay where they are until it reaches the top.
        let step = |feed: &mut Feed, ids: &[String], d| {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            feed.step(d, &ids)
        };
        step(&mut feed, &ids, -3);
        let (rows, newer) = shown(&mut feed, &ids);
        assert_eq!(rows[0], "message 6");
        assert_eq!(newer, 3);
        assert_eq!(feed.cursor.as_deref(), Some("m6"));
        step(&mut feed, &ids, -1);
        assert_eq!(
            shown(&mut feed, &ids).0[0],
            "message 5",
            "scrolls at the edge"
        );

        // New chat while reading back moves neither the cursor nor the view.
        let mut more = ids.clone();
        more.extend(["m10".into(), "m11".into()]);
        let (rows, newer) = shown(&mut feed, &more);
        assert_eq!(rows, ["message 5", "message 6", "message 7", "message 8"]);
        assert_eq!(newer, 6);

        // Walking back down to the newest follows the feed again.
        step(&mut feed, &more, 100);
        assert_eq!(feed.cursor, None);
        assert_eq!(shown(&mut feed, &more).0[3], "message 11");
    }

    #[test]
    fn done_tab_lists_handled_items_and_u_brings_the_last_one_back() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        let add = |view: &mut View, qid: &str, author: &str, support: Option<&str>| {
            let kind = EventKind::QuestionAdd {
                qid: qid.into(),
                author: author.into(),
                author_id: format!("UC{author}"),
                text: "hi".into(),
                support: support.map(|label| ac_core::chat::Support {
                    kind: ChatKind::Superchat,
                    label: label.into(),
                }),
            };
            view.state.apply(&Event::new(&clock, "bot", kind));
        };
        add(&mut view, "q1", "bob", None);
        add(&mut view, "s1", "eve", Some("₹100.00"));
        add(&mut view, "q2", "cy", None);
        add(&mut view, "q3", "dan", None);
        for (secs, kind) in [
            (1, EventKind::QuestionAnswered { qid: "q1".into() }),
            (2, EventKind::QuestionAnswered { qid: "s1".into() }),
            (3, EventKind::QuestionDismiss { qid: "q2".into() }),
        ] {
            clock.advance(Duration::from_secs(secs));
            view.state.apply(&Event::new(&clock, "tui", kind));
        }
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        press(&mut app, KeyCode::Char('2'));
        app.last_frame -= Duration::from_secs(1);
        let text = screen(&mut app, &view);
        // Too narrow for all five tabs: the empty ones give way.
        assert!(
            text.contains("Inbox All 1 · Questions 1 · Done 3"),
            "{text}"
        );
        assert!(
            text.contains("Enter show on stream · x dismiss · u undo"),
            "{text}"
        );

        // u from All brings back the last one handled: cy's dismissed question.
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Question {
                qid: "q2".into(),
                action: QuestionAction::Reopen
            }
        );
        assert_eq!(
            app.message.as_deref(),
            Some("cy's question is back in the Inbox")
        );

        // Done lists newest first with what happened; Enter shows one again.
        press(&mut app, KeyCode::Char('['));
        let text = screen(&mut app, &view);
        let rows: Vec<&str> = text.lines().filter(|l| l.contains(": hi")).collect();
        assert!(rows[0].contains("✕") && rows[0].contains("cy"), "{rows:?}");
        assert!(rows[1].contains("♥") && rows[1].contains("eve"), "{rows:?}");
        assert!(rows[2].contains("✔") && rows[2].contains("bob"), "{rows:?}");
        assert!(text.contains("Enter/u back to waiting"), "{text}");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Question {
                qid: "s1".into(),
                action: QuestionAction::Reopen
            }
        );
        assert_eq!(
            app.message.as_deref(),
            Some("eve's Super Chat is back in the Inbox")
        );
        press(&mut app, KeyCode::Char('x'));
        assert!(cmd_rx.try_recv().is_err(), "x does nothing in Done");
    }

    #[test]
    fn palette_obs_and_settings_commands() {
        let p = |line: &str| palette(line, now());
        assert_eq!(
            p("mute Desktop Audio off"),
            Ok(Palette::Command(Command::Mute {
                input: "Desktop Audio".into(),
                on: Some(false)
            }))
        );
        assert_eq!(
            p("mute Mic/Aux"),
            Ok(Palette::Command(Command::Mute {
                input: "Mic/Aux".into(),
                on: None
            }))
        );
        assert_eq!(
            p("vol Mic/Aux -6dB"),
            Ok(Palette::Command(Command::Volume {
                input: "Mic/Aux".into(),
                db: -6.0
            }))
        );
        assert!(p("volume Mic").is_err());
        assert_eq!(
            p("hide Camera"),
            Ok(Palette::Command(Command::Source {
                source: "Camera".into(),
                scene: None,
                visible: Some(false)
            }))
        );
        assert_eq!(
            p("set tui.wrap_lines on"),
            Ok(Palette::Set {
                key: "tui.wrap_lines".into(),
                value: Some("on".into())
            })
        );
        assert_eq!(p("set"), Ok(Palette::Settings));
    }

    #[test]
    fn typed_set_writes_the_config_and_tab_completes_from_obs() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        view.state.scene = Some("Code".into());
        let dir = tempfile::tempdir().unwrap();
        view.config_path = dir.path().join("aftercast.toml");
        std::fs::write(&view.config_path, "").unwrap();
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        let typed = |app: &mut App, text: &str| {
            press(app, KeyCode::Char(':'));
            for c in text.chars() {
                press(app, KeyCode::Char(c));
            }
        };
        // `:set stream.chapter_on_brb off` writes the file and reloads.
        typed(&mut app, "set stream.chapter_on_brb off");
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Reload);
        let file = std::fs::read_to_string(&view.config_path).unwrap();
        assert!(file.contains("chapter_on_brb = false"), "{file}");
        // Tab completes the mic's name from OBS; Enter runs the full line.
        typed(&mut app, "mute mi");
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Mute {
                input: "Mic/Aux".into(),
                on: None
            }
        );
        // Ctrl+J moves down the list (typing j would type); Tab takes the pick.
        typed(&mut app, "s");
        let ctrl = |app: &mut App, c| {
            app.key(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
                &view,
            )
        };
        ctrl(&mut app, 'j');
        ctrl(&mut app, 'j');
        ctrl(&mut app, 'k');
        assert_eq!(app.pick, 1);
        let Mode::Input { text, .. } = &app.mode else {
            panic!("palette open")
        };
        assert_eq!(text, "s");
        app.mode = Mode::Normal;
        // Enter on a half-typed name takes the suggestion first, then runs.
        typed(&mut app, "hide cam");
        press(&mut app, KeyCode::Enter);
        assert!(cmd_rx.try_recv().is_err());
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Source {
                source: "Camera".into(),
                scene: None,
                visible: Some(false)
            }
        );
    }

    #[test]
    fn palette_history_is_saved_and_offered_first() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        let dir = tempfile::tempdir().unwrap();
        view.config_path = dir.path().join("aftercast.toml");
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock.clone(), rx, cmd_tx);
        app.mode = Mode::Normal;
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        for line in ["mute Mic/Aux", "sync", "mute Mic/Aux"] {
            press(&mut app, KeyCode::Char(':'));
            for c in line.chars() {
                press(&mut app, KeyCode::Char(c));
            }
            press(&mut app, KeyCode::Enter);
        }
        let saved = std::fs::read_to_string(dir.path().join("state").join("history")).unwrap();
        assert_eq!(
            saved,
            "sync
mute Mic/Aux
"
        );
        // A fresh TUI reads it back: the newest line is the first suggestion.
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        app.key(KeyEvent::new(KeyCode::Char(':'), KeyModifiers::NONE), &view);
        assert_eq!(app.suggestions("", &view)[0].line, "mute Mic/Aux");
    }

    #[test]
    fn a_draft_home_starts_on_enter_and_quits_keeping_nothing_on_q() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        view.begun = false;
        view.fresh = true;
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock.clone(), rx, cmd_tx);
        let text = screen(&mut app, &view);
        assert!(text.contains("▶ Start the session"), "{text}");
        assert!(text.contains("Quit, keep nothing"), "{text}");
        // No status bar on Home: the page starts at the top.
        assert!(text.lines().next().unwrap().contains("aftercast"), "{text}");
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        // Esc does not leave a draft Home.
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Preflight);
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Begin);
        // Enter leaves for the dashboard (the run loop keeps Home until the session says it
        // started); back on the draft Home, q quits.
        assert_eq!(app.mode, Mode::Normal);
        app.mode = Mode::Preflight;
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Quit);
        assert!(app.exit);
    }

    #[test]
    fn home_is_navigated_with_j_k_and_enter_acts_on_the_focused_item() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        view.begun = false;
        view.fresh = true;
        view.state.scene = Some("Code".into());
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        let draw = |app: &mut App| {
            let mut t =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 32)).unwrap();
            t.draw(|f| app.draw(f, &view)).unwrap();
            let b = t.backend().buffer().clone();
            (0..b.area.height)
                .map(|y| {
                    (0..b.area.width)
                        .map(|x| b[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                )
        };
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        // Start, Quit, the recording fix, the mic, the camera.
        press(&mut app, KeyCode::Char('j'));
        assert!(draw(&mut app).contains("▶ Quit, keep nothing"));
        press(&mut app, KeyCode::Char('j'));
        let text = draw(&mut app);
        assert!(text.contains("Enter start recording"), "{text}");
        assert!(text.contains(" R  start recording in OBS"), "{text}");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Record {
                action: ac_core::command::OutputAction::Start
            }
        );
        // Tab: on to audio; Enter mutes, l raises the volume by 1 dB.
        press(&mut app, KeyCode::Tab);
        assert!(draw(&mut app).contains("Enter mute Mic/Aux"));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Mute {
                input: "Mic/Aux".into(),
                on: None
            }
        );
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Volume {
                input: "Mic/Aux".into(),
                db: -2.0
            }
        );
        // j: the camera in the scene on air; Enter hides it.
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            cmd_rx.try_recv().unwrap(),
            Command::Source {
                source: "Camera".into(),
                scene: None,
                visible: None
            }
        );
        // j past the last item wraps to Start, k from Start to the last; Enter starts.
        press(&mut app, KeyCode::Char('j'));
        assert!(draw(&mut app).contains("Enter start the session"));
        press(&mut app, KeyCode::Char('k'));
        assert!(draw(&mut app).contains("Enter hide Camera"));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Begin);
    }

    #[test]
    fn mic_muted_shows_in_the_status_bar() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        Arc::make_mut(&mut view.obs).audio[0].muted = true;
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, _cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        assert!(screen(&mut app, &view).contains("MIC MUTED"));
    }

    #[test]
    fn comma_opens_settings_and_a_change_is_saved_then_reloaded() {
        let clock = Arc::new(Clock::new());
        let mut view = sample_view(&clock);
        let dir = tempfile::tempdir().unwrap();
        view.config_path = dir.path().join("aftercast.toml");
        std::fs::write(
            &view.config_path,
            "# keep me
",
        )
        .unwrap();
        let (_tx, rx) = watch::channel(view.clone());
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let mut app = App::new(clock, rx, cmd_tx);
        app.mode = Mode::Normal;
        let press = |app: &mut App, code| app.key(KeyEvent::new(code, KeyModifiers::NONE), &view);
        press(&mut app, KeyCode::Char(','));
        let text = screen(&mut app, &view);
        assert!(
            text.contains("Settings") && text.contains("Wrap long lines"),
            "{text}"
        );
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(cmd_rx.try_recv().unwrap(), Command::Reload);
        let file = std::fs::read_to_string(&view.config_path).unwrap();
        assert!(
            file.starts_with("# keep me") && file.contains("wrap_lines = true"),
            "{file}"
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
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
