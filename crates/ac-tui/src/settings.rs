//! The settings page (`,`): the settings worth changing from the TUI, grouped in
//! sections, each saved to `aftercast.toml` the moment it changes (comments kept), then
//! reloaded so it applies live where it can.

use std::path::Path;
use std::time::{Duration, Instant};

use ac_core::command::Command;
use ac_core::config::{
    Captions, Config, Encoder, KeysOverlay, OcrEngine, ReplyMode, Transcriber, Value,
};
use ac_core::obs::Media;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};

use crate::theme::{ACCENT, FAINT, INFO, MUTED, OK, RAISED, SELECTED, TEXT, WARN, hint, mix};

/// What a setting holds and how it is changed.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Bool,
    Int {
        min: i64,
        max: i64,
        step: i64,
    },
    Float {
        min: f64,
        max: f64,
        step: f64,
    },
    Text,
    /// Names separated by commas, saved as a list.
    List,
    Choice(&'static [&'static str]),
}

/// When a change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    Now,
    Restart,
    NextSession,
    NextPost,
}

impl When {
    /// One or two words, for the tags under a setting.
    pub fn short(self) -> &'static str {
        match self {
            When::Now => "now",
            When::Restart => "after restart",
            When::NextSession => "next session",
            When::NextPost => "next post",
        }
    }

    pub fn text(self) -> &'static str {
        match self {
            When::Now => "applies now",
            When::Restart => "applies after restarting aftercast",
            When::NextSession => "applies from the next session",
            When::NextPost => "applies to the next post-processing or render",
        }
    }
}

pub struct Setting {
    pub section: usize,
    /// Sub-heading within the section ("" for none).
    pub group: &'static str,
    /// Dotted key in `aftercast.toml`.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub when: When,
    pub get: fn(&Config) -> Value,
}

pub const SECTIONS: [&str; 9] = [
    "Captions", "Capture", "Chat bot", "Display", "OBS", "OBS live", "Post", "Stream", "YouTube",
];

/// The section listing what OBS has right now: audio inputs and every scene's sources.
pub const OBS_LIVE: usize = 5;

/// One row of the page: a setting from `aftercast.toml`, or something live in OBS.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Row {
    Setting(usize),
    Audio(usize),
    Item(usize),
}

fn text(s: &str) -> Value {
    Value::Text(s.to_owned())
}

/// Every setting on the page, in display order.
pub fn all() -> Vec<Setting> {
    use Kind::*;
    use When::*;
    let s = |section, group, key, label, help, kind, when, get| Setting {
        section,
        group,
        key,
        label,
        help,
        kind,
        when,
        get,
    };
    let mut settings = vec![
        // Captions
        s(
            0,
            "Captions",
            "captions.enabled",
            "Live captions",
            "Caption your microphone on the captions overlay.",
            Bool,
            Now,
            |c| Value::Bool(c.captions.enabled),
        ),
        s(
            0,
            "Captions",
            "captions.device",
            "Microphone",
            "Name as ffmpeg lists it: ffmpeg -list_devices true -f dshow -i dummy.",
            Text,
            Now,
            |c| text(&c.captions.device),
        ),
        s(
            0,
            "Captions",
            "captions.model",
            "Captions model",
            "Smaller is faster: base.en and small.en keep up best; .en models are English only.",
            Choice(&[
                "tiny.en",
                "base.en",
                "small.en",
                "small",
                "medium",
                "distil-large-v3",
                "large-v3-turbo",
            ]),
            Now,
            |c| text(&c.captions.model),
        ),
        s(
            0,
            "Sync",
            "stream.delay_ms",
            "Stream delay (ms)",
            "OBS shows cameras, captures and audio this late so captions land on the words; overlays and scene switches wait too. 0 is off.",
            Int {
                min: 0,
                max: 500,
                step: 50,
            },
            Now,
            |c| Value::Int(c.stream.delay_ms.into()),
        ),
        // Capture
        s(
            1,
            "",
            "capture.keys",
            "Record key presses",
            "Record typed keys (never in privacy or password boxes).",
            Bool,
            Restart,
            |c| Value::Bool(c.capture.keys),
        ),
        s(
            1,
            "",
            "capture.shell",
            "Record shell commands",
            "Record shell commands (aftercast shell-hook).",
            Bool,
            Restart,
            |c| Value::Bool(c.capture.shell),
        ),
        // Chat bot
        s(
            2,
            "",
            "bot.enabled",
            "Chat bot",
            "Answer !commands in chat.",
            Bool,
            Restart,
            |c| Value::Bool(c.bot.enabled),
        ),
        s(
            2,
            "",
            "bot.prefix",
            "Command prefix",
            "What chat commands start with.",
            Text,
            Restart,
            |c| text(&c.bot.prefix),
        ),
        s(
            2,
            "",
            "bot.default_reply",
            "Replies go to",
            "Where replies go; chat costs 20 units each.",
            Choice(&["overlay", "chat", "both"]),
            Restart,
            |c| {
                text(match c.bot.default_reply {
                    ReplyMode::Overlay => "overlay",
                    ReplyMode::Chat => "chat",
                    ReplyMode::Both => "both",
                })
            },
        ),
        s(
            2,
            "",
            "bot.global_cooldown_s",
            "Global cooldown (s)",
            "Least time between two replies.",
            Int {
                min: 0,
                max: 600,
                step: 1,
            },
            Restart,
            |c| Value::Int(c.bot.global_cooldown_s as i64),
        ),
        s(
            2,
            "",
            "bot.user_cooldown_s",
            "Per-viewer cooldown (s)",
            "Least time between one viewer's commands.",
            Int {
                min: 0,
                max: 3600,
                step: 5,
            },
            Restart,
            |c| Value::Int(c.bot.user_cooldown_s as i64),
        ),
        s(
            2,
            "",
            "bot.max_chat_replies_per_hour",
            "Chat replies per hour",
            "Chat replies per hour; more go to the overlay.",
            Int {
                min: 0,
                max: 1000,
                step: 5,
            },
            Restart,
            |c| Value::Int(c.bot.max_chat_replies_per_hour.into()),
        ),
        // Display
        s(
            3,
            "",
            "tui.wrap_lines",
            "Wrap long lines",
            "Wrap long lines instead of cutting them.",
            Bool,
            Now,
            |c| Value::Bool(c.tui.wrap_lines),
        ),
        s(
            3,
            "",
            "tui.animations",
            "Animations",
            "Pulsing dots, spinners, gliding panes.",
            Bool,
            Now,
            |c| Value::Bool(c.tui.animations),
        ),
        s(
            3,
            "",
            "tui.name_colors",
            "Name colors",
            "A color per chat author.",
            Bool,
            Now,
            |c| Value::Bool(c.tui.name_colors),
        ),
        s(
            3,
            "",
            "tui.home_layout",
            "Home layout",
            "columns: a grid with scene tabs. tabs: Checks, Mixer, Sources.",
            Choice(&["columns", "tabs"]),
            Now,
            |c| text(c.tui.home_layout.name()),
        ),
        // OBS
        s(
            4,
            "Scenes",
            "obs.scenes.main",
            "Main scene",
            "The primary scene: BRB and privacy return to it; Enter on Starting Soon goes to it.",
            Text,
            Now,
            |c| text(&c.obs.scenes.main),
        ),
        s(
            4,
            "Scenes",
            "obs.scenes.brb",
            "BRB scene",
            "Where b switches to.",
            Text,
            Now,
            |c| text(&c.obs.scenes.brb),
        ),
        s(
            4,
            "Scenes",
            "obs.scenes.privacy",
            "Privacy scene",
            "Where p switches to.",
            Text,
            Now,
            |c| text(&c.obs.scenes.privacy),
        ),
        s(
            4,
            "Scenes",
            "obs.scenes.starting",
            "Starting scene",
            "Where Go Live opens (with the starting screen on).",
            Text,
            Now,
            |c| text(&c.obs.scenes.starting),
        ),
        s(
            4,
            "Scenes",
            "obs.scenes.ending",
            "Ending scene",
            "The end screen :end shows.",
            Text,
            Now,
            |c| text(&c.obs.scenes.ending),
        ),
        s(
            4,
            "Recording",
            "obs.require_recording",
            "Warn when not recording",
            "Warn while OBS is not recording.",
            Bool,
            Now,
            |c| Value::Bool(c.obs.require_recording),
        ),
        s(
            4,
            "Recording",
            "obs.record_into_session",
            "Record into the session folder",
            "Record into the session's media folder.",
            Bool,
            Restart,
            |c| Value::Bool(c.obs.record_into_session),
        ),
        s(
            2,
            "Notifications",
            "notify.supporters",
            "Notify for supporters",
            "A Super Chat, Super Sticker or membership pops up a desktop notification.",
            Bool,
            Now,
            |c| Value::Bool(c.notify.supporters),
        ),
        s(
            2,
            "Notifications",
            "notify.sound",
            "Notification sound",
            "Play Windows' notification sound with it.",
            Bool,
            Now,
            |c| Value::Bool(c.notify.sound),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.enabled",
            "Hide the camera under the pointer",
            "The camera source hides while the mouse pointer is over its place on the screen, and returns after.",
            Bool,
            Now,
            |c| Value::Bool(c.obs.facecam.enabled),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.auto",
            "Follow the camera in OBS",
            "Read where the camera is from OBS in each scene. The place below is used until OBS answers, or when this is off.",
            Bool,
            Now,
            |c| Value::Bool(c.obs.facecam.auto),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.monitor",
            "Screen number",
            "The screen the pointer moves on, as Windows Settings numbers it (1, 2, ...). 0 is the one the scene's display capture shows.",
            Int {
                min: 0,
                max: 8,
                step: 1,
            },
            Now,
            |c| Value::Int(c.obs.facecam.monitor.into()),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.source",
            "Camera source",
            "The OBS source to hide, e.g. Video Capture Device (in the scene on air).",
            Text,
            Now,
            |c| text(&c.obs.facecam.source),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.x",
            "Camera left",
            "Left edge of the camera on the 1920x1080 canvas (the screen OBS captures).",
            Int {
                min: 0,
                max: 1920,
                step: 10,
            },
            Now,
            |c| Value::Int(c.obs.facecam.x.into()),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.y",
            "Camera top",
            "Top edge of the camera on the canvas.",
            Int {
                min: 0,
                max: 1080,
                step: 10,
            },
            Now,
            |c| Value::Int(c.obs.facecam.y.into()),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.width",
            "Camera width",
            "Width of the camera on the canvas.",
            Int {
                min: 1,
                max: 1920,
                step: 10,
            },
            Now,
            |c| Value::Int(c.obs.facecam.width.into()),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.height",
            "Camera height",
            "Height of the camera on the canvas.",
            Int {
                min: 1,
                max: 1080,
                step: 10,
            },
            Now,
            |c| Value::Int(c.obs.facecam.height.into()),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.margin",
            "Margin around it",
            "Pixels around the camera that count as over it.",
            Int {
                min: 0,
                max: 400,
                step: 10,
            },
            Now,
            |c| Value::Int(c.obs.facecam.margin.into()),
        ),
        s(
            4,
            "Facecam",
            "obs.facecam.return_ms",
            "Return after (ms)",
            "The camera comes back this long after the pointer left, so it does not flicker.",
            Int {
                min: 0,
                max: 5000,
                step: 100,
            },
            Now,
            |c| Value::Int(c.obs.facecam.return_ms.into()),
        ),
        s(
            4,
            "Status bar",
            "overlay.bar.order",
            "Segment order",
            "Docked overlays from left to right, comma separated: now, brb, latency, chapters, toast.",
            List,
            Now,
            |c| Value::List(c.overlay.bar.order.clone()),
        ),
        s(
            4,
            "Status bar",
            "overlay.bar.animate",
            "Animate segments",
            "Segments slide in and out, and a changed value flashes.",
            Bool,
            Now,
            |c| Value::Bool(c.overlay.bar.animate),
        ),
        s(
            4,
            "Status bar",
            "overlay.bar.marquee",
            "Scroll like a ticker",
            "The segments scroll left forever.",
            Bool,
            Now,
            |c| Value::Bool(c.overlay.bar.marquee),
        ),
        s(
            4,
            "Status bar",
            "overlay.bar.speed",
            "Ticker speed (px/s)",
            "How fast the segments scroll when the ticker is on.",
            Int {
                min: 10,
                max: 600,
                step: 10,
            },
            Now,
            |c| Value::Int(c.overlay.bar.speed.into()),
        ),
        s(
            4,
            "Alert sound",
            "overlay.sound.enabled",
            "Alert sound",
            "Sound effects of the Super Chat alert (the overlay's audio goes through OBS).",
            Bool,
            Now,
            |c| Value::Bool(c.overlay.sound.enabled),
        ),
        s(
            4,
            "Alert sound",
            "overlay.sound.skin",
            "Alert sound skin",
            "synth, or a folder in <home>/overlays/sounds with sound files named after what they are for.",
            Text,
            Now,
            |c| text(&c.overlay.sound.skin),
        ),
        s(
            4,
            "Alert sound",
            "overlay.sound.volume",
            "Alert volume %",
            "Loudness of the alert's sound effects.",
            Int {
                min: 0,
                max: 100,
                step: 5,
            },
            Now,
            |c| Value::Int(c.overlay.sound.volume.into()),
        ),
        s(
            4,
            "Alert sound",
            "overlay.sound.brb_fade",
            "BRB music fade (s)",
            "Seconds each BRB track takes to fade in from silence and out to silence (0: no fade).",
            Int {
                min: 0,
                max: 30,
                step: 1,
            },
            Now,
            |c| Value::Int(c.overlay.sound.brb_fade.into()),
        ),
        s(
            4,
            "Alert sound",
            "overlay.sound.brb_clip",
            "BRB music clip (s)",
            "Seconds played from a random place in each BRB track (0: the whole track; a shorter track plays in full).",
            Int {
                min: 0,
                max: 600,
                step: 5,
            },
            Now,
            |c| Value::Int(c.overlay.sound.brb_clip.into()),
        ),
        s(
            4,
            "Overlay look",
            "overlay.font_sans",
            "Overlay font",
            "Font of the overlay cards.",
            Text,
            Now,
            |c| text(&c.overlay.font_sans),
        ),
        s(
            4,
            "Overlay look",
            "overlay.font_mono",
            "Overlay mono font",
            "Font of timers and typed keys.",
            Text,
            Now,
            |c| text(&c.overlay.font_mono),
        ),
        s(
            4,
            "Overlay look",
            "overlay.bg",
            "Overlay background",
            "Card color, e.g. #303030.",
            Text,
            Now,
            |c| text(&c.overlay.bg),
        ),
        s(
            4,
            "Overlay look",
            "overlay.accent",
            "Overlay accent",
            "Edge and highlight color, e.g. #FF5555.",
            Text,
            Now,
            |c| text(&c.overlay.accent),
        ),
        s(
            4,
            "Overlay look",
            "overlay.fg",
            "Overlay text color",
            "e.g. #FFFFFF.",
            Text,
            Now,
            |c| text(&c.overlay.fg),
        ),
        s(
            4,
            "Overlay look",
            "overlay.muted",
            "Overlay muted color",
            "Timers and key borders, e.g. #9A9A9A.",
            Text,
            Now,
            |c| text(&c.overlay.muted),
        ),
        // Post
        s(
            6,
            "Source",
            "post.use_vod",
            "Use the YouTube VOD",
            "Use the YouTube VOD, not the recording.",
            Bool,
            NextPost,
            |c| Value::Bool(c.post.use_vod),
        ),
        s(
            6,
            "Transcription",
            "post.transcriber",
            "Transcriber",
            "Speech to text engine.",
            Choice(&["faster-whisper", "whisperx", "whisper-cpp"]),
            NextPost,
            |c| {
                text(match c.post.transcriber {
                    Transcriber::FasterWhisper => "faster-whisper",
                    Transcriber::Whisperx => "whisperx",
                    Transcriber::WhisperCpp => "whisper-cpp",
                })
            },
        ),
        s(
            6,
            "Transcription",
            "post.whisper_model",
            "Whisper model",
            "Model name, or a ggml .bin for whisper-cpp.",
            Text,
            NextPost,
            |c| text(&c.post.whisper_model),
        ),
        s(
            6,
            "Transcription",
            "post.whisper_compute_type",
            "Whisper precision",
            "int8_float16 or int8 if video memory runs out.",
            Choice(&["float16", "int8_float16", "int8", "float32"]),
            NextPost,
            |c| text(&c.post.whisper_compute_type),
        ),
        s(
            6,
            "Transcription",
            "post.language",
            "Language",
            "auto, or a code such as en or hi.",
            Text,
            NextPost,
            |c| text(&c.post.language),
        ),
        s(
            6,
            "Transcription",
            "post.correct",
            "Fix misheard terms",
            "Fix misheard terms with a small local model.",
            Bool,
            NextPost,
            |c| Value::Bool(c.post.correct),
        ),
        s(
            6,
            "Transcription",
            "post.correct_model",
            "Correction model",
            "owner/repo/file.gguf on Hugging Face.",
            Text,
            NextPost,
            |c| text(&c.post.correct_model),
        ),
        s(
            6,
            "Transcription",
            "post.correct_below",
            "Check words below",
            "Words Whisper was less sure of than this get checked.",
            Float {
                min: 0.0,
                max: 1.0,
                step: 0.05,
            },
            NextPost,
            |c| Value::Float(c.post.correct_below),
        ),
        s(
            6,
            "Screen",
            "post.frame_interval_s",
            "Frame every (s)",
            "Sample a frame this often (scene changes add more).",
            Int {
                min: 5,
                max: 300,
                step: 5,
            },
            NextPost,
            |c| Value::Int(c.post.frame_interval_s.into()),
        ),
        s(
            6,
            "Screen",
            "post.scene_threshold",
            "Scene change threshold",
            "How different a frame must be to count as a new scene.",
            Float {
                min: 0.05,
                max: 1.0,
                step: 0.05,
            },
            NextPost,
            |c| Value::Float(c.post.scene_threshold),
        ),
        s(
            6,
            "Screen",
            "post.phash_max_distance",
            "Duplicate frame distance",
            "Frames closer than this are kept once.",
            Int {
                min: 0,
                max: 32,
                step: 1,
            },
            NextPost,
            |c| Value::Int(c.post.phash_max_distance.into()),
        ),
        s(
            6,
            "Screen",
            "post.frame_width",
            "Frame width",
            "Width frames are saved at.",
            Int {
                min: 320,
                max: 3840,
                step: 160,
            },
            NextPost,
            |c| Value::Int(c.post.frame_width.into()),
        ),
        s(
            6,
            "Screen",
            "post.ocr",
            "Read screen text (OCR)",
            "Read the text on screen in sampled frames.",
            Bool,
            NextPost,
            |c| Value::Bool(c.post.ocr),
        ),
        s(
            6,
            "Screen",
            "post.ocr_engine",
            "OCR engine",
            "glm-ocr (GPU, best on code) or tesseract (CPU).",
            Choice(&["glm-ocr", "tesseract"]),
            NextPost,
            |c| {
                text(match c.post.ocr_engine {
                    OcrEngine::GlmOcr => "glm-ocr",
                    OcrEngine::Tesseract => "tesseract",
                })
            },
        ),
        s(
            6,
            "Screen",
            "post.ocr_parallel",
            "OCR in parallel",
            "Frames GLM-OCR reads at once.",
            Int {
                min: 1,
                max: 16,
                step: 1,
            },
            NextPost,
            |c| Value::Int(c.post.ocr_parallel.into()),
        ),
        s(
            6,
            "Screen",
            "capture.git_max_depth",
            "Git search depth",
            "How deep post looks for git repos under the git roots.",
            Int {
                min: 0,
                max: 10,
                step: 1,
            },
            NextPost,
            |c| Value::Int(c.capture.git_max_depth as i64),
        ),
        s(
            6,
            "Activity",
            "post.silence_db",
            "Silence level (dB)",
            "Quieter than this counts as silence.",
            Float {
                min: -80.0,
                max: 0.0,
                step: 1.0,
            },
            NextPost,
            |c| Value::Float(c.post.silence_db),
        ),
        s(
            6,
            "Activity",
            "post.silence_min_s",
            "Shortest silence (s)",
            "Silences shorter than this are ignored.",
            Float {
                min: 0.5,
                max: 60.0,
                step: 0.5,
            },
            NextPost,
            |c| Value::Float(c.post.silence_min_s),
        ),
        s(
            6,
            "Activity",
            "post.focus_min_keys_per_10s",
            "Focus typing rate",
            "Keys per 10 s that count as focused work.",
            Int {
                min: 0,
                max: 100,
                step: 1,
            },
            NextPost,
            |c| Value::Int(c.post.focus_min_keys_per_10s.into()),
        ),
        s(
            6,
            "Clips",
            "render.encoder",
            "Video encoder",
            "h264_nvenc (NVIDIA) or libx264.",
            Choice(&["h264_nvenc", "libx264"]),
            NextPost,
            |c| {
                text(match c.render.encoder {
                    Encoder::H264Nvenc => "h264_nvenc",
                    Encoder::Libx264 => "libx264",
                })
            },
        ),
        s(
            6,
            "Clips",
            "render.cq",
            "Video quality (CQ)",
            "Lower is better and bigger.",
            Int {
                min: 0,
                max: 51,
                step: 1,
            },
            NextPost,
            |c| Value::Int(c.render.cq.into()),
        ),
        s(
            6,
            "Clips",
            "render.captions",
            "Captions",
            "Burn subtitles, write an .srt, or none.",
            Choice(&["burn", "srt", "none"]),
            NextPost,
            |c| {
                text(match c.render.captions {
                    Captions::Burn => "burn",
                    Captions::Srt => "srt",
                    Captions::None => "none",
                })
            },
        ),
        s(
            6,
            "Clips",
            "render.keys_overlay",
            "Keys overlay in clips",
            "Burn key presses, an alpha layer, or none.",
            Choice(&["burn", "alpha", "none"]),
            NextPost,
            |c| {
                text(match c.render.keys_overlay {
                    KeysOverlay::Burn => "burn",
                    KeysOverlay::Alpha => "alpha",
                    KeysOverlay::None => "none",
                })
            },
        ),
        s(
            6,
            "Clips",
            "render.pad_start_ms",
            "Clip lead-in (ms)",
            "Extra time before each clip.",
            Int {
                min: 0,
                max: 3000,
                step: 50,
            },
            NextPost,
            |c| Value::Int(c.render.pad_start_ms.into()),
        ),
        s(
            6,
            "Clips",
            "render.pad_end_ms",
            "Clip tail (ms)",
            "Extra time after each clip.",
            Int {
                min: 0,
                max: 3000,
                step: 50,
            },
            NextPost,
            |c| Value::Int(c.render.pad_end_ms.into()),
        ),
        s(
            6,
            "Clips",
            "render.short_max_s",
            "Longest short (s)",
            "Shorts are cut to at most this long.",
            Int {
                min: 15,
                max: 180,
                step: 5,
            },
            NextPost,
            |c| Value::Int(c.render.short_max_s.into()),
        ),
        // Stream
        s(
            7,
            "Show",
            "channel.series",
            "Series name",
            "The show's name, in titles and the captions prompt.",
            Text,
            NextSession,
            |c| text(&c.channel.series),
        ),
        s(
            7,
            "Show",
            "channel.short",
            "Short name",
            "Short form of the series, e.g. IDD.",
            Text,
            NextSession,
            |c| text(&c.channel.short),
        ),
        s(
            7,
            "Show",
            "channel.episode_prefix",
            "Episode prefix",
            "Before the episode number in session folders, e.g. ep.",
            Text,
            NextSession,
            |c| text(&c.channel.episode_prefix),
        ),
        s(
            7,
            "Show",
            "channel.episode_pad",
            "Episode digits",
            "Episode numbers are padded to this many digits.",
            Int {
                min: 1,
                max: 6,
                step: 1,
            },
            NextSession,
            |c| Value::Int(c.channel.episode_pad as i64),
        ),
        s(
            7,
            "Show",
            "stream.first_chapter_title",
            "First chapter title",
            "Title of the 0:00 chapter.",
            Text,
            Now,
            |c| text(&c.stream.first_chapter_title),
        ),
        s(
            7,
            "Chapters",
            "stream.chapter_on_brb",
            "Chapter on BRB",
            "BRB adds a \"Break\" chapter.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_brb),
        ),
        s(
            7,
            "Chapters",
            "stream.chapter_on_question",
            "Chapter on question",
            "A question shown on stream adds a \"Q: …\" chapter.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_question),
        ),
        s(
            7,
            "Chapters",
            "stream.chapter_on_quest_change",
            "Chapter on new quest",
            "A new quest starts a chapter.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_quest_change),
        ),
        s(
            7,
            "Chapters",
            "stream.chapter_on_latency",
            "Chapter on latency check",
            ":latency adds a \"Latency check\" chapter.",
            Bool,
            Now,
            |c| Value::Bool(c.stream.chapter_on_latency),
        ),
        // YouTube
        s(
            8,
            "Description",
            "youtube.push_on_start",
            "Push description at start",
            "Push the description at start (50 units).",
            Bool,
            NextSession,
            |c| Value::Bool(c.youtube.push_on_start),
        ),
        s(
            8,
            "Description",
            "youtube.push_on_end",
            "Push description at end",
            "Push the final description at the end (50 units).",
            Bool,
            Now,
            |c| Value::Bool(c.youtube.push_on_end),
        ),
        s(
            8,
            "Chat",
            "youtube.chat_idle_max_s",
            "Chat poll when idle (s)",
            "Longest wait between chat polls when chat is quiet.",
            Int {
                min: 5,
                max: 300,
                step: 5,
            },
            Restart,
            |c| Value::Int(c.youtube.chat_idle_max_s as i64),
        ),
        s(
            8,
            "Chat",
            "stream.default_latency_s",
            "Default latency (s)",
            "Stream delay until :latency measures it.",
            Float {
                min: 0.0,
                max: 600.0,
                step: 0.5,
            },
            Now,
            |c| Value::Float(c.stream.default_latency_s),
        ),
        s(
            8,
            "Quota",
            "youtube.daily_quota_reserve",
            "Quota reserve",
            "Quota kept for manual actions.",
            Int {
                min: 0,
                max: 10_000,
                step: 100,
            },
            Restart,
            |c| Value::Int(c.youtube.daily_quota_reserve.into()),
        ),
    ];
    // Each overlay's frame, next to what drives it.
    settings.extend(frame!(0, "Captions", captions));
    settings.extend(frame!(1, "Keys", keys));
    settings.extend(frame!(2, "Question", question));
    settings.extend(frame!(2, "Toast", toast));
    settings.extend(frame!(7, "Now", now));
    settings.extend(frame!(7, "Chapters", chapters));
    settings.extend(frame!(7, "BRB", brb));
    settings.extend(frame!(7, "Latency", latency));
    settings.extend(frame!(7, "Starting", starting));
    settings.extend(frame!(7, "Status bar", status));
    settings.extend(frame!(7, "Alert", alert));
    settings.extend([
        dock!(2, "Toast", toast),
        dock!(7, "Now", now),
        dock!(7, "Chapters", chapters),
        dock!(7, "BRB", brb),
        dock!(7, "Latency", latency),
    ]);
    settings.push(Setting {
        section: 4,
        group: "Overlay look",
        key: "overlay.skin",
        label: "Overlay skin pack",
        help: "Skin every overlay wears unless it has its own: nvim, clean, broadcast, glass, minimal or a custom one.",
        kind: Kind::Text,
        when: When::Now,
        get: |c| Value::Text(c.overlay.skin.clone()),
    });
    settings.push(Setting {
        section: 4,
        group: "Overlay look",
        key: "overlay.avatar",
        label: "Starting screen picture",
        help: "A picture file in <home>/overlays, e.g. logo.jpg.",
        kind: Kind::Text,
        when: When::Now,
        get: |c| Value::Text(c.overlay.avatar.clone()),
    });
    settings.extend([
        Setting {
            section: 4,
            group: "Launch",
            key: "obs.launch",
            label: "Start OBS",
            help: "Start OBS when it is not running, never in safe mode; it stays open when aftercast exits.",
            kind: Kind::Bool,
            when: When::Restart,
            get: |c| Value::Bool(c.obs.launch),
        },
        Setting {
            section: 4,
            group: "Launch",
            key: "obs.path",
            label: "OBS program",
            help: "obs64.exe; empty finds it in Program Files.",
            kind: Kind::Text,
            when: When::Restart,
            get: |c| Value::Text(c.obs.path.display().to_string()),
        },
        Setting {
            section: 7,
            group: "Start",
            key: "stream.starting_screen",
            label: "Starting screen",
            help: "Go Live opens on the starting scene, counting up; Enter in the TUI goes to the main scene.",
            kind: Kind::Bool,
            when: When::Now,
            get: |c| Value::Bool(c.stream.starting_screen),
        },
        Setting {
            section: 7,
            group: "End",
            key: "stream.end_screen",
            label: "End screen",
            help: ":end shows the ending scene first and asks how long; then the stream ends.",
            kind: Kind::Bool,
            when: When::Now,
            get: |c| Value::Bool(c.stream.end_screen),
        },
        Setting {
            section: 7,
            group: "End",
            key: "stream.end_screen_s",
            label: "End screen seconds",
            help: "What :end offers; type another number at the prompt for this time only.",
            kind: Kind::Int {
                min: 1,
                max: 600,
                step: 5,
            },
            when: When::Now,
            get: |c| Value::Int(c.stream.end_screen_s.into()),
        },
    ]);
    settings
}

/// One overlay's frame (`[overlay.<name>]`) in a group "<Title> overlay". The rehearse
/// page's overlay editor changes the same values with the mouse.
macro_rules! frame {
    ($section:expr, $title:literal, $name:ident) => {{
        let int = |field, label, help, min, max, step, get| Setting {
            section: $section,
            group: concat!($title, " overlay"),
            key: field,
            label,
            help,
            kind: Kind::Int { min, max, step },
            when: When::Now,
            get,
        };
        [
            int(
                concat!("overlay.", stringify!($name), ".x"),
                concat!($title, " x"),
                "Left edge of the frame on the 1920x1080 canvas. Drag it on the rehearse page.",
                -1920,
                3840,
                10,
                |c| Value::Int(c.overlay.$name.x.into()),
            ),
            int(
                concat!("overlay.", stringify!($name), ".y"),
                concat!($title, " y"),
                "Top edge of the frame on the canvas.",
                -1080,
                2160,
                10,
                |c| Value::Int(c.overlay.$name.y.into()),
            ),
            int(
                concat!("overlay.", stringify!($name), ".width"),
                concat!($title, " width"),
                "Width the content lays out in, before Size scales it.",
                20,
                3840,
                20,
                |c| Value::Int(c.overlay.$name.width.into()),
            ),
            int(
                concat!("overlay.", stringify!($name), ".height"),
                concat!($title, " height"),
                "Height the content lays out in, before Size scales it.",
                20,
                2160,
                20,
                |c| Value::Int(c.overlay.$name.height.into()),
            ),
            int(
                concat!("overlay.", stringify!($name), ".size"),
                concat!($title, " size %"),
                "Scales the frame and everything in it; 100 is the built-in size.",
                10,
                400,
                5,
                |c| Value::Int(c.overlay.$name.size.into()),
            ),
            int(
                concat!("overlay.", stringify!($name), ".rotate"),
                concat!($title, " rotation"),
                "Degrees clockwise around the frame's center.",
                -180,
                180,
                5,
                |c| Value::Int(c.overlay.$name.rotate.into()),
            ),
            Setting {
                section: $section,
                group: concat!($title, " overlay"),
                key: concat!("overlay.", stringify!($name), ".align"),
                label: concat!($title, " align"),
                help: "Line the content up on the left, center or right of the frame.",
                kind: Kind::Choice(&["left", "center", "right"]),
                when: When::Now,
                get: |c| Value::Text(c.overlay.$name.align.name().into()),
            },
            Setting {
                section: $section,
                group: concat!($title, " overlay"),
                key: concat!("overlay.", stringify!($name), ".bg"),
                label: concat!($title, " background"),
                help: "Card color, #RRGGBB; empty uses the overlay background in OBS.",
                kind: Kind::Text,
                when: When::Now,
                get: |c| Value::Text(c.overlay.$name.bg.clone()),
            },
            int(
                concat!("overlay.", stringify!($name), ".opacity"),
                concat!($title, " background opacity %"),
                "0 is see-through, 100 solid.",
                0,
                100,
                5,
                |c| Value::Int(c.overlay.$name.opacity.into()),
            ),
            Setting {
                section: $section,
                group: concat!($title, " overlay"),
                key: concat!("overlay.", stringify!($name), ".skin"),
                label: concat!($title, " skin"),
                help: "This overlay's own skin; empty wears the skin pack.",
                kind: Kind::Text,
                when: When::Now,
                get: |c| Value::Text(c.overlay.$name.skin.clone()),
            },
            Setting {
                section: $section,
                group: concat!($title, " overlay"),
                key: concat!("overlay.", stringify!($name), ".hide_when"),
                label: concat!($title, " hidden while"),
                help: "Overlays that hide this one while they are on screen, comma separated (e.g. question, brb).",
                kind: Kind::List,
                when: When::Now,
                get: |c| Value::List(c.overlay.$name.hide_when.clone()),
            },
        ]
    }};
}
use frame;

/// Whether an overlay shows in the status bar instead of its own frame.
macro_rules! dock {
    ($section:expr, $title:literal, $name:ident) => {
        Setting {
            section: $section,
            group: concat!($title, " overlay"),
            key: concat!("overlay.", stringify!($name), ".dock"),
            label: concat!($title, " in the status bar"),
            help: "Show it as a segment of the status bar instead of in its own frame.",
            kind: Kind::Bool,
            when: When::Now,
            get: |c| Value::Bool(c.overlay.$name.dock),
        }
    };
}
use dock;

pub fn show(v: &Value) -> String {
    match v {
        Value::Bool(true) => "on".into(),
        Value::Bool(false) => "off".into(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{f}"),
        Value::Text(t) if t.is_empty() => "(empty)".into(),
        Value::Text(t) => t.clone(),
        Value::List(l) => l.join(", "),
    }
}

/// What a key press asks of the TUI.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Stay,
    Close,
    /// The file changed: reload it.
    Reload,
    /// Send this to the controller (an OBS change).
    Command(Command),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Page {
    section: usize,
    row: usize,
    /// The search filter; `typing` while the search box has the keyboard.
    search: String,
    typing: bool,
    /// Inline editor for a number or text.
    edit: Option<String>,
    /// The last change: which key, when (for the switch animation and the status line).
    changed: Option<(&'static str, Instant)>,
    status: Option<(String, bool)>,
}

impl Page {
    /// The rows listed right now: the section's, or every match of the search (settings,
    /// OBS audio inputs and sources alike).
    fn visible(&self, all: &[Setting], obs: &Media) -> Vec<Row> {
        let q = self.search.to_lowercase();
        let hit = |texts: &[&str]| texts.iter().any(|t| t.to_lowercase().contains(&q));
        let mut rows: Vec<Row> = (0..all.len())
            .filter(|&i| {
                let s = &all[i];
                if q.is_empty() {
                    s.section == self.section
                } else {
                    hit(&[s.label, s.key, s.help, SECTIONS[s.section]])
                }
            })
            .map(Row::Setting)
            .collect();
        let live = q.is_empty() && self.section == OBS_LIVE;
        if live || !q.is_empty() {
            for (i, a) in obs.audio.iter().enumerate() {
                if live || hit(&[&a.name, "mute volume audio mic", SECTIONS[OBS_LIVE]]) {
                    rows.push(Row::Audio(i));
                }
            }
            // Sources grouped by scene, scenes in OBS's order.
            for scene in &obs.scenes {
                for (i, item) in obs.items.iter().enumerate() {
                    if item.scene == *scene
                        && (live || hit(&[&item.source, scene, "show hide source"]))
                    {
                        rows.push(Row::Item(i));
                    }
                }
            }
        }
        rows
    }

    fn selected(&self, all: &[Setting], obs: &Media) -> Option<Row> {
        self.visible(all, obs).get(self.row).copied()
    }

    /// The selected row when it is a setting.
    fn setting<'a>(&self, all: &'a [Setting], obs: &Media) -> Option<&'a Setting> {
        match self.selected(all, obs)? {
            Row::Setting(i) => Some(&all[i]),
            _ => None,
        }
    }

    /// A key on an OBS row: what to ask OBS.
    fn obs_key(
        &mut self,
        code: ratatui::crossterm::event::KeyCode,
        row: Row,
        obs: &Media,
    ) -> Option<Command> {
        use ratatui::crossterm::event::KeyCode as K;
        let cmd = match row {
            Row::Audio(i) => {
                let a = obs.audio.get(i)?;
                let volume = |db: f64| Command::Volume {
                    input: a.name.clone(),
                    db,
                };
                match code {
                    K::Enter | K::Char(' ') => Command::Mute {
                        input: a.name.clone(),
                        on: Some(!a.muted),
                    },
                    K::Char('l') | K::Right | K::Char('+') => volume((a.volume_db + 1.0).round()),
                    K::Char('h') | K::Left | K::Char('-') => volume((a.volume_db - 1.0).round()),
                    K::Char('r') => volume(0.0),
                    _ => return None,
                }
            }
            Row::Item(i) => {
                let item = obs.items.get(i)?;
                let visible = match code {
                    K::Enter | K::Char(' ') => !item.enabled,
                    K::Char('l') | K::Right => true,
                    K::Char('h') | K::Left => false,
                    _ => return None,
                };
                Command::Source {
                    source: item.source.clone(),
                    scene: Some(item.scene.clone()),
                    visible: Some(visible),
                }
            }
            Row::Setting(_) => return None,
        };
        self.changed = Some(("obs", Instant::now()));
        self.status = Some(("Sent to OBS".into(), true));
        Some(cmd)
    }

    pub fn key(
        &mut self,
        code: ratatui::crossterm::event::KeyCode,
        config: &Config,
        path: &Path,
        obs: &Media,
    ) -> Outcome {
        use ratatui::crossterm::event::KeyCode as K;
        let all = all();
        if let Some(buffer) = &mut self.edit {
            match code {
                K::Esc => self.edit = None,
                K::Backspace => {
                    buffer.pop();
                }
                K::Char(c) => buffer.push(c),
                K::Enter => {
                    let buffer = self.edit.take().unwrap_or_default();
                    let Some(s) = self.setting(&all, obs) else {
                        return Outcome::Stay;
                    };
                    let value = match s.kind {
                        Kind::Int { min, max, .. } => match buffer.trim().parse::<i64>() {
                            Ok(n) => Value::Int(n.clamp(min, max)),
                            Err(_) => {
                                return self.fail(format!("{:?} is not a whole number", buffer));
                            }
                        },
                        Kind::Float { min, max, .. } => match buffer.trim().parse::<f64>() {
                            Ok(n) if n.is_finite() => Value::Float(n.clamp(min, max)),
                            _ => return self.fail(format!("{:?} is not a number", buffer)),
                        },
                        _ => Value::Text(buffer.trim().to_owned()),
                    };
                    return self.save(s, Some(value), path);
                }
                _ => {}
            }
            return Outcome::Stay;
        }
        if self.typing {
            match code {
                K::Esc => {
                    self.search.clear();
                    self.typing = false;
                }
                K::Enter | K::Down | K::Tab => self.typing = false,
                K::Backspace => {
                    self.search.pop();
                }
                K::Char(c) => self.search.push(c),
                _ => {}
            }
            self.row = 0;
            return Outcome::Stay;
        }
        let len = self.visible(&all, obs).len();
        if let Some(row @ (Row::Audio(_) | Row::Item(_))) = self.selected(&all, obs)
            && let Some(cmd) = self.obs_key(code, row, obs)
        {
            return Outcome::Command(cmd);
        }
        match code {
            // Esc clears a search first, then closes.
            K::Esc if !self.search.is_empty() => {
                self.search.clear();
                self.row = 0;
            }
            K::Esc | K::Char('q') | K::Char(',') => return Outcome::Close,
            K::Char('/') => {
                self.typing = true;
                self.row = 0;
            }
            K::Char('j') | K::Down => self.row = (self.row + 1).min(len.saturating_sub(1)),
            K::Char('k') | K::Up => self.row = self.row.saturating_sub(1),
            K::Char('g') | K::Home => self.row = 0,
            K::Char('G') | K::End => self.row = len.saturating_sub(1),
            K::Tab | K::Char(']') => self.go(self.section as isize + 1),
            K::BackTab | K::Char('[') => self.go(self.section as isize - 1),
            K::Char(c @ '1'..='9') => {
                let n = c as usize - '1' as usize;
                if n < SECTIONS.len() {
                    self.go(n as isize);
                }
            }
            K::Char('r') => {
                if let Some(s) = self.setting(&all, obs) {
                    return self.save(s, None, path);
                }
            }
            K::Enter | K::Char(' ') => {
                let Some(s) = self.setting(&all, obs) else {
                    return Outcome::Stay;
                };
                let current = (s.get)(config);
                return match (s.kind, current) {
                    (Kind::Bool, Value::Bool(b)) => self.save(s, Some(Value::Bool(!b)), path),
                    (Kind::Choice(_), _) => self.step(s, 1, config, path),
                    (_, v) => {
                        self.edit = Some(match v {
                            Value::Text(t) => t,
                            v => show(&v),
                        });
                        Outcome::Stay
                    }
                };
            }
            K::Char('l') | K::Right | K::Char('+') => {
                if let Some(s) = self.setting(&all, obs) {
                    return self.step(s, 1, config, path);
                }
            }
            K::Char('h') | K::Left | K::Char('-') => {
                if let Some(s) = self.setting(&all, obs) {
                    return self.step(s, -1, config, path);
                }
            }
            _ => {}
        }
        Outcome::Stay
    }

    fn go(&mut self, section: isize) {
        self.search.clear();
        self.section = section.rem_euclid(SECTIONS.len() as isize) as usize;
        self.row = 0;
    }

    /// ←/→: a switch goes off/on, a choice moves, a number steps.
    fn step(&mut self, s: &Setting, dir: i64, config: &Config, path: &Path) -> Outcome {
        let value = match (s.kind, (s.get)(config)) {
            (Kind::Bool, Value::Bool(b)) => {
                if b == (dir > 0) {
                    return Outcome::Stay;
                }
                Value::Bool(dir > 0)
            }
            (Kind::Choice(options), Value::Text(t)) => {
                let i = options.iter().position(|o| *o == t).unwrap_or(0) as i64;
                let n = options.len() as i64;
                Value::Text(options[(i + dir).rem_euclid(n) as usize].to_owned())
            }
            (Kind::Int { min, max, step }, Value::Int(v)) => {
                Value::Int((v + dir * step).clamp(min, max))
            }
            (Kind::Float { min, max, step }, Value::Float(v)) => {
                Value::Float((v + dir as f64 * step).clamp(min, max))
            }
            _ => return Outcome::Stay,
        };
        self.save(s, Some(value), path)
    }

    fn save(&mut self, s: &Setting, value: Option<Value>, path: &Path) -> Outcome {
        let reset = value.is_none();
        match ac_core::config::set_value(path, s.key, value) {
            Ok(()) => {
                self.changed = Some((s.key, Instant::now()));
                let what = if reset { "Reset to default" } else { "Saved" };
                self.status = Some((format!("{what} · {}", s.when.text()), true));
                Outcome::Reload
            }
            Err(e) => self.fail(e.to_string()),
        }
    }

    fn fail(&mut self, message: String) -> Outcome {
        self.status = Some((message, false));
        Outcome::Stay
    }

    pub fn hints(&self) -> Vec<Span<'static>> {
        let pairs: &[(&str, &str)] = if self.edit.is_some() {
            &[("Enter", "save"), ("Esc", "cancel")]
        } else if self.typing {
            &[
                ("type", "to search"),
                ("Enter", "results"),
                ("Esc", "clear"),
            ]
        } else {
            &[
                ("j/k", "move"),
                ("Space", "toggle / edit"),
                ("h/l", "change"),
                ("Tab", "section"),
                ("/", "search"),
                ("r", "reset"),
                ("Esc", "close"),
            ]
        };
        let mut spans = vec![Span::raw(" ")];
        for (k, l) in pairs {
            spans.extend(hint(k, l));
        }
        spans
    }

    /// The page, in cards like Home: Sections on the left; the section's settings in a lit
    /// card (keys act there); under it, the selected setting described in one line and a
    /// row of tags (key, default, when it applies).
    pub fn draw(
        &self,
        frame: &mut Frame,
        config: &Config,
        path: &Path,
        obs: &Media,
        scene: Option<&str>,
        motion: bool,
    ) {
        let all = all();
        let defaults = Config::default();
        let area = frame.area();
        let full = ratatui::layout::Rect {
            height: area.height.saturating_sub(1),
            ..area
        };
        frame.render_widget(Clear, full);
        let page = ratatui::layout::Rect {
            x: full.x + 1,
            width: full.width.saturating_sub(2),
            ..full
        };
        let [head, _, body, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(4),
            Constraint::Length(1),
        ])
        .areas(page);

        // Header: the title and file; on the right, the search box.
        let visible = self.visible(&all, obs);
        let search = if self.typing || !self.search.is_empty() {
            let caret = if self.typing { "▏" } else { "" };
            vec![
                Span::styled(" / ", Style::new().fg(Color::Black).bg(INFO).bold()),
                Span::styled(format!(" {}{caret} ", self.search), Style::new().fg(TEXT)),
                Span::styled(format!("{} ", visible.len()), Style::new().fg(FAINT)),
            ]
        } else {
            vec![
                Span::styled(" / ", Style::new().fg(MUTED).bg(RAISED)),
                Span::styled(" search ", Style::new().fg(FAINT)),
            ]
        };
        let [title, find] =
            Layout::horizontal([Constraint::Min(10), Constraint::Length(30)]).areas(head);
        frame.render_widget(
            Line::from(vec![
                Span::styled(" ◉ ", Style::new().fg(Color::Black).bg(ACCENT).bold()),
                Span::styled(" Settings  ", Style::new().fg(TEXT).bold()),
                Span::styled(path.display().to_string(), Style::new().fg(FAINT)),
            ]),
            title,
        );
        frame.render_widget(Line::from(search).alignment(Alignment::Right), find);

        let [side, main] = Layout::horizontal([Constraint::Length(26), Constraint::Min(20)])
            .spacing(1)
            .areas(body);

        // Sections: number, name, and a dot with how many settings differ from the default.
        let mut lines = Vec::new();
        for (i, name) in SECTIONS.iter().enumerate() {
            let changed = all
                .iter()
                .filter(|s| s.section == i && (s.get)(config) != (s.get)(&defaults))
                .count();
            let active = self.search.is_empty() && i == self.section;
            let (chip, label) = if active {
                (
                    Style::new().fg(Color::Black).bg(ACCENT).bold(),
                    Style::new().fg(TEXT).bold(),
                )
            } else {
                (Style::new().fg(MUTED).bg(RAISED), Style::new().fg(MUTED))
            };
            let mut spans = vec![
                Span::styled(format!(" {} ", i + 1), chip),
                Span::styled(format!(" {name}"), label),
            ];
            if i == OBS_LIVE {
                if obs.scenes.is_empty() {
                    spans.push(Span::styled(" ○", Style::new().fg(FAINT)));
                } else if obs.audio.iter().any(|a| a.is_mic() && a.muted) {
                    spans.push(Span::styled(" mic off", Style::new().fg(ACCENT)));
                }
            }
            if changed > 0 {
                spans.push(Span::styled(
                    format!(" •{changed}"),
                    Style::new().fg(ACCENT),
                ));
            }
            lines.push(Line::from(spans));
            lines.push(Line::raw(""));
        }
        let sections = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(crate::theme::BORDER))
            .title(Span::styled(" Sections ", Style::new().fg(MUTED).bold()))
            .title_bottom(Span::styled(" Tab 1-9 ", Style::new().fg(FAINT)))
            .padding(Padding::new(1, 1, 1, 0));
        frame.render_widget(Paragraph::new(lines).block(sections), side);

        // The settings: the lit card.
        let [list_area, _, about] = Layout::vertical([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .areas(main);
        let heading = if self.search.is_empty() {
            format!(" {} ", SECTIONS[self.section])
        } else {
            format!(" “{}” ", self.search)
        };
        let list_block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(ACCENT))
            .title(Span::styled(heading, Style::new().fg(TEXT).bold()))
            .padding(Padding::new(0, 1, 1, 0));
        let list = list_block.inner(list_area);
        let width = list.width as usize;
        let mut rows = Vec::new();
        if visible.is_empty() {
            let empty = if self.search.is_empty() && self.section == OBS_LIVE {
                "  OBS not connected"
            } else {
                "  nothing matches"
            };
            rows.push(Line::styled(empty, Style::new().fg(FAINT)));
        }
        let mut group = "";
        let mut selected_line = 0;
        for (r, &row) in visible.iter().enumerate() {
            if r == self.row {
                selected_line = rows.len();
            }
            let i = match row {
                Row::Setting(i) => i,
                other => {
                    let prefix = !self.search.is_empty();
                    rows.push(obs_row(other, obs, scene, r == self.row, prefix, width));
                    continue;
                }
            };
            let s = &all[i];
            // A sub-heading where the group changes (not in search results).
            if self.search.is_empty() && !s.group.is_empty() && s.group != group {
                if !rows.is_empty() {
                    rows.push(Line::raw(""));
                }
                rows.push(Line::styled(
                    format!("  {}", s.group.to_uppercase()),
                    Style::new().fg(MUTED).bold(),
                ));
                if r == self.row {
                    selected_line = rows.len();
                }
            }
            group = s.group;
            let value = (s.get)(config);
            let modified = value != (s.get)(&defaults);
            let selected = r == self.row;
            let mut label = String::new();
            if !self.search.is_empty() {
                label.push_str(&format!("{} › ", SECTIONS[s.section]));
            }
            label.push_str(s.label);
            let mut left = vec![
                Span::styled(if selected { "▍" } else { " " }, Style::new().fg(ACCENT)),
                Span::styled(if modified { "• " } else { "  " }, Style::new().fg(ACCENT)),
                Span::styled(
                    label,
                    if selected {
                        Style::new().fg(TEXT).bold()
                    } else {
                        Style::new().fg(TEXT)
                    },
                ),
            ];
            if s.when == When::Restart {
                left.push(Span::styled(" ↻", Style::new().fg(FAINT)));
            }
            let just = self
                .changed
                .filter(|(k, at)| *k == s.key && at.elapsed() < Duration::from_millis(400))
                .map(|(_, at)| at.elapsed().as_secs_f64() / 0.4);
            let right = if selected && self.edit.is_some() {
                let buffer = self.edit.clone().unwrap_or_default();
                vec![Span::styled(
                    format!(" {buffer}▏ "),
                    Style::new().fg(TEXT).bg(RAISED),
                )]
            } else {
                widget(&s.kind, &value, selected, if motion { just } else { None })
            };
            let used: usize = left.iter().chain(&right).map(Span::width).sum();
            let mut spans = left;
            spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
            spans.extend(right);
            let mut line = Line::from(spans);
            if selected {
                line = line.style(Style::new().bg(SELECTED));
            }
            rows.push(line);
        }
        let skip = (selected_line + 1).saturating_sub(list.height as usize);
        let rows: Vec<Line> = rows.into_iter().skip(skip).collect();
        frame.render_widget(list_block, list_area);
        frame.render_widget(Paragraph::new(rows), list);

        // The selected one, described: its name and one line, then tags.
        let tag = |text: String, color: Color| {
            Span::styled(format!(" {text} "), Style::new().fg(color).bg(RAISED))
        };
        let described =
            if let Some(text) = self.selected(&all, obs).and_then(|row| obs_about(row, obs)) {
                Some(text)
            } else {
                self.setting(&all, obs).map(|s| {
                    let default = show(&(s.get)(&defaults));
                    let when = if s.when == When::Now { OK } else { WARN };
                    vec![
                        Line::styled(format!(" {}", s.help), Style::new().fg(TEXT)),
                        Line::raw(""),
                        Line::from(vec![
                            Span::raw(" "),
                            tag(s.key.to_owned(), FAINT),
                            Span::raw(" "),
                            tag(format!("default {default}"), MUTED),
                            Span::raw(" "),
                            tag(s.when.short().to_owned(), when),
                        ]),
                    ]
                })
            };
        if let Some(text) = described {
            frame.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), about);
        }

        // Result of the last change, fading after a few seconds.
        if let Some((message, ok)) = &self.status {
            let age = self
                .changed
                .map_or(0.0, |(_, at)| at.elapsed().as_secs_f64());
            let base = if *ok { OK } else { ACCENT };
            let color = if *ok {
                mix(base, FAINT, (age - 3.0) / 2.0)
            } else {
                base
            };
            let mark = if *ok { "✔" } else { "✖" };
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!(" {mark} {message}"),
                    Style::new().fg(color),
                )),
                status,
            );
        }
    }
}

/// An OBS row: an audio input with its volume and mute switch, or a source with its
/// visibility switch.
fn obs_row(
    row: Row,
    obs: &Media,
    program: Option<&str>,
    selected: bool,
    prefix: bool,
    width: usize,
) -> Line<'static> {
    let arrows = Style::new().fg(if selected { MUTED } else { FAINT });
    let name_style = if selected {
        Style::new().fg(TEXT).bold()
    } else {
        Style::new().fg(TEXT)
    };
    let mut left = vec![
        Span::styled(if selected { "▍" } else { " " }, Style::new().fg(ACCENT)),
        Span::raw("  "),
    ];
    if prefix {
        left.push(Span::styled(
            format!("{} › ", SECTIONS[OBS_LIVE]),
            Style::new().fg(TEXT),
        ));
    }
    let right = match row {
        Row::Audio(i) => {
            let a = &obs.audio[i];
            left.push(Span::styled(a.name.clone(), name_style));
            let kind = if a.is_mic() {
                "mic"
            } else if a.kind.contains("output_capture") {
                "desktop"
            } else {
                "audio"
            };
            left.push(Span::styled(format!("  {kind}"), Style::new().fg(FAINT)));
            let (track, label, color) = if a.muted {
                ("●━━", " muted ", ACCENT)
            } else {
                ("━━●", " live  ", OK)
            };
            vec![
                Span::styled("‹ ", arrows),
                Span::styled(format!("{:.1} dB", a.volume_db), Style::new().fg(TEXT)),
                Span::styled(" ›  ", arrows),
                Span::styled(track, Style::new().fg(color)),
                Span::styled(label, Style::new().fg(if a.muted { ACCENT } else { TEXT })),
            ]
        }
        Row::Item(i) => {
            let item = &obs.items[i];
            let on_air = program == Some(item.scene.as_str());
            left.push(Span::styled(
                if on_air { "● " } else { "  " },
                Style::new().fg(ACCENT),
            ));
            left.push(Span::styled(
                format!("{} › ", item.scene),
                Style::new().fg(MUTED),
            ));
            left.push(Span::styled(item.source.clone(), name_style));
            let (track, label, color) = if item.enabled {
                ("━━●", " shown ", OK)
            } else {
                ("●━━", " hidden", FAINT)
            };
            vec![
                Span::styled(track, Style::new().fg(color)),
                Span::styled(
                    label,
                    Style::new().fg(if item.enabled { TEXT } else { MUTED }),
                ),
            ]
        }
        Row::Setting(_) => Vec::new(),
    };
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
    spans.extend(right);
    let line = Line::from(spans);
    if selected {
        line.style(Style::new().bg(SELECTED))
    } else {
        line
    }
}

/// The description under the list for an OBS row: its name, what the keys do, tags.
fn obs_about(row: Row, obs: &Media) -> Option<Vec<Line<'static>>> {
    let (title, keys, tag) = match row {
        Row::Audio(i) => {
            let a = obs.audio.get(i)?;
            (
                a.name.clone(),
                "Space mute · h/l volume · r 0 dB",
                a.kind.clone(),
            )
        }
        Row::Item(i) => {
            let item = obs.items.get(i)?;
            (
                format!("{} · {}", item.source, item.scene),
                "Space show/hide · h hide · l show",
                "scene item".to_owned(),
            )
        }
        Row::Setting(_) => return None,
    };
    let chip = |text: String, color: Color| {
        Span::styled(format!(" {text} "), Style::new().fg(color).bg(RAISED))
    };
    Some(vec![
        Line::from(vec![
            Span::styled(format!(" {title}  "), Style::new().fg(TEXT).bold()),
            Span::styled(keys, Style::new().fg(MUTED)),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::raw(" "),
            chip(tag, FAINT),
            Span::raw(" "),
            chip("now, in OBS".into(), OK),
        ]),
    ])
}

/// The value column: a switch, `‹ choice ›`, a number or a text value. `just` is the
/// progress (0..1) of a switch that was just flipped, for its knob to slide.
fn widget(kind: &Kind, value: &Value, selected: bool, just: Option<f64>) -> Vec<Span<'static>> {
    let arrows = Style::new().fg(if selected { MUTED } else { FAINT });
    match (kind, value) {
        (Kind::Bool, Value::Bool(on)) => {
            let track = match just {
                Some(p) if p < 0.5 => "━●━",
                _ if *on => "━━●",
                _ => "●━━",
            };
            let color = if *on { OK } else { FAINT };
            vec![
                Span::styled(track, Style::new().fg(color)),
                Span::styled(
                    if *on { " on  " } else { " off " },
                    Style::new().fg(if *on { TEXT } else { MUTED }),
                ),
            ]
        }
        (Kind::Choice(_) | Kind::Int { .. } | Kind::Float { .. }, v) => vec![
            Span::styled("‹ ", arrows),
            Span::styled(show(v), Style::new().fg(TEXT).bold()),
            Span::styled(" › ", arrows),
        ],
        (_, v) => {
            let empty = matches!(v, Value::Text(t) if t.is_empty());
            vec![
                Span::styled(show(v), Style::new().fg(if empty { FAINT } else { TEXT })),
                Span::styled(if selected { " ✎ " } else { "   " }, Style::new().fg(MUTED)),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::KeyCode as K;

    #[test]
    fn every_setting_reads_its_default_and_saves_back_valid() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftercast.toml");
        std::fs::write(&path, "# mine\n").unwrap();
        let defaults = Config::default();
        for s in all() {
            // Writing the default back must give a valid config with the same value.
            let value = (s.get)(&defaults);
            ac_core::config::set_value(&path, s.key, Some(value.clone()))
                .unwrap_or_else(|e| panic!("{}: {e}", s.key));
            let config = Config::load(&path).unwrap();
            assert_eq!((s.get)(&config), value, "{}", s.key);
            if let Kind::Choice(options) = s.kind {
                for o in options {
                    ac_core::config::set_value(&path, s.key, Some(Value::Text((*o).into())))
                        .unwrap_or_else(|e| panic!("{} = {o}: {e}", s.key));
                }
            }
        }
    }

    #[test]
    fn keys_toggle_step_edit_search_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("aftercast.toml");
        std::fs::write(&path, "").unwrap();
        let load = || Config::load(&path).unwrap();
        let mut page = Page::default();

        // Display › Wrap long lines: Space flips it and asks for a reload.
        page.key(K::Char('4'), &load(), &path, &Media::default());
        assert_eq!(
            page.key(K::Char(' '), &load(), &path, &Media::default()),
            Outcome::Reload
        );
        assert!(load().tui.wrap_lines);
        // l on an "on" switch does nothing; h turns it off.
        assert_eq!(
            page.key(K::Char('l'), &load(), &path, &Media::default()),
            Outcome::Stay
        );
        page.key(K::Char('h'), &load(), &path, &Media::default());
        assert!(!load().tui.wrap_lines);

        // Stream › First chapter title (fifth row): typed text.
        page.key(K::Char('8'), &load(), &path, &Media::default());
        for _ in 0..4 {
            page.key(K::Char('j'), &load(), &path, &Media::default());
        }
        page.key(K::Enter, &load(), &path, &Media::default());
        for _ in 0..5 {
            page.key(K::Backspace, &load(), &path, &Media::default());
        }
        for c in "Intro".chars() {
            page.key(K::Char(c), &load(), &path, &Media::default());
        }
        assert_eq!(
            page.key(K::Enter, &load(), &path, &Media::default()),
            Outcome::Reload
        );
        assert_eq!(load().stream.first_chapter_title, "Intro");

        // YouTube › Default latency (fourth row): a number refuses garbage and keeps the file.
        page.key(K::Char('9'), &load(), &path, &Media::default());
        for _ in 0..3 {
            page.key(K::Char('j'), &load(), &path, &Media::default());
        }
        page.key(K::Enter, &load(), &path, &Media::default());
        page.key(K::Char('x'), &load(), &path, &Media::default());
        assert_eq!(
            page.key(K::Enter, &load(), &path, &Media::default()),
            Outcome::Stay
        );
        assert!(page.status.as_ref().is_some_and(|(_, ok)| !ok));
        // ...and steps with l.
        page.key(K::Char('l'), &load(), &path, &Media::default());
        assert_eq!(load().stream.default_latency_s, 5.5);
        // r resets it.
        page.key(K::Char('r'), &load(), &path, &Media::default());
        assert_eq!(load().stream.default_latency_s, 5.0);

        // Search finds settings in any section; choices cycle.
        page.key(K::Char('/'), &load(), &path, &Media::default());
        for c in "replies go".chars() {
            page.key(K::Char(c), &load(), &path, &Media::default());
        }
        page.key(K::Enter, &load(), &path, &Media::default());
        page.key(K::Char(' '), &load(), &path, &Media::default());
        assert_eq!(load().bot.default_reply, ReplyMode::Chat);
        // Esc clears the search, a second Esc closes the page.
        assert_eq!(
            page.key(K::Esc, &load(), &path, &Media::default()),
            Outcome::Stay
        );
        assert_eq!(
            page.key(K::Esc, &load(), &path, &Media::default()),
            Outcome::Close
        );
    }
}
