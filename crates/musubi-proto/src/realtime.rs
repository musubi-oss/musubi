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
    /// Enter the matchmaker. Replies with `MatchmakerTicket`; later `MatchmakerMatched`.
    MatchmakerAdd {
        min_count: u32,
        max_count: u32,
        #[serde(default = "default_query")]
        query: String,
        #[serde(default)]
        string_properties: std::collections::HashMap<String, String>,
        #[serde(default)]
        numeric_properties: std::collections::HashMap<String, f64>,
        #[serde(default = "one")]
        count_multiple: u32,
    },
    MatchmakerRemove {
        ticket: String,
    },
    /// Parties: small realtime groups with a leader. Closed parties admit members via join requests.
    PartyCreate {
        #[serde(default)]
        open: bool,
        #[serde(default = "default_party_size")]
        max_size: u32,
    },
    PartyJoin {
        party_id: String,
    },
    PartyLeave {
        party_id: String,
    },
    /// Leader only: make another member leader.
    PartyPromote {
        party_id: String,
        presence: UserPresence,
    },
    /// Leader only: accept a pending join request.
    PartyAccept {
        party_id: String,
        presence: UserPresence,
    },
    /// Leader only: kick a member or reject a join request.
    PartyRemove {
        party_id: String,
        presence: UserPresence,
    },
    /// Leader only: disband.
    PartyClose {
        party_id: String,
    },
    PartyJoinRequestList {
        party_id: String,
    },
    PartyDataSend {
        party_id: String,
        op_code: i64,
        #[serde(default)]
        data: Vec<u8>,
    },
    /// Leader only: matchmake the whole party as one ticket.
    PartyMatchmakerAdd {
        party_id: String,
        min_count: u32,
        max_count: u32,
        #[serde(default = "default_query")]
        query: String,
        #[serde(default)]
        string_properties: std::collections::HashMap<String, String>,
        #[serde(default)]
        numeric_properties: std::collections::HashMap<String, f64>,
        #[serde(default = "one")]
        count_multiple: u32,
    },
    PartyMatchmakerRemove {
        party_id: String,
        ticket: String,
    },
}

fn default_party_size() -> u32 {
    4
}

fn default_query() -> String {
    "*".into()
}

fn one() -> u32 {
    1
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
    MatchmakerTicket {
        ticket: String,
    },
    /// A group was formed. `token` is presented when joining the resulting match.
    MatchmakerMatched {
        ticket: String,
        token: String,
        users: Vec<MatchmakerUser>,
        #[serde(rename = "self")]
        me: MatchmakerUser,
    },
    /// Reply to create/join/accept: the party as the receiver sees it.
    Party {
        party_id: String,
        open: bool,
        max_size: u32,
        #[serde(rename = "self")]
        me: UserPresence,
        leader: UserPresence,
        presences: Vec<UserPresence>,
    },
    /// Sent to the leader when someone asks to join a closed party, and as the reply to `PartyJoinRequestList`.
    PartyJoinRequest {
        party_id: String,
        presences: Vec<UserPresence>,
    },
    PartyLeader {
        party_id: String,
        presence: UserPresence,
    },
    PartyPresenceEvent {
        party_id: String,
        joins: Vec<UserPresence>,
        leaves: Vec<UserPresence>,
    },
    PartyData {
        party_id: String,
        presence: UserPresence,
        op_code: i64,
        data: Vec<u8>,
    },
    PartyMatchmakerTicket {
        party_id: String,
        ticket: String,
    },
    PartyClose {
        party_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchmakerUser {
    pub presence: UserPresence,
    #[serde(default)]
    pub string_properties: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub numeric_properties: std::collections::HashMap<String, f64>,
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
