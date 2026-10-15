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
    /// Relayed multiplayer: create a client-hosted match (server relays data, no authority).
    MatchCreate {
        #[serde(default)]
        name: Option<String>,
    },
    /// Join by match id, or by matchmaker `token` (all matched users may join the same match).
    MatchJoin {
        #[serde(default)]
        match_id: Option<String>,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        metadata: std::collections::HashMap<String, String>,
    },
    MatchLeave {
        match_id: String,
    },
    /// Send to everyone in the match (or only `presences` when given). `reliable=false` lets
    /// transports drop under pressure.
    /// Server meshing: stream entity deltas around a point of interest in a world.
    WorldSubscribe {
        world: String,
        x: f64,
        y: f64,
        radius: f64,
    },
    WorldUnsubscribe {
        world: String,
    },
    /// Player input for an entity; the replication layer routes it to the current owner.
    WorldInput {
        world: String,
        entity: String,
        input: serde_json::Value,
    },
    MatchDataSend {
        match_id: String,
        op_code: i64,
        #[serde(default)]
        data: Vec<u8>,
        #[serde(default)]
        presences: Vec<UserPresence>,
        #[serde(default = "yes")]
        reliable: bool,
    },
    /// Invite an account into my party; the invitee may then `party_join` a closed party directly.
    PartyInvite {
        party_id: String,
        account_id: AccountId,
    },
}

fn yes() -> bool {
    true
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
    /// First message of a session opened with `resumable`, and again after a successful resume.
    Session {
        session_id: String,
        resume_token: String,
        resumed: bool,
    },
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
    /// The ticket waited longer than the server's TTL and was dropped; add again to keep searching.
    MatchmakerExpired {
        ticket: String,
    },
    /// A group was formed. `token` is presented when joining the resulting match.
    MatchmakerMatched {
        ticket: String,
        token: String,
        users: Vec<MatchmakerUser>,
        #[serde(rename = "self")]
        me: MatchmakerUser,
        /// Dedicated server allocated for the group (tickets carried `fleet=<name>`).
        #[serde(default)]
        server: Option<MatchmakerServer>,
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
    /// Reply to `MatchCreate` / `MatchJoin`.
    Match {
        match_id: String,
        /// `true` when a server-side handler owns the match; relayed matches are `false`.
        authoritative: bool,
        #[serde(default)]
        label: Option<String>,
        size: u32,
        presences: Vec<UserPresence>,
        #[serde(rename = "self")]
        me: UserPresence,
    },
    /// Server meshing: what changed inside the client's interest since the last delta.
    WorldDelta {
        world: String,
        tick: u64,
        spawned: Vec<WorldEntity>,
        updated: Vec<WorldEntity>,
        despawned: Vec<String>,
    },
    MatchData {
        match_id: String,
        presence: Option<UserPresence>,
        op_code: i64,
        data: Vec<u8>,
        reliable: bool,
    },
    MatchPresenceEvent {
        match_id: String,
        joins: Vec<UserPresence>,
        leaves: Vec<UserPresence>,
    },
    /// Someone added (`added = true`) or removed a reaction; `count` is the new total for the emoji.
    ChannelMessageReaction {
        channel_id: String,
        message_id: String,
        emoji: String,
        account_id: AccountId,
        username: String,
        added: bool,
        count: u32,
    },
    /// Delivered to every session of the invited account.
    PartyInvite {
        party_id: String,
        from: UserPresence,
    },
}

/// Entity as seen by a client: position plus (changed) components with their versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldEntity {
    pub id: String,
    pub x: f64,
    pub y: f64,
    pub components: Vec<WorldComponent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldComponent {
    pub name: String,
    pub value: serde_json::Value,
    pub version: u64,
}

/// Where to connect when the matchmaker allocated a dedicated server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchmakerServer {
    pub server_id: String,
    pub fleet_id: String,
    pub address: String,
    pub port: u16,
    /// Shared by the group; the game server validates it via `/v1/gameserver/connect`.
    pub connect_token: String,
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
    /// Emoji reactions aggregated per emoji (history only; live messages start empty).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<Reaction>,
}

/// One emoji on a message: how many accounts reacted with it, and whether the viewer did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    #[serde(default)]
    pub me: bool,
}
