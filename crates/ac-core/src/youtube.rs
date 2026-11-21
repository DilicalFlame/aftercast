//! What YouTube says about a broadcast and its stream, as Home shows it.

use crate::time::Timestamp;

/// A broadcast's lifecycle, settings and ingest, from `liveBroadcasts` and `liveStreams`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LiveStatus {
    /// `created`, `ready`, `testing`, `live`, `complete`, ...
    pub life_cycle: String,
    /// `public`, `unlisted` or `private`.
    pub privacy: Option<String>,
    pub scheduled_start: Option<Timestamp>,
    /// `normal`, `low` or `ultraLow`.
    pub latency: Option<String>,
    pub dvr: Option<bool>,
    pub auto_start: Option<bool>,
    pub auto_stop: Option<bool>,
    /// `inactive`, `ready` or `active` (YouTube is receiving video); `None` if no stream is bound.
    pub stream: Option<String>,
    /// `good`, `ok`, `bad` or `noData`.
    pub health: Option<String>,
    /// The stream's resolution and frame rate as YouTube sees them (`1080p`, `60fps`).
    pub resolution: Option<String>,
    pub frame_rate: Option<String>,
    /// What YouTube flags about the incoming stream (bitrate, keyframes, ...).
    pub issues: Vec<String>,
}
