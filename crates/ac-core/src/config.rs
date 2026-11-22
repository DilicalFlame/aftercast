//! `aftercast.toml`: typed config, loader and validation (specs/03-config.md).
//!
//! Validation happens during deserialization (enums, regexes, unknown keys), so every
//! error carries the file, line and column of the offending key or value.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{Error, Result, parse_toml};

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub channel: Channel,
    pub paths: Paths,
    pub youtube: Youtube,
    pub obs: Obs,
    pub stream: Stream,
    pub readme: Readme,
    /// Free-form values available to templates as `{{ vars.key }}`.
    pub vars: toml::Table,
    pub bot: Bot,
    pub overlay: Overlay,
    pub captions: LiveCaptions,
    pub capture: Capture,
    pub hotkeys: Hotkeys,
    pub post: Post,
    pub render: Render,
    pub external: External,
    pub tui: Tui,
    pub notify: Notify,
}

/// `[notify]`: notifications on the desktop (Windows toasts), even while another app has focus.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Notify {
    /// A Super Chat, Super Sticker or membership pops up with who and how much.
    pub supporters: bool,
    /// With Windows' notification sound.
    pub sound: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            supporters: true,
            sound: true,
        }
    }
}

/// `[tui]`: how the terminal UI looks; changed from its settings page (`,`).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Tui {
    /// Wrap every chat message, Inbox item and log event instead of cutting it.
    pub wrap_lines: bool,
    /// Pulsing dots, spinners and gliding panes; off keeps everything still.
    pub animations: bool,
    /// Every chat author gets their own color.
    pub name_colors: bool,
    /// How Home arranges its sections.
    pub home_layout: HomeLayout,
}

impl Default for Tui {
    fn default() -> Self {
        Self {
            wrap_lines: false,
            animations: true,
            name_colors: true,
            home_layout: HomeLayout::Columns,
        }
    }
}

/// How Home arranges its cards (Checks, Status, Audio, On air) under the primary card
/// (`tui.home_layout`; `v` on Home cycles them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomeLayout {
    /// A 2 by 2 grid: Checks and Status on top, On air and Audio below. Layouts that
    /// were removed load as this one.
    #[serde(alias = "page", alias = "row", alias = "classic")]
    Columns,
    /// Three tabs: Checks (with Status), Audio, On air.
    Tabs,
}

impl HomeLayout {
    pub const ALL: [HomeLayout; 2] = [HomeLayout::Columns, HomeLayout::Tabs];

    pub fn name(self) -> &'static str {
        match self {
            HomeLayout::Columns => "columns",
            HomeLayout::Tabs => "tabs",
        }
    }
}

/// A value written by [`set_value`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    List(Vec<String>),
}

/// Sets the dotted `key` (e.g. `stream.chapter_on_brb`) in the config file, or removes it
/// with `None` so the default applies again. Comments and layout of the rest of the file
/// are kept. The result is validated first: an invalid value is refused and nothing is
/// written.
pub fn set_value(path: &Path, key: &str, value: Option<Value>) -> Result<()> {
    set_values(path, [(key, value)])
}

/// [`set_value`] for several keys in one write: all of them are saved, or none.
pub fn set_values<'a>(
    path: &Path,
    values: impl IntoIterator<Item = (&'a str, Option<Value>)>,
) -> Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::io(path)(e)),
    };
    let refuse = |message: String| {
        Error::Toml(crate::error::TomlError {
            path: path.to_path_buf(),
            line: 1,
            col: 1,
            message,
        })
    };
    // A file that does not parse is reported with its line and column, and left alone.
    parse_toml::<toml::Table>(path, &text)?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| refuse(e.to_string()))?;
    // In a file of only comments they are the document's trailing text; keep them on top.
    let only_comments = doc.as_table().is_empty();
    let mut first_table = None;
    for (key, value) in values {
        set_in(&mut doc, key, value).map_err(refuse)?;
        first_table = first_table.or_else(|| key.split_once('.').map(|(t, _)| t.to_owned()));
    }
    if only_comments
        && let Some(first) = first_table
        && let Some(t) = doc.get_mut(&first).and_then(toml_edit::Item::as_table_mut)
    {
        let head = doc_trailing(&text);
        t.decor_mut().set_prefix(head);
        doc.set_trailing("");
    }
    let new = doc.to_string();
    parse_toml::<Config>(path, &new)?;
    let tmp = path.with_extension("toml.tmp");
    fs::write(&tmp, &new).map_err(Error::io(&tmp))?;
    fs::rename(&tmp, path).map_err(Error::io(path))
}

/// Sets or removes one dotted key in the document.
fn set_in(
    doc: &mut toml_edit::DocumentMut,
    key: &str,
    value: Option<Value>,
) -> std::result::Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    let (name, tables) = parts.split_last().expect("split yields one part");
    let mut table = doc.as_table_mut();
    for part in tables {
        if !table.contains_key(part) {
            let mut new = toml_edit::Table::new();
            new.set_implicit(false);
            table.insert(part, toml_edit::Item::Table(new));
        }
        table = table[part]
            .as_table_mut()
            .ok_or_else(|| format!("{key}: {part} is not a table"))?;
    }
    match value {
        None => {
            table.remove(name);
        }
        Some(value) => {
            let value = match value {
                Value::Bool(b) => toml_edit::value(b),
                Value::Int(i) => toml_edit::value(i),
                Value::Float(f) => toml_edit::value(f),
                Value::Text(t) => toml_edit::value(t),
                Value::List(l) => toml_edit::value(l.into_iter().collect::<toml_edit::Array>()),
            };
            // Keep a trailing comment on the line, in the same column, if the key had one.
            let old = table.get(name).and_then(toml_edit::Item::as_value).cloned();
            table[name] = value;
            if let (Some(old), Some(v)) = (old, table[name].as_value_mut()) {
                let width = |v: &toml_edit::Value| {
                    let mut v = v.clone();
                    v.decor_mut().clear();
                    v.to_string().chars().count()
                };
                let (was, now) = (width(&old), width(v));
                let mut decor = old.decor().clone();
                let suffix = decor.suffix().and_then(|s| s.as_str()).map(str::to_owned);
                if let Some(suffix) = suffix {
                    let comment = suffix.trim_start_matches(' ');
                    let spaces = suffix.len() - comment.len();
                    if comment.starts_with('#') && spaces > 1 {
                        let keep = (spaces + was).saturating_sub(now).max(1);
                        decor.set_suffix(format!("{}{comment}", " ".repeat(keep)));
                    }
                }
                *v.decor_mut() = decor;
            }
        }
    }
    Ok(())
}

/// The text of a file that holds no keys or tables (comments and blank lines), ending in a
/// newline.
fn doc_trailing(text: &str) -> String {
    let mut head = text.trim_end().to_owned();
    if !head.is_empty() {
        head.push('\n');
    }
    head
}

impl Config {
    /// Loads and validates the config file.
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => parse_toml(path, &text),
            Err(e) if e.kind() == ErrorKind::NotFound => {
                Err(Error::ConfigMissing(path.to_path_buf()))
            }
            Err(e) => Err(Error::io(path)(e)),
        }
    }
}

/// True for `"keyring:<name>"` / `"env:<NAME>"` references and empty values.
pub fn is_secret_ref(value: &str) -> bool {
    value.is_empty() || value.starts_with("keyring:") || value.starts_with("env:")
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Channel {
    pub series: String,
    pub short: String,
    pub episode_prefix: String,
    pub episode_pad: usize,
    pub title_mode: TitleMode,
    pub title_format: String,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            series: "Insomnia Driven Development".into(),
            short: "IDD".into(),
            episode_prefix: "ep".into(),
            episode_pad: 3,
            title_mode: TitleMode::Keep,
            title_format: "{{ channel.short }} #{{ stream.episode_padded }} · {{ quest.title | default('Live coding') }}".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleMode {
    Keep,
    Format,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Paths {
    pub sessions: PathBuf,
    pub templates: PathBuf,
    pub editor: String,
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub yt_dlp: PathBuf,
    pub python: PathBuf,
    pub whisper_cli: PathBuf,
    pub tesseract: PathBuf,
}

impl Default for Paths {
    fn default() -> Self {
        Self {
            sessions: "sessions".into(),
            templates: "templates".into(),
            editor: "code".into(),
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            yt_dlp: "yt-dlp".into(),
            python: "python".into(),
            whisper_cli: "whisper-cli".into(),
            tesseract: "tesseract".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Youtube {
    pub client_secret_file: PathBuf,
    pub token_profile: String,
    pub push_on_start: bool,
    pub push_on_end: bool,
    pub live_autosync: bool,
    pub autosync_debounce_s: u64,
    pub autosync_min_interval_s: u64,
    pub daily_quota_reserve: u32,
    /// Longest wait before reopening chat after YouTube closed it with no new messages
    /// (each reopen costs 5 units; YouTube closes the stream about every 10 s).
    pub chat_idle_max_s: u64,
    pub bot_profile: Option<String>,
    /// Quota cost overrides by API method name, e.g. `"videos.update" = 50`.
    pub costs: BTreeMap<String, u32>,
}

impl Default for Youtube {
    fn default() -> Self {
        Self {
            client_secret_file: "state/client_secret.json".into(),
            token_profile: "main".into(),
            push_on_start: true,
            push_on_end: true,
            live_autosync: false,
            autosync_debounce_s: 5,
            autosync_min_interval_s: 90,
            daily_quota_reserve: 1500,
            chat_idle_max_s: 30,
            bot_profile: None,
            costs: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Obs {
    pub url: String,
    /// Secret: `"keyring:<name>"`, `"env:<NAME>"` or a plain value.
    pub password: String,
    pub require_recording: bool,
    /// While a session runs, OBS records into its `media` folder (the previous folder is
    /// restored on exit), so the master lives with the session and a quit draft keeps none.
    pub record_into_session: bool,
    /// Start OBS when it is not running (its WebSocket on this computer), never in safe mode.
    pub launch: bool,
    /// While the broadcast waits for Go Live, OBS starts streaming by itself (YouTube sees the
    /// video, then Go Live takes the broadcast live at once). Tried once per session.
    pub auto_stream: bool,
    /// `obs64.exe`; empty finds it in Program Files.
    pub path: PathBuf,
    pub scenes: ObsScenes,
    pub facecam: Facecam,
}

/// `[obs.facecam]`: the camera source hides while the mouse pointer is over its place on
/// the screen, so what is under it can be read, and comes back when the pointer leaves.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Facecam {
    pub enabled: bool,
    /// Read where the camera is from OBS (it can sit elsewhere in each scene). `x`, `y`,
    /// `width` and `height` below are used until OBS answers, or when this is off.
    pub auto: bool,
    /// The OBS source to hide (in the scene on air).
    pub source: String,
    /// The screen the pointer moves on: `\\.\DISPLAYn` as Windows Settings numbers it
    /// (1, 2, ...). 0 takes the monitor the on-air scene's display capture shows (the
    /// primary screen when it has none).
    pub monitor: u8,
    /// Where the camera sits on the 1920x1080 canvas, which is the screen OBS captures
    /// (the pointer is scaled from the primary screen to it).
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Pixels around the camera that count as over it.
    pub margin: u16,
    /// The camera returns this long after the pointer left (so it does not flicker).
    pub return_ms: u32,
}

impl Facecam {
    /// The configured place: left, top, width, height.
    pub fn rect(&self) -> [f32; 4] {
        [self.x, self.y, self.width, self.height].map(f32::from)
    }

    /// Whether the canvas point is over `rect` (left, top, width, height) or its margin.
    pub fn over(&self, rect: [f32; 4], px: f32, py: f32) -> bool {
        let m = f32::from(self.margin);
        let [x, y, w, h] = rect;
        px >= x - m && px <= x + w + m && py >= y - m && py <= y + h + m
    }
}

impl Default for Facecam {
    fn default() -> Self {
        Self {
            enabled: false,
            auto: true,
            monitor: 0,
            source: "Video Capture Device".into(),
            x: 1360,
            y: 680,
            width: 560,
            height: 400,
            margin: 40,
            return_ms: 600,
        }
    }
}

impl Default for Obs {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:4455".into(),
            password: "keyring:obs".into(),
            require_recording: true,
            record_into_session: true,
            launch: true,
            auto_stream: true,
            path: PathBuf::new(),
            scenes: ObsScenes::default(),
            facecam: Facecam::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObsScenes {
    pub main: String,
    pub brb: String,
    pub privacy: String,
    pub starting: String,
    pub ending: String,
}

impl ObsScenes {
    pub fn all(&self) -> [&str; 5] {
        [
            &self.main,
            &self.brb,
            &self.privacy,
            &self.starting,
            &self.ending,
        ]
    }
}

impl Default for ObsScenes {
    fn default() -> Self {
        Self {
            main: "Code".into(),
            brb: "BRB".into(),
            privacy: "Privacy".into(),
            starting: "Starting Soon".into(),
            ending: "Ending".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Stream {
    pub default_latency_s: f64,
    pub first_chapter_title: String,
    pub chapter_on_quest_change: bool,
    pub chapter_on_brb: bool,
    pub chapter_on_question: bool,
    /// Wrap a `:latency` measurement in a "Latency check" chapter, so viewers can skip it.
    pub chapter_on_latency: bool,
    /// OBS shows cameras, captures and audio this late (0 to 500 ms), so live captions land
    /// on the words; overlays and scene switches wait as long. 0 is off.
    pub delay_ms: u32,
    /// `:end` first shows the `ending` scene, asking how long (prefilled with
    /// `end_screen_s`), then stops streaming, recording and the virtual camera.
    pub end_screen: bool,
    /// Seconds of end screen `:end` offers (1 to 600).
    pub end_screen_s: u32,
    /// Go Live opens on the `starting` scene (with the starting overlay counting up); Enter
    /// in the TUI moves on to the main scene.
    pub starting_screen: bool,
}

impl Default for Stream {
    fn default() -> Self {
        Self {
            default_latency_s: 5.0,
            first_chapter_title: "Start".into(),
            chapter_on_quest_change: true,
            chapter_on_brb: true,
            chapter_on_question: false,
            chapter_on_latency: true,
            delay_ms: 0,
            end_screen: true,
            end_screen_s: 30,
            starting_screen: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Readme {
    pub seed_template: String,
    pub max_chars: usize,
    pub expose_env: Vec<String>,
    pub link_style: LinkStyle,
}

impl Default for Readme {
    fn default() -> Self {
        Self {
            seed_template: "session-readme.md".into(),
            max_chars: 5000,
            expose_env: Vec::new(),
            link_style: LinkStyle::TextColonUrl,
        }
    }
}

/// How `[text](url)` is written in the plain-text description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkStyle {
    /// `text: url`
    TextColonUrl,
    /// `text (url)`
    TextParenUrl,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bot {
    pub enabled: bool,
    pub prefix: String,
    pub global_cooldown_s: u64,
    pub user_cooldown_s: u64,
    pub default_reply: ReplyMode,
    pub max_chat_replies_per_hour: u32,
    /// Channel ids (UC...) that get owner commands, like a second account on a phone.
    pub owner_accounts: Vec<String>,
    pub commands: BTreeMap<String, BotCommand>,
    pub moderation: Moderation,
}

impl Default for Bot {
    fn default() -> Self {
        Self {
            enabled: true,
            prefix: "!".into(),
            global_cooldown_s: 3,
            user_cooldown_s: 20,
            default_reply: ReplyMode::Overlay,
            max_chat_replies_per_hour: 30,
            owner_accounts: Vec::new(),
            commands: BTreeMap::new(),
            moderation: Moderation::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplyMode {
    Overlay,
    Chat,
    Both,
}

/// A custom static chat command (`[bot.commands]`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotCommand {
    pub text: String,
    /// `None` uses `bot.default_reply`.
    pub reply: Option<ReplyMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Moderation {
    pub delete_links_from_non_mods: bool,
    pub banned_words: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overlay {
    pub port: u16,
    pub font_mono: String,
    pub font_sans: String,
    pub bg: String,
    pub accent: String,
    pub fg: String,
    pub muted: String,
    /// Overlay names from front to back, as OBS stacks their sources (see [`Overlay::order`]).
    #[serde(deserialize_with = "overlay_order")]
    pub order: Vec<String>,
    /// The skin pack: every overlay wears this skin unless `[overlay.<name>] skin` says
    /// otherwise. Built in: `nvim`, `clean`, `broadcast`, `glass`, `minimal`; more from
    /// `<home>/overlays/skins/<overlay>/<skin>.css`.
    #[serde(deserialize_with = "skin_pack")]
    pub skin: String,
    /// Picture on the starting screen: a file in `<home>/overlays` (the aftercast mark when
    /// it is missing).
    pub avatar: String,
    /// `[overlay.<name>]`: each overlay's frame on the 1920x1080 canvas.
    pub now: Framed<0>,
    pub question: Framed<1>,
    pub toast: Framed<2>,
    pub brb: Framed<3>,
    pub latency: Framed<4>,
    pub chapters: Framed<5>,
    pub keys: Framed<6>,
    pub captions: Framed<7>,
    /// The starting screen: full canvas, behind everything.
    pub starting: Framed<8>,
    /// A thin bar where `dock`ed overlays share the space.
    pub status: Framed<9>,
    /// The animation that plays for a Super Chat.
    pub alert: Framed<10>,
    /// `[overlay.bar]`: what the status bar does with its segments.
    pub bar: Bar,
    /// `[overlay.brb_screen]`: the picture, QR code and texts of the BRB screen.
    pub brb_screen: BrbScreen,
    /// `[overlay.sound]`: sound effects of the alert (and the rehearse page's mock player).
    pub sound: Sound,
}

/// `[overlay.brb_screen]`: what the BRB screen shows besides its title and music.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrbScreen {
    /// Your picture (`[overlay] avatar`).
    pub pfp: bool,
    pub pfp_shape: PfpShape,
    /// Picture size in pixels (80 to 700).
    pub pfp_size: u16,
    /// The QR code.
    pub qr: bool,
    /// QR size in pixels (120 to 800).
    pub qr_size: u16,
    /// What the QR code holds: any URL or text. Empty: your UPI id (`[vars.support] upi`).
    pub qr_link: String,
    /// The words under the QR code.
    pub qr_caption: String,
    /// Your own lines of text under the title (a handle, a link, ...).
    pub text: Vec<String>,
    /// Where each part stands (top left, pixels in the frame) once moved by hand; unset: it
    /// takes its place in the layout. Parts: `title` (with the timer), `text`, `music`, `pfp`, `qr`.
    pub title_x: Option<i16>,
    pub title_y: Option<i16>,
    pub text_x: Option<i16>,
    pub text_y: Option<i16>,
    pub music_x: Option<i16>,
    pub music_y: Option<i16>,
    pub pfp_x: Option<i16>,
    pub pfp_y: Option<i16>,
    pub qr_x: Option<i16>,
    pub qr_y: Option<i16>,
    /// Width of the music card once it is moved by hand (300 to 1800; unset: 1100).
    pub music_w: Option<u16>,
    /// What a skin changes (`[overlay.brb_screen.skins.<skin>]`): the places and sizes above,
    /// so each skin has its own arrangement.
    pub skins: BTreeMap<String, BrbLook>,
}

/// One skin's arrangement of the BRB screen; unset fields fall back to `[overlay.brb_screen]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrbLook {
    pub pfp_size: Option<u16>,
    pub qr_size: Option<u16>,
    pub music_w: Option<u16>,
    pub title_x: Option<i16>,
    pub title_y: Option<i16>,
    pub text_x: Option<i16>,
    pub text_y: Option<i16>,
    pub music_x: Option<i16>,
    pub music_y: Option<i16>,
    pub pfp_x: Option<i16>,
    pub pfp_y: Option<i16>,
    pub qr_x: Option<i16>,
    pub qr_y: Option<i16>,
}

impl BrbScreen {
    /// The options as the skin worn sees them, as JSON (its own fields over the common ones).
    pub fn effective(&self, skin: &str) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let (Some(own), Some(map)) = (self.skins.get(skin), v.as_object_mut())
            && let Ok(serde_json::Value::Object(own)) = serde_json::to_value(own)
        {
            for (k, x) in own.into_iter().filter(|(_, x)| !x.is_null()) {
                map.insert(k, x);
            }
        }
        v
    }
}

impl Default for BrbScreen {
    fn default() -> Self {
        Self {
            pfp: true,
            pfp_shape: PfpShape::Circle,
            pfp_size: 260,
            qr: true,
            qr_size: 420,
            qr_link: String::new(),
            qr_caption: "Scan to support".into(),
            text: Vec::new(),
            title_x: None,
            title_y: None,
            text_x: None,
            text_y: None,
            music_x: None,
            music_y: None,
            pfp_x: None,
            pfp_y: None,
            qr_x: None,
            qr_y: None,
            music_w: None,
            skins: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PfpShape {
    Circle,
    Rounded,
    Square,
}

/// `[overlay.bar]`: the status bar's segments.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Bar {
    /// Segments from left to right (docked overlays only; the rest are not shown).
    #[serde(deserialize_with = "bar_order")]
    pub order: Vec<String>,
    /// Segments slide in and out, and a changed value flashes.
    pub animate: bool,
    /// The segments scroll left forever, like a ticker.
    pub marquee: bool,
    /// Ticker speed in pixels per second.
    pub speed: u16,
    /// Extra segments at the right end, from `viewers` and `clock`.
    #[serde(deserialize_with = "bar_extras")]
    pub extras: Vec<String>,
}

impl Default for Bar {
    fn default() -> Self {
        Self {
            order: DOCKABLE.map(String::from).to_vec(),
            animate: true,
            marquee: false,
            speed: 60,
            extras: Vec::new(),
        }
    }
}

/// The segments the bar has besides docked overlays.
pub const BAR_EXTRAS: [&str; 2] = ["viewers", "clock"];

fn bar_extras<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    use serde::de::Error as _;
    let list = Vec::<String>::deserialize(d)?;
    match list.iter().find(|n| !BAR_EXTRAS.contains(&n.as_str())) {
        Some(bad) => Err(D::Error::custom(format!(
            "{bad:?} in the bar extras: only {}",
            BAR_EXTRAS.join(", ")
        ))),
        None => Ok(list),
    }
}

/// `[overlay.bar] order`: names of dockable overlays only.
fn bar_order<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    use serde::de::Error as _;
    let order = Vec::<String>::deserialize(d)?;
    if let Some(bad) = order.iter().find(|n| !DOCKABLE.contains(&n.as_str())) {
        return Err(D::Error::custom(format!(
            "{bad:?} in the bar order: only {} can dock",
            DOCKABLE.join(", ")
        )));
    }
    Ok(order)
}

/// `[overlay.sound]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sound {
    pub enabled: bool,
    /// 0 to 100.
    pub volume: u8,
    /// The sound skin: a folder in `<home>/overlays/sounds/` with the alert's sound files
    /// (`coins.wav` for the coin shower, `alert.wav` for any), see the README.
    #[serde(deserialize_with = "skin_pack")]
    pub skin: String,
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            enabled: true,
            volume: 70,
            skin: "default".into(),
        }
    }
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            port: 7878,
            font_mono: "JetBrains Mono".into(),
            font_sans: "Inter".into(),
            bg: "#303030".into(),
            accent: "#FF5555".into(),
            fg: "#FFFFFF".into(),
            muted: "#9A9A9A".into(),
            order: Vec::new(),
            skin: "nvim".into(),
            avatar: "logo.jpg".into(),
            now: Framed::default(),
            question: Framed::default(),
            toast: Framed::default(),
            brb: Framed::default(),
            latency: Framed::default(),
            chapters: Framed::default(),
            keys: Framed::default(),
            captions: Framed::default(),
            starting: Framed::default(),
            status: Framed::default(),
            alert: Framed::default(),
            bar: Bar::default(),
            brb_screen: BrbScreen::default(),
            sound: Sound::default(),
        }
    }
}

impl Overlay {
    /// Every overlay's name, frontmost first: `order` as set, then the rest as built in.
    pub fn order(&self) -> Vec<&'static str> {
        let mut order: Vec<&'static str> = Vec::new();
        for name in self
            .order
            .iter()
            .chain(DEFAULT_ORDER.map(String::from).iter())
        {
            if let Some((n, ..)) = FRAMES.iter().find(|f| f.0 == name)
                && !order.contains(n)
            {
                order.push(n);
            }
        }
        order
    }

    /// Every overlay's name and frame, in [`FRAMES`] order.
    pub fn frames(&self) -> [(&'static str, &Frame); 11] {
        [
            (FRAMES[0].0, &self.now),
            (FRAMES[1].0, &self.question),
            (FRAMES[2].0, &self.toast),
            (FRAMES[3].0, &self.brb),
            (FRAMES[4].0, &self.latency),
            (FRAMES[5].0, &self.chapters),
            (FRAMES[6].0, &self.keys),
            (FRAMES[7].0, &self.captions),
            (FRAMES[8].0, &self.starting),
            (FRAMES[9].0, &self.status),
            (FRAMES[10].0, &self.alert),
        ]
    }
}

/// Overlays from front to back when `[overlay] order` does not say.
pub const DEFAULT_ORDER: [&str; 11] = [
    "toast", "alert", "status", "question", "keys", "captions", "now", "chapters", "brb",
    "latency", "starting",
];

/// Overlays that can `dock` in the status bar instead of showing in their own frame.
pub const DOCKABLE: [&str; 5] = ["now", "brb", "latency", "chapters", "toast"];

/// `[overlay] order`: names of built-in overlays only.
/// A skin name: lowercase letters, digits, `-` and `_` (it names a file).
/// A sound file's name as the overlay server serves it.
pub fn sound_file_ok(name: &str) -> bool {
    !name.is_empty()
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

pub fn skin_name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// `[overlay] skin`: the pack every overlay without its own skin wears.
fn skin_pack<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    use serde::de::Error as _;
    let skin = String::deserialize(d)?;
    if !skin_name_ok(&skin) {
        return Err(D::Error::custom(format!(
            "skin {skin:?}: lowercase letters, digits, - and _"
        )));
    }
    Ok(skin)
}

fn overlay_order<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<String>, D::Error> {
    use serde::de::Error as _;
    let order = Vec::<String>::deserialize(d)?;
    if let Some(bad) = order
        .iter()
        .find(|n| !FRAMES.iter().any(|f| f.0 == n.as_str()))
    {
        return Err(D::Error::custom(format!(
            "unknown overlay {bad:?} in order"
        )));
    }
    Ok(order)
}

/// Built-in frame per overlay: name, `x`, `y`, `width`, `height`, `align`. Where the
/// content sits up and down in its frame is the overlay's own (keys and questions at the
/// bottom, `now` and toasts at the top, ...; see overlay.css).
pub const FRAMES: [(&str, i16, i16, u16, u16, Align); 11] = [
    ("now", 40, 40, 900, 80, Align::Left),
    ("question", 360, 680, 1200, 340, Align::Center),
    ("toast", 1180, 40, 700, 200, Align::Right),
    ("brb", 0, 0, 1920, 1080, Align::Center),
    ("latency", 460, 390, 1000, 300, Align::Center),
    ("chapters", 40, 960, 900, 80, Align::Left),
    ("keys", 1400, 120, 480, 520, Align::Center),
    ("captions", 260, 740, 1400, 200, Align::Left),
    ("starting", 0, 0, 1920, 1080, Align::Center),
    ("status", 0, 1044, 1920, 36, Align::Left),
    ("alert", 360, 860, 1200, 170, Align::Center),
];

/// Where an overlay sits and how it looks: a box on the canvas the overlay lays its
/// content out in, like a frame in a design tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Top left corner in canvas pixels; may be off the canvas.
    pub x: i16,
    pub y: i16,
    /// The box the content lays out in, before `size` scales it.
    pub width: u16,
    pub height: u16,
    /// Scale in percent (text and all), from the top left corner.
    pub size: u16,
    /// Degrees clockwise around the frame's center.
    pub rotate: i16,
    /// Content lines up left, center or right in the frame.
    pub align: Align,
    /// Card or bubble color, `#RRGGBB` or `#RGB`; empty uses `[overlay] bg`.
    pub bg: String,
    /// Background opacity in percent.
    pub opacity: u8,
    /// This overlay's own skin; empty wears the pack (`[overlay] skin`).
    pub skin: String,
    /// Shows in the status bar instead of its own frame (see [`DOCKABLE`]).
    pub dock: bool,
    /// Hidden while any of these overlays is on screen, e.g. keys while a question shows.
    pub hide_when: Vec<String>,
    /// Plays no sound (the alert's file, the BRB's music).
    pub mute: bool,
    /// No longer used (the BRB plays `<home>/overlays/sounds/brb/`); still read so old files load.
    pub playlist: String,
    /// The alert's sound: a file in a folder of `<home>/overlays/sounds/` as `folder/file.mp3`
    /// (empty: the sound folder in `[overlay.sound]`, files named after the animations).
    pub sound: String,
    /// The BRB's playlist plays in a random order.
    pub shuffle: bool,
    /// Docked in the status bar: stays in its place while the others scroll like a ticker.
    pub pin: bool,
    /// What each skin changes in the frame, by skin name (`[overlay.<name>.skins.<skin>]`): the
    /// frame is edited per skin, so scaling the coin shower leaves the other animations alone.
    pub skins: BTreeMap<String, SkinFrame>,
}

/// The frame fields a skin may change; what it leaves out comes from the overlay's own frame.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkinFrame {
    pub x: Option<i16>,
    pub y: Option<i16>,
    pub width: Option<u16>,
    pub height: Option<u16>,
    pub size: Option<u16>,
    pub rotate: Option<i16>,
    pub align: Option<Align>,
    pub bg: Option<String>,
    pub opacity: Option<u8>,
}

impl Frame {
    /// The frame as `skin` wears it: its changes over this one.
    pub fn for_skin(&self, skin: &str) -> Frame {
        let mut f = self.clone();
        if let Some(s) = self.skins.get(skin) {
            f.x = s.x.unwrap_or(f.x);
            f.y = s.y.unwrap_or(f.y);
            f.width = s.width.unwrap_or(f.width);
            f.height = s.height.unwrap_or(f.height);
            f.size = s.size.unwrap_or(f.size);
            f.rotate = s.rotate.unwrap_or(f.rotate);
            f.align = s.align.unwrap_or(f.align);
            f.bg = s.bg.clone().unwrap_or(f.bg);
            f.opacity = s.opacity.unwrap_or(f.opacity);
        }
        f
    }
}

/// An overlay's [`Frame`]; the index picks its defaults in [`FRAMES`], so a table that
/// sets only `x` keeps that overlay's own size and alignment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Framed<const I: usize>(pub Frame);

impl<const I: usize> std::ops::Deref for Framed<I> {
    type Target = Frame;
    fn deref(&self) -> &Frame {
        &self.0
    }
}

impl<const I: usize> Default for Framed<I> {
    fn default() -> Self {
        let (_, x, y, width, height, align) = FRAMES[I];
        Self(Frame {
            x,
            y,
            width,
            height,
            size: 100,
            rotate: 0,
            align,
            bg: String::new(),
            opacity: 100,
            skin: String::new(),
            dock: false,
            hide_when: Vec::new(),
            mute: false,
            playlist: String::new(),
            sound: String::new(),
            shuffle: true,
            pin: false,
            skins: BTreeMap::new(),
        })
    }
}

// By hand: serde's derive does not take const generics.
impl<'de, const I: usize> Deserialize<'de> for Framed<I> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Raw {
            x: Option<i16>,
            y: Option<i16>,
            width: Option<u16>,
            height: Option<u16>,
            size: Option<u16>,
            rotate: Option<i16>,
            align: Option<Align>,
            bg: Option<String>,
            opacity: Option<u8>,
            skin: Option<String>,
            dock: Option<bool>,
            hide_when: Option<Vec<String>>,
            mute: Option<bool>,
            playlist: Option<String>,
            sound: Option<String>,
            shuffle: Option<bool>,
            pin: Option<bool>,
            skins: Option<BTreeMap<String, RawSkin>>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawSkin {
            x: Option<i16>,
            y: Option<i16>,
            width: Option<u16>,
            height: Option<u16>,
            size: Option<u16>,
            rotate: Option<i16>,
            align: Option<Align>,
            bg: Option<String>,
            opacity: Option<u8>,
        }
        let raw = Raw::deserialize(d)?;
        let def = Self::default().0;
        let f = Frame {
            x: raw.x.unwrap_or(def.x),
            y: raw.y.unwrap_or(def.y),
            width: raw.width.unwrap_or(def.width),
            height: raw.height.unwrap_or(def.height),
            size: raw.size.unwrap_or(def.size),
            rotate: raw.rotate.unwrap_or(def.rotate),
            align: raw.align.unwrap_or(def.align),
            bg: raw.bg.unwrap_or(def.bg),
            opacity: raw.opacity.unwrap_or(def.opacity),
            skin: raw.skin.unwrap_or(def.skin),
            dock: raw.dock.unwrap_or(def.dock),
            hide_when: raw.hide_when.unwrap_or(def.hide_when),
            mute: raw.mute.unwrap_or(def.mute),
            playlist: raw.playlist.unwrap_or(def.playlist),
            sound: raw.sound.unwrap_or(def.sound),
            shuffle: raw.shuffle.unwrap_or(def.shuffle),
            pin: raw.pin.unwrap_or(def.pin),
            skins: raw
                .skins
                .unwrap_or_default()
                .into_iter()
                .map(|(name, s)| {
                    if !skin_name_ok(&name) {
                        return Err(D::Error::custom(format!(
                            "skin {name:?}: lowercase letters, digits, - and _"
                        )));
                    }
                    if s.bg
                        .as_deref()
                        .is_some_and(|b| !b.is_empty() && rgb(b).is_none())
                    {
                        return Err(D::Error::custom("bg: use #RRGGBB, #RGB or \"\""));
                    }
                    if s.opacity.is_some_and(|o| o > 100) {
                        return Err(D::Error::custom("opacity is a percent, 0 to 100"));
                    }
                    if s.size.is_some_and(|z| !(10..=400).contains(&z)) {
                        return Err(D::Error::custom("size is a percent, 10 to 400"));
                    }
                    if s.width == Some(0) || s.height == Some(0) {
                        return Err(D::Error::custom("width and height must be above 0"));
                    }
                    Ok((
                        name,
                        SkinFrame {
                            x: s.x,
                            y: s.y,
                            width: s.width,
                            height: s.height,
                            size: s.size,
                            rotate: s.rotate,
                            align: s.align,
                            bg: s.bg,
                            opacity: s.opacity,
                        },
                    ))
                })
                .collect::<std::result::Result<_, _>>()?,
        };
        if !f.playlist.is_empty() && !skin_name_ok(&f.playlist) {
            return Err(D::Error::custom(format!(
                "playlist {:?}: lowercase letters, digits, - and _",
                f.playlist
            )));
        }
        if !f.sound.is_empty()
            && !f
                .sound
                .split_once('/')
                .is_some_and(|(dir, file)| skin_name_ok(dir) && sound_file_ok(file))
        {
            return Err(D::Error::custom(format!(
                "sound {:?}: folder/file.mp3 (letters, digits, . - and _)",
                f.sound
            )));
        }
        let own = FRAMES[I].0;
        if f.dock && !DOCKABLE.contains(&own) {
            return Err(D::Error::custom(format!(
                "{own} cannot dock in the status bar (only {})",
                DOCKABLE.join(", ")
            )));
        }
        if let Some(bad) = f
            .hide_when
            .iter()
            .find(|n| n.as_str() == own || !FRAMES.iter().any(|fr| fr.0 == n.as_str()))
        {
            return Err(D::Error::custom(format!(
                "hide_when {bad:?}: name another overlay"
            )));
        }
        if !f.bg.is_empty() && rgb(&f.bg).is_none() {
            return Err(D::Error::custom(format!(
                "bg {:?}: use #RRGGBB, #RGB or \"\" for the theme color",
                f.bg
            )));
        }
        if !f.skin.is_empty() && !skin_name_ok(&f.skin) {
            return Err(D::Error::custom(format!(
                "skin {:?}: lowercase letters, digits, - and _, or \"\" for the pack",
                f.skin
            )));
        }
        if f.opacity > 100 {
            return Err(D::Error::custom("opacity is a percent, 0 to 100"));
        }
        if !(10..=400).contains(&f.size) {
            return Err(D::Error::custom("size is a percent, 10 to 400"));
        }
        if f.width == 0 || f.height == 0 {
            return Err(D::Error::custom("width and height must be above 0"));
        }
        Ok(Self(f))
    }
}

/// `#RRGGBB` or `#RGB` as red, green, blue.
pub fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.strip_prefix('#')?;
    let digit = |i: usize, n: usize| u8::from_str_radix(h.get(i..i + n)?, 16).ok();
    match h.len() {
        6 => Some((digit(0, 2)?, digit(2, 2)?, digit(4, 2)?)),
        3 => Some((digit(0, 1)? * 17, digit(1, 1)? * 17, digit(2, 1)? * 17)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    pub fn name(self) -> &'static str {
        match self {
            Align::Left => "left",
            Align::Center => "center",
            Align::Right => "right",
        }
    }
}

/// `[captions]`: live captions from the microphone on the `captions` overlay.
#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LiveCaptions {
    pub enabled: bool,
    /// Microphone name as ffmpeg lists it (`ffmpeg -list_devices true -f dshow -i dummy`).
    pub device: String,
    /// faster-whisper model; runs on the GPU next to OBS.
    pub model: String,
}

impl Default for LiveCaptions {
    fn default() -> Self {
        Self {
            enabled: false,
            device: String::new(),
            model: "large-v3-turbo".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Capture {
    pub keys: bool,
    /// Editors and terminals: typed text is kept without asking Windows whether the focused
    /// field is a password box (which would switch Electron apps into screen reader mode).
    #[serde(alias = "keys_allow_apps")]
    pub keys_trusted_apps: Vec<String>,
    /// Apps where nothing is recorded or shown (password managers, Windows sign-in prompts).
    pub keys_deny_apps: Vec<String>,
    pub keys_deny_title_patterns: Vec<Pattern>,
    pub shell: bool,
    pub shell_spool: PathBuf,
    pub secret_patterns: Vec<Pattern>,
    pub git_roots: Vec<PathBuf>,
    pub git_max_depth: usize,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            keys: true,
            keys_trusted_apps: [
                "Code.exe",
                "Code - Insiders.exe",
                "Cursor.exe",
                "Zed.exe",
                "WindowsTerminal.exe",
                "wezterm-gui.exe",
                "alacritty.exe",
                "nvim.exe",
                "nvim-qt.exe",
                "neovide.exe",
                "pwsh.exe",
                "powershell.exe",
                "conhost.exe",
            ]
            .map(String::from)
            .into(),
            keys_deny_apps: [
                "Bitwarden.exe",
                "1Password.exe",
                "KeePass.exe",
                "KeePassXC.exe",
                "Dashlane.exe",
                "NordPass.exe",
                "Enpass.exe",
                "Proton Pass.exe",
                "CredentialUIBroker.exe",
                "consent.exe",
                "LogonUI.exe",
                "LockApp.exe",
            ]
            .map(String::from)
            .into(),
            keys_deny_title_patterns: patterns(&[
                "(?i)passw",
                "(?i)passcode",
                "(?i)bitwarden",
                "(?i)1password",
                "(?i)keepass",
                r"(?i)\.env",
                "(?i)secret",
                "(?i)credential",
                "(?i)log ?in",
                "(?i)sign ?in",
                "(?i)sign ?up",
                "(?i)two-factor|2fa|verification code|one-time code",
                "(?i)incognito|inprivate|private browsing",
            ]),
            shell: true,
            shell_spool: "state/shell-spool.jsonl".into(),
            secret_patterns: patterns(&[
                "sk-[A-Za-z0-9_-]{20,}",
                "ghp_[A-Za-z0-9]{30,}",
                "github_pat_[A-Za-z0-9_]{30,}",
                "AKIA[0-9A-Z]{16}",
                "AIza[0-9A-Za-z_-]{35}",
                "xox[baprs]-[A-Za-z0-9-]{10,}",
                "-----BEGIN [A-Z ]*PRIVATE KEY-----",
            ]),
            git_roots: Vec::new(),
            git_max_depth: 3,
        }
    }
}

/// A regex compiled (and thereby validated) when the config is loaded.
#[derive(Debug, Clone)]
pub struct Pattern(pub Regex);

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let src = String::deserialize(d)?;
        Regex::new(&src).map(Pattern).map_err(|e| {
            // regex errors span several lines; the last one names the problem.
            let msg = e.to_string();
            let reason = msg.lines().last().unwrap_or_default();
            serde::de::Error::custom(format!(
                "invalid regex {src:?}: {}",
                reason.trim_start_matches("error: ")
            ))
        })
    }
}

fn patterns(sources: &[&str]) -> Vec<Pattern> {
    sources
        .iter()
        .map(|s| Pattern(Regex::new(s).expect("default pattern is valid")))
        .collect()
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hotkeys {
    pub mark: String,
    pub checkpoint: String,
    pub privacy: String,
    pub brb: String,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            mark: "Ctrl+Alt+M".into(),
            checkpoint: "Ctrl+Alt+C".into(),
            privacy: "Ctrl+Alt+P".into(),
            brb: "Ctrl+Alt+B".into(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Post {
    pub auto_after_finalize: bool,
    pub use_vod: bool,
    pub transcriber: Transcriber,
    pub whisper_model: String,
    pub whisper_compute_type: String,
    pub language: String,
    pub frame_interval_s: u32,
    pub scene_threshold: f64,
    pub phash_max_distance: u32,
    pub frame_width: u32,
    pub silence_db: f64,
    pub silence_min_s: f64,
    pub focus_min_keys_per_10s: u32,
    pub ocr: bool,
    /// Which OCR reads the frames (`post.ocr`).
    pub ocr_engine: OcrEngine,
    /// Frames GLM-OCR reads at once (one model on the GPU, several in flight).
    pub ocr_parallel: u32,
    /// Words the transcriber should expect (names, tools, jargon), e.g. "Rust", "cargo".
    pub vocabulary: Vec<String>,
    /// Let a local model replace misheard words with the stream's terms (stage `correct`).
    pub correct: bool,
    /// The correction model, `owner/repo/file.gguf` on Hugging Face.
    pub correct_model: String,
    /// Look-alike words Whisper was less sure of than this are checked (0 to 1).
    pub correct_below: f64,
}

impl Default for Post {
    fn default() -> Self {
        Self {
            auto_after_finalize: false,
            use_vod: false,
            transcriber: Transcriber::FasterWhisper,
            whisper_model: "large-v3-turbo".into(),
            whisper_compute_type: "float16".into(),
            language: "auto".into(),
            frame_interval_s: 30,
            scene_threshold: 0.30,
            phash_max_distance: 6,
            frame_width: 1280,
            silence_db: -35.0,
            silence_min_s: 4.0,
            focus_min_keys_per_10s: 3,
            ocr: false,
            ocr_engine: OcrEngine::GlmOcr,
            ocr_parallel: 4,
            vocabulary: Vec::new(),
            correct: false,
            correct_model: "unsloth/Qwen3.5-4B-GGUF/Qwen3.5-4B-Q4_K_M.gguf".into(),
            correct_below: 0.95,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transcriber {
    FasterWhisper,
    Whisperx,
    WhisperCpp,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Render {
    pub encoder: Encoder,
    pub cq: u32,
    pub pad_start_ms: u32,
    pub pad_end_ms: u32,
    pub short_max_s: u32,
    pub keys_overlay: KeysOverlay,
    pub captions: Captions,
}

impl Default for Render {
    fn default() -> Self {
        Self {
            encoder: Encoder::H264Nvenc,
            cq: 21,
            pad_start_ms: 250,
            pad_end_ms: 400,
            short_max_s: 60,
            keys_overlay: KeysOverlay::Alpha,
            captions: Captions::Srt,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoder {
    H264Nvenc,
    Libx264,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OcrEngine {
    /// GLM-OCR (0.9B vision model) through llama.cpp on the GPU, installed by `aftercast setup`.
    GlmOcr,
    Tesseract,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KeysOverlay {
    Burn,
    Alpha,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Captions {
    Burn,
    Srt,
    None,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct External {
    /// Current quest JSON written by an external app; empty = none.
    pub current_quest_file: PathBuf,
}

#[cfg(test)]
mod tests {
    #[test]
    fn set_value_keeps_comments_validates_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftercast.toml");
        std::fs::write(
            &path,
            "# my config
[stream]
chapter_on_brb = true  # BRB chapters
",
        )
        .unwrap();
        set_value(&path, "stream.chapter_on_brb", Some(Value::Bool(false))).unwrap();
        set_value(&path, "tui.wrap_lines", Some(Value::Bool(true))).unwrap();
        set_value(
            &path,
            "bot.moderation.delete_links_from_non_mods",
            Some(Value::Bool(true)),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(
                "# my config
"
            ),
            "{text}"
        );
        assert!(
            text.contains("chapter_on_brb = false # BRB chapters"),
            "{text}"
        );
        let config = Config::load(&path).unwrap();
        assert!(!config.stream.chapter_on_brb);
        assert!(config.tui.wrap_lines);
        assert!(config.bot.moderation.delete_links_from_non_mods);

        // A value of the wrong type is refused and the file is untouched.
        let err = set_value(
            &path,
            "stream.chapter_on_brb",
            Some(Value::Text("x".into())),
        );
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);

        // None removes the key: the default applies again.
        set_value(&path, "stream.chapter_on_brb", None).unwrap();
        assert!(Config::load(&path).unwrap().stream.chapter_on_brb);
    }

    use super::*;
    use crate::home::DEFAULT_CONFIG;

    fn parse(text: &str) -> Result<Config> {
        parse_toml(Path::new("aftercast.toml"), text)
    }

    fn err(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn overlay_frames_keep_their_own_defaults_and_validate() {
        let c = parse("[overlay.keys]\nx = -100\nalign = \"right\"\n").unwrap();
        assert_eq!((c.overlay.keys.x, c.overlay.keys.y), (-100, FRAMES[6].2));
        assert_eq!(c.overlay.keys.align, Align::Right);
        assert_eq!(c.overlay.keys.width, FRAMES[6].3);
        assert_eq!(c.overlay.captions, Framed::default());
        assert!(err("[overlay.keys]\nz = 1\n").contains("unknown field"));
        assert!(err("[overlay.keys]\nalign = \"middle\"\n").contains("unknown variant"));
        assert!(err("[overlay.now]\nbg = \"red\"\n").contains("#RRGGBB"));
        assert!(err("[overlay.now]\nopacity = 150\n").contains("percent"));
        assert_eq!(rgb("#0af"), Some((0, 170, 255)));
        assert_eq!(rgb("#FF5555"), Some((255, 85, 85)));
        assert_eq!(rgb("#FF555"), None);

        // Skins: a pack for all, an overlay's own on top; names that could not be files
        // are refused.
        let c = parse("[overlay]\nskin = \"clean\"\n[overlay.keys]\nskin = \"my-keys\"\n").unwrap();
        assert_eq!(
            (c.overlay.skin.as_str(), c.overlay.keys.skin.as_str()),
            ("clean", "my-keys")
        );
        assert_eq!(Config::default().overlay.skin, "nvim");
        assert!(err("[overlay]\nskin = \"../x\"\n").contains("skin"));
        assert!(err("[overlay.keys]\nskin = \"A B\"\n").contains("skin"));

        // Order: as set, then the rest as built in; unknown names are refused.
        let c = parse("[overlay]\norder = [\"keys\", \"now\", \"keys\"]\n").unwrap();
        let order = c.overlay.order();
        assert_eq!(&order[..3], ["keys", "now", "toast"]);
        assert_eq!(order.len(), 11);

        // The status bar's options: only dockable names in the order.
        let c = parse("[overlay.bar]\norder = [\"toast\", \"now\"]\nmarquee = true\nspeed = 90\n")
            .unwrap();
        assert_eq!(c.overlay.bar.order, ["toast", "now"]);
        assert!(c.overlay.bar.marquee && c.overlay.bar.animate);
        assert!(err("[overlay.bar]\norder = [\"keys\"]\n").contains("can dock"));
        assert_eq!(Config::default().overlay.sound.volume, 70);

        // Behaviour: dock only the dockable, hide_when names another overlay.
        let c = parse(
            "[overlay.now]
dock = true
[overlay.keys]
hide_when = [\"question\"]
",
        )
        .unwrap();
        assert!(c.overlay.now.dock && !c.overlay.chapters.dock);
        assert_eq!(c.overlay.keys.hide_when, ["question"]);
        assert!(
            err("[overlay.keys]
dock = true
")
            .contains("cannot dock")
        );
        assert!(
            err("[overlay.keys]
hide_when = [\"keys\"]
")
            .contains("hide_when")
        );
        assert!(
            err("[overlay.keys]
hide_when = [\"nope\"]
")
            .contains("hide_when")
        );
        assert!(err("[overlay]\norder = [\"nope\"]\n").contains("unknown overlay"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftercast.toml");
        let list = Value::List(vec!["question".into(), "keys".into()]);
        set_value(&path, "overlay.order", Some(list)).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().overlay.order()[..2],
            ["question", "keys"]
        );
    }

    #[test]
    fn default_file_parses() {
        let c = parse(DEFAULT_CONFIG).unwrap();
        assert_eq!(c.channel.episode_pad, 3);
        assert_eq!(c.obs.scenes.brb, "BRB");
        assert_eq!(c.bot.commands["links"].reply, Some(ReplyMode::Chat));
        assert_eq!(
            c.vars["support"]["kofi"].as_str(),
            Some("https://ko-fi.com/you")
        );
        assert_eq!(c.capture.secret_patterns.len(), 7);
        assert!(
            c.capture.secret_patterns[1]
                .0
                .is_match(&format!("ghp_{}", "a".repeat(36)))
        );
        assert_eq!(c.post.transcriber, Transcriber::FasterWhisper);
        assert_eq!(c.render.encoder, Encoder::H264Nvenc);
        assert_eq!(c.youtube.bot_profile, None);
    }

    #[test]
    fn empty_file_yields_defaults_matching_default_file() {
        let empty = parse("").unwrap();
        let file = parse(DEFAULT_CONFIG).unwrap();
        // Spot-check a value from every section that has machine-independent defaults.
        assert_eq!(empty.channel.title_format, file.channel.title_format);
        assert_eq!(empty.paths.yt_dlp, file.paths.yt_dlp);
        assert_eq!(
            empty.youtube.daily_quota_reserve,
            file.youtube.daily_quota_reserve
        );
        assert_eq!(empty.obs.password, file.obs.password);
        assert_eq!(
            empty.stream.default_latency_s,
            file.stream.default_latency_s
        );
        assert_eq!(empty.readme.link_style, file.readme.link_style);
        assert_eq!(empty.bot.user_cooldown_s, file.bot.user_cooldown_s);
        assert_eq!(empty.overlay.accent, file.overlay.accent);
        assert_eq!(
            empty.capture.keys_trusted_apps,
            file.capture.keys_trusted_apps
        );
        assert_eq!(empty.capture.keys_deny_apps, file.capture.keys_deny_apps);
        assert_eq!(
            empty.capture.keys_deny_title_patterns.len(),
            file.capture.keys_deny_title_patterns.len()
        );
        assert_eq!(empty.hotkeys.checkpoint, file.hotkeys.checkpoint);
        assert_eq!(empty.post.silence_db, file.post.silence_db);
        assert_eq!(empty.render.keys_overlay, file.render.keys_overlay);
    }

    #[test]
    fn unknown_key_reports_location() {
        let e = err("[channel]\nshort = \"IDD\"\nshrot = \"x\"\n");
        assert!(e.starts_with("aftercast.toml:3:1: "), "{e}");
        assert!(e.contains("shrot"), "{e}");
    }

    #[test]
    fn invalid_values_report_location() {
        let e = err("[render]\nencoder = \"h265\"\n");
        assert!(e.starts_with("aftercast.toml:2:11: "), "{e}");

        let e = err("[capture]\nsecret_patterns = [\"ok\", \"(unclosed\"]\n");
        assert!(e.starts_with("aftercast.toml:2:"), "{e}");
        assert!(e.contains("invalid regex \"(unclosed\""), "{e}");

        let e = err("[overlay]\nport = 70000\n");
        assert!(e.starts_with("aftercast.toml:2:8: "), "{e}");
    }

    #[test]
    fn syntax_error_reports_location() {
        let e = err("[channel\n");
        assert!(e.starts_with("aftercast.toml:1:"), "{e}");
        assert!(!e.contains('\n'), "{e}");
    }

    #[test]
    fn missing_file_is_a_config_error() {
        let e = Config::load(Path::new("does/not/exist.toml")).unwrap_err();
        assert!(matches!(e, Error::ConfigMissing(_)));
        assert_eq!(e.exit_code(), crate::exit::CONFIG);
    }

    #[test]
    fn secret_refs() {
        assert!(is_secret_ref("keyring:obs"));
        assert!(is_secret_ref("env:OBS_PASSWORD"));
        assert!(is_secret_ref(""));
        assert!(!is_secret_ref("hunter2"));
    }

    #[test]
    fn brb_screen_is_arranged_per_skin() {
        let c = parse(
            "[overlay.brb_screen]
pfp_size = 300
pfp_x = 10

[overlay.brb_screen.skins.glass]
pfp_size = 500
qr_y = 7
",
        )
        .unwrap();
        let glass = c.overlay.brb_screen.effective("glass");
        assert_eq!(
            (
                glass["pfp_size"].as_i64(),
                glass["pfp_x"].as_i64(),
                glass["qr_y"].as_i64()
            ),
            (Some(500), Some(10), Some(7))
        );
        let plain = c.overlay.brb_screen.effective("clean");
        assert_eq!(
            (plain["pfp_size"].as_i64(), plain["qr_y"].as_i64()),
            (Some(300), None)
        );
    }
}
