//! Live session: startup, the controller loop, YouTube polling and README watching
//! (specs/01-architecture.md, specs/04-session-and-folders.md "Lifecycle").

mod capture;
mod chat;
mod obs;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ac_core::Home;
use ac_core::chat::ChatMessage;
use ac_core::command::Command;
use ac_core::config::Config;
use ac_core::event::EventLog;
use ac_core::event::{
    EndReason, Event, EventKind, OutputState, PushTrigger, QuestSource, ServiceStatus, Source,
    StreamStatus,
};
use ac_core::recordings::Recordings;
use ac_core::session::{NewSession, Session, Status};
use ac_core::session_state::SessionState;
use ac_core::state::State;
use ac_core::time::{Clock, Timestamp};
use ac_template::chapters::Chapter;
use ac_template::{ContextInput, Description, README_NAME, RenderError, Renderer};
use ac_tui::{Check, CheckState, KeysStatus, View};
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
    /// A file in `<home>/overlays` changed (rehearsal only).
    OverlaysChanged,
    Obs(ac_obs::Update),
    Chat(ac_youtube::chat::ChatUpdate),
    /// The external current-quest file changed.
    QuestFile,
    /// Answered once everything sent before it is handled and published (scripts).
    Barrier(oneshot::Sender<()>),
    /// A key press from the keyboard hook (or a script), not yet filtered.
    Key(ac_capture::keys::RawKey),
    /// The keyboard hook started, or why it failed.
    KeysHook(std::result::Result<(), String>),
    /// Read new lines from the shell spool.
    ShellPoll,
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
    /// `aftercast rehearse`: real OBS and overlays; simulated YouTube and chat; a throwaway
    /// session under `<home>/rehearsal/`; overlay files reload in OBS when saved.
    pub rehearsal: bool,
}

/// Video id of the simulated broadcast in a rehearsal.
const REHEARSAL_VIDEO: &str = "rehearsal01";
const REHEARSAL_FILES: [(&str, &str); 2] = [
    (
        "youtube/videos.list.json",
        include_str!("../rehearsal/youtube/videos.list.json"),
    ),
    (
        "youtube/channels.list.json",
        include_str!("../rehearsal/youtube/channels.list.json"),
    ),
];
const REHEARSAL_CHAT: &str = include_str!("../rehearsal/chat.jsonl");
/// Overlay files are reloaded in OBS this long after the last change (editors write twice).
const OVERLAY_DEBOUNCE: Duration = Duration::from_millis(300);

/// Fresh rehearsal: the simulated YouTube answers and demo chat (kept if edited) in
/// `<home>/rehearsal`, and no earlier rehearsal session (episode 0) in `sessions`.
fn prepare_rehearsal(home: &Home, config: &Config, dir: &Path) -> Result<()> {
    Session::remove_rehearsals(home, config)?;
    // `sessions` and `state.toml` there are from the first rehearsal version.
    for old in ["requests", "sessions"] {
        let old = dir.join(old);
        if old.exists() {
            fs::remove_dir_all(&old)?;
        }
    }
    let _ = fs::remove_file(dir.join("state.toml"));
    for (rel, text) in REHEARSAL_FILES {
        let path = dir.join(rel);
        fs::create_dir_all(path.parent().expect("has a folder"))?;
        fs::write(path, text)?;
    }
    let chat = dir.join("chat.jsonl");
    if !chat.exists() {
        fs::write(chat, REHEARSAL_CHAT)?;
    }
    Ok(())
}

/// `.render/description.pushed.txt`: the description as last pushed.
fn session_render_pushed(session: &Session) -> PathBuf {
    session.render_dir().join("description.pushed.txt")
}

/// Watches `<home>/overlays` so edited overlay files reload in OBS.
fn watch_overlays(
    dir: &Path,
    tx: mpsc::UnboundedSender<Msg>,
) -> notify::Result<notify::RecommendedWatcher> {
    fs::create_dir_all(dir).map_err(notify::Error::io)?;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok_and(|e| e.kind.is_modify() || e.kind.is_create() || e.kind.is_remove()) {
            let _ = tx.send(Msg::OverlaysChanged);
        }
    })?;
    watcher.watch(dir, RecursiveMode::Recursive)?;
    Ok(watcher)
}

/// Resolves the broadcast, opens or creates its session and starts the controller.
pub async fn start(home: &Home, config_path: &Path, opts: Options<'_>) -> Result<Handles> {
    let config = Config::load(config_path)?;
    let clock = Arc::new(Clock::new());
    let (msg_tx, msg_rx) = mpsc::unbounded_channel();
    let rehearsal_dir = home.join("rehearsal");
    if opts.rehearsal {
        prepare_rehearsal(home, &config, &rehearsal_dir)?;
    }

    let video_id = match opts.link {
        _ if opts.rehearsal => REHEARSAL_VIDEO.to_owned(),
        Some(link) => ac_youtube::video_id(link)
            .ok_or_else(|| AppError::Usage(format!("not a YouTube link or video id: {link}")))?,
        None => last_session_video(home)?,
    };

    let mut yt = if opts.rehearsal {
        // Its own quota file: a rehearsal never touches today's real quota.
        let quota = QuotaGuard::open(
            &rehearsal_dir.join("quota.json"),
            config.youtube.daily_quota_reserve,
            config.youtube.costs.clone(),
        )?;
        Client::mock(
            &rehearsal_dir,
            &rehearsal_dir.join("requests"),
            quota,
            clock.clone(),
        )
    } else {
        client(home, &config, opts.mock, clock.clone())?
    };
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
            let session = if opts.rehearsal {
                Session::create_rehearsal(home, &config, new, clock.now_ts())?
            } else {
                Session::create(home, &config, new, clock.now_ts())?
            };
            (session, false)
        }
    };
    let mut state = State::load(&home.state_file())?;
    // A rehearsal is never the "last session" that post, render and resume default to.
    if !opts.rehearsal
        && let Ok(rel) = session.dir.strip_prefix(home.root())
    {
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
    let recordings = Recordings::load(&session.recordings_file())?;
    let renderer = Renderer::new(&home.path(&config.paths.templates));
    let (events_tx, events_rx) = broadcast::channel(4096);
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    // Rehearsal page: the chat box feeds the simulated chat; the session feeds the page.
    let injector = if opts.rehearsal {
        yt.chat_injector()
    } else {
        None
    };
    let (feed_tx, page) = match injector {
        Some(inject) => {
            let (tx, rx) = watch::channel(ac_server::Feed::default());
            (Some(tx), Some(ac_server::Rehearsal::new(rx, inject)))
        }
        None => (None, None),
    };
    let yt = Arc::new(yt);
    let started = clock.now_ts();

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

    let obs = connect_obs(&config, opts.mock.is_some(), &msg_tx);

    let chat_id = session
        .meta
        .live_chat_id
        .clone()
        .or(broadcast.chat_id.clone());
    // True while a latency measurement runs, so chat reopens without waiting.
    let (chat_fast, chat_fast_rx) = watch::channel(false);
    if let Some(chat_id) = chat_id.filter(|_| broadcast.actual_end.is_none()) {
        let (yt, tx) = (yt.clone(), msg_tx.clone());
        let idle_max = std::time::Duration::from_secs(config.youtube.chat_idle_max_s);
        tokio::spawn(async move {
            yt.run_chat(&chat_id, idle_max, chat_fast_rx, |u| {
                let _ = tx.send(Msg::Chat(u));
            })
            .await;
        });
    }
    let quest_watcher = watch_quest_file(home, &config, msg_tx.clone());
    let overlay_watcher = opts
        .rehearsal
        .then(|| {
            watch_overlays(&home.join("overlays"), msg_tx.clone())
                .inspect_err(|e| tracing::warn!("overlay watcher unavailable: {e}"))
                .ok()
        })
        .flatten();
    let chat_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(session.dir.join("chat.jsonl"))
        .map(std::io::BufWriter::new)
        .inspect_err(|e| tracing::error!("chat.jsonl: {e}"))
        .ok();

    let overlays = ac_server::Overlays::default();
    let (state_tx, state_rx) = watch::channel(session_state.clone());
    let server = serve(home, &config, &overlays, state_rx, &cmd_tx, &clock, page).await;

    let bot = config.bot.enabled.then(|| ac_bot::Bot::new(&config.bot));
    let capture = capture::Capture::new(home, &config, &session);
    if config.capture.shell {
        let tx = msg_tx.clone();
        tokio::spawn(async move {
            let mut every = interval(Duration::from_secs(1));
            while tx.send(Msg::ShellPoll).is_ok() {
                every.tick().await;
            }
        });
    }
    let pushed_text = fs::read_to_string(session_render_pushed(&session)).ok();
    let mut controller = Controller {
        home: home.clone(),
        config_path: config_path.to_path_buf(),
        config: Arc::new(config),
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
        obs,
        obs_scenes: None,
        obs_media: Arc::default(),
        obs_levels: ac_core::obs::Levels::default(),
        pending_scenes: VecDeque::new(),
        recordings,
        privacy_return: None,
        bot,
        overlays,
        server,
        state_tx,
        chat: Arc::default(),
        chat_log,
        started,
        latency_run: None,
        chat_fast,
        rehearsal_feed: feed_tx,
        title: broadcast.title.clone(),
        pushed_text,
        pending_title: None,
        overlays_off: BTreeSet::new(),
        overlay_sent: BTreeMap::new(),
        capture,
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
    controller.capture_start();
    controller.on_broadcast(Ok(broadcast));
    controller.render();
    let handle = tokio::spawn(async move {
        // Dropping a watcher stops it.
        let _watchers = (watcher, quest_watcher, overlay_watcher);
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

/// Starts the OBS service (or, with `--mock`, a fake OBS that has the configured scenes).
fn connect_obs(config: &Config, mock: bool, msgs: &mpsc::UnboundedSender<Msg>) -> obs::ObsLink {
    if mock {
        let scenes: Vec<String> = config
            .obs
            .scenes
            .all()
            .iter()
            .map(|s| s.to_string())
            .collect();
        // A mic, desktop audio and a camera in the main scene, for trying the controls.
        let media = ac_core::obs::Media {
            scenes: scenes.clone(),
            audio: [
                ("Mic/Aux", "wasapi_input_capture"),
                ("Desktop Audio", "wasapi_output_capture"),
            ]
            .map(|(name, kind)| ac_core::obs::Audio {
                name: name.into(),
                kind: kind.into(),
                muted: false,
                volume_db: 0.0,
            })
            .to_vec(),
            items: vec![ac_core::obs::Item {
                scene: config.obs.scenes.main.clone(),
                source: "Camera".into(),
                id: 1,
                enabled: true,
            }],
        };
        let _ = msgs.send(Msg::Obs(ac_obs::Update::Connected(ac_obs::Snapshot {
            scene: Some(config.obs.scenes.main.clone()),
            scenes: scenes.clone(),
            media,
            ..Default::default()
        })));
        return obs::ObsLink::Fake {
            scenes,
            msgs: msgs.clone(),
        };
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let password = crate::secret::resolve(&config.obs.password)
        .inspect_err(|e| tracing::warn!("OBS password: {e}"))
        .ok()
        .flatten();
    match ac_obs::Target::parse(&config.obs.url, password) {
        Ok(target) => {
            let msgs = msgs.clone();
            tokio::spawn(ac_obs::run(target, rx, move |u| {
                let _ = msgs.send(Msg::Obs(u));
            }));
        }
        Err(error) => {
            let _ = msgs.send(Msg::Obs(ac_obs::Update::Down {
                error,
                retry_in: Duration::ZERO,
            }));
        }
    }
    obs::ObsLink::Real(tx)
}

/// Starts the overlay and local API server on `127.0.0.1:[overlay].port`. A failure
/// (port taken) leaves the session running and shows in the pre-flight checklist.
async fn serve(
    home: &Home,
    config: &Config,
    overlays: &ac_server::Overlays,
    state: watch::Receiver<SessionState>,
    commands: &mpsc::UnboundedSender<Command>,
    clock: &Arc<Clock>,
    rehearsal: Option<ac_server::Rehearsal>,
) -> std::result::Result<SocketAddr, String> {
    let token_file = home.join("state/api-token");
    let token =
        ac_server::token(&token_file).map_err(|e| format!("{}: {e}", token_file.display()))?;
    let shared = ac_server::Shared {
        overlays: overlays.clone(),
        state,
        commands: commands.clone(),
        clock: clock.clone(),
        token,
        dir: home.join("overlays"),
        theme: ac_server::theme(&config.overlay),
        rehearsal,
    };
    let port = config.overlay.port;
    let (addr, server) = ac_server::bind(port, shared)
        .await
        .map_err(|e| format!("127.0.0.1:{port}: {e}"))?;
    tokio::spawn(async move {
        if let Err(e) = server.await {
            tracing::error!("overlay server: {e}");
        }
    });
    Ok(addr)
}

/// Watches `[external].current_quest_file` (its folder, so replace-saves are seen) and
/// reads it once now.
fn watch_quest_file(
    home: &Home,
    config: &Config,
    tx: mpsc::UnboundedSender<Msg>,
) -> Option<notify::RecommendedWatcher> {
    let rel = &config.external.current_quest_file;
    if rel.as_os_str().is_empty() {
        return None;
    }
    let path = home.path(rel);
    let name = path.file_name()?.to_owned();
    let _ = tx.send(Msg::QuestFile);
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        let ours = event.paths.iter().any(|p| p.file_name() == Some(&name));
        if ours && (event.kind.is_modify() || event.kind.is_create()) {
            let _ = tx.send(Msg::QuestFile);
        }
    })
    .inspect_err(|e| tracing::warn!("quest file watcher: {e}"))
    .ok()?;
    let dir = path.parent()?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .inspect_err(|e| tracing::warn!("watching {}: {e}", dir.display()))
        .ok()?;
    Some(watcher)
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
    /// Shared with the TUI's settings page through [`View`].
    config: Arc<Config>,
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
    obs: obs::ObsLink,
    /// Scenes in OBS while connected; `None` while OBS is unreachable.
    obs_scenes: Option<Vec<String>>,
    /// Audio inputs and scene sources as OBS last reported them (empty while down).
    obs_media: Arc<ac_core::obs::Media>,
    /// Live audio peaks, shared with the TUI (not part of the published view).
    obs_levels: ac_core::obs::Levels,
    /// Scene switches sent to OBS whose echo has not arrived yet.
    pending_scenes: VecDeque<String>,
    recordings: Recordings,
    /// Scene to restore when privacy ends.
    privacy_return: Option<String>,
    /// `None` when `[bot].enabled = false`.
    bot: Option<ac_bot::Bot>,
    overlays: ac_server::Overlays,
    /// Overlay server address, or why it is not listening.
    server: std::result::Result<SocketAddr, String>,
    /// Session state for the local API.
    state_tx: watch::Sender<SessionState>,
    /// Last chat messages for the TUI.
    chat: Arc<VecDeque<ChatMessage>>,
    chat_log: Option<std::io::BufWriter<fs::File>>,
    /// Chat commands typed before this (history sent on connect) are not executed.
    started: Timestamp,
    latency_run: Option<chat::LatencyRun>,
    /// Tells the chat reader a latency measurement runs (reopen chat without waiting).
    chat_fast: watch::Sender<bool>,
    /// `aftercast rehearse`: what the rehearsal page shows.
    rehearsal_feed: Option<watch::Sender<ac_server::Feed>>,
    /// The broadcast title (as set at start or by `!title`).
    title: String,
    /// The description as last pushed to YouTube.
    pushed_text: Option<String>,
    /// A `!title` waiting for confirmation in the TUI.
    pending_title: Option<String>,
    /// Overlays turned off with `:overlay <name> off`.
    overlays_off: BTreeSet<String>,
    /// Last message sent per stateful overlay.
    overlay_sent: BTreeMap<&'static str, ac_server::OverlayMsg>,
    capture: capture::Capture,
}

impl Controller {
    async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        mut msgs: mpsc::UnboundedReceiver<Msg>,
    ) {
        let mut flush = interval(FLUSH_EVERY);
        let mut render_at: Option<Instant> = None;
        let mut refresh_at: Option<Instant> = None;
        while !self.done {
            let render_due = async {
                match render_at {
                    Some(at) => sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            let refresh_due = async {
                match refresh_at {
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
                    Msg::OverlaysChanged => refresh_at = Some(Instant::now() + OVERLAY_DEBOUNCE),
                    Msg::Obs(update) => self.on_obs(update),
                    Msg::Chat(update) => self.on_chat(update),
                    Msg::QuestFile => self.quest_file(),
                    Msg::Barrier(ack) => {
                        self.publish();
                        let _ = ack.send(());
                    }
                    Msg::Key(raw) => self.on_key(raw),
                    Msg::KeysHook(result) => self.keys_hook(result),
                    Msg::ShellPoll => self.shell_poll(),
                },
                () = render_due => {
                    render_at = None;
                    self.render();
                }
                () = refresh_due => {
                    refresh_at = None;
                    self.refresh_overlays();
                    self.notice = Some("overlays reloaded in OBS".into());
                }
                _ = flush.tick() => {
                    self.flush();
                    self.write_keys(false);
                }
                else => break,
            }
            self.publish();
        }
        self.write_keys(true);
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
            Command::Brb { on, by } => self.brb(on, by),
            Command::Privacy { on, by } => self.privacy(on, by),
            Command::Scene { name } => {
                // Typed names match case-insensitively.
                let known = self
                    .obs_scenes
                    .as_ref()
                    .map(|s| s.iter().find(|s| s.eq_ignore_ascii_case(&name)).cloned());
                match known {
                    Some(None) => self.notice = Some(format!("OBS has no scene named {name:?}")),
                    Some(Some(name)) => self.switch_scene(name),
                    None => self.switch_scene(name),
                }
            }
            Command::Mute { input, on } => self.mute(&input, on),
            Command::Volume { input, db } => self.volume(&input, db),
            Command::Source {
                source,
                scene,
                visible,
            } => self.source_visible(&source, scene.as_deref(), visible),
            Command::Record { action } => self.record(action),
            Command::Stream { on } => self.stream(on),
            Command::Meters { on } => self.meters(on),
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
            Command::Question { qid, action } => self.question(qid, action),
            Command::Say { text } => {
                self.send_chat(text.clone());
                self.emit(
                    "tui",
                    EventKind::BotReply {
                        cmd: "say".into(),
                        mode: ac_core::event::BotReplyMode::Chat,
                        text,
                    },
                );
            }
            Command::Overlay { name, on } => {
                if !ac_server::OVERLAYS.contains(&name.as_str()) {
                    self.notice = Some(format!(
                        "no overlay {name:?} (have: {})",
                        ac_server::OVERLAYS.join(", ")
                    ));
                } else if on {
                    self.overlays_off.remove(&name);
                } else {
                    self.overlays_off.insert(name);
                }
            }
            Command::Latency => self.latency_toggle(),
            Command::LatencySample { counter, at } => self.latency_sample(counter, at),
            Command::ConfirmTitle { ok } => self.confirm_title(ok).await,
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
        if let Some(Err(e)) = self.chat_log.as_mut().map(std::io::Write::flush) {
            tracing::error!("flushing chat.jsonl: {e}");
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
                self.pushed_text = Some(desc.text.clone());
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
                self.config = Arc::new(config);
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
                self.service_state("youtube", ServiceStatus::Retrying);
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
        self.service_state("youtube", ServiceStatus::Ok);
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

    fn service_state(&mut self, service: &str, state: ServiceStatus) {
        if self.state.services.get(service) != Some(&state) {
            self.emit(
                service,
                EventKind::ServiceState {
                    service: service.into(),
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
            readme: self.session.readme(),
            config: self.config.clone(),
            config_path: self.config_path.clone(),
            obs: self.obs_media.clone(),
            levels: self.obs_levels.clone(),
            meta: self.session.meta.clone(),
            state: self.state.clone(),
            chapters: self.chapters.clone(),
            description: self.description.clone(),
            render_error: self.render_error.clone(),
            quota_remaining: self.yt.quota().remaining(self.clock.now_ts()),
            notice: self.notice.clone(),
            preflight: self.preflight(),
            chat: self.chat_pane(),
            pending_title: self.pending_title.clone(),
            latency_running: self.latency_run.is_some(),
            latency_s: self.latency(),
            keys: match (&self.capture.hook, self.config.capture.keys) {
                (_, false) => KeysStatus::Off,
                (None, true) => KeysStatus::Starting,
                (Some(Ok(())), true) => KeysStatus::On,
                (Some(Err(_)), true) => KeysStatus::Error,
            },
            keys_per_min: self.keys_per_min(),
        }
    }

    /// Pre-flight checklist (specs/02-cli-and-tui.md); failures stay red until resolved.
    fn preflight(&self) -> Vec<Check> {
        let check = |label: &str, state, detail: String| Check {
            label: label.into(),
            state,
            detail,
        };
        let ok_if = |ok: bool| if ok { CheckState::Ok } else { CheckState::Fail };
        let meta = &self.session.meta;
        let mut list = vec![
            check(
                "Config valid",
                CheckState::Ok,
                self.config_path.display().to_string(),
            ),
            check(
                "Templates found",
                ok_if(self.render_error.is_none()),
                self.render_error.clone().unwrap_or_default(),
            ),
            check(
                "YouTube signed in, broadcast resolved",
                CheckState::Ok,
                meta.video_id.clone(),
            ),
            check(
                "liveChatId present",
                ok_if(meta.live_chat_id.is_some()),
                String::new(),
            ),
        ];
        let obs = &self.config.obs;
        let connected = self.obs_scenes.is_some();
        let missing: Vec<&str> = obs
            .scenes
            .all()
            .into_iter()
            .filter(|s| {
                self.obs_scenes
                    .as_ref()
                    .is_some_and(|have| !have.iter().any(|h| h == s))
            })
            .collect();
        list.push(check("OBS connected", ok_if(connected), obs.url.clone()));
        list.push(check(
            "OBS scenes exist",
            match (connected, missing.is_empty()) {
                (false, _) => CheckState::Pending,
                (true, ok) => ok_if(ok),
            },
            if missing.is_empty() {
                String::new()
            } else {
                format!("missing: {}", missing.join(", "))
            },
        ));
        // Without a connection the output state is unknown, not failed.
        let output = |active: bool, required: bool| match (active, connected) {
            (true, _) => CheckState::Ok,
            (false, true) if required => CheckState::Fail,
            _ => CheckState::Pending,
        };
        let streaming = self.state.obs_stream == Some(OutputState::Started);
        list.push(check(
            "OBS streaming",
            output(streaming, true),
            String::new(),
        ));
        let recording = matches!(
            self.state.obs_record,
            Some(OutputState::Started | OutputState::Resumed)
        );
        list.push(check(
            "OBS recording",
            output(recording, obs.require_recording),
            if recording {
                String::new()
            } else {
                "the local recording is the editing source".into()
            },
        ));
        let remaining = self.yt.quota().remaining(self.clock.now_ts());
        let reserve = self.config.youtube.daily_quota_reserve;
        list.push(check(
            "Quota remaining >= reserve",
            ok_if(remaining >= i64::from(reserve)),
            format!("{remaining} left, reserve {reserve}"),
        ));
        list.push(match &self.server {
            Ok(addr) => check(
                "Overlay server listening",
                CheckState::Ok,
                format!("http://{addr}/overlay/<name>"),
            ),
            Err(e) => check("Overlay server listening", CheckState::Fail, e.clone()),
        });
        if self.config.capture.keys {
            list.push(match &self.capture.hook {
                Some(Ok(())) => check("Key capture running", CheckState::Ok, String::new()),
                Some(Err(e)) => check("Key capture running", CheckState::Fail, e.clone()),
                None => check(
                    "Key capture running",
                    CheckState::Pending,
                    "starts with the TUI".into(),
                ),
            });
        }
        if self.config.capture.shell {
            // Optional: shown, never red.
            let spool = self.home.path(&self.config.capture.shell_spool);
            let recent = fs::metadata(&spool)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|age| age.as_secs() < 24 * 3600));
            list.push(if recent {
                check(
                    "Shell hook seen in last 24 h",
                    CheckState::Ok,
                    String::new(),
                )
            } else {
                check(
                    "Shell hook seen in last 24 h",
                    CheckState::Pending,
                    "optional: aftercast shell-hook pwsh".into(),
                )
            });
        }
        list
    }

    fn publish(&mut self) {
        if let Some(feed) = &self.rehearsal_feed {
            let rendered = self.description.as_ref().map(|d| d.text.clone());
            feed.send_replace(ac_server::Feed {
                title: self.title.clone(),
                live_since: self.session.meta.anchors.yt_actual_start,
                description: self.pushed_text.clone().unwrap_or_default(),
                draft: rendered.filter(|r| Some(r) != self.pushed_text.as_ref()),
                scene: self.state.scene.clone(),
                chat: self.chat.clone(),
            });
        }
        let measuring = self.latency_run.is_some();
        self.chat_fast.send_if_modified(|fast| {
            let changed = *fast != measuring;
            *fast = measuring;
            changed
        });
        self.sync_overlays();
        self.state_tx.send_replace(self.state.clone());
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
