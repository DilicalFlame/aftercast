//! Live chat messages, normalised from YouTube (specs/06-youtube.md, "Live chat").
//! One JSON object per line in the session's `chat.jsonl`.

use serde::{Deserialize, Serialize};

use crate::event::Level;
use crate::time::Timestamp;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub t_received: Timestamp,
    #[serde(default)]
    pub t_published: Option<Timestamp>,
    pub author_channel_id: String,
    pub author_name: String,
    #[serde(default)]
    pub is_owner: bool,
    #[serde(default)]
    pub is_moderator: bool,
    #[serde(default)]
    pub is_member: bool,
    pub kind: ChatKind,
    pub text: String,
    /// Super Chat / Super Sticker amount as YouTube displays it, e.g. "₹100.00".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    Text,
    Superchat,
    Supersticker,
    Membership,
    Deleted,
    Other,
}

impl ChatMessage {
    /// Highest permission level of the author.
    pub fn level(&self) -> Level {
        if self.is_owner {
            Level::Owner
        } else if self.is_moderator {
            Level::Mod
        } else if self.is_member {
            Level::Member
        } else {
            Level::Viewer
        }
    }
}
