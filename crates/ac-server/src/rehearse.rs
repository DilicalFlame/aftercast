//! The rehearsal page (`aftercast rehearse`): a stand-in for the stream page with the OBS
//! output (Virtual Camera), the description as pushed, and a chat to type into as any
//! viewer, moderator, member or the owner.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use ac_core::chat::{ChatKind, ChatMessage};
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
}

pub struct Rehearse {
    pub feed: watch::Receiver<Feed>,
    /// Typed messages go into the simulated chat stream.
    pub chat: mpsc::UnboundedSender<ChatMessage>,
    next_id: AtomicU64,
}

impl Rehearse {
    pub fn new(feed: watch::Receiver<Feed>, chat: mpsc::UnboundedSender<ChatMessage>) -> Self {
        Self {
            feed,
            chat,
            next_id: AtomicU64::new(1),
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
    let html = PAGE.replace("/* theme */", &s.theme);
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
    }))
    .into_response()
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
