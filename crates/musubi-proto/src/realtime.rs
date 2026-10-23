use musubi_core::{AccountId, Code};
use serde::{Deserialize, Serialize};

use crate::UserPresence;

/// Delivery semantics of a realtime frame, the same names on every transport.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Arrives, in order (TCP/WebSocket, QUIC stream, UDP channel 0).
    ReliableOrdered,
    /// Arrives, any order; degrades to reliable_ordered on today's transports.
    ReliableUnordered,
    /// May be lost; stale frames are dropped by `seq` (QUIC/WebTransport datagram, WebRTC
    /// unordered unreliable channel, UDP channel 1).
    UnreliableSequenced,
    /// May be lost, any order (datagrams).
    Unreliable,
}

impl Channel {
    pub fn reliable(self) -> bool {
        matches!(self, Channel::ReliableOrdered | Channel::ReliableUnordered)
    }

    /// Channels a transport implements natively; anything else degrades to reliable_ordered.
    pub fn native(transport: &str) -> Vec<Channel> {
        match transport {
            "quic" | "webtransport" | "webrtc" => vec![
                Channel::ReliableOrdered,
                Channel::UnreliableSequenced,
                Channel::Unreliable,
            ],
            "udp" => vec![Channel::ReliableOrdered, Channel::UnreliableSequenced],
            _ => vec![Channel::ReliableOrdered],
        }
    }
}

/// Client → server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ClientMessage {
    Ping,
    /// Set this session's status text (`None` clears it). Visible to followers.
    StatusUpdate {
        #[serde(default)]
        status: Option<String>,
        /// Rich presence object; `null`/absent keeps the current one, `{}` clears it.
        #[serde(default, with = "crate::json_in_binary")]
        rich: Option<serde_json::Value>,
        /// Appear offline to followers (absent = unchanged).
        #[serde(default)]
        appear_offline: Option<bool>,
    },
    /// Start receiving presence events for these users; replies with a snapshot.
    StatusFollow {
        account_ids: Vec<AccountId>,
    },
    StatusUnfollow {
        account_ids: Vec<AccountId>,
    },
    /// Receive `storage_update` for one object (`key`) or for every object of an owner's
    /// collection (`key` omitted; own objects only). Another owner's object must be readable.
    StorageSubscribe {
        collection: String,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        owner_id: Option<AccountId>,
    },
    StorageUnsubscribe {
        collection: String,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        owner_id: Option<AccountId>,
    },
    /// Call a server-side RPC over the socket.
    Rpc {
        id: String,
        #[serde(default, with = "crate::json_in_binary")]
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
        #[serde(with = "crate::json_in_binary")]
        content: serde_json::Value,
        /// Thread: id of the message this one answers (same channel).
        #[serde(default)]
        reply_to: Option<String>,
    },
    ChannelMessageUpdate {
        channel_id: String,
        message_id: String,
        #[serde(with = "crate::json_in_binary")]
        content: serde_json::Value,
    },
    ChannelMessageRemove {
        channel_id: String,
        message_id: String,
    },
    /// "I am typing" — fanned out to the channel as `ChannelTyping`, never stored; the server
    /// forwards at most one per 2 s per channel, so send it freely.
    ChannelTyping {
        channel_id: String,
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
    /// Confirm a `MatchmakerReadyCheck`; the match is delivered once every ticket confirmed.
    MatchmakerReady {
        ticket: String,
    },
    /// Parties: small realtime groups with a leader. Closed parties admit members via join requests.
    PartyCreate {
        #[serde(default)]
        open: bool,
        #[serde(default = "default_party_size")]
        max_size: u32,
        /// Free-form label (≤ 256 chars) that `party_list` can search by.
        #[serde(default)]
        label: Option<String>,
        /// Hidden parties never show up in `party_list`.
        #[serde(default)]
        hidden: bool,
    },
    /// Leader only: change label / open / hidden / max_size; members get a fresh `party`.
    PartyUpdate {
        party_id: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        open: Option<bool>,
        #[serde(default)]
        hidden: Option<bool>,
        #[serde(default)]
        max_size: Option<u32>,
    },
    /// Cluster-wide listing of visible parties with a free seat.
    PartyList {
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        label_prefix: Option<String>,
        #[serde(default)]
        open: Option<bool>,
        #[serde(default = "default_party_list_limit")]
        limit: u32,
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
        /// Photon `SuppressRoomEvents` / `SuppressPlayerInfo`: members get no join / leave
        /// presence events and the `Match` reply carries no member list (`size` only) — for big
        /// rooms where the game tracks players through its own data messages.
        #[serde(default)]
        suppress_presence: bool,
    },
    /// Client-measured round trip to the server (ping→pong), used to pick relayed-match hosts.
    Rtt {
        ms: u32,
    },
    /// Relayed matches: the current host hands the host role to another member.
    MatchHostTransfer {
        match_id: String,
        session_id: musubi_core::SessionId,
    },
    /// Join by match id, or by matchmaker `token` (all matched users may join the same match).
    MatchJoin {
        #[serde(default)]
        match_id: Option<String>,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        metadata: std::collections::HashMap<String, String>,
        /// Watch only (authoritative matches): not a player, inputs refused.
        #[serde(default)]
        spectator: bool,
        /// Spectator stream delay in ticks (anti-ghosting); the match's
        /// `params.spectator_delay_ticks` is the floor.
        #[serde(default)]
        delay_ticks: u64,
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
        /// Per-tick delta budget in bytes (estimated); default = the world's `client_budget_bytes`.
        /// Entities are ranked by relevance × age and deferred when the budget is spent.
        #[serde(default)]
        budget_bytes: Option<u32>,
        /// Fine filter: object of component name → value every delivered entity must match.
        #[serde(default, with = "crate::json_in_binary")]
        filter: Option<serde_json::Value>,
        /// Most deltas per second this client wants (default: every tick); the world may cap it
        /// lower. Skipped ticks are folded into the next delta, nothing is lost.
        #[serde(default)]
        max_hz: Option<u32>,
        /// Position-only updates of entities this client already knows go out as unreliable,
        /// sequenced frames (`WorldDelta.sequenced`; datagrams on QUIC/WebTransport, a lost one is
        /// superseded by the next move) and are confirmed reliably every `keyframe_ticks`.
        /// Spawns, despawns and component changes are always reliable.
        #[serde(default)]
        unreliable_updates: bool,
        /// Container whose contents this client wants to see even while it is closed (its own
        /// inventory, the chest it is browsing); omit/null to stop looking inside.
        #[serde(default)]
        inside: Option<String>,
    },
    WorldUnsubscribe {
        world: String,
    },
    /// Player input for an entity; the replication layer routes it to the current owner.
    WorldInput {
        world: String,
        entity: String,
        #[serde(with = "crate::json_in_binary")]
        input: serde_json::Value,
    },
    /// Engine-level networking relayed through the replication layer (Unreal `UMusubiNetDriver`):
    /// open a packet relay to whichever simulator owns `entity` now. Reply `net_opened` carries
    /// the `conn` id to use on this entity (below 2^53: safe as a JSON number) and the packet
    /// size limit.
    NetOpen {
        world: String,
        entity: String,
    },
    /// One engine packet (opaque bytes) for the current owner of `entity`; `conn` is the id from
    /// `net_opened`. Routed like `world_input` (buffered while the entity is in transit) and
    /// delivered to the owner as a `net_packet` simulator event. Unreliable by nature: transports
    /// with an unreliable lane may drop it, the engine's own reliability sits on top.
    NetPacket {
        world: String,
        entity: String,
        conn: u64,
        #[serde(default)]
        data: Vec<u8>,
    },
    /// The engine closed its connection; the owner gets a `net_close` event.
    NetClose {
        world: String,
        entity: String,
        conn: u64,
        #[serde(default)]
        reason: String,
    },
    /// Deterministic-rollback matches (`params.rollback`): an input stamped with the simulation
    /// `tick` it belongs to. Inputs for a future tick wait for it; inputs for a past tick within
    /// the rollback window make the server re-simulate from that tick; older ones are dropped.
    /// Each is acknowledged with `match_input_ack`.
    MatchInput {
        match_id: String,
        tick: u64,
        /// Client-side counter echoed in the ack so the client can trim its input buffer.
        #[serde(default)]
        seq: u64,
        #[serde(default)]
        op_code: i64,
        #[serde(default)]
        data: Vec<u8>,
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
        /// Delivery channel; overrides `reliable` when set. Transports without the channel
        /// degrade (see `transport` → `channels`): WebSocket delivers everything reliable_ordered.
        #[serde(default)]
        channel: Option<Channel>,
    },
    /// What this connection can do: reply `transport` lists the native channels of the transport
    /// in use and the frame limit; `compress: "lz4"` turns on per-frame lz4 for binary (`postcard`)
    /// sessions — every binary frame from then on, both ways, is tagged `0` raw / `1` lz4 block
    /// (`lz4_wrap`), frames over 256 bytes compressed.
    Transport {
        #[serde(default)]
        compress: Option<String>,
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

fn default_party_list_limit() -> u32 {
    50
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
    /// A subscribed storage object was written (`value`, `version`) or deleted.
    StorageUpdate {
        collection: String,
        key: String,
        owner_id: AccountId,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        value: Option<serde_json::Value>,
        #[serde(default)]
        deleted: bool,
    },
    Rpc {
        id: String,
        #[serde(with = "crate::json_in_binary")]
        payload: serde_json::Value,
    },
    Notification {
        id: String,
        subject: String,
        #[serde(with = "crate::json_in_binary")]
        content: serde_json::Value,
        code: i32,
        sender_id: Option<AccountId>,
        create_time: i64,
        persistent: bool,
        /// Unix time after which the client should drop it (absent = keep).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expire_time: Option<i64>,
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
    /// Someone in the channel is typing (ephemeral; show it for a couple of seconds).
    ChannelTyping {
        channel_id: String,
        user: UserPresence,
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
    /// A group was formed but must confirm: answer `MatchmakerReady` within `deadline_ms`.
    MatchmakerReadyCheck {
        ticket: String,
        token: String,
        deadline_ms: u64,
        users: Vec<MatchmakerUser>,
    },
    /// The ready-check ran out. `requeued`: this ticket confirmed and is back in the queue with
    /// its wait kept; otherwise it dodged and its account is blocked for `penalty_secs`.
    MatchmakerReadyFailed {
        ticket: String,
        token: String,
        requeued: bool,
        penalty_secs: u64,
    },
    /// Reply to create/join/accept: the party as the receiver sees it.
    Party {
        party_id: String,
        open: bool,
        max_size: u32,
        #[serde(default)]
        label: String,
        #[serde(default)]
        hidden: bool,
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
    /// Reply to `party_list`.
    PartyList {
        parties: Vec<PartyListing>,
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
        /// Relayed matches: the member that simulates the game; `None` for authoritative ones.
        #[serde(default)]
        host: Option<UserPresence>,
        /// Relayed matches created with `suppress_presence`: `presences` is empty and no
        /// `MatchPresenceEvent` is sent for this match.
        #[serde(default)]
        suppress_presence: bool,
    },
    /// Relayed matches: the host changed (left, dropped or handed over) — `host` simulates now.
    MatchHost {
        match_id: String,
        host: UserPresence,
    },
    /// Server meshing: what changed inside the client's interest since the last delta.
    WorldDelta {
        world: String,
        tick: u64,
        spawned: Vec<WorldEntity>,
        updated: Vec<WorldEntity>,
        despawned: Vec<uuid::Uuid>,
        /// Slowest time scale among the cells around the client (1.0 = real time; EVE-style
        /// time dilation when a simulator cannot keep up). Interpolate against it.
        #[serde(default = "one_f64")]
        time_scale: f64,
        /// Unreliable, sequenced frame (positions only, see `WorldSubscribe.unreliable_updates`):
        /// apply when `tick` is newer than the last applied; never carries spawns or despawns.
        #[serde(default)]
        sequenced: bool,
    },
    /// Reply to `net_open`: `conn` identifies this relay on `entity`, `server` is the simulator
    /// that owns it right now (`null` while it is between owners), `max_packet` the largest `data`
    /// accepted on the relay.
    NetOpened {
        world: String,
        entity: String,
        conn: u64,
        #[serde(default)]
        server: Option<uuid::Uuid>,
        max_packet: u32,
    },
    /// An engine packet from the simulator owning the entity behind `conn`.
    NetPacket {
        conn: u64,
        #[serde(default)]
        data: Vec<u8>,
    },
    /// The simulator closed the relay (its connection to this client).
    NetClose {
        conn: u64,
        #[serde(default)]
        reason: String,
    },
    /// Control-lane bytes from the simulator owning the entity behind `conn` (`net_meta` command):
    /// reliable and ordered, delivered before any later `net_packet` from the same owner.
    NetMeta {
        conn: u64,
        #[serde(default)]
        data: Vec<u8>,
    },
    /// The entity behind `conn` finished a handover: packets now reach `server`, which knows
    /// nothing of the engine connection the old owner had. The engine must restart its connection
    /// on the same `conn` (the old owner has already been told to close its side).
    NetRehome {
        world: String,
        entity: String,
        conn: u64,
        #[serde(default)]
        server: Option<uuid::Uuid>,
    },
    /// `match_input` received: `applied` is `false` when the tick was older than the rollback
    /// window (or the match does not run with rollback).
    MatchInputAck {
        match_id: String,
        tick: u64,
        seq: u64,
        applied: bool,
    },
    /// Rollback matches: FNV-1a 64 hash of the authoritative state after `tick` (every
    /// `params.rollback.hash_every` ticks) — a client whose predicted state hashes differently
    /// mispredicted and must resync.
    MatchTickHash {
        match_id: String,
        tick: u64,
        hash: u64,
    },
    /// Typed match state (IDL `[matches.<m>] state` + `sync`): full public snapshot on join.
    MatchState {
        match_id: String,
        tick: u64,
        state: serde_json::Value,
    },
    /// RFC 6902 patch from the previous synced state to the current one, after a handler call.
    MatchStateDelta {
        match_id: String,
        tick: u64,
        ops: Vec<serde_json::Value>,
    },
    MatchData {
        match_id: String,
        presence: Option<UserPresence>,
        op_code: i64,
        data: Vec<u8>,
        reliable: bool,
        /// Channel the frame was sent on: `unreliable_sequenced` frames carry `seq` (per match,
        /// per session, from 0) — drop one whose `seq` is below the last applied.
        #[serde(default)]
        channel: Option<Channel>,
        #[serde(default)]
        seq: Option<u64>,
    },
    /// Reply to `transport`: the transport in use (`ws`, `quic`, `webtransport`, `webrtc`, `udp`),
    /// its native channels, the frame limit, supported compression and whether it is on now.
    Transport {
        transport: String,
        channels: Vec<Channel>,
        max_frame_bytes: usize,
        compression: Vec<String>,
        compressed: bool,
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
    /// Fan-out from a module/server stream (`stream.send` host op or `POST /v1/server/streams/{id}/send`):
    /// `stream` is the free-form stream id the session joined, `sender` the originating player
    /// when a player-context module call sent it (absent for server-originated data).
    StreamData {
        stream: String,
        #[serde(with = "crate::json_in_binary")]
        data: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender: Option<UserPresence>,
    },
}

/// Entity as seen by a client: position plus (changed) components with their versions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldEntity {
    pub id: uuid::Uuid,
    /// Binary wire: whole centimetres (see `musubi_proto::pos_cm`).
    #[serde(with = "crate::pos_cm")]
    pub x: f64,
    #[serde(with = "crate::pos_cm")]
    pub y: f64,
    pub components: Vec<WorldComponent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorldComponent {
    pub name: std::sync::Arc<str>,
    pub value: crate::JsonText,
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
    /// Team index assigned by a queue with teams.
    #[serde(default)]
    pub team: Option<u32>,
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
    #[serde(with = "crate::json_in_binary")]
    pub content: serde_json::Value,
    pub create_time: i64,
    pub update_time: i64,
    pub persistent: bool,
    /// Emoji reactions aggregated per emoji (history only; live messages start empty).
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    /// Thread parent, when this message is a reply.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Accounts @-mentioned in the content (resolved server-side).
    #[serde(default)]
    pub mentions: Vec<AccountId>,
}

/// One emoji on a message: how many accounts reacted with it, and whether the viewer did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    #[serde(default)]
    pub me: bool,
}

fn one_f64() -> f64 {
    1.0
}

/// One row of `party_list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartyListing {
    pub party_id: String,
    pub label: String,
    pub open: bool,
    pub max_size: u32,
    pub size: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader_id: Option<String>,
}
