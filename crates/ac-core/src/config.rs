//! `aftercast.toml`: typed config, loader and validation (specs/03-config.md).
//!
//! Validation happens during deserialization (enums, regexes, unknown keys), so every
//! error carries the file, line and column of the offending key or value.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Deserializer};

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
    pub capture: Capture,
    pub hotkeys: Hotkeys,
    pub post: Post,
    pub render: Render,
    pub external: External,
    pub tui: Tui,
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

/// Home's arrangement (`tui.home_layout`; `v` on Home cycles them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HomeLayout {
    /// What needs you on the left; audio and the scene on air on the right.
    Columns,
    /// One full-width page: what needs you, audio, the scene on air.
    Page,
    /// Checks on the left; audio and the scene's sources, one per line, on the right.
    Classic,
    /// Two tabs, Checks and Studio, each with the whole page.
    Tabs,
}

impl HomeLayout {
    pub const ALL: [HomeLayout; 4] = [
        HomeLayout::Columns,
        HomeLayout::Page,
        HomeLayout::Classic,
        HomeLayout::Tabs,
    ];

    pub fn name(self) -> &'static str {
        match self {
            HomeLayout::Columns => "columns",
            HomeLayout::Page => "page",
            HomeLayout::Classic => "classic",
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
}

/// Sets the dotted `key` (e.g. `stream.chapter_on_brb`) in the config file, or removes it
/// with `None` so the default applies again. Comments and layout of the rest of the file
/// are kept. The result is validated first: an invalid value is refused and nothing is
/// written.
pub fn set_value(path: &Path, key: &str, value: Option<Value>) -> Result<()> {
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
    let parts: Vec<&str> = key.split('.').collect();
    let (name, tables) = parts.split_last().expect("split yields one part");
    // In a file of only comments they are the document's trailing text; keep them on top.
    let only_comments = doc.as_table().is_empty();
    let mut table = doc.as_table_mut();
    for part in tables {
        if !table.contains_key(part) {
            let mut new = toml_edit::Table::new();
            new.set_implicit(false);
            table.insert(part, toml_edit::Item::Table(new));
        }
        table = table[part]
            .as_table_mut()
            .ok_or_else(|| refuse(format!("{key}: {part} is not a table")))?;
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
    if only_comments
        && let Some(first) = tables.first()
        && let Some(t) = doc.get_mut(first).and_then(toml_edit::Item::as_table_mut)
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
    pub scenes: ObsScenes,
}

impl Default for Obs {
    fn default() -> Self {
        Self {
            url: "ws://127.0.0.1:4455".into(),
            password: "keyring:obs".into(),
            require_recording: true,
            record_into_session: true,
            scenes: ObsScenes::default(),
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

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Overlay {
    pub port: u16,
    pub font_mono: String,
    pub font_sans: String,
    pub bg: String,
    pub accent: String,
    pub fg: String,
    pub muted: String,
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
}
