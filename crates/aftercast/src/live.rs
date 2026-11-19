//! Live session: startup, the controller loop, YouTube polling and README watching
//! (specs/01-architecture.md, specs/04-session-and-folders.md "Lifecycle").

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ac_core::Home;
use ac_core::command::Command;
use ac_core::config::Config;
use ac_core::event::EventLog;
use ac_core::event::{
    EndReason, Event, EventKind, PushTrigger, QuestSource, ServiceStatus, Source, StreamStatus,
};
use ac_core::session::{NewSession, Session, Status};
use ac_core::session_state::SessionState;
use ac_core::state::State;
use ac_core::time::Clock;
use ac_template::chapters::Chapter;
use ac_template::{ContextInput, Description, README_NAME, RenderError, Renderer};
use ac_tui::View;
use ac_youtube::auth::Auth;
use ac_youtube::quota::{QuotaGuard, Spent};
use ac_youtube::{Broadcast, Client, LiveContent};
use notify::{RecursiveMode, Watcher};
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::time::{Instant, interval, sleep, sleep_until};

use crate::error::{AppError, Result};

/// Wait after the last README change before rendering (editors save in bursts).
const README_DEBOUNCE: Duration = Duration::from_millis(500);
/// Buffered events are written to disk at least this often.
const FLUSH_EVERY: Duration = Duration::from_millis(250);

/// Messages into the controller besides plain UI commands.
#[derive(Debug)]
pub enum Msg {
    /// A command whose caller waits until it has been handled (scripts).
    Command(Command, oneshot::Sender<()>),
    Spent(Spent),
    Broadcast(Result<Broadcast, String>),
    ReadmeChanged,
}

/// Everything a UI needs to drive and display a running session.
pub struct Handles {
    pub clock: Arc<Clock>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub msgs: mpsc::UnboundedSender<Msg>,
    pub view: watch::Receiver<View>,
    /// Subscribed before the first event, so no startup event is missed.
    pub events: broadcast::Receiver<Arc<Event>>,
    pub controller: tokio::task::JoinHandle<()>,
}

pub struct Options<'a> {
    pub link: Option<&'a str>,
    pub mock: Option<&'a Path>,
    pub allow_ended: bool,
}

/// Resolves the broadcast, opens or creates its session and starts the controller.
pub async fn start(home: &Home, config_path: &Path, opts: Options<'_>) -> Result<Handles> {
    let config = Config::load(config_path)?;
    let clock = Arc::new(Clock::new());
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();

    let video_id = match opts.link {
        Some(link) => ac_youtube::video_id(link)
            .ok_or_else(|| AppError::Usage(format!("not a YouTube link or video id: {link}")))?,
        None => last_session_video(home)?,
    };

    let mut yt = client(home, &config, opts.mock, clock.clone())?;
    let spent_tx = msg_tx.clone();
    yt.on_spent(move |spent| {
        let _ = spent_tx.send(Msg::Spent(spent));
    });

    let broadcast = yt.video(&video_id).await?;
    if broadcast.content == LiveContent::None && !opts.allow_ended {
        return Err(AppError::Usage(format!(
            "{video_id} is not a live or upcoming broadcast (pass --allow-ended to finalize an ended stream)"
        )));
    }
    let channel = yt.my_channel().await?;
    if channel.id != broadcast.channel_id {
        return Err(AppError::Usage(format!(
            "{video_id} belongs to another channel; signed in as {} ({})",
            channel.title, channel.id
        )));
    }

    let (session, resumed) = match Session::find(home, &config, &video_id)? {
        Some(session) => (session, true),
        None => {
            let new = NewSession {
                video_id: video_id.clone(),
                live_chat_id: broadcast.chat_id.clone(),
                title: Some(broadcast.title.clone()),
            };
            (Session::create(home, &config, new, clock.now_ts())?, false)
        }
    };
    let mut state = State::load(&home.state_file())?;
    if let Ok(rel) = session.dir.strip_prefix(home.root()) {
        state.last_session = Some(rel.to_path_buf());
        state.save(&home.state_file())?;
    }
    yt.quota_mut().set_session(Some(session.name()));

    let (session_state, log) = session.replay()?;
    // Keep event times monotonic across restarts, even if the system clock stepped back.
    if let Some(last) = session_state.log.back() {
        let behind = last.t.secs_since(clock.now_ts());
        if behind > 0.0 {
            clock.advance(Duration::from_secs_f64(behind));
        }
    }
    let renderer = Renderer::new(&home.path(&config.paths.templates));
    let (events_tx, events_rx) = broadcast::channel(4096);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let yt = Arc::new(yt);

    let watcher = watch_readme(&session.dir, msg_tx.clone())
        .inspect_err(|e| tracing::warn!("README watcher unavailable: {e}"))
        .ok();
    if broadcast.actual_end.is_none() {
        tokio::spawn(poll(
            yt.clone(),
            video_id.clone(),
            broadcast.content,
            msg_tx.clone(),
        ));
    }

    let mut controller = Controller {
        home: home.clone(),
        config_path: config_path.to_path_buf(),
        config,
        clock: clock.clone(),
        yt,
        session,
        state: session_state,
        log,
        renderer,
        events: events_tx.clone(),
        view: None,
        chapters: Vec::new(),
        description: None,
        render_error: None,
        notice: None,
        done: false,
    };
    let (view_tx, view_rx) = watch::channel(controller.snapshot());
    controller.view = Some(view_tx);

    controller.emit(
        "core",
        EventKind::SessionStart {
            episode: controller.session.meta.episode,
            video_id,
            resumed,
        },
    );
    controller.on_broadcast(Ok(broadcast));
    controller.render();
    let handle = tokio::spawn(async move {
        let _watcher = watcher; // dropping it stops watching
        if controller.config.youtube.push_on_start {
            controller.push(PushTrigger::Start, false).await;
        }
        controller.run(cmd_rx, msg_rx).await;
    });

    Ok(Handles {
        clock,
        commands: cmd_tx,
        msgs: msg_tx,
        view: view_rx,
        events: events_rx,
        controller: handle,
    })
}

/// The YouTube client: mock fixtures, or the signed-in profile.
fn client(home: &Home, config: &Config, mock: Option<&Path>, clock: Arc<Clock>) -> Result<Client> {
    let quota = QuotaGuard::open(
        &home.join("state/quota.json"),
        config.youtube.daily_quota_reserve,
        config.youtube.costs.clone(),
    )?;
    Ok(match mock {
        Some(fixture) => Client::mock(fixture, &home.join("mock/requests"), quota, clock),
        None => {
            let secret = home.path(&config.youtube.client_secret_file);
            Client::rest(
                Auth::load(&secret, &config.youtube.token_profile)?,
                quota,
                clock,
            )
        }
    })
}

/// No link given: resume the last session.
fn last_session_video(home: &Home) -> Result<String> {
    let state = State::load(&home.state_file())?;
    let rel = state
        .last_session
        .ok_or_else(|| AppError::Usage("no previous session; pass the stream link".into()))?;
    Ok(Session::open(&home.root().join(rel))?.meta.video_id)
}

/// Polls `videos.list` (30 s while upcoming, 60 s while live) until the stream ends.
async fn poll(
    yt: Arc<Client>,
    id: String,
    mut content: LiveContent,
    tx: mpsc::UnboundedSender<Msg>,
) {
    loop {
        let wait = if content == LiveContent::Upcoming {
            30
        } else {
            60
        };
        sleep(Duration::from_secs(wait)).await;
        let result = yt.video(&id).await;
        let ended = result.as_ref().is_ok_and(|b| b.actual_end.is_some());
        if let Ok(b) = &result {
            content = b.content;
        }
        if tx
            .send(Msg::Broadcast(result.map_err(|e| e.to_string())))
            .is_err()
            || ended
        {
            return;
        }
    }
}

/// Watches the session folder (not the file, so rename-saves are seen) for README writes.
fn watch_readme(
    dir: &Path,
    tx: mpsc::UnboundedSender<Msg>,
) -> notify::Result<notify::RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        let is_write = event.kind.is_modify() || event.kind.is_create();
        let is_readme = event
            .paths
            .iter()
            .any(|p| p.file_name().is_some_and(|n| n == README_NAME));
        if is_write && is_readme {
            let _ = tx.send(Msg::ReadmeChanged);
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Owns the session: turns commands and service messages into events, which it appends
/// to `events.jsonl` before broadcasting (so a slow subscriber can never lose them).
struct Controller {
    home: Home,
    config_path: PathBuf,
    config: Config,
    clock: Arc<Clock>,
    yt: Arc<Client>,
    session: Session,
    state: SessionState,
    log: EventLog,
    renderer: Renderer,
    events: broadcast::Sender<Arc<Event>>,
    view: Option<watch::Sender<View>>,
    chapters: Vec<Chapter>,
    description: Option<Description>,
    render_error: Option<String>,
    notice: Option<String>,
    done: bool,
}

impl Controller {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut msgs: mpsc::UnboundedReceiver<Msg>,
    ) {
        let mut flush = interval(FLUSH_EVERY);
        let mut render_at: Option<Instant> = None;
        while !self.done {
            let render_due = async {
                match render_at {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                Some(cmd) = commands.recv() => self.command(cmd).await,
                Some(msg) = msgs.recv() => match msg {
                    Msg::Command(cmd, ack) => {
                        self.command(cmd).await;
                        let _ = ack.send(());
                    }
                    Msg::Spent(s) => self.emit("youtube", EventKind::QuotaSpent {
                        method: s.method.to_owned(),
                        units: s.units,
                        remaining: s.remaining,
                    }),
                    Msg::Broadcast(result) => self.on_broadcast(result),
                    Msg::ReadmeChanged => render_at = Some(Instant::now() + README_DEBOUNCE),
                },
                () = render_due => {
                    render_at = None;
                    self.render();
                }
                _ = flush.tick() => self.flush(),
                else => break,
            }
            self.publish();
        }
        self.flush();
    }

    async fn command(&mut self, cmd: Command) {
        self.notice = None;
        match cmd {
            Command::Checkpoint {
                title,
                pressed_at,
                by,
            } => {
                let title: String = title.trim().chars().take(100).collect();
                let cid = ulid::Ulid::generate().to_string();
                self.emit_render(
                    by_src(by),
                    EventKind::Checkpoint {
                        cid,
                        title,
                        pressed_at,
                        by,
                        latency_corrected_s: None,
                    },
                );
            }
            Command::EditCheckpoint { cid, title } => {
                let title = title.trim().chars().take(100).collect();
                self.emit_render("tui", EventKind::CheckpointEdit { cid, title });
            }
            Command::DeleteCheckpoint { cid } => {
                self.emit_render("tui", EventKind::CheckpointDelete { cid })
            }
            Command::Mark {
                label,
                pressed_at,
                by,
            } => {
                let label = label.map(|l| l.trim().to_owned()).filter(|l| !l.is_empty());
                self.emit(
                    by_src(by),
                    EventKind::Mark {
                        label,
                        pressed_at,
                        by,
                        replay_path: None,
                    },
                );
            }
            Command::Note { text, pressed_at } => {
                self.emit("tui", EventKind::Note { text, pressed_at })
            }
            Command::Quest { title } => self.emit_render(
                "tui",
                EventKind::QuestSet {
                    id: None,
                    title,
                    source: QuestSource::Manual,
                    extra: None,
                },
            ),
            Command::Render => {
                self.render();
            }
            Command::Sync => self.push(PushTrigger::Manual, false).await,
            Command::Finalize => self.finalize().await,
            Command::Reload => self.reload(),
            Command::Quit => {
                self.emit(
                    "core",
                    EventKind::SessionEnd {
                        reason: Some(EndReason::Quit),
                    },
                );
                self.done = true;
            }
        }
    }

    /// Appends, applies and broadcasts one event.
    fn emit(&mut self, src: &str, kind: EventKind) {
        let event = Event::new(&self.clock, src, kind);
        if let Err(e) = self.log.append(&event) {
            tracing::error!("writing event: {e}");
        }
        self.state.apply(&event);
        let _ = self.events.send(Arc::new(event));
    }

    /// Emits an event that changes chapters or quest, then re-renders.
    fn emit_render(&mut self, src: &str, kind: EventKind) {
        self.emit(src, kind);
        self.render();
    }

    fn flush(&mut self) {
        if let Err(e) = self.log.flush() {
            tracing::error!("flushing events: {e}");
        }
    }

    /// Renders README.md into `.render/`; keeps the last good render on errors.
    fn render(&mut self) -> Option<Description> {
        let (ctx, chapters) = ac_template::context(&ContextInput {
            config: &self.config,
            meta: &self.session.meta,
            state: &self.state,
            now: self.clock.now_ts(),
        });
        let published = if ac_template::chapters::block(&chapters).is_empty() {
            0
        } else {
            chapters.len()
        };
        self.chapters = chapters;
        let result = fs::read_to_string(self.session.readme())
            .map_err(|e| RenderError::Template {
                name: README_NAME.into(),
                line: 1,
                col: 1,
                message: e.to_string(),
            })
            .and_then(|src| self.renderer.render(&src, &ctx, &self.config.readme));
        match result {
            Ok(desc) => {
                let dir = self.session.render_dir();
                let written = fs::create_dir_all(&dir)
                    .and_then(|()| fs::write(dir.join("README.rendered.md"), &desc.markdown))
                    .and_then(|()| fs::write(dir.join("description.txt"), &desc.text));
                if let Err(e) = written {
                    tracing::error!("writing render output: {e}");
                }
                self.emit(
                    "core",
                    EventKind::DescRendered {
                        chars: desc.chars(),
                        chapters: published,
                        ok: true,
                        error: None,
                    },
                );
                self.render_error = None;
                self.description = Some(desc.clone());
                Some(desc)
            }
            Err(e) => {
                self.emit(
                    "core",
                    EventKind::DescRendered {
                        chars: 0,
                        chapters: published,
                        ok: false,
                        error: Some(e.to_string()),
                    },
                );
                self.render_error = Some(e.to_string());
                None
            }
        }
    }

    /// Renders and pushes the description unless it equals the last pushed one.
    async fn push(&mut self, trigger: PushTrigger, critical: bool) {
        let Some(desc) = self.render() else {
            let error = self.render_error.clone();
            self.notice = Some(format!(
                "not pushed: {}",
                error.as_deref().unwrap_or("render failed")
            ));
            self.emit("youtube", EventKind::DescPushFailed { error, trigger });
            return;
        };
        let pushed_path = self.session.render_dir().join("description.pushed.txt");
        if fs::read_to_string(&pushed_path).is_ok_and(|pushed| pushed == desc.text) {
            tracing::info!("description unchanged; not pushed");
            self.notice = Some("description unchanged; not pushed".into());
            return;
        }
        let video_id = self.session.meta.video_id.clone();
        match self
            .yt
            .update_description(&video_id, &desc.text, critical)
            .await
        {
            Ok(()) => {
                if let Err(e) = fs::write(&pushed_path, &desc.text) {
                    tracing::error!("{}: {e}", pushed_path.display());
                }
                self.notice = Some(format!("description pushed ({} chars)", desc.chars()));
                self.emit(
                    "youtube",
                    EventKind::DescPushed {
                        chars: desc.chars(),
                        trigger,
                    },
                );
            }
            Err(e) => {
                self.notice = Some(format!("push failed: {e}"));
                self.emit(
                    "youtube",
                    EventKind::DescPushFailed {
                        error: Some(e.to_string()),
                        trigger,
                    },
                );
            }
        }
    }

    async fn finalize(&mut self) {
        if self.config.youtube.push_on_end {
            self.push(PushTrigger::End, true).await;
        }
        self.set_status(Status::Finalized);
        self.emit(
            "core",
            EventKind::SessionEnd {
                reason: Some(EndReason::Finalized),
            },
        );
        self.done = true;
    }

    /// `:reload`: re-read config and templates; an invalid config keeps the old one.
    fn reload(&mut self) {
        match Config::load(&self.config_path) {
            Ok(config) => {
                self.renderer = Renderer::new(&self.home.path(&config.paths.templates));
                self.config = config;
                self.notice = Some("config and templates reloaded".into());
            }
            Err(e) => self.notice = Some(format!("reload failed, keeping old config: {e}")),
        }
        self.render();
    }

    /// Applies a `videos.list` result: anchors, live/ended status, service state.
    fn on_broadcast(&mut self, result: Result<Broadcast, String>) {
        let broadcast = match result {
            Ok(b) => b,
            Err(error) => {
                self.service_state(ServiceStatus::Retrying);
                self.emit(
                    "youtube",
                    EventKind::ServiceError {
                        service: "youtube".into(),
                        error: Some(error),
                        retry_in_s: 60.0,
                    },
                );
                return;
            }
        };
        self.service_state(ServiceStatus::Ok);
        let anchors = &mut self.session.meta.anchors;
        let mut changed = false;
        // actualStartTime never changes for a video; keep the first one seen.
        if anchors.yt_actual_start.is_none() && broadcast.actual_start.is_some() {
            anchors.yt_actual_start = broadcast.actual_start;
            changed = true;
        }
        if broadcast.actual_end.is_some() && anchors.yt_actual_end != broadcast.actual_end {
            anchors.yt_actual_end = broadcast.actual_end;
            changed = true;
        }
        if broadcast.chat_id.is_some() && self.session.meta.live_chat_id.is_none() {
            self.session.meta.live_chat_id = broadcast.chat_id.clone();
            changed = true;
        }
        if changed {
            self.save_session();
        }

        let status = match (broadcast.content, broadcast.actual_end) {
            (_, Some(_)) => StreamStatus::Ended,
            (LiveContent::Upcoming, None) => StreamStatus::Upcoming,
            _ => StreamStatus::Live,
        };
        if self.state.stream == Some(status) || self.state.ended {
            return;
        }
        if status == StreamStatus::Ended {
            self.emit(
                "youtube",
                EventKind::StreamEnded {
                    source: Source::Youtube,
                },
            );
            self.set_status(Status::Ended);
        } else {
            self.emit(
                "youtube",
                EventKind::StreamStatus {
                    status,
                    source: Source::Youtube,
                },
            );
            if status == StreamStatus::Live {
                self.set_status(Status::Live);
            }
        }
    }

    fn service_state(&mut self, state: ServiceStatus) {
        if self.state.services.get("youtube") != Some(&state) {
            self.emit(
                "youtube",
                EventKind::ServiceState {
                    service: "youtube".into(),
                    state,
                },
            );
        }
    }

    /// Moves the lifecycle forward only (created, live, ended, finalized).
    fn set_status(&mut self, status: Status) {
        if self.session.meta.status < status {
            self.session.meta.status = status;
            self.save_session();
        }
    }

    fn save_session(&self) {
        if let Err(e) = self.session.save() {
            tracing::error!("saving session.toml: {e}");
        }
    }

    fn snapshot(&self) -> View {
        View {
            session: self.session.name(),
            meta: self.session.meta.clone(),
            state: self.state.clone(),
            chapters: self.chapters.clone(),
            description: self.description.clone(),
            render_error: self.render_error.clone(),
            quota_remaining: self.yt.quota().remaining(self.clock.now_ts()),
            notice: self.notice.clone(),
        }
    }

    fn publish(&self) {
        if let Some(view) = &self.view {
            view.send_replace(self.snapshot());
        }
    }
}

/// Event `src` for an action's origin.
fn by_src(by: ac_core::event::By) -> &'static str {
    use ac_core::event::By;
    match by {
        By::Tui => "tui",
        By::Hotkey => "hotkey",
        By::Palette => "palette",
        By::Mod | By::OwnerChat => "bot",
        By::ObsScene => "obs",
    }
}
