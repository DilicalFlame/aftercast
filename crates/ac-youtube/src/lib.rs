//! YouTube Data API: OAuth, quota accounting, broadcast lookup and description push
//! (specs/06-youtube.md), live chat (`chat`) and sending chat messages.

pub mod auth;
pub mod chat;
mod mock;
pub mod quota;

use std::path::Path;
use std::sync::Arc;

use ac_core::time::{Clock, Timestamp};
use serde_json::{Map, Value, json};

pub use ac_core::youtube::LiveStatus;
use auth::Auth;
use mock::Mock;
use quota::{Method, QuotaGuard, Spent};

const API: &str = "https://www.googleapis.com/youtube/v3";
/// Appended to auth errors that signing in again fixes.
pub(crate) const LOGIN_HINT: &str = "run `aftercast auth login`";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "{method} would leave {remaining} units, below the {reserve}-unit reserve (youtube.daily_quota_reserve)"
    )]
    QuotaReserve {
        method: &'static str,
        remaining: i64,
        reserve: u32,
    },
    #[error("YouTube quota exhausted for today (resets at midnight Pacific Time)")]
    QuotaExhausted,
    #[error("quota ledger: {0}")]
    Ledger(String),
    #[error("{0}")]
    Auth(String),
    #[error("YouTube API error {status} {reason}: {message}")]
    Api {
        status: u16,
        reason: String,
        message: String,
    },
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("mock: {0}")]
    Mock(String),
    #[error("{0}")]
    Broadcast(String),
}

impl Error {
    pub fn exit_code(&self) -> u8 {
        use ac_core::exit;
        match self {
            Self::Auth(_) => exit::AUTH,
            Self::QuotaExhausted | Self::QuotaReserve { .. } => exit::QUOTA,
            _ => exit::GENERIC,
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Extracts the 11-character video id from a stream link or a bare id.
pub fn video_id(input: &str) -> Option<String> {
    let input = input.trim();
    let is_id = |s: &str| {
        s.len() == 11
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    if is_id(input) {
        return Some(input.to_owned());
    }
    let rest = input.split_once("://").map_or(input, |(_, rest)| rest);
    let (host, path) = rest.split_once('/')?;
    let host = host.trim_start_matches("www.").trim_start_matches("m.");
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let segments: Vec<&str> = path.split('/').collect();
    let candidate = match (host, segments.as_slice()) {
        ("youtu.be", [id, ..]) => *id,
        ("youtube.com", ["live", id, ..]) => *id,
        ("youtube.com", ["watch"]) => query.split('&').find_map(|kv| kv.strip_prefix("v="))?,
        ("studio.youtube.com", ["video", id, ..]) => *id,
        _ => return None,
    };
    is_id(candidate).then(|| candidate.to_owned())
}

/// `snippet.liveBroadcastContent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveContent {
    Live,
    Upcoming,
    None,
}

/// What `videos.list` says about a broadcast.
#[derive(Debug, Clone, PartialEq)]
pub struct Broadcast {
    pub video_id: String,
    pub title: String,
    pub channel_id: String,
    pub content: LiveContent,
    pub chat_id: Option<String>,
    pub actual_start: Option<Timestamp>,
    pub actual_end: Option<Timestamp>,
}

impl Broadcast {
    fn from_response(id: &str, response: &Value) -> Result<Self> {
        let item = &response["items"][0];
        if item.is_null() {
            return Err(Error::Broadcast(format!("video {id} not found")));
        }
        let snippet = &item["snippet"];
        let details = &item["liveStreamingDetails"];
        let text = |v: &Value| v.as_str().map(str::to_owned);
        let time = |v: &Value| v.as_str().and_then(|s| s.parse().ok());
        Ok(Self {
            video_id: id.to_owned(),
            title: text(&snippet["title"]).unwrap_or_default(),
            channel_id: text(&snippet["channelId"]).unwrap_or_default(),
            content: match snippet["liveBroadcastContent"].as_str() {
                Some("live") => LiveContent::Live,
                Some("upcoming") => LiveContent::Upcoming,
                _ => LiveContent::None,
            },
            chat_id: text(&details["activeLiveChatId"]),
            actual_start: time(&details["actualStartTime"]),
            actual_end: time(&details["actualEndTime"]),
        })
    }
}

/// The authenticated channel.
#[derive(Debug, Clone, PartialEq)]
pub struct Channel {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Http {
    Get,
    Put,
    Post,
}

#[derive(Debug, Clone)]
struct Request {
    method: Method,
    http: Http,
    path: &'static str,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
}

enum Backend {
    Rest { http: reqwest::Client, auth: Auth },
    Mock(Mock),
}

type SpentHook = Box<dyn Fn(Spent) + Send + Sync>;

/// YouTube API client; every call is checked and recorded by the quota guard.
pub struct Client {
    backend: Backend,
    quota: QuotaGuard,
    clock: Arc<Clock>,
    on_spent: Option<SpentHook>,
}

impl Client {
    pub fn rest(auth: Auth, quota: QuotaGuard, clock: Arc<Clock>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("aftercast/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("HTTP client builds");
        Self::new(Backend::Rest { http, auth }, quota, clock)
    }

    /// Canned responses from `<fixture>/youtube/<method>.json`; requests are recorded
    /// as JSON files in `record_dir`. No network.
    pub fn mock(fixture: &Path, record_dir: &Path, quota: QuotaGuard, clock: Arc<Clock>) -> Self {
        let mock = Mock::new(fixture, record_dir, clock.now_ts());
        Self::new(Backend::Mock(mock), quota, clock)
    }

    /// With the mock backend: a sender that adds messages to the chat stream (a rehearsal's
    /// chat box). The channel's own posts are then echoed into the stream, as on YouTube.
    pub fn chat_injector(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedSender<ac_core::chat::ChatMessage>> {
        match &self.backend {
            Backend::Mock(mock) => Some(mock.injector()),
            Backend::Rest { .. } => None,
        }
    }

    fn new(backend: Backend, quota: QuotaGuard, clock: Arc<Clock>) -> Self {
        Self {
            backend,
            quota,
            clock,
            on_spent: None,
        }
    }

    /// Called after every recorded call (to emit `quota.spent`).
    pub fn on_spent(&mut self, hook: impl Fn(Spent) + Send + Sync + 'static) {
        self.on_spent = Some(Box::new(hook));
    }

    pub fn quota(&self) -> &QuotaGuard {
        &self.quota
    }

    pub fn quota_mut(&mut self) -> &mut QuotaGuard {
        &mut self.quota
    }

    /// Seconds until the access token expires, if one has been fetched.
    pub fn token_expires_in(&self) -> Option<u64> {
        match &self.backend {
            Backend::Rest { auth, .. } => auth.expires_in().map(|d| d.as_secs()),
            Backend::Mock(_) => None,
        }
    }

    /// `videos.list(part=snippet,liveStreamingDetails,status)`.
    pub async fn video(&self, id: &str) -> Result<Broadcast> {
        let response = self
            .call(
                Request {
                    method: Method::VideosList,
                    http: Http::Get,
                    path: "videos",
                    query: vec![
                        ("part", "snippet,liveStreamingDetails,status".into()),
                        ("id", id.into()),
                    ],
                    body: None,
                },
                false,
            )
            .await?;
        Broadcast::from_response(id, &response)
    }

    /// `channels.list(mine=true)`.
    pub async fn my_channel(&self) -> Result<Channel> {
        let response = self
            .call(
                Request {
                    method: Method::ChannelsList,
                    http: Http::Get,
                    path: "channels",
                    query: vec![("part", "snippet".into()), ("mine", "true".into())],
                    body: None,
                },
                false,
            )
            .await?;
        let item = &response["items"][0];
        Ok(Channel {
            id: item["id"]
                .as_str()
                .ok_or_else(|| Error::Auth("the signed-in account has no YouTube channel".into()))?
                .to_owned(),
            title: item["snippet"]["title"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        })
    }

    /// The id of this channel's broadcast to work on: one already live, else the soonest
    /// upcoming one (`liveBroadcasts.list`, 1 unit each). `None` when nothing is ready.
    pub async fn find_broadcast(&self) -> Result<Option<String>> {
        for status in ["active", "upcoming"] {
            let response = self
                .call(
                    Request {
                        method: Method::LiveBroadcastsList,
                        http: Http::Get,
                        path: "liveBroadcasts",
                        query: vec![
                            ("part", "id,snippet".into()),
                            ("broadcastStatus", status.into()),
                        ],
                        body: None,
                    },
                    false,
                )
                .await?;
            if let Some(id) = soonest_broadcast(&response) {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Where a broadcast stands: its lifecycle and, once a stream is bound, whether YouTube
    /// receives video and how healthy it is (`liveBroadcasts.list` + `liveStreams.list`).
    pub async fn live_status(&self, id: &str) -> Result<LiveStatus> {
        let list = |method, path, part: &str, id: &str| Request {
            method,
            http: Http::Get,
            path,
            query: vec![("part", part.into()), ("id", id.into())],
            body: None,
        };
        let response = self
            .call(
                list(
                    Method::LiveBroadcastsList,
                    "liveBroadcasts",
                    "status,contentDetails,snippet",
                    id,
                ),
                false,
            )
            .await?;
        let item = &response["items"][0];
        let life_cycle = item["status"]["lifeCycleStatus"]
            .as_str()
            .ok_or_else(|| Error::Broadcast(format!("{id} is not a broadcast of this channel")))?
            .to_owned();
        let text = |v: &Value| v.as_str().map(str::to_owned);
        let details = &item["contentDetails"];
        let mut status = LiveStatus {
            life_cycle,
            privacy: text(&item["status"]["privacyStatus"]),
            scheduled_start: item["snippet"]["scheduledStartTime"]
                .as_str()
                .and_then(|s| s.parse().ok()),
            latency: text(&details["latencyPreference"]),
            dvr: details["enableDvr"].as_bool(),
            auto_start: details["enableAutoStart"].as_bool(),
            auto_stop: details["enableAutoStop"].as_bool(),
            ..LiveStatus::default()
        };
        if let Some(bound) = item["contentDetails"]["boundStreamId"].as_str() {
            let response = self
                .call(
                    list(Method::LiveStreamsList, "liveStreams", "status,cdn", bound),
                    false,
                )
                .await?;
            let item = &response["items"][0];
            let s = &item["status"];
            status.stream = text(&s["streamStatus"]);
            status.health = text(&s["healthStatus"]["status"]);
            status.resolution = text(&item["cdn"]["resolution"]).filter(|r| r != "variable");
            status.frame_rate = text(&item["cdn"]["frameRate"]).filter(|r| r != "variable");
            status.issues = s["healthStatus"]["configurationIssues"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| text(&i["description"]).or_else(|| text(&i["reason"])))
                .collect();
        }
        Ok(status)
    }

    /// `liveBroadcasts.transition` to `testing` or `live` (50 units).
    pub async fn transition(&self, id: &str, to: &str) -> Result<()> {
        self.call(
            Request {
                method: Method::LiveBroadcastsTransition,
                http: Http::Post,
                path: "liveBroadcasts/transition",
                query: vec![
                    ("part", "status".into()),
                    ("id", id.into()),
                    ("broadcastStatus", to.into()),
                ],
                body: None,
            },
            true,
        )
        .await?;
        Ok(())
    }

    /// Replaces the video description, sending the full current snippet back so the
    /// title, category, tags and languages are preserved. `critical` calls may use the
    /// quota reserve (the final push).
    pub async fn update_description(
        &self,
        id: &str,
        description: &str,
        critical: bool,
    ) -> Result<()> {
        self.update_snippet(id, "description", description, critical)
            .await
    }

    /// Sets the video title (`!title`), preserving everything else in the snippet.
    pub async fn update_title(&self, id: &str, title: &str) -> Result<()> {
        self.update_snippet(id, "title", title, false).await
    }

    /// `videos.update(part=snippet)` with one field changed. The API clears omitted
    /// mutable fields, so the current ones are read first and sent back.
    async fn update_snippet(
        &self,
        id: &str,
        field: &str,
        value: &str,
        critical: bool,
    ) -> Result<()> {
        let current = self
            .call(
                Request {
                    method: Method::VideosList,
                    http: Http::Get,
                    path: "videos",
                    query: vec![("part", "snippet".into()), ("id", id.into())],
                    body: None,
                },
                critical,
            )
            .await?;
        let snippet = current["items"][0]["snippet"]
            .as_object()
            .ok_or_else(|| Error::Broadcast(format!("video {id} not found")))?;
        let mut update: Map<String, Value> = [
            "title",
            "categoryId",
            "tags",
            "description",
            "defaultLanguage",
            "defaultAudioLanguage",
        ]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), snippet.get(key)?.clone())))
        .collect();
        update.insert(field.into(), value.into());
        self.call(
            Request {
                method: Method::VideosUpdate,
                http: Http::Put,
                path: "videos",
                query: vec![("part", "snippet".into())],
                body: Some(json!({ "id": id, "snippet": update })),
            },
            critical,
        )
        .await?;
        Ok(())
    }

    /// `liveChatMessages.insert`: posts `text` (at most 200 characters, cut with "…").
    pub async fn send_chat(&self, chat_id: &str, text: &str) -> Result<()> {
        let text: String = if text.chars().count() > 200 {
            text.chars().take(199).chain(['…']).collect()
        } else {
            text.to_owned()
        };
        self.call(
            Request {
                method: Method::ChatInsert,
                http: Http::Post,
                path: "liveChat/messages",
                query: vec![("part", "snippet".into())],
                body: Some(json!({ "snippet": {
                    "liveChatId": chat_id,
                    "type": "textMessageEvent",
                    "textMessageDetails": { "messageText": text },
                }})),
            },
            false,
        )
        .await?;
        Ok(())
    }

    async fn call(&self, request: Request, critical: bool) -> Result<Value> {
        self.quota
            .check(request.method, critical, self.clock.now_ts())?;
        let result = match &self.backend {
            Backend::Rest { http, auth } => send(http, auth, &request).await,
            Backend::Mock(mock) => mock.send(&request),
        };
        // A request that never left (no token) costs nothing; everything else is recorded.
        if !matches!(result, Err(Error::Auth(_))) {
            self.record(request.method, result.is_ok());
        }
        match result {
            Err(Error::Api { reason, .. }) if reason == "quotaExceeded" => {
                self.quota.mark_exhausted(self.clock.now_ts());
                Err(Error::QuotaExhausted)
            }
            other => other,
        }
    }
}

impl Client {
    /// Records a call that reached YouTube in the quota ledger.
    fn record(&self, method: Method, ok: bool) {
        let spent = self.quota.record(method, ok, self.clock.now_ts());
        if let Some(hook) = &self.on_spent {
            hook(spent);
        }
    }
}

async fn send(http: &reqwest::Client, auth: &Auth, request: &Request) -> Result<Value> {
    let token = auth.access_token(http).await?;
    let url = format!("{API}/{}", request.path);
    let builder = match request.http {
        Http::Get => http.get(url),
        Http::Put => http.put(url),
        Http::Post => http.post(url),
    };
    let mut builder = builder.bearer_auth(token).query(&request.query);
    if let Some(body) = &request.body {
        builder = builder.json(body);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    match status {
        200..=299 => Ok(body),
        401 => Err(Error::Auth(format!(
            "YouTube rejected the access token; {LOGIN_HINT}"
        ))),
        _ => Err(api_error(status, &body)),
    }
}

/// The id of the item with the earliest `scheduledStartTime` (RFC 3339 UTC sorts as text).
fn soonest_broadcast(response: &Value) -> Option<String> {
    response["items"]
        .as_array()?
        .iter()
        .filter_map(|i| {
            Some((
                i["snippet"]["scheduledStartTime"].as_str().unwrap_or(""),
                i["id"].as_str()?,
            ))
        })
        .min()
        .map(|(_, id)| id.to_owned())
}

/// Maps a Google API error body to [`Error::Api`].
fn api_error(status: u16, body: &Value) -> Error {
    let error = &body["error"];
    Error::Api {
        status,
        reason: error["errors"][0]["reason"]
            .as_str()
            .unwrap_or("unknown")
            .to_owned(),
        message: error["message"].as_str().unwrap_or("no message").to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_id_from_links() {
        let id = Some("dQw4w9WgXcQ".to_owned());
        for link in [
            "dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ",
            "https://www.youtube.com/live/dQw4w9WgXcQ?si=abc",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=10",
            "https://m.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
            "https://studio.youtube.com/video/dQw4w9WgXcQ/livestreaming",
            "youtube.com/live/dQw4w9WgXcQ",
        ] {
            assert_eq!(video_id(link), id, "{link}");
        }
        for bad in [
            "",
            "short",
            "https://example.com/live/dQw4w9WgXcQ",
            "https://youtu.be/",
            "https://www.youtube.com/watch?x=1",
        ] {
            assert_eq!(video_id(bad), None, "{bad}");
        }
    }

    #[test]
    fn broadcast_parses_live_details() {
        let response = json!({ "items": [{
            "snippet": { "title": "IDD #042", "channelId": "UC1", "liveBroadcastContent": "live" },
            "liveStreamingDetails": { "actualStartTime": "2026-11-19T09:28:55Z", "activeLiveChatId": "Cg0" }
        }]});
        let b = Broadcast::from_response("abcdefghijk", &response).unwrap();
        assert_eq!(b.content, LiveContent::Live);
        assert_eq!(b.chat_id.as_deref(), Some("Cg0"));
        assert_eq!(
            b.actual_start,
            Some("2026-11-19T09:28:55Z".parse().unwrap())
        );
        assert_eq!(b.actual_end, None);
        assert!(Broadcast::from_response("x", &json!({ "items": [] })).is_err());
    }

    #[test]
    fn soonest_broadcast_picks_earliest_start() {
        let r = json!({ "items": [
            { "id": "bbbbbbbbbbb", "snippet": { "scheduledStartTime": "2026-11-22T10:00:00Z" } },
            { "id": "aaaaaaaaaaa", "snippet": { "scheduledStartTime": "2026-11-21T10:00:00Z" } },
        ]});
        assert_eq!(soonest_broadcast(&r).as_deref(), Some("aaaaaaaaaaa"));
        assert_eq!(soonest_broadcast(&json!({})), None);
        assert_eq!(soonest_broadcast(&json!({ "items": [] })), None);
    }

    #[test]
    fn api_error_extracts_reason() {
        let body = json!({ "error": { "code": 400, "message": "bad description", "errors": [{ "reason": "invalidDescription" }] } });
        let Error::Api {
            status,
            reason,
            message,
        } = api_error(400, &body)
        else {
            panic!()
        };
        assert_eq!(
            (status, reason.as_str(), message.as_str()),
            (400, "invalidDescription", "bad description")
        );
    }
}
