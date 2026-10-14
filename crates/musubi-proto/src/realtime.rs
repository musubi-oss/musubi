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
}

impl ServerMessage {
    pub fn error(e: &musubi_core::Error) -> Self {
        ServerMessage::Error {
            code: e.code(),
            message: e.to_string(),
        }
    }
}
