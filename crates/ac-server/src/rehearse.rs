//! The rehearse page (`aftercast rehearse`): a stand-in for the stream page with the OBS
//! output (Virtual Camera), the description as pushed, and a chat to type into as any
//! viewer, moderator, member or the owner.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ac_core::chat::{ChatKind, ChatMessage};
use ac_core::command::Command;
use ac_core::config::{self, Config};
use ac_core::time::Timestamp;
use axum::Json;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, watch};

use crate::Shared;

const PAGE: &str = include_str!("../overlays/rehearse.html");
const MAX_TEXT: usize = 200;
const MAX_NAME: usize = 40;

/// What the page shows, published by the session.
#[derive(Debug, Clone, Default)]
pub struct Feed {
    pub title: String,
    pub live_since: Option<Timestamp>,
    /// The description as last pushed (what viewers would see).
    pub description: String,
    /// The current rendered description when it differs from the pushed one.
    pub draft: Option<String>,
    pub scene: Option<String>,
    pub chat: Arc<VecDeque<ChatMessage>>,
    /// The scenes and sources OBS has (empty while OBS is not connected).
    pub media: Arc<ac_core::obs::Media>,
    /// The sources of the scene being edited.
    pub items: Arc<ac_core::obs::SceneDetail>,
    pub brb: bool,
    pub privacy: bool,
}

pub struct Rehearse {
    pub feed: watch::Receiver<Feed>,
    /// Typed messages go into the simulated chat stream.
    pub chat: mpsc::UnboundedSender<ChatMessage>,
    next_id: AtomicU64,
    /// The session's `media` folder: the recording the player can go back in.
    media: Option<std::path::PathBuf>,
}

impl Rehearse {
    /// Lets the page's player go back in the recording in `dir`.
    pub fn with_media(mut self, dir: std::path::PathBuf) -> Self {
        self.media = Some(dir);
        self
    }

    pub(crate) fn media(&self) -> Option<&std::path::Path> {
        self.media.as_deref()
    }

    pub fn new(feed: watch::Receiver<Feed>, chat: mpsc::UnboundedSender<ChatMessage>) -> Self {
        Self {
            feed,
            chat,
            next_id: AtomicU64::new(1),
            media: None,
        }
    }
}

fn off() -> Response {
    (
        StatusCode::NOT_FOUND,
        "the rehearse page exists only during `aftercast rehearse`",
    )
        .into_response()
}

pub(crate) async fn page(State(s): State<Arc<Shared>>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    let html = PAGE.replace("/* theme */", &s.overlays.theme());
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response()
}

pub(crate) async fn feed(State(s): State<Arc<Shared>>) -> Response {
    let Some(r) = &s.rehearse else { return off() };
    let f = r.feed.borrow().clone();
    let chat: Vec<_> = f
        .chat
        .iter()
        .map(|m| {
            json!({ "id": m.id, "author": m.author_name, "text": m.text, "owner": m.is_owner,
                    "moderator": m.is_moderator, "member": m.is_member,
                    "superchat": m.kind == ChatKind::Superchat, "amount": m.amount,
                    "t": m.t_received })
        })
        .collect();
    Json(json!({
        "title": f.title,
        "live_since": f.live_since,
        "now": s.clock.now_ts(),
        "description": f.description,
        "draft": f.draft,
        "scene": f.scene,
        "chat": chat,
        // Which overlay sources each scene has, and whether they show: the editor lists
        // those. Null without OBS.
        "obs": overlays_in(&f.media),
        "items": &*f.items,
        "mode": if f.privacy { "privacy" } else if f.brb { "brb" } else { "normal" },
    }))
    .into_response()
}

/// OBS's scenes with the overlay sources in each, or `None` while OBS is not connected.
fn overlays_in(media: &ac_core::obs::Media) -> Option<serde_json::Value> {
    let overlays: Vec<_> = media
        .items
        .iter()
        .filter_map(|i| {
            let overlay = i.overlay.as_ref()?;
            Some(
                json!({ "scene": i.scene, "source": i.source, "overlay": overlay,
                         "enabled": i.enabled }),
            )
        })
        .collect();
    (!media.scenes.is_empty()).then(|| json!({ "scenes": media.scenes, "overlays": overlays }))
}

#[derive(Deserialize)]
pub(crate) struct Source {
    scene: String,
    source: String,
    enabled: bool,
}

/// Shows or hides an overlay's source in a scene of OBS (the editor's eye button).
pub(crate) async fn source(State(s): State<Arc<Shared>>, Json(p): Json<Source>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    let _ = s.commands.send(Command::Source {
        source: p.source,
        scene: Some(p.scene),
        visible: Some(p.enabled),
    });
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct Watch {
    scene: String,
}

/// The editor looks at a scene of OBS: its sources are listed in the feed (repeated every few
/// seconds while it does).
pub(crate) async fn watch(State(s): State<Arc<Shared>>, Json(p): Json<Watch>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    let _ = s.commands.send(Command::WatchScene { scene: p.scene });
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct ItemChange {
    scene: String,
    id: i64,
    #[serde(flatten)]
    edit: ac_core::obs::ItemEdit,
}

/// Changes a source of a scene in OBS: its place, lock, order, crop, name (the editor).
pub(crate) async fn item(State(s): State<Arc<Shared>>, Json(p): Json<ItemChange>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    let _ = s.commands.send(Command::EditItem {
        scene: p.scene,
        id: p.id,
        edit: p.edit,
    });
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct Mode {
    /// `normal`, `brb` or `privacy`.
    mode: String,
}

/// Puts the session in a mode for real (the editor's Normal, BRB and Privacy): the OBS scene
/// switches and the overlays follow, so the video shows it.
pub(crate) async fn mode(State(s): State<Arc<Shared>>, Json(m): Json<Mode>) -> Response {
    let Some(r) = &s.rehearse else { return off() };
    let (brb, privacy) = match m.mode.as_str() {
        "normal" => (false, false),
        "brb" => (true, false),
        "privacy" => (false, true),
        _ => return (StatusCode::BAD_REQUEST, "normal, brb or privacy").into_response(),
    };
    let f = r.feed.borrow().clone();
    let by = ac_core::event::By::Tui;
    // What is switching off goes first, so the scenes do not fight.
    let mut cmds = Vec::new();
    if f.brb && !brb {
        cmds.push(Command::Brb {
            on: Some(false),
            by,
        });
    }
    if f.privacy && !privacy {
        cmds.push(Command::Privacy {
            on: Some(false),
            by,
        });
    }
    if brb && !f.brb {
        cmds.push(Command::Brb { on: Some(true), by });
    }
    if privacy && !f.privacy {
        cmds.push(Command::Privacy { on: Some(true), by });
    }
    for c in cmds {
        let _ = s.commands.send(c);
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct AddSource {
    scene: String,
    overlay: String,
}

/// Adds an overlay to an OBS scene as a Browser source (the editor's right-click menu).
pub(crate) async fn add_source(State(s): State<Arc<Shared>>, Json(p): Json<AddSource>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    if !crate::OVERLAYS.contains(&p.overlay.as_str()) {
        return (StatusCode::BAD_REQUEST, "no such overlay").into_response();
    }
    let _ = s.commands.send(Command::AddOverlay {
        scene: p.scene,
        overlay: p.overlay,
    });
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct Typed {
    author: String,
    /// `viewer`, `member`, `moderator` or `owner`.
    role: String,
    text: String,
    #[serde(default)]
    amount: Option<String>,
}

pub(crate) async fn chat(State(s): State<Arc<Shared>>, Json(t): Json<Typed>) -> Response {
    let Some(r) = &s.rehearse else { return off() };
    let text = t.text.trim();
    let author = t.author.trim();
    if text.is_empty() || author.is_empty() {
        return (StatusCode::BAD_REQUEST, "name and message are required").into_response();
    }
    if text.chars().count() > MAX_TEXT || author.chars().count() > MAX_NAME {
        return (
            StatusCode::BAD_REQUEST,
            format!("at most {MAX_TEXT} characters per message and {MAX_NAME} per name"),
        )
            .into_response();
    }
    let (owner, moderator, member) = match t.role.as_str() {
        "viewer" => (false, false, false),
        "member" => (false, false, true),
        "moderator" => (false, true, false),
        "owner" => (true, false, false),
        other => {
            return (StatusCode::BAD_REQUEST, format!("unknown role {other:?}")).into_response();
        }
    };
    let amount = t
        .amount
        .map(|a| a.trim().to_owned())
        .filter(|a| !a.is_empty());
    let now = s.clock.now_ts();
    let msg = ChatMessage {
        id: format!("typed-{}", r.next_id.fetch_add(1, Ordering::Relaxed)),
        t_received: now,
        t_published: Some(now),
        // Same name, same viewer: cooldowns and owner_accounts work per name.
        author_channel_id: if owner {
            "UC-channel".into()
        } else {
            format!("UC-{author}")
        },
        author_name: author.to_owned(),
        is_owner: owner,
        is_moderator: moderator,
        is_member: member,
        kind: if amount.is_some() {
            ChatKind::Superchat
        } else {
            ChatKind::Text
        },
        text: text.to_owned(),
        amount,
    };
    if r.chat.send(msg).is_err() {
        return (StatusCode::SERVICE_UNAVAILABLE, "chat is not running").into_response();
    }
    StatusCode::NO_CONTENT.into_response()
}

/// Every overlay's frame as saved in `aftercast.toml`:
/// `{ "keys": { "x", "y", "width", "height", "size", "rotate", "align", "bg", "opacity" } }`.
pub(crate) async fn frames(State(s): State<Arc<Shared>>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    match Config::load(&s.config) {
        Ok(c) => {
            let frames: serde_json::Map<_, _> = c
                .overlay
                .frames()
                .into_iter()
                .map(|(name, f)| {
                    let frame = json!({ "x": f.x, "y": f.y, "width": f.width, "height": f.height,
                                        "size": f.size, "rotate": f.rotate, "align": f.align.name(),
                                        "bg": f.bg, "opacity": f.opacity, "skin": f.skin,
                                        "dock": f.dock, "hide_when": f.hide_when, "mute": f.mute,
                                        "sound": f.sound, "shuffle": f.shuffle, "pin": f.pin, "skins": skins_json(&f.skins) });
                    (name.into(), frame)
                })
                .collect();
            let order = c.overlay.order();
            let skins = crate::skins(&s.dir);
            // Packs: every skin name some overlay wears.
            // (the alert's animations are not packs.)
            let mut packs: Vec<&String> = skins
                .iter()
                .filter(|(n, _)| **n != "alert")
                .flat_map(|(_, s)| s)
                .collect();
            packs.sort_by_key(|p| {
                (
                    crate::SKINS
                        .iter()
                        .position(|s| s == p)
                        .unwrap_or(usize::MAX),
                    p.as_str(),
                )
            });
            packs.dedup();
            Json(
                json!({ "frames": frames, "order": order, "theme_bg": c.overlay.bg,
                         "pack": c.overlay.skin, "skins": skins, "packs": packs,
                         "bar": { "order": c.overlay.bar.order, "animate": c.overlay.bar.animate,
                                  "marquee": c.overlay.bar.marquee, "speed": c.overlay.bar.speed,
                                  "extras": c.overlay.bar.extras },
                         "brb_screen": c.overlay.brb_screen,
                         "sound": { "enabled": c.overlay.sound.enabled, "volume": c.overlay.sound.volume,
                                    "skin": c.overlay.sound.skin },
                         "sound_skins": crate::sound_skins(&s.dir),
                         "sounds": crate::sound_files(&s.dir),
                         "tracks": crate::media::tracks(&s.dir),
                         "scenes": c.obs.scenes.all() }),
            )
            .into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// A frame's per-skin changes as JSON: only the fields a skin sets.
fn skins_json(skins: &std::collections::BTreeMap<String, config::SkinFrame>) -> serde_json::Value {
    let map: serde_json::Map<_, _> = skins
        .iter()
        .map(|(name, s)| {
            let mut o = serde_json::Map::new();
            let mut put = |k: &str, v: Option<serde_json::Value>| {
                if let Some(v) = v {
                    o.insert(k.into(), v);
                }
            };
            put("x", s.x.map(|v| json!(v)));
            put("y", s.y.map(|v| json!(v)));
            put("width", s.width.map(|v| json!(v)));
            put("height", s.height.map(|v| json!(v)));
            put("size", s.size.map(|v| json!(v)));
            put("rotate", s.rotate.map(|v| json!(v)));
            put("align", s.align.map(|v| json!(v.name())));
            put("bg", s.bg.as_ref().map(|v| json!(v)));
            put("opacity", s.opacity.map(|v| json!(v)));
            (name.clone(), serde_json::Value::Object(o))
        })
        .collect();
    serde_json::Value::Object(map)
}

#[derive(Deserialize)]
pub(crate) struct Framed {
    name: String,
    x: Option<i16>,
    y: Option<i16>,
    width: Option<u16>,
    height: Option<u16>,
    size: Option<u16>,
    rotate: Option<i16>,
    align: Option<String>,
    bg: Option<String>,
    opacity: Option<u8>,
    skin: Option<String>,
    dock: Option<bool>,
    hide_when: Option<Vec<String>>,
    mute: Option<bool>,
    sound: Option<String>,
    shuffle: Option<bool>,
    pin: Option<bool>,
    /// The skin the position and look fields belong to: they are saved as that skin's changes
    /// (`[overlay.<name>.skins.<skin>]`); without it, as the overlay's own.
    for_skin: Option<String>,
    /// Back to the built-in frame (everything else is ignored).
    #[serde(default)]
    reset: bool,
}

/// Saves one overlay's frame into `aftercast.toml` (comments kept; all fields or none) and
/// reloads the config, which restyles the overlays in OBS and shows the new values in the
/// TUI settings.
pub(crate) async fn frame(State(s): State<Arc<Shared>>, Json(f): Json<Framed>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    if !crate::OVERLAYS.contains(&f.name.as_str()) {
        return (StatusCode::BAD_REQUEST, "no such overlay").into_response();
    }
    if f.for_skin
        .as_deref()
        .is_some_and(|k| !config::skin_name_ok(k))
    {
        return (StatusCode::BAD_REQUEST, "no such skin").into_response();
    }
    let saved = if f.reset {
        // With a skin: only what that skin changed; else the whole overlay.
        let key = match &f.for_skin {
            Some(skin) => format!("overlay.{}.skins.{skin}", f.name),
            None => format!("overlay.{}", f.name),
        };
        config::set_value(&s.config, &key, None)
    } else {
        let int = |v: i64| Some(config::Value::Int(v));
        let text = |v: String| Some(config::Value::Text(v));
        // Position and look belong to the skin being edited; the rest to the overlay.
        let look = |field: &str| match &f.for_skin {
            Some(skin) => format!("overlay.{}.skins.{skin}.{field}", f.name),
            None => format!("overlay.{}.{field}", f.name),
        };
        let fields = [
            ("x", f.x.map(|v| int(v.clamp(-1920, 3840).into()))),
            ("y", f.y.map(|v| int(v.clamp(-1080, 2160).into()))),
            ("width", f.width.map(|v| int(v.clamp(20, 3840).into()))),
            ("height", f.height.map(|v| int(v.clamp(20, 2160).into()))),
            ("size", f.size.map(|v| int(v.clamp(10, 400).into()))),
            ("rotate", f.rotate.map(|v| int(v.clamp(-180, 180).into()))),
            ("opacity", f.opacity.map(|v| int(v.min(100).into()))),
            ("align", f.align.map(text)),
            ("bg", f.bg.map(text)),
            ("skin", f.skin.map(text)),
            ("dock", f.dock.map(|v| Some(config::Value::Bool(v)))),
            (
                "hide_when",
                f.hide_when.map(|v| Some(config::Value::List(v))),
            ),
            ("mute", f.mute.map(|v| Some(config::Value::Bool(v)))),
            ("sound", f.sound.map(text)),
            ("shuffle", f.shuffle.map(|v| Some(config::Value::Bool(v)))),
            ("pin", f.pin.map(|v| Some(config::Value::Bool(v)))),
        ];
        let keys: Vec<(String, Option<config::Value>)> = fields
            .into_iter()
            .filter_map(|(field, v)| {
                // Only position and look go to the skin; the others stay the overlay's.
                let skinned = matches!(
                    field,
                    "x" | "y" | "width" | "height" | "size" | "rotate" | "opacity" | "align" | "bg"
                );
                let key = if skinned {
                    look(field)
                } else {
                    format!("overlay.{}.{field}", f.name)
                };
                v.map(|v| (key, v))
            })
            .collect();
        config::set_values(&s.config, keys.iter().map(|(k, v)| (k.as_str(), v.clone())))
    };
    if let Err(e) = saved {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let _ = s.commands.send(Command::Reload);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct Order {
    /// Overlay names, frontmost first.
    order: Vec<String>,
}

/// Saves `[overlay] order` and reloads the config, which stacks the overlay sources in OBS
/// the same way.
pub(crate) async fn order(State(s): State<Arc<Shared>>, Json(o): Json<Order>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    // Unknown names are refused by the config check in set_value.
    let list = Some(config::Value::List(o.order));
    if let Err(e) = config::set_value(&s.config, "overlay.order", list) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let _ = s.commands.send(Command::Reload);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
pub(crate) struct Options {
    order: Option<Vec<String>>,
    animate: Option<bool>,
    marquee: Option<bool>,
    speed: Option<u16>,
    extras: Option<Vec<String>>,
    /// Any of the `[overlay.brb_screen]` fields.
    brb_screen: Option<serde_json::Map<String, serde_json::Value>>,
    /// With `brb_screen`: the skin whose own arrangement (places, sizes) the fields are.
    brb_skin: Option<String>,
    sound: Option<bool>,
    volume: Option<u8>,
    sound_skin: Option<String>,
}

/// Saves the status bar's options (`[overlay.bar]`) and the sound (`[overlay.sound]`):
/// every field that is present, all or none.
pub(crate) async fn options(State(s): State<Arc<Shared>>, Json(o): Json<Options>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    let int = |v: i64| Some(config::Value::Int(v));
    let fields = [
        (
            "overlay.bar.order",
            o.order.map(|v| Some(config::Value::List(v))),
        ),
        (
            "overlay.bar.animate",
            o.animate.map(|v| Some(config::Value::Bool(v))),
        ),
        (
            "overlay.bar.marquee",
            o.marquee.map(|v| Some(config::Value::Bool(v))),
        ),
        (
            "overlay.bar.speed",
            o.speed.map(|v| int(v.clamp(10, 600).into())),
        ),
        (
            "overlay.bar.extras",
            o.extras.map(|v| Some(config::Value::List(v))),
        ),
        (
            "overlay.sound.enabled",
            o.sound.map(|v| Some(config::Value::Bool(v))),
        ),
        (
            "overlay.sound.volume",
            o.volume.map(|v| int(v.min(100).into())),
        ),
        (
            "overlay.sound.skin",
            o.sound_skin.map(|v| Some(config::Value::Text(v))),
        ),
    ];
    let mut keys: Vec<(String, Option<config::Value>)> = fields
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v)))
        .collect();
    for (k, v) in o.brb_screen.into_iter().flatten() {
        let value = match v {
            serde_json::Value::Bool(b) => config::Value::Bool(b),
            serde_json::Value::Number(n) => config::Value::Int(n.as_i64().unwrap_or(0)),
            serde_json::Value::String(t) => config::Value::Text(t),
            serde_json::Value::Null => {
                keys.push((brb_key(o.brb_skin.as_deref(), &k), None));
                continue;
            }
            serde_json::Value::Array(a) => config::Value::List(
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => continue,
        };
        keys.push((brb_key(o.brb_skin.as_deref(), &k), Some(value)));
    }
    if let Err(e) = config::set_values(&s.config, keys.iter().map(|(k, v)| (k.as_str(), v.clone())))
    {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let _ = s.commands.send(Command::Reload);
    StatusCode::NO_CONTENT.into_response()
}

/// A BRB screen field's key: places and sizes belong to the skin when one is given.
fn brb_key(skin: Option<&str>, field: &str) -> String {
    let own = field.ends_with("_x")
        || field.ends_with("_y")
        || field.ends_with("_size")
        || field == "music_w";
    match skin.filter(|s| own && config::skin_name_ok(s)) {
        Some(s) => format!("overlay.brb_screen.skins.{s}.{field}"),
        None => format!("overlay.brb_screen.{field}"),
    }
}

#[derive(Deserialize)]
pub(crate) struct Pack {
    skin: String,
}

/// Saves the skin pack (`[overlay] skin`), worn by every overlay without its own skin.
pub(crate) async fn pack(State(s): State<Arc<Shared>>, Json(p): Json<Pack>) -> Response {
    if s.rehearse.is_none() {
        return off();
    }
    // A name that could not be a file is refused by the config check in set_value.
    let skin = Some(config::Value::Text(p.skin));
    if let Err(e) = config::set_value(&s.config, "overlay.skin", skin) {
        return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
    }
    let _ = s.commands.send(Command::Reload);
    StatusCode::NO_CONTENT.into_response()
}
