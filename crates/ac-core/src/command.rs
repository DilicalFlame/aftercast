//! Commands: requests sent by the TUI, scripts, hotkeys, the bot and the local API to the
//! session controller, which turns them into events (specs/01-architecture.md).
//!
//! The local API (`POST /api/command`) takes them as JSON, e.g.
//! `{"cmd": "mark", "label": "nice", "by": "palette"}`; a missing `pressed_at` is filled
//! with the time the request arrived.

use serde::{Deserialize, Serialize};

use crate::event::By;
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// Time is captured when the key is pressed, before the title prompt opens.
    Checkpoint {
        title: String,
        pressed_at: Timestamp,
        by: By,
    },
    EditCheckpoint {
        cid: String,
        title: String,
    },
    DeleteCheckpoint {
        cid: String,
    },
    Mark {
        #[serde(default)]
        label: Option<String>,
        pressed_at: Timestamp,
        by: By,
    },
    Note {
        text: String,
        pressed_at: Timestamp,
    },
    /// BRB on/off; `None` toggles.
    Brb {
        #[serde(default)]
        on: Option<bool>,
        by: By,
    },
    /// Privacy mode on/off; `None` toggles.
    Privacy {
        #[serde(default)]
        on: Option<bool>,
        by: By,
    },
    /// Switch the OBS program scene (`:scene`).
    Scene {
        name: String,
    },
    /// Mute, unmute (`Some`) or toggle (`None`) an OBS audio input (`:mute`).
    Mute {
        input: String,
        on: Option<bool>,
    },
    /// Set an OBS audio input's volume in dB (`:volume`).
    Volume {
        input: String,
        db: f64,
    },
    /// Show, hide (`Some`) or toggle (`None`) a source in a scene; no scene means the
    /// program scene (`:show`, `:hide`).
    Source {
        source: String,
        scene: Option<String>,
        visible: Option<bool>,
    },
    /// Add an overlay to an OBS scene as a Browser source the size of the canvas (the
    /// editor's right-click menu).
    AddOverlay {
        scene: String,
        overlay: String,
    },
    /// Start, stop, pause or resume the OBS recording (`:record`).
    Record {
        action: OutputAction,
    },
    /// Start or stop the OBS stream (`:stream`; stopping asks first in the TUI).
    Stream {
        on: bool,
    },
    /// End the stream (`:end`): show the end screen (the `ending` scene) for `screen_s`
    /// seconds, 0 for none, then stop OBS streaming, recording and the virtual camera.
    End {
        screen_s: u32,
    },
    /// Back out of an `:end` while its end screen shows (`:end cancel`).
    CancelEnd,
    /// Start the session prepared on Home: until then nothing lasting or public happens.
    Begin,
    /// Go live: start the session, OBS recording and streaming together, then move the
    /// YouTube broadcast to live once YouTube receives the video.
    GoLive,
    /// Ask OBS for live audio levels (while a page showing meters is open) or stop.
    Meters {
        on: bool,
    },
    /// Manual quest label (`:quest`).
    Quest {
        title: String,
    },
    /// Show, answer or dismiss a viewer question.
    Question {
        qid: String,
        action: QuestionAction,
    },
    /// Send a chat message as the channel (`:say`).
    Say {
        text: String,
    },
    /// Turn an overlay on or off (`:overlay <name> on|off`).
    Overlay {
        name: String,
        on: bool,
    },
    /// Start (or stop) the latency measurement (`:latency`).
    Latency,
    /// The counter value seen on the stream (`!lat <n>`, `:lat <n>`) at time `at`.
    LatencySample {
        counter: u64,
        at: Timestamp,
    },
    /// Answer to a pending `!title` from chat.
    ConfirmTitle {
        ok: bool,
    },
    /// Re-render the description now (no push).
    Render,
    /// Push the description now (`s` + Enter, `:sync`).
    Sync,
    /// Final push and session end (`:finalize`).
    Finalize,
    /// Run the post-stream pipeline on this session now (`p` on the end screen), as
    /// `aftercast post` does.
    PostProcess,
    /// Re-read config and templates (`:reload`).
    Reload,
    /// End the session without finalizing.
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputAction {
    Start,
    Stop,
    Pause,
    Resume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAction {
    Show,
    Answer,
    Dismiss,
    /// Back to waiting in the Inbox, after an answer, thanks or dismissal.
    Reopen,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_json_shape() {
        let cmd: Command = serde_json::from_str(
            r#"{"cmd":"mark","label":"nice","by":"palette","pressed_at":"2026-11-19T10:00:00.000Z"}"#,
        )
        .unwrap();
        assert!(
            matches!(cmd, Command::Mark { label: Some(l), by: By::Palette, .. } if l == "nice")
        );
        let cmd: Command = serde_json::from_str(r#"{"cmd":"brb","by":"tui"}"#).unwrap();
        assert_eq!(
            cmd,
            Command::Brb {
                on: None,
                by: By::Tui
            }
        );
        assert!(serde_json::from_str::<Command>(r#"{"cmd":"explode"}"#).is_err());
    }
}
