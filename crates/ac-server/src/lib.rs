//! Overlay and local API server (specs/09-streambot.md, "Overlay server"): axum on
//! `127.0.0.1` only. Overlay pages for OBS browser sources, a WebSocket that pushes
//! [`OverlayMsg`]s, `GET /api/state` and token-protected `POST /api/command`.

mod dvr;
mod media;
mod rehearse;

pub use rehearse::{Feed, Rehearse};

use std::collections::BTreeMap;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ac_core::command::Command;
use ac_core::config;
use ac_core::session_state::SessionState;
use ac_core::time::Clock;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;

/// Built-in overlays.
pub const OVERLAYS: [&str; 11] = [
    "now", "question", "toast", "brb", "latency", "chapters", "keys", "captions", "starting",
    "status", "alert",
];
const PAGE: &str = include_str!("../overlays/overlay.html");
const SCRIPT: &str = include_str!("../overlays/overlay.js");
const STYLE: &str = include_str!("../overlays/overlay.css");
const SFX: &str = include_str!("../overlays/sfx.js");
const ALERT: &str = include_str!("../overlays/alert.js");
const MUSIC: &str = include_str!("../overlays/music.js");
const DVR: &str = include_str!("../overlays/dvr.js");
pub const TOKEN_HEADER: &str = "x-aftercast-token";

/// `{ "overlay": "question", "action": "show", "data": {...} }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OverlayMsg {
    pub overlay: String,
    pub action: Action,
    #[serde(default)]
    pub data: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Show,
    Hide,
    Update,
}

/// Sends overlay messages to every connected page. Cheap to clone.
#[derive(Clone)]
pub struct Overlays {
    tx: broadcast::Sender<OverlayMsg>,
    /// Last show/hide per overlay, replayed to pages that (re)connect.
    last: Arc<Mutex<BTreeMap<String, OverlayMsg>>>,
    /// `[stream] delay_ms`: OBS shows its sources this late, so every overlay but the
    /// captions waits as long to stay with the picture. Changes live.
    delay_ms: Arc<AtomicU64>,
    /// Delayed messages, in order; `None` sends at once (no runtime, as in tests).
    queue: Option<mpsc::UnboundedSender<(Instant, OverlayMsg)>>,
    /// `:root { --bg: ... }` and every overlay's place, from `[overlay]` (see [`theme`]).
    theme: Arc<Mutex<String>>,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            tx: broadcast::channel(256).0,
            last: Arc::default(),
            delay_ms: Arc::default(),
            queue: None,
            theme: Arc::new(Mutex::new(theme(&config::Overlay::default()))),
        }
    }
}

impl Overlays {
    /// Overlays that can trail the picture (captions excepted), in order. Needs a tokio
    /// runtime.
    pub fn delayed(ms: u64) -> Self {
        let mut overlays = Self::default();
        overlays.set_delay(ms);
        let (tx, mut rx) = mpsc::unbounded_channel::<(Instant, OverlayMsg)>();
        let out = overlays.clone();
        tokio::spawn(async move {
            while let Some((at, msg)) = rx.recv().await {
                tokio::time::sleep_until(at).await;
                out.emit(msg);
            }
        });
        overlays.queue = Some(tx);
        overlays
    }

    pub fn set_delay(&self, ms: u64) {
        self.delay_ms.store(ms, Ordering::Relaxed);
    }

    /// New colors and places: open pages restyle at once (not delayed), new pages load
    /// with them.
    pub fn set_theme(&self, css: String) {
        *self.theme.lock().expect("overlay theme lock") = css.clone();
        self.emit(OverlayMsg {
            overlay: "theme".into(),
            action: Action::Show,
            data: serde_json::json!({ "css": css }),
        });
    }

    pub fn theme(&self) -> String {
        self.theme.lock().expect("overlay theme lock").clone()
    }

    pub fn send(&self, msg: OverlayMsg) {
        match &self.queue {
            // Through the queue even at 0 ms, so nothing overtakes what still waits there.
            Some(tx) if msg.overlay != "captions" => {
                let delay = Duration::from_millis(self.delay_ms.load(Ordering::Relaxed));
                let _ = tx.send((Instant::now() + delay, msg));
            }
            _ => self.emit(msg),
        }
    }

    fn emit(&self, msg: OverlayMsg) {
        // Toasts are moments, not state: never replayed.
        if msg.action != Action::Update {
            let mut last = self.last.lock().expect("overlay state lock");
            last.insert(msg.overlay.clone(), msg.clone());
        }
        let _ = self.tx.send(msg);
    }
}

/// Everything the server needs from the session.
pub struct Shared {
    pub overlays: Overlays,
    pub state: watch::Receiver<SessionState>,
    pub commands: mpsc::UnboundedSender<Command>,
    pub clock: Arc<Clock>,
    pub token: String,
    /// `<home>/overlays`: files here override the built-in page, script and style.
    pub dir: PathBuf,
    /// `aftercast.toml`: the rehearse page saves overlay places into it.
    pub config: PathBuf,
    /// `aftercast rehearse`: serves the rehearse page at `/rehearse`.
    pub rehearse: Option<Rehearse>,
}

/// Binds `127.0.0.1:port` (0 picks a free port) and returns the address and the server
/// future to spawn.
pub async fn bind(
    port: u16,
    shared: Shared,
) -> std::io::Result<(SocketAddr, impl Future<Output = std::io::Result<()>>)> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let app = Router::new()
        .route("/overlay/assets/{file}", get(asset))
        .route("/overlay/{name}", get(page))
        .route("/overlay/skins/{overlay}/{file}", get(skin_css))
        .route("/overlay/sounds/{skin}/{file}", get(sound_file))
        .route("/overlay/music/{playlist}/{file}", get(media::music_index))
        .route("/overlay/cover/{playlist}/{file}", get(media::music_cover))
        .route("/overlay/upi.svg", get(media::upi_svg))
        .route("/overlay/brb.json", get(media::brb_json))
        .route("/ws", get(ws))
        .route("/api/state", get(state))
        .route("/api/command", post(command))
        .route("/rehearse", get(rehearse::page))
        .route("/rehearse/feed", get(rehearse::feed))
        .route("/rehearse/dvr/index.json", get(dvr::index))
        .route("/rehearse/dvr/file", get(dvr::file))
        .route("/rehearse/chat", post(rehearse::chat))
        .route(
            "/rehearse/frames",
            get(rehearse::frames).post(rehearse::frame),
        )
        .route("/rehearse/order", post(rehearse::order))
        .route(
            "/rehearse/layers",
            get(rehearse::layers).post(rehearse::save_layers),
        )
        .route("/rehearse/pack", post(rehearse::pack))
        .route("/rehearse/options", post(rehearse::options))
        .route("/rehearse/source", post(rehearse::source))
        .route("/rehearse/obs/watch", post(rehearse::watch))
        .route("/rehearse/obs/item", post(rehearse::item))
        .route("/rehearse/obs/watch-source", post(rehearse::watch_source))
        .route("/rehearse/obs/watch-shot", post(rehearse::watch_shot))
        .route("/rehearse/obs/add", post(rehearse::add_obs_source))
        .route("/rehearse/obs/shot", get(rehearse::shot))
        .route("/rehearse/obs/source", post(rehearse::source_edit))
        .route("/rehearse/add-source", post(rehearse::add_source))
        .route("/rehearse/mode", post(rehearse::mode))
        .with_state(Arc::new(shared));
    Ok((addr, async move { axum::serve(listener, app).await }))
}

/// CSS variables for the theme tokens in `[overlay]`, and each overlay's frame: `--x`,
/// `--y` (top left), `--w`, `--h`, `--s` (scale), `--r` (rotation), `--align`,
/// `--text-align` and `--bg` (with its opacity; `--bg-solid` without).
pub fn theme(o: &config::Overlay) -> String {
    let mut css = format!(
        ":root {{ --bg: {}; --bg-solid: {}; --accent: {}; --fg: {}; --muted: {}; --font-mono: \"{}\"; --font-sans: \"{}\"; }}",
        o.bg, o.bg, o.accent, o.fg, o.muted, o.font_mono, o.font_sans
    );
    css.push_str(&format!(
        "\n:root {{ --bar-order: {}; --bar-animate: {}; --bar-marquee: {}; --bar-speed: {}; --bar-extras: {}; --sound: {}; --volume: {}; --sound-skin: {}; }}",
        if o.bar.order.is_empty() {
            "none".into()
        } else {
            o.bar.order.join(" ")
        },
        u8::from(o.bar.animate),
        u8::from(o.bar.marquee),
        o.bar.speed,
        if o.bar.extras.is_empty() {
            "none".into()
        } else {
            o.bar.extras.join(" ")
        },
        u8::from(o.sound.enabled),
        o.sound.volume,
        o.sound.skin
    ));
    // Each segment of the status bar wears its overlay's skin (the pack when it has none).
    let looks: Vec<String> = o
        .frames()
        .into_iter()
        .filter(|(n, _)| config::DOCKABLE.contains(n))
        .map(|(n, f)| {
            let skin = if f.skin.is_empty() { &o.skin } else { &f.skin };
            let skin = if SKINS.contains(&skin.as_str()) {
                skin.as_str()
            } else {
                SKINS[0]
            };
            format!("{n}={skin}")
        })
        .collect();
    css.push_str(&format!("\n:root {{ --seg-skins: {}; }}", looks.join(" ")));
    // Where each segment sits in the bar (its align) and how opaque it is.
    let (mut aligns, mut opacities, mut pins) = (Vec::new(), Vec::new(), Vec::new());
    for (n, f) in o
        .frames()
        .into_iter()
        .filter(|(n, _)| config::DOCKABLE.contains(n))
    {
        let skin = if f.skin.is_empty() { &o.skin } else { &f.skin };
        let f = f.for_skin(skin);
        aligns.push(format!("{n}={}", f.align.name()));
        opacities.push(format!("{n}={}", f.opacity));
        pins.push(format!("{n}={}", u8::from(f.pin)));
    }
    css.push_str(&format!(
        "\n:root {{ --seg-aligns: {}; --seg-opacities: {}; --seg-pins: {}; }}",
        aligns.join(" "),
        opacities.join(" "),
        pins.join(" ")
    ));
    // The overlays shown in the status bar instead of their own frame.
    let docked: Vec<&str> = o
        .frames()
        .into_iter()
        .filter(|(_, f)| f.dock)
        .map(|(n, _)| n)
        .collect();
    css.push_str(&format!(
        "\n:root {{ --docked: {}; }}",
        if docked.is_empty() {
            "none".into()
        } else {
            docked.join(" ")
        }
    ));
    for (name, f) in o.frames() {
        // The frame as the skin it wears changes it (the alert wears its first animation when
        // the pack is not one of its own).
        let worn = if f.skin.is_empty() { &o.skin } else { &f.skin };
        let worn = if name == "alert" && !ALERT_SKINS.contains(&worn.as_str()) {
            ALERT_SKINS[0]
        } else {
            worn.as_str()
        };
        let f = &f.for_skin(worn);
        let (flex, text) = match f.align {
            config::Align::Left => ("flex-start", "left"),
            config::Align::Center => ("center", "center"),
            config::Align::Right => ("flex-end", "right"),
        };
        let color = if f.bg.is_empty() { &o.bg } else { &f.bg };
        let skin = if f.skin.is_empty() { &o.skin } else { &f.skin };
        let (bg, solid) = match config::rgb(color) {
            Some((r, g, b)) => (
                format!("rgb({r} {g} {b} / {}%)", f.opacity),
                format!("rgb({r} {g} {b})"),
            ),
            // `[overlay] bg` is not checked: a named color keeps its own opacity.
            None => (color.clone(), color.clone()),
        };
        let hide = if f.hide_when.is_empty() {
            "none".to_owned()
        } else {
            f.hide_when.join(" ")
        };
        css.push_str(&format!(
            "\n[data-overlay=\"{name}\"] {{ --x: {}px; --y: {}px; --w: {}px; --h: {}px; --s: {}; --r: {}deg; --align: {flex}; --text-align: {text}; --bg: {bg}; --bg-solid: {solid}; --skin: {skin}; --hide: {hide}; --mute: {}; --sound-file: {}; --shuffle: {}; }}",
            f.x,
            f.y,
            f.width,
            f.height,
            f64::from(f.size) / 100.0,
            f.rotate,
            u8::from(f.mute),
            if f.sound.is_empty() { "none" } else { &f.sound },
            u8::from(f.shuffle)
        ));
    }
    css
}

/// Reads the API token from `path`, creating a random one first if missing.
pub fn token(path: &Path) -> std::io::Result<String> {
    if let Ok(token) = fs::read_to_string(path)
        && !token.trim().is_empty()
    {
        return Ok(token.trim().to_owned());
    }
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, &token)?;
    Ok(token)
}

/// A file from `<home>/overlays`, or the built-in one.
fn file(dir: &Path, name: &str, builtin: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap_or_else(|_| builtin.to_owned())
}

async fn page(State(s): State<Arc<Shared>>, UrlPath(name): UrlPath<String>) -> Response {
    if !OVERLAYS.contains(&name.as_str()) {
        return (StatusCode::NOT_FOUND, "no such overlay").into_response();
    }
    let html = file(&s.dir, &format!("{name}.html"), PAGE)
        .replace("{{ name }}", &name)
        .replace("/* theme */", &s.overlays.theme());
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

/// `overlay.js`/`overlay.css` (home copy or built-in), else any file in `<home>/overlays`
/// (images, fonts, video for custom pages).
/// Built-in skins, the same five for every overlay (the first is how it looks without
/// one); any overlay also wears custom skins from `<home>/overlays/skins/<overlay>/<skin>.css`.
pub const SKINS: [&str; 5] = ["nvim", "clean", "broadcast", "glass", "minimal"];

/// The alert's own skins: one animation each.
pub const ALERT_SKINS: [&str; 5] = ["dino", "pacman", "moonwalk", "coins", "neon"];

/// Every overlay's skins: built in, then custom ones found in `<dir>/skins/<overlay>/`.
pub fn skins(dir: &Path) -> BTreeMap<&'static str, Vec<String>> {
    OVERLAYS
        .iter()
        .map(|&name| {
            // The status bar has none: its segments wear their own overlay's skin.
            let built_in: &[&str] = match name {
                "alert" => &ALERT_SKINS,
                "status" => &[],
                _ => &SKINS,
            };
            let mut list: Vec<String> = built_in.iter().map(|&k| k.to_owned()).collect();
            let mut custom: Vec<String> = fs::read_dir(dir.join("skins").join(name))
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|e| {
                    e.file_name()
                        .to_str()?
                        .strip_suffix(".css")
                        .map(str::to_owned)
                })
                .filter(|s| config::skin_name_ok(s) && !list.contains(s))
                .collect();
            custom.sort();
            list.extend(custom);
            (name, list)
        })
        .collect()
}

const SOUND_EXTS: [&str; 5] = ["wav", "mp3", "ogg", "flac", "m4a"];

/// Every sound file to choose from, as `folder/file.mp3` (the BRB's playlist folder is not one).
pub fn sound_files(dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for skin in sound_skins(dir).into_iter().filter(|s| s != "brb") {
        for e in fs::read_dir(dir.join("sounds").join(&skin))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(file) = e.file_name().into_string() else {
                continue;
            };
            let audio = file
                .rsplit_once('.')
                .is_some_and(|(_, x)| SOUND_EXTS.contains(&x.to_ascii_lowercase().as_str()));
            if audio && config::sound_file_ok(&file) {
                found.push(format!("{skin}/{file}"));
            }
        }
    }
    found.sort();
    found
}

/// The sound skins: the folders in `<dir>/sounds/`.
pub fn sound_skins(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(dir.join("sounds"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| config::skin_name_ok(n))
        .collect();
    found.sort();
    found
}

/// `/overlay/sounds/{skin}/index.json` lists a sound skin's files; any other name serves that
/// file from `<home>/overlays/sounds/{skin}/`.
async fn sound_file(
    State(s): State<Arc<Shared>>,
    UrlPath((skin, file)): UrlPath<(String, String)>,
) -> Response {
    let folder = s.dir.join("sounds").join(&skin);
    let ext = file
        .rsplit_once('.')
        .map_or(String::new(), |(_, e)| e.to_ascii_lowercase());
    let plain = file
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !config::skin_name_ok(&skin) || !plain {
        return StatusCode::NOT_FOUND.into_response();
    }
    if file == "index.json" {
        let mut files: Vec<String> = fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                n.rsplit_once('.')
                    .is_some_and(|(_, e)| SOUND_EXTS.contains(&e.to_ascii_lowercase().as_str()))
            })
            .collect();
        files.sort();
        return Json(files).into_response();
    }
    let mime = match ext.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    match fs::read(folder.join(&file)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `/overlay/skins/{overlay}/{skin}.css`: a custom skin's stylesheet, or nothing (a built-in
/// skin lives in overlay.css). Pages ask for their skin's file whenever it changes.
async fn skin_css(
    State(s): State<Arc<Shared>>,
    UrlPath((overlay, file)): UrlPath<(String, String)>,
) -> Response {
    let css = file
        .strip_suffix(".css")
        .filter(|skin| config::skin_name_ok(skin) && OVERLAYS.contains(&overlay.as_str()))
        .and_then(|skin| {
            fs::read_to_string(
                s.dir
                    .join("skins")
                    .join(&overlay)
                    .join(format!("{skin}.css")),
            )
            .ok()
        })
        .unwrap_or_default();
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], css).into_response()
}

async fn asset(State(s): State<Arc<Shared>>, UrlPath(name): UrlPath<String>) -> Response {
    let builtin = match name.as_str() {
        "overlay.js" => Some(SCRIPT),
        "overlay.css" => Some(STYLE),
        "sfx.js" => Some(SFX),
        "alert.js" => Some(ALERT),
        "music.js" => Some(MUSIC),
        "dvr.js" => Some(DVR),
        _ => None,
    };
    let ext = name
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    };
    if let Some(builtin) = builtin {
        return ([(header::CONTENT_TYPE, mime)], file(&s.dir, &name, builtin)).into_response();
    }
    // One plain file name: no separators, no `..`, no hidden files.
    if name.starts_with('.') || name.contains(['/', '\\', ':']) {
        return (StatusCode::NOT_FOUND, "no such asset").into_response();
    }
    match fs::read(s.dir.join(&name)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime)], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such asset").into_response(),
    }
}

async fn ws(State(s): State<Arc<Shared>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| push(socket, s))
}

/// Sends the current overlay state, then every new message, until the page goes away.
async fn push(mut socket: WebSocket, s: Arc<Shared>) {
    let mut rx = s.overlays.tx.subscribe();
    let current: Vec<OverlayMsg> = s
        .overlays
        .last
        .lock()
        .expect("overlay state lock")
        .values()
        .cloned()
        .collect();
    let text = |m: &OverlayMsg| Message::Text(serde_json::to_string(m).expect("serializes").into());
    for msg in &current {
        if socket.send(text(msg)).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(msg) => if socket.send(text(&msg)).await.is_err() { return },
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(_)) => {} // pages only listen
                _ => return,
            },
        }
    }
}

async fn state(State(s): State<Arc<Shared>>) -> Json<SessionState> {
    Json(s.state.borrow().clone())
}

async fn command(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let token = headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok());
    if token != Some(s.token.as_str()) {
        return (
            StatusCode::UNAUTHORIZED,
            "missing or wrong X-Aftercast-Token",
        )
            .into_response();
    }
    // Time-stamped commands without a time happened now.
    if let Some(obj) = body.as_object_mut()
        && matches!(
            obj.get("cmd").and_then(Value::as_str),
            Some("checkpoint" | "mark" | "note")
        )
        && !obj.contains_key("pressed_at")
    {
        obj.insert("pressed_at".into(), s.clock.now_ts().to_string().into());
    }
    match serde_json::from_value::<Command>(body) {
        Ok(cmd) => match s.commands.send(cmd) {
            Ok(()) => StatusCode::ACCEPTED.into_response(),
            Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "session ended").into_response(),
        },
        Err(e) => (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use futures_util::StreamExt;
    use serde_json::json;

    use super::*;

    async fn start(dir: &Path) -> (SocketAddr, Overlays, mpsc::UnboundedReceiver<Command>) {
        let overlays = Overlays::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let shared = Shared {
            overlays: overlays.clone(),
            state: watch::channel(SessionState::default()).1,
            commands: tx,
            clock: Arc::new(Clock::new()),
            token: "secret".into(),
            dir: dir.to_path_buf(),
            config: dir.join("aftercast.toml"),
            rehearse: None,
        };
        let (addr, server) = bind(0, shared).await.unwrap();
        tokio::spawn(server);
        (addr, overlays, rx)
    }

    fn show_question() -> OverlayMsg {
        OverlayMsg {
            overlay: "question".into(),
            action: Action::Show,
            data: json!({ "author": "bob", "text": "why rust?" }),
        }
    }

    #[tokio::test]
    async fn ws_replays_state_and_pushes_within_100ms() {
        let dir = tempfile::tempdir().unwrap();
        let (addr, overlays, _rx) = start(dir.path()).await;
        overlays.send(OverlayMsg {
            overlay: "now".into(),
            action: Action::Show,
            data: json!({ "quest": "Parser" }),
        });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let first = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&first).unwrap()["overlay"],
            "now"
        );

        let sent = Instant::now();
        overlays.send(show_question());
        let msg = tokio::time::timeout(Duration::from_millis(100), ws.next())
            .await
            .expect("pushed within 100 ms")
            .unwrap()
            .unwrap();
        let msg: OverlayMsg = serde_json::from_str(&msg.into_text().unwrap()).unwrap();
        assert_eq!(msg, show_question());
        assert!(sent.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn pages_api_token_and_commands() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("overlay.css"), "/* mine */").unwrap();
        let (addr, _overlays, mut rx) = start(dir.path()).await;
        let http = reqwest::Client::new();
        let get = |path: &str| http.get(format!("http://{addr}{path}")).send();

        let page = get("/overlay/question")
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(page.contains("data-overlay=\"question\""), "{page}");
        assert!(page.contains("--accent: #FF5555"), "{page}");
        assert_eq!(get("/overlay/nope").await.unwrap().status(), 404);
        let css = get("/overlay/assets/overlay.css").await.unwrap();
        assert_eq!(
            css.text().await.unwrap(),
            "/* mine */",
            "home override wins"
        );
        fs::write(dir.path().join("logo.png"), b"fakepng").unwrap();
        let logo = get("/overlay/assets/logo.png").await.unwrap();
        assert_eq!(logo.headers()["content-type"], "image/png");
        assert_eq!(&logo.bytes().await.unwrap()[..], b"fakepng");
        for bad in ["nope.png", "..%5Csecret", "..%2Fsecret", ".hidden"] {
            let status = get(&format!("/overlay/assets/{bad}"))
                .await
                .unwrap()
                .status();
            assert_eq!(status, 404, "{bad}");
        }
        let state: Value = get("/api/state").await.unwrap().json().await.unwrap();
        assert_eq!(state["brb"], false);

        let post = |token: &str, body: Value| {
            http.post(format!("http://{addr}/api/command"))
                .header(TOKEN_HEADER, token)
                .json(&body)
                .send()
        };
        let mark = json!({ "cmd": "mark", "by": "palette" });
        assert_eq!(post("wrong", mark.clone()).await.unwrap().status(), 401);
        assert_eq!(
            post("secret", json!({ "cmd": "x" }))
                .await
                .unwrap()
                .status(),
            400
        );
        assert_eq!(post("secret", mark).await.unwrap().status(), 202);
        assert!(matches!(
            rx.recv().await,
            Some(Command::Mark { label: None, .. })
        ));
    }

    #[tokio::test]
    async fn the_editor_edits_obs_sources_through_commands() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (feed_tx, feed_rx) = watch::channel(Feed::default());
        let shared = Shared {
            overlays: Overlays::default(),
            state: watch::channel(SessionState::default()).1,
            commands: tx,
            clock: Arc::new(Clock::new()),
            token: "secret".into(),
            dir: dir.path().to_path_buf(),
            config: dir.path().join("aftercast.toml"),
            rehearse: Some(Rehearse::new(feed_rx, mpsc::unbounded_channel().0)),
        };
        let (addr, server) = bind(0, shared).await.unwrap();
        tokio::spawn(server);
        let http = reqwest::Client::new();
        let base = format!("http://{addr}/rehearse");
        let watch_it = http
            .post(format!("{base}/obs/watch"))
            .json(&json!({ "scene": "Code" }))
            .send()
            .await
            .unwrap();
        assert_eq!(watch_it.status(), 204);
        assert_eq!(
            rx.recv().await,
            Some(Command::WatchScene {
                scene: "Code".into()
            })
        );
        let edit = http
            .post(format!("{base}/obs/item"))
            .json(&json!({ "scene": "Code", "id": 7, "x": 12.5, "locked": true, "crop": [1, 2, 3, 4] }))
            .send()
            .await
            .unwrap();
        assert_eq!(edit.status(), 204);
        let Some(Command::EditItem { scene, id, edit }) = rx.recv().await else {
            panic!("an EditItem command");
        };
        assert_eq!((scene.as_str(), id), ("Code", 7));
        assert_eq!(
            (edit.x, edit.y, edit.locked, edit.crop),
            (Some(12.5), None, Some(true), Some([1, 2, 3, 4]))
        );
        // A new source is added the same way.
        let a = http
            .post(format!("{base}/obs/add"))
            .json(&json!({ "scene": "Code", "kind": "text_gdiplus_v3", "name": "Hello", "settings": { "text": "hi" } }))
            .send()
            .await
            .unwrap();
        assert_eq!(a.status(), 204);
        let Some(Command::AddSource {
            scene,
            kind,
            name,
            settings,
        }) = rx.recv().await
        else {
            panic!("an AddSource command");
        };
        assert_eq!(
            (scene.as_str(), kind.as_str(), name.as_str()),
            ("Code", "text_gdiplus_v3", "Hello")
        );
        assert_eq!(settings, json!({ "text": "hi" }));
        // A source's settings and filters are watched and edited the same way.
        let w = http
            .post(format!("{base}/obs/watch-source"))
            .json(&json!({ "source": "Cam" }))
            .send()
            .await
            .unwrap();
        assert_eq!(w.status(), 204);
        assert_eq!(
            rx.recv().await,
            Some(Command::WatchSource {
                source: "Cam".into()
            })
        );
        let e = http
            .post(format!("{base}/obs/source"))
            .json(&json!({ "source": "Cam", "settings": { "active": false }, "filter": "Key", "filter_enabled": false }))
            .send()
            .await
            .unwrap();
        assert_eq!(e.status(), 204);
        let Some(Command::EditSource { source, edit }) = rx.recv().await else {
            panic!("an EditSource command");
        };
        assert_eq!(
            (source.as_str(), edit.filter.as_str(), edit.filter_enabled),
            ("Cam", "Key", Some(false))
        );
        assert_eq!(edit.settings, Some(json!({ "active": false })));
        // The sources OBS listed come back in the feed.
        feed_tx.send_replace(Feed {
            items: Arc::new(ac_core::obs::SceneDetail {
                scene: "Code".into(),
                canvas: [2880.0, 1620.0],
                items: vec![ac_core::obs::ItemDetail {
                    id: 7,
                    source: "Cam".into(),
                    ..Default::default()
                }],
                input_kinds: vec![],
            }),
            ..Feed::default()
        });
        let feed: Value = http
            .get(format!("{base}/feed"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(feed["items"]["items"][0]["source"], "Cam");
        assert_eq!(feed["items"]["canvas"], json!([2880.0, 1620.0]));
    }

    #[tokio::test]
    async fn rehearse_saves_overlay_frames_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("aftercast.toml");
        fs::write(&config, "# mine\n").unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let shared = Shared {
            overlays: Overlays::default(),
            state: watch::channel(SessionState::default()).1,
            commands: tx,
            clock: Arc::new(Clock::new()),
            token: "secret".into(),
            dir: dir.path().to_path_buf(),
            config: config.clone(),
            rehearse: Some(Rehearse::new(
                watch::channel(Feed::default()).1,
                mpsc::unbounded_channel().0,
            )),
        };
        let (addr, server) = bind(0, shared).await.unwrap();
        tokio::spawn(server);
        let url = format!("http://{addr}/rehearse/frames");
        let http = reqwest::Client::new();
        let post = |body: Value| http.post(&url).json(&body).send();
        let get = || async {
            let v: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
            v["frames"].clone()
        };

        let frame = json!({ "name": "keys", "x": -50, "y": 9999, "width": 300, "height": 700,
                            "size": 80, "rotate": -12, "align": "right", "bg": "#112233",
                            "opacity": 60 });
        assert_eq!(post(frame).await.unwrap().status(), 204);
        assert_eq!(rx.recv().await, Some(Command::Reload));
        assert_eq!(
            get().await["keys"],
            json!({ "x": -50, "y": 2160, "width": 300, "height": 700, "size": 80, "rotate": -12,
                    "align": "right", "bg": "#112233", "opacity": 60, "skin": "",
                    "dock": false, "hide_when": [], "mute": false, "sound": "", "shuffle": true, "pin": false, "skins": {} })
        );
        // Position and look can be saved for one skin: the overlay's own frame stays, the theme
        // gives the skin worn its changes, and a reset takes only that skin's back.
        let r = post(
            json!({ "name": "keys", "for_skin": "clean", "size": 150, "x": 7,
                              "mute": true }),
        );
        assert_eq!(r.await.unwrap().status(), 204);
        let keys = get().await["keys"].clone();
        assert_eq!(
            (keys["size"].clone(), keys["x"].clone()),
            (json!(80), json!(-50))
        );
        assert_eq!(keys["skins"]["clean"], json!({ "size": 150, "x": 7 }));
        assert_eq!(keys["mute"], true);
        let theme = config::Config::load(&config).unwrap().overlay;
        let css = theme_for(&theme, "clean");
        assert!(
            css.contains("--x: 7px") && css.contains("--s: 1.5") && css.contains("--mute: 1"),
            "{css}"
        );
        let r = post(json!({ "name": "keys", "for_skin": "clean", "reset": true }));
        assert_eq!(r.await.unwrap().status(), 204);
        assert_eq!(get().await["keys"]["skins"], json!({}));
        let r = post(json!({ "name": "keys", "for_skin": "../x", "size": 100 }));
        assert_eq!(r.await.unwrap().status(), 400);
        let r = post(json!({ "name": "keys", "hide_when": ["question", "toast"] }));
        assert_eq!(r.await.unwrap().status(), 204);
        assert_eq!(
            get().await["keys"]["hide_when"],
            json!(["question", "toast"])
        );
        let r = post(json!({ "name": "keys", "dock": true }));
        assert_eq!(r.await.unwrap().status(), 400, "keys cannot dock");
        assert_eq!(get().await["now"]["width"], config::FRAMES[0].3);
        assert!(fs::read_to_string(&config).unwrap().starts_with("# mine"));

        // One bad field and nothing is saved.
        let bad = json!({ "name": "keys", "x": 0, "bg": "red" });
        assert_eq!(post(bad).await.unwrap().status(), 400);
        assert_eq!(get().await["keys"]["x"], -50, "nothing saved");
        let bad = post(json!({ "name": "nope", "x": 0 }));
        assert_eq!(bad.await.unwrap().status(), 400);

        let r = post(json!({ "name": "keys", "reset": true }));
        assert_eq!(r.await.unwrap().status(), 204);
        assert_eq!(get().await["keys"]["x"], config::FRAMES[6].1);

        // Layer order: saved, reloaded, refused when it names an unknown overlay.
        let order_url = format!("http://{addr}/rehearse/order");
        let r = http
            .post(&order_url)
            .json(&json!({ "order": ["keys", "toast"] }))
            .send();
        assert_eq!(r.await.unwrap().status(), 204);
        let all: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(all["order"][0], "keys");
        assert_eq!(all["order"].as_array().unwrap().len(), 11);
        let r = http
            .post(&order_url)
            .json(&json!({ "order": ["nope"] }))
            .send();
        assert_eq!(r.await.unwrap().status(), 400);

        // Layer folders: empty before the first save, kept in the home, only an object accepted.
        let layers_url = format!("http://{addr}/rehearse/layers");
        let got: Value = http
            .get(&layers_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(got, json!({}));
        let doc = json!({ "folders": { "Scene": { "list": [{ "id": "f1", "name": "Camera" }] } } });
        let r = http.post(&layers_url).json(&doc).send();
        assert_eq!(r.await.unwrap().status(), 204);
        let got: Value = http
            .get(&layers_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(got, doc);
        let r = http.post(&layers_url).json(&json!([1])).send();
        assert_eq!(r.await.unwrap().status(), 400);

        // Skins: a pack, an overlay's own, custom files listed and served, odd names refused.
        let skins = dir.path().join("skins/keys");
        fs::create_dir_all(&skins).unwrap();
        fs::write(skins.join("neon.css"), ".key { color: lime; }").unwrap();
        fs::write(skins.join("Bad Name.css"), "").unwrap();
        let pack_url = format!("http://{addr}/rehearse/pack");
        let r = http
            .post(&pack_url)
            .json(&json!({ "skin": "clean" }))
            .send();
        assert_eq!(r.await.unwrap().status(), 204);
        let r = http.post(&pack_url).json(&json!({ "skin": "../x" })).send();
        assert_eq!(r.await.unwrap().status(), 400);
        let r = post(json!({ "name": "keys", "skin": "neon" }));
        assert_eq!(r.await.unwrap().status(), 204);
        let all: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(all["pack"], "clean");
        assert_eq!(all["frames"]["keys"]["skin"], "neon");
        assert_eq!(
            all["skins"]["keys"],
            json!(["nvim", "clean", "broadcast", "glass", "minimal", "neon"])
        );
        assert_eq!(all["skins"]["now"].as_array().unwrap().len(), 5);
        assert!(all["packs"].as_array().unwrap().contains(&json!("neon")));
        for (path, want) in [
            ("keys/neon.css", ".key { color: lime; }"),
            ("keys/clean.css", ""),
            ("keys/..%2F..%2Faftercast.toml.css", ""),
            ("nope/neon.css", ""),
        ] {
            let u = format!("http://{addr}/overlay/skins/{path}");
            let got = http.get(u).send().await.unwrap().text().await.unwrap();
            assert_eq!(got, want, "{path}");
        }

        // Sound skins: listed, their files served, odd names refused.
        let sounds = dir.path().join("sounds/mine");
        fs::create_dir_all(&sounds).unwrap();
        fs::write(sounds.join("coin.wav"), b"RIFF").unwrap();
        fs::write(sounds.join("notes.txt"), b"no").unwrap();
        let all: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(all["sound_skins"], json!(["mine"]));
        assert_eq!(all["sound"]["skin"], "default");
        let base = format!("http://{addr}/overlay/sounds");
        let list: Value = http
            .get(format!("{base}/mine/index.json"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(list, json!(["coin.wav"]));
        let got = http
            .get(format!("{base}/mine/coin.wav"))
            .send()
            .await
            .unwrap();
        assert_eq!(got.status(), 200);
        assert_eq!(got.bytes().await.unwrap().as_ref(), b"RIFF");
        for bad in [
            "mine/notes.txt",
            "mine/..%2Fcoin.wav",
            "Bad%20Name/coin.wav",
            "mine/missing.wav",
        ] {
            let got = http.get(format!("{base}/{bad}")).send().await.unwrap();
            assert_eq!(got.status(), 404, "{bad}");
        }
        let r = http
            .post(format!("http://{addr}/rehearse/options"))
            .json(&json!({ "sound_skin": "mine" }))
            .send();
        assert_eq!(r.await.unwrap().status(), 204);
        let all: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(all["sound"]["skin"], "mine");

        // The BRB screen: playlists listed with their tracks, the UPI QR code made from the config.
        let list = dir.path().join("sounds/brb");
        fs::create_dir_all(&list).unwrap();
        fs::write(list.join("b side.mp3"), b"not really audio").unwrap();
        fs::write(list.join("a song.mp3"), b"not really audio").unwrap();
        fs::write(list.join("cover.png"), b"PNG").unwrap();
        let all: Value = http.get(&url).send().await.unwrap().json().await.unwrap();
        assert_eq!(all["tracks"], json!(2));
        let music = format!("http://{addr}/overlay/music/brb");
        let tracks: Value = http
            .get(format!("{music}/index.json"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(tracks[0]["file"], "a song.mp3");
        assert_eq!(
            tracks[0]["title"], "a song",
            "the file's name when the tags say nothing"
        );
        assert_eq!(tracks[1]["file"], "b side.mp3");
        let got = http
            .get(format!("{music}/a%20song.mp3"))
            .send()
            .await
            .unwrap();
        assert_eq!(got.status(), 200);
        let art = http
            .get(format!("http://{addr}/overlay/cover/brb/a%20song.mp3"))
            .send()
            .await
            .unwrap();
        assert_eq!(art.status(), 200);
        assert_eq!(art.bytes().await.unwrap().as_ref(), b"PNG");
        assert_eq!(
            http.get(format!("{music}/..%2Fx.mp3"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        let none = http
            .get(format!("http://{addr}/overlay/upi.svg"))
            .send()
            .await
            .unwrap();
        assert_eq!(none.status(), 404, "no UPI id yet");
        let mut text = fs::read_to_string(&config).unwrap();
        text.push_str(
            "
[vars.support]
upi = \"me@bank\"
",
        );
        fs::write(&config, text).unwrap();
        let qr = http
            .get(format!("http://{addr}/overlay/upi.svg"))
            .send()
            .await
            .unwrap();
        assert_eq!(qr.status(), 200);
        assert!(qr.text().await.unwrap().starts_with("<svg"));
    }

    /// The theme css of `o` with `skin` as the pack every overlay wears.
    fn theme_for(o: &config::Overlay, skin: &str) -> String {
        let mut o = o.clone();
        o.skin = skin.into();
        theme(&o)
    }

    #[test]
    fn theme_frames_every_overlay() {
        let mut o = config::Overlay::default();
        o.keys.0.bg = "#102030".into();
        o.keys.0.opacity = 40;
        o.keys.0.rotate = -5;
        let css = theme(&o);
        let keys = css.lines().find(|l| l.contains("\"keys\"")).unwrap();
        assert!(
            keys.contains("--x: 1400px; --y: 120px; --w: 480px; --h: 520px"),
            "{keys}"
        );
        assert!(keys.contains("--r: -5deg; --align: center"), "{keys}");
        assert!(keys.contains("--skin: nvim;"), "the pack: {keys}");
        assert!(
            keys.contains("--bg: rgb(16 32 48 / 40%); --bg-solid: rgb(16 32 48)"),
            "{keys}"
        );
        assert!(
            css.contains("--bg: rgb(48 48 48 / 100%)"),
            "theme color for the rest"
        );
        assert_eq!(css.matches("--s:").count(), OVERLAYS.len());
    }

    #[test]
    fn token_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/api-token");
        let a = token(&path).unwrap();
        assert_eq!(a.len(), 48);
        assert_eq!(token(&path).unwrap(), a);
    }
}
