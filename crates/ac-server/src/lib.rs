//! Overlay and local API server (specs/09-streambot.md, "Overlay server"): axum on
//! `127.0.0.1` only. Overlay pages for OBS browser sources, a WebSocket that pushes
//! [`OverlayMsg`]s, `GET /api/state` and token-protected `POST /api/command`.

mod rehearse;

pub use rehearse::{Feed, Rehearse};

use std::collections::BTreeMap;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// Built-in overlays.
pub const OVERLAYS: [&str; 7] = [
    "now", "question", "toast", "brb", "latency", "chapters", "keys",
];
const PAGE: &str = include_str!("../overlays/overlay.html");
const SCRIPT: &str = include_str!("../overlays/overlay.js");
const STYLE: &str = include_str!("../overlays/overlay.css");
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
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            tx: broadcast::channel(256).0,
            last: Arc::default(),
        }
    }
}

impl Overlays {
    pub fn send(&self, msg: OverlayMsg) {
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
    /// `:root { --bg: ... }` from `[overlay]`.
    pub theme: String,
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
        .route("/ws", get(ws))
        .route("/api/state", get(state))
        .route("/api/command", post(command))
        .route("/rehearse", get(rehearse::page))
        .route("/rehearse/feed", get(rehearse::feed))
        .route("/rehearse/chat", post(rehearse::chat))
        .with_state(Arc::new(shared));
    Ok((addr, async move { axum::serve(listener, app).await }))
}

/// CSS variables for the theme tokens in `[overlay]`.
pub fn theme(o: &config::Overlay) -> String {
    format!(
        ":root {{ --bg: {}; --accent: {}; --fg: {}; --muted: {}; --font-mono: \"{}\"; --font-sans: \"{}\"; }}",
        o.bg, o.accent, o.fg, o.muted, o.font_mono, o.font_sans
    )
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
        .replace("/* theme */", &s.theme);
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

/// `overlay.js`/`overlay.css` (home copy or built-in), else any file in `<home>/overlays`
/// (images, fonts, video for custom pages).
async fn asset(State(s): State<Arc<Shared>>, UrlPath(name): UrlPath<String>) -> Response {
    let builtin = match name.as_str() {
        "overlay.js" => Some(SCRIPT),
        "overlay.css" => Some(STYLE),
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
            theme: theme(&config::Overlay::default()),
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

    #[test]
    fn token_is_created_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/api-token");
        let a = token(&path).unwrap();
        assert_eq!(a.len(), 48);
        assert_eq!(token(&path).unwrap(), a);
    }
}
