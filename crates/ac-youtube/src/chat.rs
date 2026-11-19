//! Live chat over gRPC `liveChatMessages.streamList` (specs/06-youtube.md, "Live chat").
//!
//! The messages below are the subset of Google's `stream_list.proto` (package
//! `youtube.api.v3`) that aftercast reads, written out by hand so the build needs no
//! `protoc`. Field numbers are from the "Streaming Live Chat" guide; unknown fields are
//! skipped by prost. Oneof members are plain optional fields (same wire format).

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use ac_core::chat::{ChatKind, ChatMessage};
use ac_core::time::Timestamp;
use tonic::Code;
use tonic::transport::{Channel, ClientTlsConfig};

use crate::quota::Method;
use crate::{Backend, Client};

const ENDPOINT: &str = "https://youtube.googleapis.com";
const STREAM_LIST: &str = "/youtube.api.v3.V3DataLiveChatMessageService/StreamList";
/// Message ids remembered to drop the history YouTube resends on every (re)connect.
const SEEN: usize = 10_000;
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListRequest {
    #[prost(string, optional, tag = "1")]
    live_chat_id: Option<String>,
    #[prost(uint32, optional, tag = "98")]
    max_results: Option<u32>,
    #[prost(string, optional, tag = "99")]
    page_token: Option<String>,
    #[prost(string, repeated, tag = "100")]
    part: Vec<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ListResponse {
    #[prost(string, optional, tag = "2")]
    offline_at: Option<String>,
    #[prost(string, optional, tag = "100602")]
    next_page_token: Option<String>,
    #[prost(message, repeated, tag = "1007")]
    items: Vec<LiveChatMessage>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct LiveChatMessage {
    #[prost(string, optional, tag = "101")]
    id: Option<String>,
    #[prost(message, optional, tag = "2")]
    snippet: Option<Snippet>,
    #[prost(message, optional, tag = "3")]
    author_details: Option<AuthorDetails>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Snippet {
    /// `LiveChatMessageSnippet.TypeWrapper.Type`.
    #[prost(int32, optional, tag = "1")]
    r#type: Option<i32>,
    #[prost(string, optional, tag = "4")]
    published_at: Option<String>,
    #[prost(string, optional, tag = "16")]
    display_message: Option<String>,
    #[prost(message, optional, tag = "19")]
    text_message_details: Option<TextDetails>,
    #[prost(message, optional, tag = "27")]
    super_chat_details: Option<SuperChatDetails>,
    #[prost(message, optional, tag = "28")]
    super_sticker_details: Option<SuperStickerDetails>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct TextDetails {
    #[prost(string, optional, tag = "1")]
    message_text: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SuperChatDetails {
    #[prost(string, optional, tag = "3")]
    amount_display_string: Option<String>,
    #[prost(string, optional, tag = "4")]
    user_comment: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct SuperStickerDetails {
    #[prost(string, optional, tag = "3")]
    amount_display_string: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct AuthorDetails {
    #[prost(string, optional, tag = "10101")]
    channel_id: Option<String>,
    #[prost(string, optional, tag = "103")]
    display_name: Option<String>,
    #[prost(bool, optional, tag = "5")]
    is_chat_owner: Option<bool>,
    #[prost(bool, optional, tag = "6")]
    is_chat_sponsor: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    is_chat_moderator: Option<bool>,
}

/// What the chat service reports to the session.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatUpdate {
    Connected,
    Message(ChatMessage),
    /// Connection lost; retrying after `retry_in`.
    Down {
        error: String,
        retry_in: Duration,
    },
    /// The broadcast went offline (`offlineAt`): the stream ended.
    Offline,
    /// Chat is disabled, ended, or access was refused; not retried (each try costs quota).
    Stopped(String),
}

/// Normalises one streamed message; `None` when it carries no id.
fn normalise(m: LiveChatMessage, now: Timestamp) -> Option<ChatMessage> {
    let snippet = m.snippet.unwrap_or_default();
    let author = m.author_details.unwrap_or_default();
    let kind = match snippet.r#type.unwrap_or(0) {
        1 => ChatKind::Text,
        2 => ChatKind::Deleted,
        15 => ChatKind::Superchat,
        16 => ChatKind::Supersticker,
        7 | 17..=19 => ChatKind::Membership,
        _ => ChatKind::Other,
    };
    let superchat = snippet.super_chat_details;
    let text = snippet
        .text_message_details
        .and_then(|t| t.message_text)
        .or_else(|| superchat.as_ref().and_then(|s| s.user_comment.clone()))
        .or(snippet.display_message)
        .unwrap_or_default();
    let amount = superchat.and_then(|s| s.amount_display_string).or_else(|| {
        snippet
            .super_sticker_details
            .and_then(|s| s.amount_display_string)
    });
    Some(ChatMessage {
        id: m.id?,
        t_received: now,
        t_published: snippet.published_at.and_then(|t| t.parse().ok()),
        author_channel_id: author.channel_id.unwrap_or_default(),
        author_name: author.display_name.unwrap_or_default(),
        is_owner: author.is_chat_owner.unwrap_or(false),
        is_moderator: author.is_chat_moderator.unwrap_or(false),
        is_member: author.is_chat_sponsor.unwrap_or(false),
        kind,
        text,
        amount,
    })
}

/// Remembers the last [`SEEN`] message ids.
#[derive(Default)]
struct Seen {
    set: HashSet<String>,
    order: VecDeque<String>,
}

impl Seen {
    /// True the first time an id is seen.
    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_owned()) {
            return false;
        }
        self.order.push_back(id.to_owned());
        if self.order.len() > SEEN
            && let Some(old) = self.order.pop_front()
        {
            self.set.remove(&old);
        }
        true
    }
}

impl Client {
    /// Streams chat for `chat_id` until the broadcast goes offline or chat is refused,
    /// reconnecting with the last page token (backoff 1 s to 30 s). Each (re)connect
    /// costs one `streamList` call of quota. In `--mock` mode, plays `<fixture>/chat.jsonl`.
    pub async fn run_chat(&self, chat_id: &str, send: impl Fn(ChatUpdate)) {
        if let Backend::Mock(mock) = &self.backend {
            send(ChatUpdate::Connected);
            return mock.play_chat(&self.clock, send).await;
        }
        let mut seen = Seen::default();
        let mut page_token = None;
        let mut backoff = Duration::from_secs(1);
        loop {
            let result = self
                .stream_chat(chat_id, &mut page_token, &mut seen, &send)
                .await;
            let (error, retry) = match result {
                Ok(Stream::Offline) => return send(ChatUpdate::Offline),
                // The server closed a healthy stream; reconnect right away.
                Ok(Stream::Closed) => {
                    backoff = Duration::from_secs(1);
                    continue;
                }
                Err(Failure::Fatal(error)) => return send(ChatUpdate::Stopped(error)),
                Err(Failure::Retry(error)) => (error, backoff),
            };
            send(ChatUpdate::Down {
                error,
                retry_in: retry,
            });
            tokio::time::sleep(retry).await;
            backoff = (backoff * 2).min(MAX_BACKOFF);
        }
    }

    async fn stream_chat(
        &self,
        chat_id: &str,
        page_token: &mut Option<String>,
        seen: &mut Seen,
        send: &impl Fn(ChatUpdate),
    ) -> Result<Stream, Failure> {
        let Backend::Rest { http, auth } = &self.backend else {
            unreachable!("mock chat is played above");
        };
        let now = || self.clock.now_ts();
        let retry = |e: &dyn std::fmt::Display| Failure::Retry(e.to_string());
        self.quota
            .check(Method::ChatStreamList, false, now())
            .map_err(|e| Failure::Fatal(e.to_string()))?;
        let token = auth.access_token(http).await.map_err(|e| retry(&e))?;
        let tls = ClientTlsConfig::new().with_native_roots();
        let channel = Channel::from_static(ENDPOINT)
            .tls_config(tls)
            .map_err(|e| retry(&e))?
            .connect()
            .await
            .map_err(|e| retry(&e))?;
        let mut grpc = tonic::client::Grpc::new(channel);
        grpc.ready().await.map_err(|e| retry(&e))?;
        let mut request = tonic::Request::new(ListRequest {
            live_chat_id: Some(chat_id.to_owned()),
            max_results: Some(500),
            page_token: page_token.clone(),
            part: vec!["id".into(), "snippet".into(), "authorDetails".into()],
        });
        let bearer = format!("Bearer {token}")
            .parse()
            .map_err(|_| Failure::Fatal("access token is not a valid header".into()))?;
        request.metadata_mut().insert("authorization", bearer);
        let path = http::uri::PathAndQuery::from_static(STREAM_LIST);
        let codec = tonic_prost::ProstCodec::<ListRequest, ListResponse>::default();
        let response = grpc.server_streaming(request, path, codec).await;
        // The call reached YouTube: it costs quota whether or not it was accepted.
        self.record(Method::ChatStreamList, response.is_ok());
        let mut stream = response.map_err(status_failure)?.into_inner();
        send(ChatUpdate::Connected);
        loop {
            let Some(page) = stream.message().await.map_err(status_failure)? else {
                return Ok(Stream::Closed);
            };
            if page.next_page_token.is_some() {
                *page_token = page.next_page_token;
            }
            for item in page.items {
                if let Some(msg) = normalise(item, now())
                    && seen.insert(&msg.id)
                {
                    send(ChatUpdate::Message(msg));
                }
            }
            if page.offline_at.is_some() {
                return Ok(Stream::Offline);
            }
        }
    }
}

enum Stream {
    Offline,
    Closed,
}

enum Failure {
    Retry(String),
    Fatal(String),
}

/// Errors that more tries cannot fix stop the service (each try costs quota).
fn status_failure(s: tonic::Status) -> Failure {
    let text = format!("{:?}: {}", s.code(), s.message());
    match s.code() {
        Code::FailedPrecondition => Failure::Fatal(format!("chat disabled or ended ({text})")),
        Code::PermissionDenied | Code::NotFound | Code::InvalidArgument => Failure::Fatal(text),
        Code::Unauthenticated => Failure::Retry(format!("{text}; {}", crate::LOGIN_HINT)),
        _ => Failure::Retry(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn decodes_and_normalises_a_superchat() {
        let wire = ListResponse {
            offline_at: None,
            next_page_token: Some("tok".into()),
            items: vec![LiveChatMessage {
                id: Some("m1".into()),
                snippet: Some(Snippet {
                    r#type: Some(15),
                    published_at: Some("2026-11-19T10:00:00Z".into()),
                    display_message: Some("₹100 from ana: hi".into()),
                    text_message_details: None,
                    super_chat_details: Some(SuperChatDetails {
                        amount_display_string: Some("₹100.00".into()),
                        user_comment: Some("hi".into()),
                    }),
                    super_sticker_details: None,
                }),
                author_details: Some(AuthorDetails {
                    channel_id: Some("UCana".into()),
                    display_name: Some("ana".into()),
                    is_chat_owner: None,
                    is_chat_sponsor: Some(true),
                    is_chat_moderator: Some(false),
                }),
            }],
        }
        .encode_to_vec();
        let page = ListResponse::decode(wire.as_slice()).unwrap();
        assert_eq!(page.next_page_token.as_deref(), Some("tok"));
        let now: Timestamp = "2026-11-19T10:00:01Z".parse().unwrap();
        let msg = normalise(page.items[0].clone(), now).unwrap();
        assert_eq!(msg.kind, ChatKind::Superchat);
        assert_eq!(msg.text, "hi");
        assert_eq!(msg.amount.as_deref(), Some("₹100.00"));
        assert_eq!(msg.level(), ac_core::event::Level::Member);
        assert_eq!(
            msg.t_published,
            Some("2026-11-19T10:00:00Z".parse().unwrap())
        );
    }

    #[test]
    fn seen_drops_duplicates_and_forgets_old_ids() {
        let mut seen = Seen::default();
        assert!(seen.insert("a"));
        assert!(!seen.insert("a"));
        for n in 0..SEEN {
            seen.insert(&n.to_string());
        }
        assert!(seen.insert("a"), "oldest id forgotten");
    }
}
