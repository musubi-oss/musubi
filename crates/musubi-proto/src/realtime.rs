use musubi_core::{AccountId, Code};
use serde::{Deserialize, Serialize};

use crate::UserPresence;

/// Client → server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    Ping,
    /// Set this session's status text (`None` clears it). Visible to followers.
    StatusUpdate {
        #[serde(default)]
        status: Option<String>,
    },
    /// Start receiving presence events for these users; replies with a snapshot.
    StatusFollow {
        account_ids: Vec<AccountId>,
    },
    StatusUnfollow {
        account_ids: Vec<AccountId>,
    },
    /// Call a server-side RPC over the socket.
    Rpc {
        id: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
    /// Join a chat channel. `target` is a room name, a user id (direct) or a group id.
    ChannelJoin {
        target: String,
        kind: ChannelKind,
        #[serde(default)]
        persistence: bool,
        #[serde(default)]
        hidden: bool,
    },
    ChannelLeave {
        channel_id: String,
    },
    ChannelMessageSend {
        channel_id: String,
        content: serde_json::Value,
    },
    ChannelMessageUpdate {
        channel_id: String,
        message_id: String,
        content: serde_json::Value,
    },
    ChannelMessageRemove {
        channel_id: String,
        message_id: String,
    },
}

/// Server → client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ServerMessage {
    Pong,
    Error {
        code: Code,
        message: String,
    },
    /// Snapshot in reply to `status_follow`, and incremental updates afterwards.
    Status {
        #[serde(default)]
        presences: Vec<UserPresence>,
    },
    StatusPresenceEvent {
        #[serde(default)]
        joins: Vec<UserPresence>,
        #[serde(default)]
        leaves: Vec<UserPresence>,
    },
    Rpc {
        id: String,
        payload: serde_json::Value,
    },
    Notification {
        id: String,
        subject: String,
        content: serde_json::Value,
        code: i32,
        sender_id: Option<AccountId>,
        create_time: i64,
        persistent: bool,
    },
    /// Reply to `ChannelJoin`.
    Channel {
        id: String,
        kind: ChannelKind,
        presences: Vec<UserPresence>,
        #[serde(rename = "self")]
        me: UserPresence,
    },
    ChannelMessage(ChannelMessage),
    /// Ack for send/update/remove: the stored message id and timestamps.
    ChannelMessageAck {
        channel_id: String,
        message_id: String,
        code: i32,
        create_time: i64,
        update_time: i64,
        persistent: bool,
    },
    ChannelPresenceEvent {
        channel_id: String,
        joins: Vec<UserPresence>,
        leaves: Vec<UserPresence>,
    },
}

impl ServerMessage {
    pub fn error(e: &musubi_core::Error) -> Self {
        ServerMessage::Error {
            code: e.code(),
            message: e.to_string(),
        }
    }
}

/// Chat channel kinds (Nakama numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i16", try_from = "i16")]
#[repr(i16)]
pub enum ChannelKind {
    Room = 2,
    Direct = 3,
    Group = 4,
}

impl ChannelKind {
    pub fn from_i16(v: i16) -> Self {
        match v {
            3 => Self::Direct,
            4 => Self::Group,
            _ => Self::Room,
        }
    }
}

impl From<ChannelKind> for i16 {
    fn from(k: ChannelKind) -> i16 {
        k as i16
    }
}

impl TryFrom<i16> for ChannelKind {
    type Error = String;
    fn try_from(v: i16) -> Result<Self, String> {
        match v {
            2..=4 => Ok(Self::from_i16(v)),
            _ => Err(format!("bad channel kind {v}")),
        }
    }
}

/// A chat message, both as persisted history and as the live event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub channel_id: String,
    pub message_id: String,
    pub kind: ChannelKind,
    /// 0 = message, 1 = updated, 2 = removed; negative codes are system events.
    pub code: i32,
    pub sender_id: Option<AccountId>,
    pub username: String,
    pub content: serde_json::Value,
    pub create_time: i64,
    pub update_time: i64,
    pub persistent: bool,
}
