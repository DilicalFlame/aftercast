//! Overlay and local API server (specs/09-streambot.md, "Overlay server"): axum on
//! `127.0.0.1` only. Overlay pages for OBS browser sources, a WebSocket that pushes
//! [`OverlayMsg`]s, `GET /api/state` and token-protected `POST /api/command`.

mod dvr;
mod media;
pub mod plugins;
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
use axum::extract::{Path as UrlPath, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
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
const SFX: &str = include_str!("../overlays/sfx.js");
const DVR: &str = include_str!("../overlays/dvr.js");
const SDK: &str = include_str!("../overlays/sdk.js");
const AUDIO: &str = include_str!("../overlays/audio.js");
const UI: &str = include_str!("../overlays/ui.js");
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
    /// `{ plugin id: { params, skin, enabled } }`, sent with the theme.
    theme_extra: Arc<Mutex<Value>>,
    /// `[overlay.<name>] hide_when`: overlay -> the overlays it hides for.
    rules: Arc<Mutex<BTreeMap<String, Vec<String>>>>,
    /// Overlays on screen: until a hide (`None`), or until a time for the ones that never hide.
    active: Arc<Mutex<BTreeMap<String, Option<std::time::Instant>>>>,
    /// The overlays hidden right now, as last sent.
    suppressed: Arc<Mutex<Vec<String>>>,
}

impl Default for Overlays {
    fn default() -> Self {
        Self {
            tx: broadcast::channel(256).0,
            last: Arc::default(),
            delay_ms: Arc::default(),
            queue: None,
            theme: Arc::new(Mutex::new(theme(&config::Overlay::default()))),
            theme_extra: Arc::new(Mutex::new(Value::Object(Default::default()))),
            rules: Arc::default(),
            active: Arc::default(),
            suppressed: Arc::default(),
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

    /// New colors and places (and plugin settings): open pages restyle at once (not delayed),
    /// new pages load with them.
    pub fn set_theme(&self, css: String, plugins: Value) {
        *self.theme.lock().expect("overlay theme lock") = css.clone();
        *self.theme_extra.lock().expect("overlay theme lock") = plugins.clone();
        self.emit(OverlayMsg {
            overlay: "theme".into(),
            action: Action::Show,
            data: serde_json::json!({ "css": css, "plugins": plugins }),
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

    /// How long overlays that never send a hide count as on screen.
    fn lately(name: &str) -> Option<Duration> {
        match name {
            "toast" => Some(Duration::from_millis(5500)),
            "captions" => Some(Duration::from_millis(4500)),
            "keys" => Some(Duration::from_millis(3000)),
            "alert" => Some(Duration::from_millis(7000)),
            _ => None,
        }
    }

    /// Which overlays are hidden because one they are set to hide for is on screen. Sent to the
    /// pages as the `__suppress` message whenever the set changes.
    pub fn suppress_tick(&self) {
        let now = std::time::Instant::now();
        let on: Vec<String> = {
            let active = self.active.lock().expect("overlay active lock");
            active
                .iter()
                .filter(|(_, until)| until.is_none_or(|t| t > now))
                .map(|(n, _)| n.clone())
                .collect()
        };
        let mut hidden: Vec<String> = self
            .rules
            .lock()
            .expect("overlay rules lock")
            .iter()
            .filter(|(_, by)| by.iter().any(|b| on.contains(b)))
            .map(|(n, _)| n.clone())
            .collect();
        hidden.sort();
        let mut last = self.suppressed.lock().expect("overlay suppressed lock");
        if *last != hidden {
            *last = hidden.clone();
            drop(last);
            self.emit(OverlayMsg {
                overlay: "__suppress".into(),
                action: Action::Show,
                data: serde_json::json!({ "names": hidden }),
            });
        }
    }

    pub fn set_rules(&self, rules: BTreeMap<String, Vec<String>>) {
        *self.rules.lock().expect("overlay rules lock") = rules;
    }

    fn emit(&self, msg: OverlayMsg) {
        if !msg.overlay.starts_with("__") && msg.overlay != "theme" {
            let mut active = self.active.lock().expect("overlay active lock");
            if msg.action == Action::Hide {
                active.remove(&msg.overlay);
            } else {
                let until = Self::lately(&msg.overlay).map(|d| std::time::Instant::now() + d);
                active.insert(msg.overlay.clone(), until);
            }
        }
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
    /// `<home>/plugins`: one folder per installed overlay plugin.
    pub plugins: PathBuf,
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
    let (plugins_dir, config_file, rehearsing) = (
        shared.plugins.clone(),
        shared.config.clone(),
        shared.rehearse.is_some(),
    );
    let watch_overlays = shared.overlays.clone();
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    let app = Router::new()
        .route("/sdk/{file}", get(sdk))
        .route("/plugin/{id}/{*path}", get(plugin_file))
        .route("/api/plugins", get(plugins_json))
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
        .route("/rehearse/plugin", post(rehearse::plugin_param))
        .route("/rehearse/plugin-parts", post(rehearse::plugin_parts))
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
        .with_state(Arc::new(shared))
        .layer(axum::middleware::from_fn(read_cors));
    let tick_overlays = shared_overlays_for_tick(&watch_overlays);
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            tick_overlays.suppress_tick();
        }
    });
    // Plugins come and go while the server runs: the theme (their frames and settings) is
    // sent again when the set changes, and while rehearsing a saved file of a plugin reloads
    // its pages.
    tokio::spawn(async move {
        let overlays = watch_overlays;
        let mut seen: BTreeMap<String, u128> = BTreeMap::new();
        let mut first = true;
        loop {
            tokio::time::sleep(Duration::from_millis(900)).await;
            let found = plugins::scan(&plugins_dir);
            let before: Vec<String> = seen.keys().cloned().collect();
            let ids: Vec<String> = found.iter().map(|p| p.id.clone()).collect();
            seen.retain(|k, _| ids.contains(k));
            for p in &found {
                let at = plugins::newest(&p.dir);
                let was = seen.insert(p.id.clone(), at);
                if rehearsing && !first && was.is_some_and(|w| w != at) {
                    overlays.emit(OverlayMsg {
                        overlay: "__plugin".into(),
                        action: Action::Update,
                        data: serde_json::json!({ "id": p.id }),
                    });
                }
            }
            if !first
                && ids != before
                && let Ok(config) = config::Config::load(&config_file)
            {
                apply_theme(&overlays, &config, &plugins_dir);
            }
            first = false;
        }
    });
    Ok((addr, async move { axum::serve(listener, app).await }))
}

/// CSS variables for the theme tokens in `[overlay]`, and each overlay's frame: `--x`,
/// `--y` (top left), `--w`, `--h`, `--s` (scale), `--r` (rotation), `--align`,
/// `--text-align` and `--bg` (with its opacity; `--bg-solid` without).
pub fn theme(o: &config::Overlay) -> String {
    theme_with(o, &[])
}

/// [`theme`] plus the frames of plugin overlays (by id).
pub fn theme_with(o: &config::Overlay, plugin_frames: &[(String, config::Frame)]) -> String {
    let mut css = format!(
        ":root {{ --bg: {}; --bg-solid: {}; --accent: {}; --fg: {}; --muted: {}; --font-mono: \"{}\"; --font-sans: \"{}\"; }}",
        o.bg, o.bg, o.accent, o.fg, o.muted, o.font_mono, o.font_sans
    );
    css.push_str(&format!(
        "\n:root {{ --bar-order: {}; --bar-animate: {}; --bar-marquee: {}; --bar-speed: {}; --bar-extras: {}; --sound: {}; --volume: {}; --sound-skin: {}; --brb-fade: {}; --brb-clip: {}; }}",
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
        o.sound.skin,
        o.sound.brb_fade,
        o.sound.brb_clip
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
        css.push_str(&frame_css(o, name, f));
    }
    for (name, f) in plugin_frames {
        css.push_str(&frame_css(o, name, f));
    }
    css
}

/// One overlay's frame as CSS variables on `[data-overlay="<name>"]`.
fn frame_css(o: &config::Overlay, name: &str, f: &config::Frame) -> String {
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
    format!(
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

async fn page(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    UrlPath(name): UrlPath<String>,
) -> Response {
    // An installed plugin: its entry in a shell (a plugin named like a built-in replaces it).
    if let Some(p) = plugins::scan(&s.plugins).into_iter().find(|p| p.id == name) {
        return plugin_page(&s, &p, host_of(&headers));
    }
    (StatusCode::NOT_FOUND, "no such overlay").into_response()
}

/// The shell of a plugin: the theme as of now (the plugin's own frame and settings read from
/// the config file, so a page never trails an edit), and what the SDK boots from.
fn plugin_page(s: &Shared, p: &plugins::Plugin, host: &str) -> Response {
    let config = config::Config::load(&s.config).unwrap_or_default();
    let cfg = config.plugins.get(&p.id);
    let mut css = s.overlays.theme();
    if !p.replaces() {
        css.push_str(&frame_css(&config.overlay, &p.id, &p.frame_in_effect(cfg)));
    }
    let boot = serde_json::json!({
        "id": p.id, "api": plugins::API, "name": p.name, "version": p.version,
        "params": p.params_in_effect(cfg), "skin": cfg.map_or("", |c| c.skin.as_str()),
        "skins": plugin_skins(s, p),
        "custom_skins": custom_skins(s, p),
        "pack": config.overlay.skin,
        "scenarios": p.scenarios.iter().map(|sc| serde_json::json!({ "name": sc.name, "state": sc.state })).collect::<Vec<_>>(),
        "volume": config.overlay.sound.volume,
        "avatar": config.overlay.avatar,
        "parts": p.parts.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        "part_positions": cfg.map_or(serde_json::json!({}), |c| serde_json::to_value(&c.parts).unwrap_or_default()),
    });
    match plugins::shell(p, &css, &boot) {
        Some(html) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    plugins::csp(p, host).as_str(),
                ),
            ],
            html,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "the plugin's entry is missing").into_response(),
    }
}

/// A plugin's skins: its own, then any custom skin stylesheet found in
/// `<home>/overlays/skins/<id>/` (the way built-in overlays could always be given more skins).
fn plugin_skins(s: &Shared, p: &plugins::Plugin) -> Vec<String> {
    let mut list: Vec<String> = p.skins.iter().map(|(id, _)| id.clone()).collect();
    list.extend(custom_skins(s, p));
    list
}

fn custom_skins(s: &Shared, p: &plugins::Plugin) -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(s.dir.join("skins").join(&p.id))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".css")
                .map(str::to_owned)
        })
        .filter(|k| config::skin_name_ok(k) && !p.skins.iter().any(|(id, _)| id == k))
        .collect();
    found.sort();
    found
}

/// `/plugin/{id}/{*path}`: a file of an installed plugin.
async fn plugin_file(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    UrlPath((id, path)): UrlPath<(String, String)>,
) -> Response {
    let Some(p) = plugins::scan(&s.plugins).into_iter().find(|p| p.id == id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match plugins::read_file(&p, &path) {
        Some(bytes) => (
            [
                (header::CONTENT_TYPE, plugins::mime(&path)),
                (header::CACHE_CONTROL, "no-store"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    plugins::csp(&p, host_of(&headers)).as_str(),
                ),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `/sdk/aftercast.js` and `/sdk/audio.js` (a copy in `<home>/overlays/` wins, for working on them).
async fn sdk(State(s): State<Arc<Shared>>, UrlPath(name): UrlPath<String>) -> Response {
    let (file_name, builtin) = match name.as_str() {
        "aftercast.js" => ("sdk.js", SDK),
        "audio.js" => ("audio.js", AUDIO),
        "ui.js" => ("ui.js", UI),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        file(&s.dir, file_name, builtin),
    )
        .into_response()
}

/// `/api/plugins`: every installed plugin's manifest and the settings in effect.
async fn plugins_json(State(s): State<Arc<Shared>>) -> Response {
    let config = config::Config::load(&s.config).unwrap_or_default();
    let list: Vec<Value> = plugins::scan(&s.plugins)
        .iter()
        .map(|p| {
            let cfg = config.plugins.get(&p.id);
            let mut m = p.manifest_json();
            m["params_in_effect"] = p.params_in_effect(cfg);
            m["skin"] = cfg.map_or("", |c| c.skin.as_str()).into();
            m
        })
        .collect();
    Json(list).into_response()
}

/// Sends the theme (colors, every overlay's frame, plugin frames and settings) to every page.
pub fn apply_theme(overlays: &Overlays, config: &config::Config, plugins_dir: &Path) {
    overlays.set_rules(
        config
            .overlay
            .frames()
            .into_iter()
            .filter(|(_, f)| !f.hide_when.is_empty())
            .map(|(n, f)| (n.to_owned(), f.hide_when.clone()))
            .collect(),
    );
    let found = plugins::scan(plugins_dir);
    overlays.set_theme(
        theme_with(&config.overlay, &plugins::frames(config, &found)),
        plugins::runtime(config, &found),
    );
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
        .filter(|skin| config::skin_name_ok(skin) && config::skin_name_ok(&overlay))
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
        "sfx.js" => Some(SFX),
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

/// Who a page is: the host's own pages and other programs see everything; a plugin page (a
/// sandboxed document, whose requests say `Origin: null`) sees the streams its manifest lists.
async fn ws(
    State(s): State<Arc<Shared>>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    upgrade: WebSocketUpgrade,
) -> Response {
    let sandboxed = headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|o| o == "null");
    let view = sandboxed.then(|| {
        let id = uri
            .query()
            .and_then(|q| q.split('&').find_map(|p| p.strip_prefix("plugin=")))
            .unwrap_or("");
        let reads = plugins::scan(&s.plugins)
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.caps.reads)
            .unwrap_or_default();
        (id.to_owned(), reads)
    });
    upgrade.on_upgrade(move |socket| push(socket, s, view))
}

/// What a sandboxed plugin page may see of `msg`: its streams, the theme (with only its own
/// settings) and the reload signal for itself.
fn visible_to(view: &Option<(String, Vec<String>)>, msg: &OverlayMsg) -> Option<OverlayMsg> {
    let Some((id, reads)) = view else {
        return Some(msg.clone());
    };
    match msg.overlay.as_str() {
        "theme" => {
            let mut m = msg.clone();
            if let Some(all) = m.data.get_mut("plugins").and_then(Value::as_object_mut) {
                all.retain(|k, _| k == id);
            }
            Some(m)
        }
        "__plugin" => (msg.data["id"] == *id).then(|| msg.clone()),
        "__suppress" => Some(msg.clone()),
        name => reads.iter().any(|r| r == name).then(|| msg.clone()),
    }
}

/// Sends the current overlay state, then every new message, until the page goes away.
async fn push(mut socket: WebSocket, s: Arc<Shared>, view: Option<(String, Vec<String>)>) {
    let mut rx = s.overlays.tx.subscribe();
    let current: Vec<OverlayMsg> = s
        .overlays
        .last
        .lock()
        .expect("overlay state lock")
        .values()
        .filter_map(|m| visible_to(&view, m))
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
                Ok(msg) => if let Some(msg) = visible_to(&view, &msg)
                    && socket.send(text(&msg)).await.is_err() { return },
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

fn shared_overlays_for_tick(o: &Overlays) -> Overlays {
    o.clone()
}

/// The `Host` a page was fetched from (127.0.0.1:port), for its Content-Security-Policy.
fn host_of(headers: &HeaderMap) -> &str {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1")
}

/// The read only services a plugin page may fetch from its opaque origin carry CORS headers; the
/// host's write endpoints, the editor and the session state never do.
async fn read_cors(req: Request, next: Next) -> Response {
    let read = req.method() == axum::http::Method::GET
        && [
            "/plugin/",
            "/sdk/",
            "/overlay/assets/",
            "/overlay/skins/",
            "/overlay/music/",
            "/overlay/cover/",
            "/overlay/sounds/",
            "/overlay/upi.svg",
        ]
        .iter()
        .any(|p| req.uri().path().starts_with(p));
    let mut res = next.run(req).await;
    if read {
        let h = res.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            header::HeaderValue::from_static("*"),
        );
        h.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            header::HeaderValue::from_static("Content-Range, Accept-Ranges, Content-Length"),
        );
    }
    res
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
            plugins: dir.join("plugins"),
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
    async fn plugins_are_served_as_shells_with_files_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let plug = dir.path().join("plugins/goal");
        fs::create_dir_all(&plug).unwrap();
        fs::write(
            plug.join("overlay.toml"),
            "[overlay]
id='goal'
name='Goal'
api=1
[frame]
x=10
y=20
width=300
height=100
[[param]]
key='n'
type='number'
default=3
",
        )
        .unwrap();
        fs::write(
            plug.join("index.html"),
            "<html><head></head><body><div id='ac-frame'></div></body></html>",
        )
        .unwrap();
        fs::write(plug.join("app.js"), "1").unwrap();
        fs::write(
            dir.path().join("aftercast.toml"),
            "[plugins.goal.params]
n = 9
",
        )
        .unwrap();
        let (addr, ..) = start(dir.path()).await;
        let get = |p: &str| reqwest::get(format!("http://{addr}{p}"));

        let page = get("/overlay/goal").await.unwrap().text().await.unwrap();
        assert!(page.contains("<base href=\"/plugin/goal/\">"), "{page}");
        assert!(page.contains("<body data-overlay=\"goal\">"));
        assert!(
            page.contains("[data-overlay=\"goal\"] { --x: 10px; --y: 20px; --w: 300px; --h: 100px")
        );
        assert!(
            page.contains("\"n\":9"),
            "the saved setting is in the boot data"
        );
        assert!(page.contains("/sdk/aftercast.js"));

        assert_eq!(get("/plugin/goal/app.js").await.unwrap().status(), 200);
        for bad in [
            "/plugin/goal/.env",
            "/plugin/goal/..%2Faftercast.toml",
            "/plugin/nope/app.js",
        ] {
            assert_eq!(get(bad).await.unwrap().status(), 404, "{bad}");
        }
        assert_eq!(get("/sdk/aftercast.js").await.unwrap().status(), 200);
        let list: Value = get("/api/plugins").await.unwrap().json().await.unwrap();
        let goal = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "goal")
            .expect("goal is listed");
        assert_eq!(goal["params_in_effect"]["n"], 9.0);
        assert!(
            list.as_array().unwrap().iter().any(|p| p["id"] == "brb"),
            "the built-in overlays are plugins too"
        );
        // A page for a plugin that is not installed is a 404 like any unknown overlay.
        assert_eq!(get("/overlay/ghost").await.unwrap().status(), 404);
    }

    #[tokio::test]
    async fn a_plugin_page_is_sandboxed_and_sees_only_what_it_reads() {
        use futures_util::StreamExt;
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let dir = tempfile::tempdir().unwrap();
        let plug = dir.path().join("plugins/goal");
        fs::create_dir_all(&plug).unwrap();
        fs::write(
            plug.join("overlay.toml"),
            "[overlay]\nid='goal'\nname='Goal'\napi=1\n[capabilities]\nreads=['now']\nnetwork=['api.example.com']\n[[part]]\nid='bar'\n",
        )
        .unwrap();
        fs::write(
            plug.join("index.html"),
            "<html><head></head><body></body></html>",
        )
        .unwrap();
        fs::write(
            dir.path().join("aftercast.toml"),
            "[plugins.goal.parts.nvim.bar]\nx = 5\ny = 6\n",
        )
        .unwrap();
        let (addr, overlays, _rx) = start(dir.path()).await;

        // The page and its files: a sandbox, only the host and the listed network hosts.
        let res = reqwest::get(format!("http://{addr}/overlay/goal"))
            .await
            .unwrap();
        let csp = res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(csp.starts_with("sandbox allow-scripts;"), "{csp}");
        assert!(
            csp.contains("default-src 'none'") && csp.contains("https://api.example.com"),
            "{csp}"
        );
        assert!(!csp.contains("allow-same-origin"));
        let page = res.text().await.unwrap();
        assert!(
            page.contains("\"parts\":[\"bar\"]") && page.contains("\"x\":5"),
            "{page}"
        );
        // Read only services carry CORS headers (a sandboxed page has an opaque origin); the
        // host's own data does not.
        let file = reqwest::get(format!("http://{addr}/sdk/aftercast.js"))
            .await
            .unwrap();
        assert_eq!(file.headers()["access-control-allow-origin"], "*");
        let plugins = reqwest::get(format!("http://{addr}/api/plugins"))
            .await
            .unwrap();
        assert!(
            plugins
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );

        // The socket: a page with `Origin: null` gets the streams of `reads`, the host's pages all.
        for (overlay, data) in [
            ("question", json!({ "text": "secret" })),
            ("now", json!({ "quest": "Parser" })),
        ] {
            overlays.send(OverlayMsg {
                overlay: overlay.into(),
                action: Action::Show,
                data,
            });
        }
        let mut request = format!("ws://{addr}/ws?plugin=goal")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("origin", "null".parse().unwrap());
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let mut seen = Vec::new();
        while let Ok(Some(Ok(m))) =
            tokio::time::timeout(Duration::from_millis(400), ws.next()).await
        {
            let v: Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
            seen.push(v["overlay"].as_str().unwrap().to_owned());
        }
        assert!(seen.contains(&"now".to_owned()), "{seen:?}");
        assert!(!seen.contains(&"question".to_owned()), "{seen:?}");
        let (mut open, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let mut all = Vec::new();
        while let Ok(Some(Ok(m))) =
            tokio::time::timeout(Duration::from_millis(400), open.next()).await
        {
            let v: Value = serde_json::from_str(m.to_text().unwrap()).unwrap();
            all.push(v["overlay"].as_str().unwrap().to_owned());
        }
        assert!(all.contains(&"question".to_owned()), "{all:?}");
    }

    #[test]
    fn overlays_hide_while_the_ones_they_hide_for_are_on_screen() {
        let o = Overlays::default();
        o.set_rules(BTreeMap::from([(
            "keys".to_owned(),
            vec!["question".to_owned()],
        )]));
        let names = |o: &Overlays| {
            o.last
                .lock()
                .unwrap()
                .get("__suppress")
                .map(|m| m.data["names"].clone())
        };
        o.suppress_tick();
        assert_eq!(
            names(&o),
            None,
            "nothing is hidden at first, and nothing is sent"
        );
        o.send(OverlayMsg {
            overlay: "question".into(),
            action: Action::Show,
            data: json!({}),
        });
        o.suppress_tick();
        assert_eq!(names(&o), Some(json!(["keys"])));
        o.send(OverlayMsg {
            overlay: "question".into(),
            action: Action::Hide,
            data: json!({}),
        });
        o.suppress_tick();
        assert_eq!(names(&o), Some(json!([])));
    }

    #[tokio::test]
    async fn pages_api_token_and_commands() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("sdk.js"), "/* mine */").unwrap();
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
        let sdk = get("/sdk/aftercast.js").await.unwrap();
        assert_eq!(
            sdk.text().await.unwrap(),
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
            plugins: dir.path().join("plugins"),
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
            plugins: dir.path().join("plugins"),
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
