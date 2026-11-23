//! Overlay plugins (specs/2026-11-23-overlay-plugins.md): a folder with an `overlay.toml`
//! manifest and an HTML entry, served as a shell with the theme, the frame rules and the SDK.

use std::fs;
use std::path::{Component, Path, PathBuf};

use ac_core::config::{self, Align, Config, Frame, PluginCfg};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The plugin API version this host speaks.
pub const API: u32 = 1;

/// Names a plugin may not take (they are paths of the host).
const RESERVED: [&str; 8] = [
    "assets", "skins", "sounds", "music", "cover", "upi", "sdk", "plugin",
];

/// An installed (or about to be installed) overlay plugin.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub entry: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub homepage: String,
    pub dir: PathBuf,
    /// The files of a plugin that ships inside aftercast (its `dir` is empty).
    pub built_in: Option<&'static [(&'static str, &'static [u8])]>,
    pub frame: ManifestFrame,
    pub caps: Caps,
    pub skins: Vec<(String, String)>,
    pub params: Vec<Param>,
    pub scenarios: Vec<Scenario>,
    /// Movable parts: `data-part="<id>"` elements the editor can drag (id, label).
    pub parts: Vec<(String, String)>,
    /// The parts whose width the user can drag (`resizable = true`).
    pub resizable_parts: Vec<String>,
    /// Settings an older version of the overlay kept elsewhere in the config: `(from, to, invert)`,
    /// `from` a dotted config key, `to` a param. Copied once when the plugin is installed.
    pub migrate: Vec<(String, String, bool)>,
}

#[derive(Debug, Clone)]
pub struct ManifestFrame {
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
    pub size: u16,
    pub align: Align,
}

/// What a plugin asks for; shown when it is installed.
#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub reads: Vec<String>,
    pub audio: bool,
    pub network: Vec<String>,
    pub media: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub key: String,
    pub kind: String,
    pub label: String,
    pub default: Value,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub options: Vec<String>,
    pub group: String,
    pub help: String,
}

#[derive(Debug, Clone)]
pub struct Scenario {
    pub name: String,
    pub state: Value,
}

const PARAM_KINDS: [&str; 8] = [
    "text", "number", "range", "bool", "color", "select", "file", "list",
];

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawManifest {
    overlay: RawOverlay,
    frame: RawFrame,
    capabilities: RawCaps,
    skin: Vec<RawSkin>,
    param: Vec<RawParam>,
    scenario: Vec<RawScenario>,
    part: Vec<RawPart>,
    migrate: Vec<RawMigrate>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMigrate {
    from: String,
    to: String,
    invert: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawPart {
    id: String,
    label: String,
    resizable: bool,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawOverlay {
    id: String,
    name: String,
    version: String,
    api: u32,
    entry: String,
    description: String,
    author: String,
    license: String,
    homepage: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawFrame {
    x: Option<i16>,
    y: Option<i16>,
    width: Option<u16>,
    height: Option<u16>,
    size: Option<u16>,
    align: Option<Align>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCaps {
    reads: Vec<String>,
    audio: bool,
    network: Vec<String>,
    media: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawSkin {
    id: String,
    name: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawParam {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    label: String,
    default: Option<toml::Value>,
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    options: Vec<String>,
    group: String,
    help: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawScenario {
    name: String,
    state: toml::Table,
}

/// A plugin id: 1 to 32 of `a-z 0-9 - _`, not one of the host's own path names.
pub fn id_ok(id: &str) -> bool {
    config::skin_name_ok(id) && !RESERVED.contains(&id)
}

fn json_of(v: &toml::Value) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

fn default_of(kind: &str, given: Option<&toml::Value>, options: &[String]) -> Value {
    match given {
        Some(v) => json_of(v),
        None => match kind {
            "number" | "range" => json!(0),
            "bool" => json!(false),
            "color" => json!("#ffffff"),
            "select" => json!(options.first().cloned().unwrap_or_default()),
            "list" => json!([]),
            _ => json!(""),
        },
    }
}

/// Whether `v` has the JSON type that a param of `kind` takes (and is in range).
fn value_fits(p: &Param, v: &Value) -> bool {
    match p.kind.as_str() {
        "number" | "range" => v
            .as_f64()
            .is_some_and(|n| p.min.is_none_or(|m| n >= m) && p.max.is_none_or(|m| n <= m)),
        "bool" => v.is_boolean(),
        "color" => v.as_str().is_some_and(|c| config::rgb(c).is_some()),
        "select" => v.as_str().is_some_and(|s| p.options.iter().any(|o| o == s)),
        "list" => v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
        _ => v.is_string(),
    }
}

impl Plugin {
    /// Reads and checks `<dir>/overlay.toml` and that the entry is there.
    pub fn load(dir: &Path) -> Result<Plugin, String> {
        Self::load_with(dir, true)
    }

    /// [`Plugin::load`] without the entry having to exist yet (a project that is not built).
    pub fn load_unbuilt(dir: &Path) -> Result<Plugin, String> {
        Self::load_with(dir, false)
    }

    fn load_with(dir: &Path, entry_must_exist: bool) -> Result<Plugin, String> {
        let path = dir.join("overlay.toml");
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(
            dir,
            &path.display().to_string(),
            &text,
            entry_must_exist,
            None,
        )
    }

    /// A plugin that ships inside aftercast (see [`BUILT_IN`]).
    pub fn load_built_in(
        files: &'static [(&'static str, &'static [u8])],
    ) -> Result<Plugin, String> {
        let text = files
            .iter()
            .find(|(n, _)| *n == "overlay.toml")
            .and_then(|(_, b)| std::str::from_utf8(b).ok())
            .ok_or("a built-in plugin without an overlay.toml")?;
        Self::parse(
            Path::new(""),
            "built-in overlay.toml",
            text,
            true,
            Some(files),
        )
    }

    /// Whether this plugin takes the place of one that ships with aftercast (same id).
    pub fn overrides_system(&self) -> bool {
        !self.is_built_in() && built_in_plugins().iter().any(|b| b.id == self.id)
    }

    pub fn is_built_in(&self) -> bool {
        self.built_in.is_some()
    }

    fn parse(
        dir: &Path,
        label: &str,
        text: &str,
        entry_must_exist: bool,
        built_in: Option<&'static [(&'static str, &'static [u8])]>,
    ) -> Result<Plugin, String> {
        let raw: RawManifest = toml::from_str(text).map_err(|e| format!("{label}: {e}"))?;
        let o = raw.overlay;
        if !id_ok(&o.id) {
            return Err(format!(
                "overlay.id {:?}: 1 to 32 of a-z 0-9 - _, and not one of {}",
                o.id,
                RESERVED.join(", ")
            ));
        }
        if o.name.trim().is_empty() {
            return Err("overlay.name is required".into());
        }
        if o.api == 0 {
            return Err("overlay.api is required (this host speaks 1)".into());
        }
        if o.api > API {
            return Err(format!(
                "this plugin is for API {}; this aftercast speaks {API}: update aftercast",
                o.api
            ));
        }
        let entry = if o.entry.is_empty() {
            "index.html".into()
        } else {
            o.entry
        };
        if !safe_rel(&entry) || !entry.ends_with(".html") {
            return Err(format!(
                "overlay.entry {entry:?}: a .html file inside the folder"
            ));
        }
        let present = match built_in {
            Some(files) => files.iter().any(|(n, _)| *n == entry),
            None => dir.join(&entry).is_file(),
        };
        if entry_must_exist && !present {
            return Err(format!("the entry {entry:?} is not in the folder"));
        }
        let f = raw.frame;
        let frame = ManifestFrame {
            x: f.x.unwrap_or(0),
            y: f.y.unwrap_or(0),
            width: f.width.unwrap_or(1920),
            height: f.height.unwrap_or(1080),
            size: f.size.unwrap_or(100),
            align: f.align.unwrap_or(Align::Left),
        };
        if frame.width == 0 || frame.height == 0 || !(10..=400).contains(&frame.size) {
            return Err("frame: width and height above 0, size 10 to 400".into());
        }
        let mut skins = Vec::new();
        for s in raw.skin {
            if !config::skin_name_ok(&s.id) {
                return Err(format!("skin id {:?}: a-z 0-9 - _", s.id));
            }
            let name = if s.name.is_empty() {
                s.id.clone()
            } else {
                s.name
            };
            skins.push((s.id, name));
        }
        let mut params: Vec<Param> = Vec::new();
        for p in raw.param {
            if p.key.is_empty()
                || !p
                    .key
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            {
                return Err(format!("param key {:?}: letters, digits and _", p.key));
            }
            if params.iter().any(|q| q.key == p.key) {
                return Err(format!("param {:?} is declared twice", p.key));
            }
            if !PARAM_KINDS.contains(&p.kind.as_str()) {
                return Err(format!(
                    "param {:?}: type {:?} is not one of {}",
                    p.key,
                    p.kind,
                    PARAM_KINDS.join(", ")
                ));
            }
            if p.kind == "select" && p.options.is_empty() {
                return Err(format!("param {:?}: a select needs options", p.key));
            }
            let default = default_of(&p.kind, p.default.as_ref(), &p.options);
            let param = Param {
                label: if p.label.is_empty() {
                    p.key.clone()
                } else {
                    p.label
                },
                key: p.key,
                kind: p.kind,
                default,
                min: p.min,
                max: p.max,
                step: p.step,
                options: p.options,
                group: p.group,
                help: p.help,
            };
            if !value_fits(&param, &param.default) {
                return Err(format!(
                    "param {:?}: the default {} does not fit its type",
                    param.key, param.default
                ));
            }
            params.push(param);
        }
        let scenarios = raw
            .scenario
            .into_iter()
            .map(|s| Scenario {
                name: s.name,
                state: json_of(&toml::Value::Table(s.state)),
            })
            .collect();
        let mut parts: Vec<(String, String)> = Vec::new();
        let mut resizable_parts: Vec<String> = Vec::new();
        for p in raw.part {
            if !config::skin_name_ok(&p.id) {
                return Err(format!("part id {:?}: a-z 0-9 - _", p.id));
            }
            if parts.iter().any(|(id, _)| *id == p.id) {
                return Err(format!("part {:?} is declared twice", p.id));
            }
            let label = if p.label.is_empty() {
                p.id.clone()
            } else {
                p.label
            };
            if p.resizable {
                resizable_parts.push(p.id.clone());
            }
            parts.push((p.id, label));
        }
        let mut migrate = Vec::new();
        for m in raw.migrate {
            if !params.iter().any(|p| p.key == m.to) || m.from.is_empty() {
                return Err(format!(
                    "migrate {:?} to {:?}: the target is not a param",
                    m.from, m.to
                ));
            }
            migrate.push((m.from, m.to, m.invert));
        }
        let c = raw.capabilities;
        Ok(Plugin {
            id: o.id,
            name: o.name,
            version: o.version,
            entry,
            description: o.description,
            author: o.author,
            license: o.license,
            homepage: o.homepage,
            dir: dir.to_path_buf(),
            built_in,
            frame,
            caps: Caps {
                reads: c.reads,
                audio: c.audio,
                network: c.network,
                media: c.media,
            },
            skins,
            params,
            scenarios,
            parts,
            resizable_parts,
            migrate,
        })
    }

    /// Whether `v` is a value this plugin's param `key` accepts.
    pub fn accepts(&self, key: &str, v: &Value) -> bool {
        self.params
            .iter()
            .find(|p| p.key == key)
            .is_some_and(|p| value_fits(p, v))
    }

    /// The overlay this plugin replaces, when its id is a built-in name.
    pub fn replaces(&self) -> bool {
        crate::OVERLAYS.contains(&self.id.as_str())
    }

    /// The param values in effect: manifest defaults, then `[plugins.<id>.params]` where the
    /// value has the right type.
    pub fn params_in_effect(&self, cfg: Option<&PluginCfg>) -> Value {
        let mut out = Map::new();
        for p in &self.params {
            let set = cfg
                .and_then(|c| c.params.get(&p.key))
                .map(json_of)
                .filter(|v| value_fits(p, v));
            out.insert(p.key.clone(), set.unwrap_or_else(|| p.default.clone()));
        }
        Value::Object(out)
    }

    /// The frame this plugin has: the manifest's, then `[plugins.<id>]`.
    pub fn frame_in_effect(&self, cfg: Option<&PluginCfg>) -> Frame {
        let mut f = config::Framed::<0>::default().0;
        f.x = self.frame.x;
        f.y = self.frame.y;
        f.width = self.frame.width;
        f.height = self.frame.height;
        f.size = self.frame.size;
        f.align = self.frame.align;
        if let Some(c) = cfg {
            f.x = c.x.unwrap_or(f.x);
            f.y = c.y.unwrap_or(f.y);
            f.width = c.width.filter(|w| *w > 0).unwrap_or(f.width);
            f.height = c.height.filter(|h| *h > 0).unwrap_or(f.height);
            f.size = c.size.filter(|s| (10..=400).contains(s)).unwrap_or(f.size);
            f.rotate = c.rotate.unwrap_or(f.rotate);
            f.align = c.align.unwrap_or(f.align);
            f.bg =
                c.bg.clone()
                    .filter(|b| b.is_empty() || config::rgb(b).is_some())
                    .unwrap_or_default();
            f.opacity = c.opacity.filter(|o| *o <= 100).unwrap_or(f.opacity);
            f.skin = c.skin.clone();
        }
        f
    }

    /// What the editor needs to know: everything but the plugin's folder.
    pub fn manifest_json(&self) -> Value {
        let params: Vec<Value> = self
            .params
            .iter()
            .map(|p| {
                json!({ "key": p.key, "type": p.kind, "label": p.label, "default": p.default,
                        "min": p.min, "max": p.max, "step": p.step, "options": p.options,
                        "group": p.group, "help": p.help })
            })
            .collect();
        json!({
            "id": self.id, "name": self.name, "version": self.version,
            "description": self.description, "author": self.author, "license": self.license,
            "homepage": self.homepage, "replaces": self.replaces(),
            "frame": { "x": self.frame.x, "y": self.frame.y, "width": self.frame.width,
                       "height": self.frame.height, "size": self.frame.size,
                       "align": self.frame.align.name() },
            "capabilities": { "reads": self.caps.reads, "audio": self.caps.audio,
                              "network": self.caps.network, "media": self.caps.media },
            "skins": self.skins.iter().map(|(id, name)| json!({ "id": id, "name": name })).collect::<Vec<_>>(),
            "params": params,
            "parts": self.parts.iter().map(|(id, label)| json!({ "id": id, "label": label, "resizable": self.resizable_parts.contains(id) })).collect::<Vec<_>>(),
            "scenarios": self.scenarios.iter().map(|s| json!({ "name": s.name, "state": s.state })).collect::<Vec<_>>(),
        })
    }

    /// The lines `plugin add` shows before installing.
    pub fn permissions(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.overrides_system() {
            out.push(format!(
                "replaces the {} overlay that ships with aftercast",
                self.id
            ));
        }
        if !self.caps.reads.is_empty() {
            out.push(format!("listens to: {}", self.caps.reads.join(", ")));
        }
        if self.caps.audio {
            out.push("plays sound".into());
        }
        if !self.caps.media.is_empty() {
            out.push(format!("reads playlists: {}", self.caps.media.join(", ")));
        }
        if !self.caps.network.is_empty() {
            out.push(format!("talks to: {}", self.caps.network.join(", ")));
        }
        if out.is_empty() {
            out.push("asks for nothing".into());
        }
        out
    }
}

/// The folders of the installed plugins: each folder of `<home>/plugins`, and the folder a
/// `<id>.link` file there names (`plugin add --link`: a project used in place).
fn plugin_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if path.is_dir() {
            found.push(path);
        } else if path.extension().is_some_and(|x| x == "link")
            && let Ok(text) = fs::read_to_string(&path)
        {
            found.push(PathBuf::from(text.trim()));
        }
    }
    found
}

/// Every valid plugin in `<home>/plugins`, by id. A folder that does not load is skipped.
pub fn scan(dir: &Path) -> Vec<Plugin> {
    let mut found: Vec<Plugin> = plugin_dirs(dir)
        .iter()
        .filter_map(|p| Plugin::load(p).ok())
        .collect();
    // The overlays that ship with aftercast, unless a plugin of the same id is installed.
    for p in built_in_plugins() {
        if !found.iter().any(|q| q.id == p.id) {
            found.push(p.clone());
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// The overlays that ship inside aftercast, as plugins (their files are the folders of
/// `crates/ac-server/plugins`, built into the program).
pub fn built_in_plugins() -> &'static [Plugin] {
    static LOADED: std::sync::OnceLock<Vec<Plugin>> = std::sync::OnceLock::new();
    LOADED.get_or_init(|| {
        BUILT_IN
            .iter()
            .filter_map(|files| Plugin::load_built_in(files).ok())
            .collect()
    })
}

macro_rules! built_in {
    ($id:literal, [$($file:literal),*]) => {
        &[$(($file, include_bytes!(concat!("../plugins/", $id, "/", $file)) as &[u8])),*]
    };
}

/// Every first party overlay: a plugin folder built into aftercast.
static BUILT_IN: &[&[(&str, &[u8])]] = &[
    built_in!(
        "now",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "chapters",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "latency",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "captions",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "question",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "keys",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "starting",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "status",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
    built_in!(
        "alert",
        [
            "overlay.toml",
            "index.html",
            "main.js",
            "style.css",
            "alert.js"
        ]
    ),
    built_in!(
        "brb",
        ["overlay.toml", "index.html", "main.js", "style.css"]
    ),
];

/// Like [`scan`], with the reason a folder was skipped.
pub fn scan_report(dir: &Path) -> Vec<(PathBuf, Result<Plugin, String>)> {
    let mut found: Vec<_> = plugin_dirs(dir)
        .into_iter()
        .map(|p| {
            let loaded = Plugin::load(&p);
            (p, loaded)
        })
        .collect();
    found.sort_by(|a, b| a.0.cmp(&b.0));
    for p in built_in_plugins() {
        if !found
            .iter()
            .any(|(_, r)| r.as_ref().is_ok_and(|q| q.id == p.id))
        {
            found.push((PathBuf::from("built in"), Ok(p.clone())));
        }
    }
    found
}

/// A relative path inside a folder: no `..`, no root, no dot files.
pub fn safe_rel(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path).components().all(|c| match c {
            Component::Normal(n) => !n.to_string_lossy().starts_with('.'),
            _ => false,
        })
}

/// A file of the plugin, read only if it really is inside the plugin's folder.
pub fn read_file(plugin: &Plugin, rel: &str) -> Option<Vec<u8>> {
    if !safe_rel(rel) {
        return None;
    }
    if let Some(files) = plugin.built_in {
        return files
            .iter()
            .find(|(n, _)| *n == rel)
            .map(|(_, b)| b.to_vec());
    }
    let root = plugin.dir.canonicalize().ok()?;
    let path = plugin.dir.join(rel).canonicalize().ok()?;
    if !path.starts_with(&root) || !path.is_file() {
        return None;
    }
    fs::read(path).ok()
}

/// Newest modification time of any file in the plugin's folder (for hot reload).
pub fn newest(dir: &Path) -> u128 {
    fn walk(dir: &Path, budget: &mut u32, newest: &mut u128) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            if *budget == 0 {
                return;
            }
            *budget -= 1;
            let p = e.path();
            if p.is_dir() {
                walk(&p, budget, newest);
            } else if let Ok(m) = e.metadata().and_then(|m| m.modified())
                && let Ok(d) = m.duration_since(std::time::UNIX_EPOCH)
            {
                *newest = (*newest).max(d.as_millis());
            }
        }
    }
    let (mut budget, mut newest) = (4000, 0);
    walk(dir, &mut budget, &mut newest);
    newest
}

pub fn mime(path: &str) -> &'static str {
    match path
        .rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "txt" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// A host name as it may appear in a Content-Security-Policy source.
fn host_ok(h: &str) -> bool {
    !h.is_empty()
        && h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'*'))
}

/// The Content-Security-Policy of a plugin's page and files. `sandbox` without
/// `allow-same-origin` gives the page an opaque origin: it cannot read the host's other pages or
/// call its write endpoints (they send no CORS headers), cannot keep cookies or storage, and may
/// load only from the host (read only services carry CORS headers) and from the hosts its manifest
/// lists under `capabilities.network`.
pub fn csp(plugin: &Plugin, host: &str) -> String {
    let host = if host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b':' | b'-'))
        && !host.is_empty()
    {
        host
    } else {
        "127.0.0.1"
    };
    let net: String = plugin
        .caps
        .network
        .iter()
        .filter(|h| host_ok(h))
        .map(|h| format!(" https://{h} wss://{h}"))
        .collect();
    format!(
        "sandbox allow-scripts; default-src 'none'; script-src http://{host} 'unsafe-inline'; style-src http://{host} 'unsafe-inline'{net}; img-src http://{host} data: blob:{net}; media-src http://{host} blob: data:{net}; font-src http://{host} data:{net}; connect-src http://{host} ws://{host}{net}; worker-src blob:"
    )
}

/// Rules every plugin page gets so `#ac-frame` is a frame like the built-in ones.
const BASE_CSS: &str = "html, body { margin: 0; width: 1920px; height: 1080px; overflow: hidden; background: transparent; color: var(--fg); font-family: var(--font-sans), sans-serif; }\n\
#ac-frame { position: absolute; left: var(--x, 0px); top: var(--y, 0px); width: var(--w, 1920px); height: var(--h, 1080px); box-sizing: border-box; overflow: hidden; transform-origin: 0 0; transform: translate(calc(var(--w) * var(--s, 1) / 2), calc(var(--h) * var(--s, 1) / 2)) rotate(var(--r, 0deg)) translate(calc(var(--w) * var(--s, 1) / -2), calc(var(--h) * var(--s, 1) / -2)) scale(var(--s, 1)); display: flex; flex-direction: column; justify-content: var(--v, flex-start); align-items: var(--align, flex-start); text-align: var(--text-align, left); }
#ac-frame { transition: opacity 250ms; }
#ac-frame.suppressed { opacity: 0; }
html.ac-docked #ac-frame { display: none; }
#ac-frame { --halo: 3px 0 0 var(--bg-solid), -3px 0 0 var(--bg-solid), 0 3px 0 var(--bg-solid), 0 -3px 0 var(--bg-solid), 2px 2px 0 var(--bg-solid), -2px 2px 0 var(--bg-solid), 2px -2px 0 var(--bg-solid), -2px -2px 0 var(--bg-solid), 0 6px 16px rgb(0 0 0 / 55%); }";

/// The page at `/overlay/<id>`: the entry with the host's tags added.
pub fn shell(plugin: &Plugin, theme_css: &str, boot: &Value) -> Option<String> {
    let html = String::from_utf8(read_file(plugin, &plugin.entry)?).ok()?;
    let id = &plugin.id;
    // Relative URLs in the entry resolve from the entry's own folder (dist/ for a built page).
    let folder = plugin
        .entry
        .rsplit_once('/')
        .map_or(String::new(), |(d, _)| format!("{d}/"));
    let boot = boot.to_string().replace("</", "<\\/");
    let head = format!(
        "<meta charset=\"utf-8\"><base href=\"/plugin/{id}/{folder}\"><style id=\"ac-theme\">{theme_css}</style><style id=\"ac-base\">{BASE_CSS}</style><script id=\"ac-boot\" type=\"application/json\">{boot}</script><script type=\"module\" src=\"/sdk/aftercast.js\"></script>"
    );
    let lower = html.to_ascii_lowercase();
    let mut out = match lower.find("<head>").or_else(|| {
        lower
            .find("<head ")
            .and_then(|i| lower[i..].find('>').map(|j| i + j))
    }) {
        Some(i) => {
            let end = lower[i..].find('>').map_or(i, |j| i + j + 1);
            format!("{}{head}{}", &html[..end], &html[end..])
        }
        None => format!("<!doctype html><html><head>{head}</head><body>{html}</body></html>"),
    };
    // data-overlay on <body>.
    let low = out.to_ascii_lowercase();
    if let Some(i) = low.find("<body") {
        let attr = format!(" data-overlay=\"{id}\"");
        out.insert_str(i + 5, &attr);
    }
    Some(out)
}

/// The frames plugin overlays have that are not built-in names, for the theme.
pub fn frames(config: &Config, plugins: &[Plugin]) -> Vec<(String, Frame)> {
    plugins
        .iter()
        .filter(|p| !p.replaces())
        .map(|p| (p.id.clone(), p.frame_in_effect(config.plugins.get(&p.id))))
        .collect()
}

/// `{ id: { params, skin, enabled } }` for every plugin: what the SDK needs live.
pub fn runtime(config: &Config, plugins: &[Plugin]) -> Value {
    let mut out = Map::new();
    for p in plugins {
        let cfg = config.plugins.get(&p.id);
        out.insert(
            p.id.clone(),
            json!({ "params": p.params_in_effect(cfg),
                    "skin": cfg.map_or("", |c| c.skin.as_str()),
                    "parts": cfg.map_or(json!({}), |c| serde_json::to_value(&c.parts).unwrap_or_default()),
                    "enabled": cfg.is_none_or(|c| c.enabled) }),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(manifest: &str) -> (tempfile::TempDir, Result<Plugin, String>) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("overlay.toml"), manifest).unwrap();
        fs::write(
            dir.path().join("index.html"),
            "<html><head></head><body><div id=ac-frame></div></body></html>",
        )
        .unwrap();
        let p = Plugin::load(dir.path());
        (dir, p)
    }

    #[test]
    fn manifest_loads_with_defaults_and_params() {
        let (_d, p) = make(
            "[overlay]\nid='goal'\nname='Goal'\napi=1\n[[param]]\nkey='goal'\ntype='number'\nmin=1\nmax=10\ndefault=5\n[[param]]\nkey='style'\ntype='select'\noptions=['bar','ring']\n",
        );
        let p = p.unwrap();
        assert_eq!(p.entry, "index.html");
        let mut cfg = PluginCfg::default();
        cfg.params.insert("goal".into(), toml::Value::Integer(7));
        cfg.params
            .insert("style".into(), toml::Value::String("nope".into()));
        let v = p.params_in_effect(Some(&cfg));
        assert_eq!(v["goal"], 7.0);
        assert_eq!(
            v["style"], "bar",
            "a value that is not an option falls back"
        );
        cfg.params.insert("goal".into(), toml::Value::Integer(99));
        assert_eq!(
            p.params_in_effect(Some(&cfg))["goal"],
            5,
            "out of range falls back"
        );
    }

    #[test]
    fn manifest_errors_are_plain() {
        for bad in [
            "[overlay]\nid='Bad Id'\nname='x'\napi=1\n",
            "[overlay]\nid='ok'\nname='x'\napi=2\n",
            "[overlay]\nid='ok'\nname='x'\n",
            "[overlay]\nid='assets'\nname='x'\napi=1\n",
            "[overlay]\nid='ok'\nname='x'\napi=1\nentry='../x.html'\n",
            "[overlay]\nid='ok'\nname='x'\napi=1\n[[param]]\nkey='a'\ntype='dial'\n",
        ] {
            assert!(make(bad).1.is_err(), "{bad}");
        }
    }

    #[test]
    fn parts_and_migrations_are_read_and_checked() {
        let (_d, p) = make(
            "[overlay]\nid='goal'\nname='Goal'\napi=1\n[[param]]\nkey='n'\ntype='number'\ndefault=1\n[[part]]\nid='bar'\nlabel='The bar'\n[[migrate]]\nfrom='overlay.old.n'\nto='n'\n",
        );
        let p = p.unwrap();
        assert_eq!(p.parts, [("bar".to_owned(), "The bar".to_owned())]);
        assert!(p.accepts("n", &json!(3)) && !p.accepts("n", &json!("x")));
        assert_eq!(p.migrate.len(), 1);
        assert!(
            make("[overlay]\nid='goal'\nname='Goal'\napi=1\n[[migrate]]\nfrom='a'\nto='nope'\n")
                .1
                .is_err()
        );
        assert!(
            make("[overlay]\nid='goal'\nname='Goal'\napi=1\n[[part]]\nid='A b'\n")
                .1
                .is_err()
        );
        let policy = csp(&p, "127.0.0.1:7878");
        assert!(policy.contains("connect-src http://127.0.0.1:7878 ws://127.0.0.1:7878"));
        assert!(!csp(&p, "evil.com; script-src *").contains("script-src *"));
    }

    #[test]
    fn shell_injects_the_host_tags() {
        let (_d, p) = make("[overlay]\nid='goal'\nname='Goal'\napi=1\n");
        let html = shell(&p.unwrap(), ":root{}", &json!({"id": "goal"})).unwrap();
        assert!(html.contains("<base href=\"/plugin/goal/\">"));
        assert!(html.contains("<body data-overlay=\"goal\">"));
        assert!(html.contains("/sdk/aftercast.js"));
    }

    #[test]
    fn paths_stay_inside_the_folder() {
        assert!(safe_rel("a/b.js"));
        for bad in ["../x", "/etc/passwd", "a/../b", ".env", "a/.git/x", ""] {
            assert!(!safe_rel(bad), "{bad}");
        }
    }
}
