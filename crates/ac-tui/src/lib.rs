//! Terminal UI for live sessions.

use ac_core::session::SessionMeta;
use ac_core::session_state::SessionState;
use ac_template::Description;
use ac_template::chapters::Chapter;

/// Snapshot of a live session published by the controller after every change.
#[derive(Debug, Clone)]
pub struct View {
    /// Session folder name, e.g. `2026-11-19_ep042`.
    pub session: String,
    pub meta: SessionMeta,
    pub state: SessionState,
    pub chapters: Vec<Chapter>,
    /// Last successful render (kept while the README has errors).
    pub description: Option<Description>,
    pub render_error: Option<String>,
    pub quota_remaining: i64,
    /// One-line feedback for the last action, e.g. "description unchanged".
    pub notice: Option<String>,
}
