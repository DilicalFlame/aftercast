//! Commands: requests sent by the TUI, scripts or (later) the bot and local API to the
//! session controller, which turns them into events (specs/01-architecture.md).

use crate::event::By;
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq)]
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
        label: Option<String>,
        pressed_at: Timestamp,
        by: By,
    },
    Note {
        text: String,
        pressed_at: Timestamp,
    },
    /// Manual quest label (`:quest`).
    Quest {
        title: String,
    },
    /// Re-render the description now (no push).
    Render,
    /// Push the description now (`s` + Enter, `:sync`).
    Sync,
    /// Final push and session end (`:finalize`).
    Finalize,
    /// Re-read config and templates (`:reload`).
    Reload,
    /// End the session without finalizing.
    Quit,
}
