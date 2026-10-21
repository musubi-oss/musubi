//! Deliver realtime messages to sessions on this node and, through the cluster bus, on every other node.

use std::sync::Arc;

use musubi_cluster::{BusMessage, Cluster};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use musubi_fleet::FleetService;
use musubi_proto::realtime::MatchmakerServer;

use musubi_core::{AccountId, SessionId};
use musubi_matchmaker::{Matched, MatchedWire, Matchmaker, TicketWire};
use musubi_proto::{ServerMessage, UserPresence};
use serde::{Deserialize, Serialize};

use crate::realtime::party::PartyCmdEnv;
use crate::realtime::relayed::RelayedCmdEnv;
use crate::realtime::world::{WorldCmd, WorldCmdEnv, WorldRegistry};
use crate::realtime::{Hub, PartyRegistry, RelayedRegistry};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyCheckWire {
    pub token: String,
    pub deadline_ms: u64,
    pub tickets: Vec<TicketWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyExpiredWire {
    pub token: String,
    pub requeued: Vec<TicketWire>,
    pub dodged: Vec<TicketWire>,
    pub penalty_secs: u64,
}

#[derive(Serialize, Deserialize)]
enum Wire {
    Account {
        account_id: AccountId,
        msg: ServerMessage,
    },
    Channel {
        channel_id: String,
        msg: ServerMessage,
    },
    Presence {
        target: AccountId,
        joins: Vec<UserPresence>,
        leaves: Vec<UserPresence>,
    },
    MmAdd(TicketWire),
    MmRemove(String),
    MmMatched(MatchedWire),
    /// Leader: a formed group must confirm; replicas drop the tickets and tell their sessions.
    MmReadyCheck(ReadyCheckWire),
    /// A member confirmed; only the leader acts.
    MmReady {
        ticket: String,
        session: SessionId,
    },
    /// Leader: a ready-check ran out; confirmed tickets come back, dodgers are penalised.
    MmReadyExpired(ReadyExpiredWire),
    /// A running match freed `slots` (a member left, the game server asked for more) or ended
    /// (`None`); only the leader's backfill pool acts.
    MmRelease {
        match_id: String,
        slots: Option<usize>,
    },
    Session {
        session_id: SessionId,
        msg: ServerMessage,
    },
    PartyCmd(PartyCmdEnv),
    SessionGone(SessionId),
    /// A node rebuilt a parked resumable session from its record; the holder drops its copy.
    SessionSteal(SessionId),
    /// Session count change of an account on a node (cluster-wide online set).
    Online {
        node: uuid::Uuid,
        account: AccountId,
        delta: i64,
    },
    RelayedCmd(RelayedCmdEnv),
    WorldCmd(WorldCmdEnv),
    /// Cell host → cluster, when something near the border changed: component-level delta of the
    /// cell's border band (keyframe every `ghost_keyframe_ticks`), ghosts for the hosts of the
    /// neighbouring cells.
    WorldGhosts {
        world: String,
        delta: musubi_mesh::GhostDelta,
    },
    /// Cell host → its standby replicas (`replicas`): this tick's component-level delta of the
    /// whole cell (keyframe every `repl_keyframe_ticks`). Nodes not listed ignore it.
    WorldRepl {
        world: String,
        from_node: uuid::Uuid,
        /// Host incarnation: a mirror never goes back to an older epoch of the cell's host.
        #[serde(default)]
        epoch: i64,
        replicas: Vec<uuid::Uuid>,
        delta: musubi_mesh::GhostDelta,
    },
    /// Replica → cell host: the mirror is complete up to `tick`.
    WorldReplAck {
        world: String,
        cell: musubi_mesh::CellId,
        node: uuid::Uuid,
        tick: u64,
    },
    /// Cell host → host of `entity.cell`: the entity crossed into a cell that lives on another
    /// node; full state plus inputs buffered so far.
    WorldHandover {
        world: String,
        entity: musubi_mesh::Entity,
        from_node: uuid::Uuid,
        inputs: Vec<serde_json::Value>,
        /// Cell migration: the host takes the entity even without a simulator on the cell.
        forced: bool,
        /// Set when `entity` carries only the components newer than this revision: the receiver
        /// already holds the rest as a ghost streamed by `WorldGhosts` and merges onto it.
        #[serde(default)]
        base_rev: Option<u64>,
    },
    /// Receiving host → `from_node`: handover taken (or refused: no simulator owns the cell).
    WorldHandoverAck {
        world: String,
        /// Who answers: a late ack is only believed from the node the directory names as the
        /// host of the entity's cell.
        acker: uuid::Uuid,
        entity: musubi_mesh::EntityId,
        from_node: uuid::Uuid,
        accepted: bool,
        /// Refused because the receiver had no ghost to build a partial handover on: the sender
        /// retries with the full state.
        #[serde(default)]
        resend_full: bool,
    },
    /// A node restored these entities of `cell` from a dead host's shard; a host that owns a live
    /// copy of any of them answers `WorldDrop`.
    WorldAdopted {
        world: String,
        cell: musubi_mesh::CellId,
        from_node: uuid::Uuid,
        ids: Vec<musubi_mesh::EntityId>,
    },
    /// Reply to `WorldAdopted`: `to_node` holds duplicates of these — drop them.
    WorldDrop {
        world: String,
        to_node: uuid::Uuid,
        from_node: uuid::Uuid,
        /// Each duplicate with the cell the sender holds it in and whether the sender's own copy
        /// is provisional: the receiver only believes a sender its directory names as that
        /// cell's host, and keeps a copy that wins the order (see `copy_wins`).
        ids: Vec<(musubi_mesh::EntityId, musubi_mesh::CellId, bool)>,
    },
    /// Cross-cell transaction, coordinator → cluster: hold the listed entities you own and vote.
    WorldTxPrepare {
        world: String,
        tx: uuid::Uuid,
        coordinator: uuid::Uuid,
        ops: Vec<crate::realtime::world::TxOp>,
        ttl_ticks: u64,
    },
    /// Participant → coordinator: the entities it now holds for `tx`, or why it refuses.
    WorldTxVote {
        world: String,
        tx: uuid::Uuid,
        coordinator: uuid::Uuid,
        node: uuid::Uuid,
        entities: Vec<musubi_mesh::EntityId>,
        reason: Option<String>,
    },
    WorldTxCommit {
        world: String,
        tx: uuid::Uuid,
        ops: Vec<crate::realtime::world::TxOp>,
    },
    WorldTxAbort {
        world: String,
        tx: uuid::Uuid,
        reason: String,
    },
    /// Input for an entity that lives on another node (e.g. buffered during a handover).
    WorldInput {
        world: String,
        entity: musubi_mesh::EntityId,
        input: serde_json::Value,
    },
    /// Co-locate request for an entity living on another node: pin it into `cell` (hosted by the
    /// sender) for `ttl_ticks`; its shard hands it over like a border crossing.
    WorldCoLocate {
        world: String,
        entity: musubi_mesh::EntityId,
        cell: musubi_mesh::CellId,
        by: uuid::Uuid,
        ttl_ticks: u64,
    },
    /// Coordinator → cluster: `node` now hosts `cell` of `world` (cells of one world may spread).
    WorldCellAssign {
        world: String,
        cell: musubi_mesh::CellId,
        node: uuid::Uuid,
    },
    /// A node started hosting a cell of `world`: the home replays the live subscriptions
    /// (`WorldCmd::Subscribe` per session) so the new shard knows whom to send deltas to.
    WorldSubsRequest {
        world: String,
    },
    /// A hierarchy/filter query over every shard of a split world: each cell host answers
    /// for the entities it holds with `WorldQueryReply`; the asker merges (see
    /// `WorldRegistry::query`).
    WorldQuery {
        world: String,
        req: uuid::Uuid,
        from: uuid::Uuid,
        kind: crate::realtime::world::QueryKind,
    },
    WorldQueryReply {
        world: String,
        req: uuid::Uuid,
        node: uuid::Uuid,
        entities: Vec<serde_json::Value>,
    },
    /// A receiver is missing control-lane frames `from..=until` of `to`'s `epoch`: the sender
    /// re-publishes them from its retained window (`CONTROL_WINDOW` frames).
    OrderNack {
        to: uuid::Uuid,
        epoch: u64,
        from: u64,
        until: u64,
    },
    /// Several bulk messages (replica/ghost deltas) in one bus frame: the bulk lane drains its
    /// queue into one frame so one QUIC send carries a whole tick's worth of deltas.
    /// The home deleted `world`: shards hosting its cells elsewhere go with it.
    WorldDeleted {
        world: String,
    },
    WorldBatch(Vec<Wire>),
    /// A control-lane message with its place in the sender's order: the lane keeps several
    /// sends in flight (one at a time was ~110 msg/s on a loaded node, a third of the handovers
    /// asked) and the receiver puts them back in sequence. `epoch` changes when the sender
    /// restarts, so a fresh count is not taken for a late message.
    Ordered {
        epoch: u64,
        seq: u64,
        inner: Box<Wire>,
    },
}

impl Wire {
    fn kind(&self) -> &'static str {
        match self {
            Wire::Account { .. } => "Account",
            Wire::Channel { .. } => "Channel",
            Wire::Presence { .. } => "Presence",
            Wire::MmAdd { .. } => "MmAdd",
            Wire::MmRemove { .. } => "MmRemove",
            Wire::MmMatched { .. } => "MmMatched",
            Wire::MmReadyCheck { .. } => "MmReadyCheck",
            Wire::MmReady { .. } => "MmReady",
            Wire::MmReadyExpired { .. } => "MmReadyExpired",
            Wire::MmRelease { .. } => "MmRelease",
            Wire::Session { .. } => "Session",
            Wire::PartyCmd { .. } => "PartyCmd",
            Wire::SessionGone { .. } => "SessionGone",
            Wire::SessionSteal { .. } => "SessionSteal",
            Wire::Online { .. } => "Online",
            Wire::RelayedCmd { .. } => "RelayedCmd",
            Wire::WorldCmd { .. } => "WorldCmd",
            Wire::WorldGhosts { .. } => "WorldGhosts",
            Wire::WorldRepl { .. } => "WorldRepl",
            Wire::WorldReplAck { .. } => "WorldReplAck",
            Wire::WorldHandover { .. } => "WorldHandover",
            Wire::WorldHandoverAck { .. } => "WorldHandoverAck",
            Wire::WorldAdopted { .. } => "WorldAdopted",
            Wire::WorldDrop { .. } => "WorldDrop",
            Wire::WorldTxPrepare { .. } => "WorldTxPrepare",
            Wire::WorldTxVote { .. } => "WorldTxVote",
            Wire::WorldTxCommit { .. } => "WorldTxCommit",
            Wire::WorldTxAbort { .. } => "WorldTxAbort",
            Wire::WorldInput { .. } => "WorldInput",
            Wire::WorldCoLocate { .. } => "WorldCoLocate",
            Wire::WorldCellAssign { .. } => "WorldCellAssign",
            Wire::WorldSubsRequest { .. } => "WorldSubsRequest",
            Wire::WorldQuery { .. } => "WorldQuery",
            Wire::WorldQueryReply { .. } => "WorldQueryReply",
            Wire::OrderNack { .. } => "OrderNack",
            Wire::WorldDeleted { .. } => "WorldDeleted",
            Wire::WorldBatch { .. } => "WorldBatch",
            Wire::Ordered { .. } => "Ordered",
        }
    }
}

const TOPIC: &str = "realtime";
/// Most messages folded into one bus frame. A lane sends one frame at a time, so this bounds its
/// throughput — but a frame is serialized (twice: `BusMessage.payload` is a `Value`) on the
/// runtime, and 512-message frames took 120–1 000 ms each and starved the sessions' sends (fps
/// per client 24 → 18). 64 keeps a frame under ~20 ms; the real fix is a raw payload.
const BULK_BATCH_MAX: usize = 64;
/// One publish lane: a queue drained by its own task (see `Fanout::publish`).
type Lane = OnceLock<tokio::sync::mpsc::UnboundedSender<(std::time::Instant, Wire)>>;
/// `publish_many` chunk: 512 subscribe commands are ~200 KB on the wire, far under the QUIC cap.
const PUBLISH_MANY_CHUNK: usize = 512;

/// Control-lane sends in flight per peer set. Sends complete when the peer has read the frame,
/// so this many frames overlap their ~9 ms each; order is restored on the receiver.
const CONTROL_IN_FLIGHT: usize = 8;
/// A control-lane gap older than this is given up on (the frame failed and comes back late over
/// Postgres, or never): what arrived after it is dispatched.
const ORDER_GAP_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// A gap open this long is asked for (`OrderNack`) — before the give-up above, and only when
/// a later frame keeps arriving (the check runs on arrival, there is no timer).
const ORDER_NACK_AFTER: std::time::Duration = std::time::Duration::from_millis(50);
/// One nack per gap per this interval.
const ORDER_NACK_EVERY: std::time::Duration = std::time::Duration::from_millis(200);
/// Control-lane frames a sender keeps for replay.
const CONTROL_WINDOW: usize = 256;
/// A remote session's frame waiting on the session lane this long marks the session congested:
/// the world stops building new deltas for it until it went out. One second, not a few ticks:
/// gating at 100 ms on the stand bounded the lane's wait (11.5 s → 4.8 s worst) but deferred
/// 465 000 deltas a run and cost every remote player a fifth of their frame rate (26.7 → 21.3
/// fps) — the lane's own send throughput is the ceiling there, not the queue. At one second only
/// frames that would arrive useless anyway are held back.
const SESSION_CONGESTED_AFTER: std::time::Duration = std::time::Duration::from_secs(1);
/// Given-up sequence numbers remembered per sender (a late frame among them is still new).
const ORDER_SKIPPED_KEEP: u64 = 4096;

#[derive(Default)]
struct OrderState {
    epoch: u64,
    next: u64,
    ahead: std::collections::BTreeMap<u64, Wire>,
    ahead_since: Option<std::time::Instant>,
    /// Sequence numbers skipped after `ORDER_GAP_TIMEOUT`: one of them arriving late (a replay,
    /// a Postgres redelivery) is still new; any other frame below `next` was already dispatched.
    skipped: std::collections::BTreeSet<u64>,
    /// `next` the last nack asked for, and when.
    nacked: Option<(u64, std::time::Instant)>,
}

/// What one control-lane frame does to a sender's order.
#[derive(Default)]
struct Accepted {
    ready: Vec<Wire>,
    /// Frames `from..=until` to ask the sender for.
    nack: Option<(u64, u64)>,
    /// The frame was already dispatched (a replay the gap no longer needs).
    dup: bool,
}

impl OrderState {
    fn accept(&mut self, epoch: u64, seq: u64, w: Wire, now: std::time::Instant) -> Accepted {
        let mut out = Accepted::default();
        if self.epoch != epoch {
            // the sender restarted (or this is the first message from it)
            out.ready
                .extend(std::mem::take(&mut self.ahead).into_values());
            self.epoch = epoch;
            self.next = seq;
            self.ahead_since = None;
            self.skipped.clear();
            self.nacked = None;
        }
        if seq < self.next {
            if self.skipped.remove(&seq) {
                out.ready.push(w);
            } else {
                out.dup = true;
            }
        } else if seq == self.next {
            out.ready.push(w);
            self.next += 1;
            while let Some(n) = self.ahead.remove(&self.next) {
                out.ready.push(n);
                self.next += 1;
            }
            self.ahead_since = if self.ahead.is_empty() {
                None
            } else {
                Some(now)
            };
            if self.ahead.is_empty() {
                self.nacked = None;
            }
        } else {
            self.ahead.insert(seq, w);
            let since = *self.ahead_since.get_or_insert(now);
            let open = now.saturating_duration_since(since);
            let first = *self.ahead.keys().next().unwrap_or(&seq);
            if open > ORDER_GAP_TIMEOUT {
                // skip the gap: everything held goes out in order; what was skipped stays known
                metrics::counter!("musubi_bus_order_gaps_total").increment(1);
                for s in self.next..first {
                    self.skipped.insert(s);
                }
                let mut held = std::mem::take(&mut self.ahead);
                self.next = first;
                while let Some(n) = held.remove(&self.next) {
                    out.ready.push(n);
                    self.next += 1;
                }
                self.ahead = held;
                self.ahead_since = if self.ahead.is_empty() {
                    None
                } else {
                    Some(now)
                };
                self.nacked = None;
            } else if open >= ORDER_NACK_AFTER
                && self.nacked.is_none_or(|(n, t)| {
                    n != self.next || now.saturating_duration_since(t) >= ORDER_NACK_EVERY
                })
            {
                out.nack = Some((self.next, first - 1));
                self.nacked = Some((self.next, now));
            }
        }
        let floor = self.next.saturating_sub(ORDER_SKIPPED_KEEP);
        self.skipped = self.skipped.split_off(&floor);
        out
    }
}

/// The control-lane frames this node sent last (serialized as they went out), for `OrderNack`.
#[derive(Default)]
struct ControlWindow {
    epoch: u64,
    frames: std::collections::VecDeque<(u64, Box<serde_json::value::RawValue>)>,
}

#[derive(Clone)]
pub struct Fanout {
    hub: Arc<Hub>,
    cluster: Option<Arc<Cluster>>,
    /// Accounts with live sessions on other nodes: account → node → session count.
    online_remote: Arc<dashmap::DashMap<AccountId, std::collections::HashMap<uuid::Uuid, i64>>>,
    mm: Arc<Matchmaker>,
    parties: Arc<PartyRegistry>,
    relayed: Arc<RelayedRegistry>,
    /// Only the leader runs matchmaker passes; tickets are replicated to every node.
    leader: Arc<AtomicBool>,
    fleets: Arc<OnceLock<Arc<FleetService>>>,
    worlds: Arc<OnceLock<Arc<WorldRegistry>>>,
    /// Records "recently played with" when a relayed join is executed here for a remote session.
    social: Arc<OnceLock<musubi_social::SocialService>>,
    /// Bus messages leave in the order they were published: one publisher task per node, fed by
    /// this queue (a task per message would let two inputs from one source overtake each other).
    bus: Arc<Lane>,
    bus_bulk: Arc<Lane>,
    /// Messages to sessions living on other nodes (world deltas above all): batched per drain so
    /// 1 000 remote players × 30 Hz is a few bus frames per tick, not 30 000 — and they no longer
    /// queue in front of handovers on the control lane.
    bus_session: Arc<Lane>,
    /// Player inputs for worlds homed elsewhere and inputs forwarded to the owning shard: one per
    /// player per input, batched — on the control lane they queued handovers 200 s behind them.
    bus_input: Arc<Lane>,
    /// Per sender: where its control-lane sequence stands and what arrived ahead of it.
    ordered: Arc<std::sync::Mutex<std::collections::HashMap<uuid::Uuid, OrderState>>>,
    control_window: Arc<std::sync::Mutex<ControlWindow>>,
    /// Remote sessions with frames still waiting on the session lane: how many, and since when
    /// the oldest waits. The world defers building the next delta for a session whose frame has
    /// waited longer than `SESSION_CONGESTED_AFTER` (`session_congested`).
    session_inflight: Arc<dashmap::DashMap<SessionId, (u32, std::time::Instant)>>,
    bus_user: Arc<Lane>,
}

impl Fanout {
    pub fn new(
        hub: Arc<Hub>,
        cluster: Option<Arc<Cluster>>,
        mm: Arc<Matchmaker>,
        parties: Arc<PartyRegistry>,
        relayed: Arc<RelayedRegistry>,
    ) -> Self {
        let f = Self {
            hub,
            cluster,
            mm,
            parties,
            relayed,
            leader: Arc::new(AtomicBool::new(true)),
            fleets: Arc::new(OnceLock::new()),
            bus: Arc::new(OnceLock::new()),
            bus_bulk: Arc::new(OnceLock::new()),
            bus_session: Arc::new(OnceLock::new()),
            bus_input: Arc::new(OnceLock::new()),
            ordered: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            control_window: Arc::new(std::sync::Mutex::new(ControlWindow::default())),
            session_inflight: Arc::new(dashmap::DashMap::new()),
            bus_user: Arc::new(OnceLock::new()),
            worlds: Arc::new(OnceLock::new()),
            social: Arc::new(OnceLock::new()),
            online_remote: Arc::new(dashmap::DashMap::new()),
        };
        if let Some(c) = &f.cluster {
            let publisher = f.clone();
            let node = c.node_id().0;
            f.hub.set_remote(Box::new(move |target, joins, leaves| {
                let delta = joins.len() as i64 - leaves.len() as i64;
                if delta != 0 {
                    publisher.publish(Wire::Online {
                        node,
                        account: target,
                        delta,
                    });
                }
                publisher.publish(Wire::Presence {
                    target,
                    joins,
                    leaves,
                });
            }));
            let mut members = c.member_events();
            let mut leaves = c.subscribe();
            let me = f.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        m = members.recv() => match m {
                            Ok(m) if m.state == "dead" => me.purge_node(m.id),
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(_) => break,
                        },
                        b = leaves.recv() => match b {
                            Ok(b) if b.topic == musubi_cluster::LEAVE_TOPIC => {
                                let id = serde_json::from_str::<uuid::Uuid>(b.payload.get())
                                    .ok()
                                    .or_else(|| {
                                        b.payload_value()
                                            .as_str()
                                            .and_then(|s| s.parse::<uuid::Uuid>().ok())
                                    });
                                match id {
                                    Some(id) => me.purge_node(id),
                                    None => me.purge_node(b.origin.0),
                                }
                            }
                            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                            Err(_) => break,
                        },
                    }
                }
            });
            let mut rx = c.subscribe();
            let me = f.clone();
            // the broadcast ring drops messages for a slow subscriber; applying a message takes
            // world locks and can be slow under replication traffic, so a trivial forwarder drains
            // the ring into an unbounded queue and a worker applies from there — nothing is lost,
            // a burst only grows the queue for a moment
            // two workers: control traffic (handovers, their acks, assignments) must not wait
            // behind the delta batches — with one worker a peer's ack took seconds to be applied
            // and the handover had long been rolled back
            let (q_tx, mut q_rx) =
                tokio::sync::mpsc::unbounded_channel::<musubi_cluster::BusMessage>();
            let (fast_tx, mut fast_rx) =
                tokio::sync::mpsc::unbounded_channel::<musubi_cluster::BusMessage>();
            let me_fast = f.clone();
            tokio::spawn(async move {
                while let Some(m) = fast_rx.recv().await {
                    me_fast.apply(&m);
                }
            });
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(m) => {
                            let tx = if Fanout::is_bulk(&m) { &q_tx } else { &fast_tx };
                            if tx.send(m).is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(dropped = n, "fanout bus lagged");
                        }
                        Err(_) => break,
                    }
                }
            });
            tokio::spawn(async move {
                while let Some(m) = q_rx.recv().await {
                    me.apply(&m);
                }
            });
        }
        f
    }

    pub fn account(&self, account_id: AccountId, msg: ServerMessage) {
        self.account_live(account_id, msg);
    }

    /// Drop every local session of `account` from a channel (moderation ban).
    pub fn leave_channel_everywhere(&self, channel_id: &str, account: AccountId) {
        for p in self.hub.presences_of(account) {
            self.hub.leave_channel(channel_id, p.session_id);
        }
    }

    /// Number of sessions of the account on this node.
    pub fn hub_presences(&self, account_id: AccountId) -> usize {
        self.hub.presences_of(account_id).len()
    }

    /// Forget every session a node reported: it died or left, so its accounts are offline there.
    pub fn purge_node(&self, node: uuid::Uuid) {
        self.ordered.lock().unwrap().remove(&node);
        self.online_remote.retain(|_, per_node| {
            per_node.remove(&node);
            !per_node.is_empty()
        });
    }

    /// Distinct accounts online anywhere in the cluster (local sockets plus what the bus told us).
    pub fn online_accounts(&self) -> usize {
        self.hub.online_accounts()
            + self
                .online_remote
                .iter()
                .filter(|e| self.hub.presences_of(*e.key()).is_empty())
                .count()
    }

    /// Online on this node or, as far as the bus has told us, on any other node.
    pub fn is_online_anywhere(&self, account_id: AccountId) -> bool {
        !self.hub.presences_of(account_id).is_empty()
            || self.online_remote.contains_key(&account_id)
    }

    /// Like `account`, returning how many local sessions took the message (0 = not online here).
    pub fn account_live(&self, account_id: AccountId, msg: ServerMessage) -> usize {
        let n = self.hub.send_to_account(account_id, msg.clone());
        self.publish(Wire::Account { account_id, msg });
        n
    }

    /// Like `channel`, skipping one local session (the author of a typing indicator).
    pub fn channel_except(&self, channel_id: &str, except: SessionId, msg: ServerMessage) {
        self.hub
            .broadcast_channel_except(channel_id, except, msg.clone());
        self.publish(Wire::Channel {
            channel_id: channel_id.to_string(),
            msg,
        });
    }

    pub fn channel(&self, channel_id: &str, msg: ServerMessage) {
        self.hub.broadcast_channel(channel_id, msg.clone());
        self.publish(Wire::Channel {
            channel_id: channel_id.to_string(),
            msg,
        });
    }

    /// Deliver to one session wherever it lives: locally if we have it, otherwise via the bus.
    pub fn session(&self, session_id: SessionId, msg: ServerMessage) {
        match self.hub.try_send_session(session_id, msg) {
            Ok(()) => {}
            Err(crate::realtime::SessionSendError::Full) => {
                metrics::counter!("musubi_session_dropped_total", "reason" => "queue_full")
                    .increment(1);
            }
            Err(crate::realtime::SessionSendError::NotHere(msg)) => {
                self.session_inflight
                    .entry(session_id)
                    .or_insert_with(|| (0, std::time::Instant::now()))
                    .0 += 1;
                self.publish(Wire::Session {
                    session_id,
                    msg: *msg,
                });
            }
        }
    }

    /// A remote session has a frame that has waited on the session lane longer than
    /// `SESSION_CONGESTED_AFTER`: building another delta for it now would only lengthen the
    /// queue (the stand saw 7–10 s of session-lane wait); the world skips it this tick and the
    /// client's interest accumulates the change instead. A frame merely waiting for the next
    /// batch does not count — deferring on that halved the remote players' frame rate.
    pub fn session_congested(&self, session_id: SessionId) -> bool {
        self.session_inflight
            .get(&session_id)
            .is_some_and(|e| e.0 > 0 && e.1.elapsed() > SESSION_CONGESTED_AFTER)
    }

    fn session_frames_sent(
        inflight: &dashmap::DashMap<SessionId, (u32, std::time::Instant)>,
        sids: &[SessionId],
    ) {
        for sid in sids {
            if let Some(mut e) = inflight.get_mut(sid) {
                e.0 = e.0.saturating_sub(1);
                if e.0 == 0 {
                    drop(e);
                    inflight.remove_if(sid, |_, v| v.0 == 0);
                } else {
                    // the next-oldest frame was queued after this one went in; a tick is close
                    e.1 = std::time::Instant::now();
                }
            }
        }
    }

    /// Forward a party command to the party's home node.
    pub fn party_cmd(&self, env: PartyCmdEnv) {
        self.publish(Wire::PartyCmd(env));
    }

    /// A session disconnected: every node drops it from parties homed there.
    pub fn session_gone(&self, session_id: SessionId) {
        self.publish(Wire::SessionGone(session_id));
    }

    pub fn steal_session(&self, session_id: SessionId) {
        self.publish(Wire::SessionSteal(session_id));
    }

    /// Run a matchmaker pass right now (leader only; no-op elsewhere).
    pub fn matchmake_now(&self) {
        crate::realtime::matchmaking::process_and_deliver(&self.relayed, &self.mm, self);
    }

    pub fn is_leader(&self) -> bool {
        self.leader.load(Ordering::Relaxed)
    }

    pub fn set_leader(&self, v: bool) {
        if self.leader.swap(v, Ordering::Relaxed) != v {
            tracing::info!(leader = v, "matchmaker leadership changed");
        }
    }

    pub fn mm_add(&self, ticket: &str) {
        if let Some(w) = self.mm.get(ticket) {
            self.publish(Wire::MmAdd(w));
        }
    }

    pub fn mm_remove(&self, ticket: &str) {
        self.publish(Wire::MmRemove(ticket.to_string()));
    }

    pub fn set_fleets(&self, fleets: Arc<FleetService>) {
        let _ = self.fleets.set(fleets);
    }

    pub fn set_worlds(&self, worlds: Arc<WorldRegistry>) {
        let _ = self.worlds.set(worlds);
    }

    pub fn world_cmd(&self, env: WorldCmdEnv) {
        self.publish(Wire::WorldCmd(env));
    }

    /// Accept a handed-over entity and ack it once the journal is durable.
    #[allow(clippy::too_many_arguments)]
    fn take_handover(
        &self,
        w: Arc<WorldRegistry>,
        world: String,
        entity: musubi_mesh::Entity,
        from_node: uuid::Uuid,
        inputs: Vec<serde_json::Value>,
        forced: bool,
        base_rev: Option<u64>,
    ) {
        let id = entity.id;
        if w.accept_handover(&world, entity, from_node, inputs, forced, base_rev, self) {
            metrics::counter!("musubi_world_handover_recv_total", "path" => "accepted")
                .increment(1);
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = w.flush_journal(&world).await {
                    tracing::warn!(world = %world, error = %e, "handover journal flush failed");
                }
                me.world_handover_ack(&world, id, from_node, true, false);
            });
        }
    }

    pub fn world_query(
        &self,
        world: &str,
        req: uuid::Uuid,
        from: uuid::Uuid,
        kind: crate::realtime::world::QueryKind,
    ) {
        self.publish(Wire::WorldQuery {
            world: world.to_string(),
            req,
            from,
            kind,
        });
    }

    pub fn world_query_reply(
        &self,
        world: &str,
        req: uuid::Uuid,
        node: uuid::Uuid,
        entities: Vec<serde_json::Value>,
    ) {
        self.publish(Wire::WorldQueryReply {
            world: world.to_string(),
            req,
            node,
            entities,
        });
    }

    pub fn world_deleted(&self, world: &str) {
        self.publish(Wire::WorldDeleted {
            world: world.to_string(),
        });
    }

    /// A burst of world commands (subscription replay) as a few batches, order kept.
    pub fn world_cmds(&self, envs: Vec<WorldCmdEnv>) {
        self.publish_many(envs.into_iter().map(Wire::WorldCmd).collect());
    }

    /// Publish a burst as a few `WorldBatch` messages (order kept) instead of one message each.
    /// Only on the QUIC bus: a batch would blow the Postgres NOTIFY cap and be dropped whole.
    fn publish_many(&self, items: Vec<Wire>) {
        let quic = self
            .cluster
            .as_ref()
            .is_some_and(|c| c.bus_addr().is_some());
        if !quic {
            for w in items {
                self.publish(w);
            }
            return;
        }
        let mut items = items.into_iter().peekable();
        while items.peek().is_some() {
            let chunk: Vec<Wire> = items.by_ref().take(PUBLISH_MANY_CHUNK).collect();
            self.publish(Wire::WorldBatch(chunk));
        }
    }

    /// Publish the border entities of a locally hosted cell as ghosts for neighbour-cell hosts.
    pub fn world_ghosts(&self, world: &str, delta: musubi_mesh::GhostDelta) {
        self.publish(Wire::WorldGhosts {
            world: world.to_string(),
            delta,
        });
    }

    pub fn world_repl(
        &self,
        world: &str,
        from_node: uuid::Uuid,
        epoch: i64,
        replicas: Vec<uuid::Uuid>,
        delta: musubi_mesh::GhostDelta,
    ) {
        self.publish(Wire::WorldRepl {
            world: world.to_string(),
            from_node,
            epoch,
            replicas,
            delta,
        });
    }

    pub fn world_repl_ack(
        &self,
        world: &str,
        cell: musubi_mesh::CellId,
        node: uuid::Uuid,
        tick: u64,
    ) {
        self.publish(Wire::WorldReplAck {
            world: world.to_string(),
            cell,
            node,
            tick,
        });
    }

    /// Ship an entity to the node hosting its new cell.
    pub fn world_handover(
        &self,
        world: &str,
        entity: musubi_mesh::Entity,
        from_node: uuid::Uuid,
        inputs: Vec<serde_json::Value>,
        forced: bool,
        base_rev: Option<u64>,
    ) {
        self.publish(Wire::WorldHandover {
            world: world.to_string(),
            entity,
            from_node,
            inputs,
            forced,
            base_rev,
        });
    }

    /// Answer a cross-node handover.
    pub fn world_handover_ack(
        &self,
        world: &str,
        entity: musubi_mesh::EntityId,
        from_node: uuid::Uuid,
        accepted: bool,
        resend_full: bool,
    ) {
        let acker = self.worlds.get().map(|w| w.node_id()).unwrap_or_default();
        self.publish(Wire::WorldHandoverAck {
            world: world.to_string(),
            acker,
            entity,
            from_node,
            accepted,
            resend_full,
        });
    }

    pub fn world_adopted(
        &self,
        world: &str,
        cell: musubi_mesh::CellId,
        from_node: uuid::Uuid,
        ids: Vec<musubi_mesh::EntityId>,
    ) {
        self.publish(Wire::WorldAdopted {
            world: world.to_string(),
            cell,
            from_node,
            ids,
        });
    }
    pub fn world_drop(
        &self,
        world: &str,
        to_node: uuid::Uuid,
        from_node: uuid::Uuid,
        ids: Vec<(musubi_mesh::EntityId, musubi_mesh::CellId, bool)>,
    ) {
        self.publish(Wire::WorldDrop {
            world: world.to_string(),
            to_node,
            from_node,
            ids,
        });
    }

    pub fn world_tx_prepare(
        &self,
        world: &str,
        tx: uuid::Uuid,
        coordinator: uuid::Uuid,
        ops: Vec<crate::realtime::world::TxOp>,
        ttl_ticks: u64,
    ) {
        self.publish(Wire::WorldTxPrepare {
            world: world.to_string(),
            tx,
            coordinator,
            ops,
            ttl_ticks,
        });
    }

    pub fn world_tx_vote(
        &self,
        world: &str,
        tx: uuid::Uuid,
        coordinator: uuid::Uuid,
        node: uuid::Uuid,
        entities: Vec<musubi_mesh::EntityId>,
        reason: Option<String>,
    ) {
        self.publish(Wire::WorldTxVote {
            world: world.to_string(),
            tx,
            coordinator,
            node,
            entities,
            reason,
        });
    }

    pub fn world_tx_commit(
        &self,
        world: &str,
        tx: uuid::Uuid,
        ops: Vec<crate::realtime::world::TxOp>,
    ) {
        self.publish(Wire::WorldTxCommit {
            world: world.to_string(),
            tx,
            ops,
        });
    }

    pub fn world_tx_abort(&self, world: &str, tx: uuid::Uuid, reason: String) {
        self.publish(Wire::WorldTxAbort {
            world: world.to_string(),
            tx,
            reason,
        });
    }

    /// Forward an input to whichever node owns the entity.
    pub fn world_input(
        &self,
        world: &str,
        entity: musubi_mesh::EntityId,
        input: serde_json::Value,
    ) {
        self.publish(Wire::WorldInput {
            world: world.to_string(),
            entity,
            input,
        });
    }

    pub fn world_co_locate(
        &self,
        world: &str,
        entity: musubi_mesh::EntityId,
        cell: musubi_mesh::CellId,
        by: uuid::Uuid,
        ttl_ticks: u64,
    ) {
        self.publish(Wire::WorldCoLocate {
            world: world.to_string(),
            entity,
            cell,
            by,
            ttl_ticks,
        });
    }

    /// Announce that `node` hosts `cell` of `world`.
    pub fn cell_assign(&self, world: &str, cell: musubi_mesh::CellId, node: uuid::Uuid) {
        self.publish(Wire::WorldCellAssign {
            world: world.to_string(),
            cell,
            node,
        });
    }

    /// The bus does not echo to the publisher, so every matchmaker message is handled here first
    /// and then sent to the other nodes.
    /// A running match wants players again (`Some(n)`) or ended (`None`): the leader's backfill
    /// pool learns of it wherever the member left or the request came in.
    pub fn mm_release(&self, match_id: &str, slots: Option<usize>) {
        if self.is_leader() {
            self.mm.release_slots(match_id, slots);
        }
        self.publish(Wire::MmRelease {
            match_id: match_id.to_string(),
            slots,
        });
    }

    pub fn mm_ready(&self, ticket: &str, session: SessionId) {
        self.handle_ready(ticket, session);
        self.publish(Wire::MmReady {
            ticket: ticket.to_string(),
            session,
        });
    }

    pub fn mm_ready_check(&self, p: &musubi_matchmaker::Pending) {
        let w = ReadyCheckWire {
            token: p.token.clone(),
            deadline_ms: p
                .deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis() as u64,
            tickets: p.matched.tickets.iter().map(TicketWire::from).collect(),
        };
        self.handle_ready_check(&w);
        self.publish(Wire::MmReadyCheck(w));
    }

    pub fn mm_ready_expired(&self, e: &musubi_matchmaker::ReadyExpired) {
        let w = ReadyExpiredWire {
            token: e.token.clone(),
            requeued: e.requeued.iter().map(TicketWire::from).collect(),
            dodged: e.dodged.iter().map(TicketWire::from).collect(),
            penalty_secs: e.penalty.as_secs(),
        };
        self.handle_ready_expired(&w);
        self.publish(Wire::MmReadyExpired(w));
    }

    fn handle_ready_check(&self, w: &ReadyCheckWire) {
        for t in &w.tickets {
            self.mm.cancel(&t.id);
        }
        crate::realtime::matchmaking::deliver_ready_check(&self.hub, w);
    }

    fn handle_ready(&self, ticket: &str, session: SessionId) {
        if !self.is_leader() {
            return;
        }
        match self.mm.mark_ready(ticket, session) {
            Ok(Some((m, match_id))) => {
                self.mm.register_open(&match_id, &m);
                self.deliver_matched(m, match_id);
            }
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, ticket, "ready ignored"),
        }
    }

    fn handle_ready_expired(&self, w: &ReadyExpiredWire) {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(w.penalty_secs);
        for t in &w.dodged {
            for m in &t.members {
                self.mm.penalize_until(m.presence.account_id, until);
            }
        }
        for t in &w.requeued {
            if let Err(e) = self.mm.add_replica(t.clone()) {
                tracing::warn!(error = %e, "bad requeued ticket");
            }
        }
        crate::realtime::matchmaking::deliver_ready_failed(&self.hub, &self.parties, w);
    }

    fn mm_matched(&self, m: &Matched, match_id: &str, server: Option<MatchmakerServer>) {
        self.publish(Wire::MmMatched(m.to_wire(match_id, server)));
    }

    /// Leader side: hand a formed group to its members everywhere. When every ticket asked for a
    /// `fleet`, a dedicated server is allocated first (asynchronously) and travels with the result.
    pub fn deliver_matched(&self, m: Matched, match_id: String) {
        let fleet = m.fleet();
        let me = self.clone();
        tokio::spawn(async move {
            let server = match (fleet, me.fleets.get()) {
                (Some(name), Some(fleets)) => match fleets
                    .allocate_routed(
                        &name,
                        m.region.as_deref(),
                        serde_json::json!({ "token": m.token, "users": m.users() }),
                        false,
                        std::time::Duration::ZERO,
                    )
                    .await
                {
                    Ok(a) => Some(MatchmakerServer {
                        server_id: a.server_id.to_string(),
                        fleet_id: a.fleet_id.to_string(),
                        address: a.address,
                        port: a.port as u16,
                        connect_token: a.connect_token,
                    }),
                    Err(e) => {
                        tracing::warn!(fleet = %name, region = ?m.region, error = %e, "no server for matched group; delivering without one");
                        None
                    }
                },
                _ => None,
            };
            crate::realtime::matchmaking::deliver(
                &me.hub,
                &me.parties,
                &me.relayed,
                &m,
                &match_id,
                server.as_ref(),
            );
            me.mm_matched(&m, &match_id, server);
        });
    }

    pub fn set_social(&self, social: musubi_social::SocialService) {
        let _ = self.social.set(social);
    }

    pub fn relayed_cmd(&self, env: RelayedCmdEnv) {
        self.publish(Wire::RelayedCmd(env));
    }

    /// Bulk frame: deltas, ghosts, replicas, player commands and their batches; everything
    /// else (handovers, acks, assignments, user events) is applied by the fast worker.
    fn is_bulk(m: &BusMessage) -> bool {
        let text = m.payload.get();
        let mut t = musubi_cluster::first_key(text);
        if t == Some("WorldBatch") {
            // the batch's first item decides: `{"WorldBatch":[{"Session":...`
            t = text
                .find('[')
                .and_then(|i| musubi_cluster::first_key(&text[i + 1..]));
        }
        if t == Some("Ordered") {
            return false;
        }
        matches!(
            t,
            Some(
                "Session"
                    | "WorldGhosts"
                    | "WorldRepl"
                    | "WorldReplAck"
                    | "WorldCmd"
                    | "WorldInput"
            )
        )
    }

    fn apply(&self, m: &BusMessage) {
        if m.topic != TOPIC {
            return;
        }
        if m.sent_ms > 0 {
            let lag = musubi_cluster::unix_ms().saturating_sub(m.sent_ms);
            let queue = if Fanout::is_bulk(m) { "bulk" } else { "fast" };
            metrics::histogram!("musubi_bus_inbound_lag_ms", "queue" => queue).record(lag as f64);
        }
        match m.decode::<Wire>() {
            Ok(Wire::Ordered { epoch, seq, inner }) => {
                self.dispatch_ordered(m.origin.0, epoch, seq, *inner)
            }
            Ok(w) => self.dispatch(w),
            Err(e) => tracing::warn!(error = %e, "bad fanout message"),
        }
    }

    /// Put a sender's control-lane messages back in its order: dispatch what is next, hold
    /// what arrived ahead, ask the sender for a gap open `ORDER_NACK_AFTER` (`OrderNack`, it
    /// replays from its window), drop replays already dispatched, and after `ORDER_GAP_TIMEOUT`
    /// give up on a gap (a frame of it arriving later is still dispatched). Sends are only in
    /// flight concurrently within `CONTROL_IN_FLIGHT`, so what is held is at most that many frames.
    fn dispatch_ordered(&self, origin: uuid::Uuid, epoch: u64, seq: u64, w: Wire) {
        let acc = {
            let mut all = self.ordered.lock().unwrap();
            all.entry(origin)
                .or_default()
                .accept(epoch, seq, w, std::time::Instant::now())
        };
        if acc.dup {
            metrics::counter!("musubi_bus_order_dups_total").increment(1);
        }
        if let Some((from, until)) = acc.nack {
            metrics::counter!("musubi_bus_order_nacks_total").increment(1);
            self.publish(Wire::OrderNack {
                to: origin,
                epoch,
                from,
                until,
            });
        }
        for w in acc.ready {
            self.dispatch(w);
        }
    }

    /// Another node misses control frames of ours: re-publish them from the window (they carry
    /// their original sequence numbers, every other receiver drops them as duplicates).
    fn replay_control(&self, to: uuid::Uuid, epoch: u64, from: u64, until: u64) {
        let Some(c) = self.cluster.clone() else {
            return;
        };
        if to != c.node_id().0 {
            return;
        }
        let frames: Vec<Box<serde_json::value::RawValue>> = {
            let win = self.control_window.lock().unwrap();
            if win.epoch != epoch {
                return;
            }
            win.frames
                .iter()
                .filter(|(s, _)| *s >= from && *s <= until)
                .map(|(_, r)| r.clone())
                .collect()
        };
        let missing = (until + 1)
            .saturating_sub(from)
            .saturating_sub(frames.len() as u64);
        metrics::counter!("musubi_bus_order_replayed_total", "outcome" => "replayed")
            .increment(frames.len() as u64);
        metrics::counter!("musubi_bus_order_replayed_total", "outcome" => "gone")
            .increment(missing);
        if frames.is_empty() {
            return;
        }
        tokio::spawn(async move {
            for r in frames {
                if let Err(e) = c.publish_prio(TOPIC, &r, 10).await {
                    tracing::warn!(error = %e, "control replay failed");
                }
            }
        });
    }

    fn dispatch(&self, w: Wire) {
        match w {
            Wire::WorldBatch(items) => {
                for w in items {
                    self.dispatch(w);
                }
            }
            // sequencing is applied in `apply`; one that reaches here (inside a batch) is plain
            Wire::Ordered { inner, .. } => self.dispatch(*inner),
            Wire::OrderNack {
                to,
                epoch,
                from,
                until,
            } => self.replay_control(to, epoch, from, until),
            Wire::Account { account_id, msg } => {
                self.hub.send_to_account(account_id, msg);
            }
            Wire::Channel { channel_id, msg } => {
                self.hub.broadcast_channel(&channel_id, msg);
            }
            Wire::Presence {
                target,
                joins,
                leaves,
            } => {
                self.hub.notify_followers_local(target, joins, leaves);
            }
            Wire::SessionSteal(sid) => {
                if self.hub.steal(sid) {
                    tracing::info!(session = %sid, "parked session taken over by another node");
                }
            }
            Wire::Online {
                node,
                account,
                delta,
            } => {
                let mut e = self.online_remote.entry(account).or_default();
                let n = e.entry(node).or_insert(0);
                *n += delta;
                if *n <= 0 {
                    e.remove(&node);
                }
                let empty = e.is_empty();
                drop(e);
                if empty {
                    self.online_remote.remove_if(&account, |_, m| m.is_empty());
                }
            }
            Wire::MmAdd(w) => {
                if let Err(e) = self.mm.add_replica(w) {
                    tracing::warn!(error = %e, "bad replicated ticket");
                }
            }
            Wire::MmRemove(id) => {
                self.mm.cancel(&id);
            }
            Wire::MmReadyCheck(w) => self.handle_ready_check(&w),
            Wire::MmReady { ticket, session } => self.handle_ready(&ticket, session),
            Wire::MmRelease { match_id, slots } => {
                if self.is_leader() {
                    self.mm.release_slots(&match_id, slots);
                }
            }
            Wire::MmReadyExpired(w) => self.handle_ready_expired(&w),
            Wire::MmMatched(w) => {
                let match_id = w.match_id.clone();
                let server = w.server.clone();
                match Matched::from_wire(w) {
                    Ok(matched) => {
                        for t in &matched.tickets {
                            self.mm.cancel(&t.id);
                        }
                        crate::realtime::matchmaking::deliver(
                            &self.hub,
                            &self.parties,
                            &self.relayed,
                            &matched,
                            &match_id,
                            server.as_ref(),
                        );
                    }
                    Err(e) => tracing::warn!(error = %e, "bad matched message"),
                }
            }
            Wire::Session { session_id, msg } => {
                self.hub.send_to_session(session_id, msg);
            }
            Wire::PartyCmd(env) => {
                if self.parties.is_home(&env.party_id) {
                    if let Some(reply) = crate::realtime::party::exec(
                        &self.parties,
                        self,
                        &self.mm,
                        &env.me,
                        &env.party_id,
                        env.cmd,
                    ) {
                        self.session(env.me.session_id, reply);
                    }
                }
            }
            Wire::SessionGone(sid) => {
                for t in self.parties.leave_all(self, sid) {
                    self.mm.cancel(&t);
                    self.mm_remove(&t);
                }
                self.relayed.leave_all(self, sid);
                if let Some(w) = self.worlds.get() {
                    w.unsubscribe_all(sid);
                }
            }
            Wire::WorldGhosts { world, delta } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_ghosts(&world, delta);
                }
            }
            Wire::WorldRepl {
                world,
                from_node,
                epoch,
                replicas,
                delta,
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_repl(&world, from_node, epoch, &replicas, delta, self);
                }
            }
            Wire::WorldReplAck {
                world,
                cell,
                node,
                tick,
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_repl_ack(&world, cell, node, tick);
                }
            }
            Wire::WorldHandover {
                world,
                entity,
                from_node,
                inputs,
                forced,
                base_rev,
            } => {
                if let Some(w) = self.worlds.get().cloned() {
                    metrics::counter!("musubi_world_handover_recv_total", "path" => "received")
                        .increment(1);
                    // the cached cell directory can lag the assignment the sender acted on
                    // (a cell changing host): when it says the cell is not ours, ask the
                    // leader before turning the entity away — a refusal here left the sender
                    // waiting out the whole timeout
                    match w.cell_of(&world, entity.pos) {
                        Ok(c) if w.hosts_cell_cached(&world, c) => {
                            self.take_handover(
                                w, world, entity, from_node, inputs, forced, base_rev,
                            );
                        }
                        _ => {
                            let me = self.clone();
                            tokio::spawn(async move {
                                let Ok(c) = w.cell_of_any(&world, entity.pos).await else {
                                    me.world_handover_ack(
                                        &world, entity.id, from_node, false, false,
                                    );
                                    return;
                                };
                                let fresh = w.cell_host_of(&world, c).await.ok().flatten();
                                if fresh == Some(w.node_id()) {
                                    metrics::counter!("musubi_world_handover_recv_total", "path" => "here_after_fresh_lookup").increment(1);
                                    if let Err(e) = w.ensure_hosting(&world, c).await {
                                        tracing::warn!(world = %world, ?c, error = %e, "handover target cell not hosted");
                                    }
                                    me.take_handover(
                                        w, world, entity, from_node, inputs, forced, base_rev,
                                    );
                                } else {
                                    metrics::counter!("musubi_world_handover_recv_total", "path" => "not_here_confirmed").increment(1);
                                    me.world_handover_ack(
                                        &world, entity.id, from_node, false, false,
                                    );
                                }
                            });
                        }
                    }
                }
            }
            Wire::WorldHandoverAck {
                world,
                acker,
                entity,
                from_node,
                accepted,
                resend_full,
            } => {
                if let Some(w) = self.worlds.get() {
                    metrics::counter!("musubi_world_handover_acks_total", "path" => "received", "accepted" => accepted.to_string()).increment(1);
                    if from_node == w.node_id() {
                        w.handover_acked(&world, entity, acker, accepted, resend_full, self);
                    } else {
                        metrics::counter!("musubi_world_handover_acks_total", "path" => "not_mine", "accepted" => accepted.to_string()).increment(1);
                    }
                }
            }
            Wire::WorldAdopted {
                world,
                cell: _,
                from_node,
                ids,
            } => {
                if let Some(w) = self.worlds.get() {
                    if from_node != w.node_id() {
                        let dups = w.duplicates_of(&world, from_node, &ids);
                        tracing::info!(world = %world, from = %from_node, announced = ids.len(), duplicates = dups.len(), ?dups, "adopted entities announced by another node");
                        if !dups.is_empty() {
                            self.world_drop(&world, from_node, w.node_id(), dups);
                        }
                    }
                }
            }
            Wire::WorldDrop {
                world,
                to_node,
                from_node,
                ids,
            } => {
                if let Some(w) = self.worlds.get() {
                    if to_node == w.node_id() {
                        tracing::info!(world = %world, %from_node, ?ids, "drop request for duplicate copies");
                        w.drop_duplicates(&world, from_node, &ids);
                    }
                }
            }
            Wire::WorldTxPrepare {
                world,
                tx,
                coordinator,
                ops,
                ttl_ticks,
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_tx_prepare(&world, tx, coordinator, ops, ttl_ticks, self);
                }
            }
            Wire::WorldTxVote {
                world,
                tx,
                coordinator,
                entities,
                reason,
                ..
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_tx_vote(&world, tx, coordinator, entities, reason, self);
                }
            }
            Wire::WorldTxCommit { world, tx, ops } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_tx_commit(&world, tx, ops);
                }
            }
            Wire::WorldTxAbort { world, tx, reason } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_tx_abort(&world, tx, reason);
                }
            }
            Wire::WorldInput {
                world,
                entity,
                input,
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_remote_input(&world, entity, input);
                }
            }
            Wire::WorldCoLocate {
                world,
                entity,
                cell,
                by,
                ttl_ticks,
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_remote_co_locate(&world, entity, cell, by, ttl_ticks);
                }
            }
            Wire::WorldSubsRequest { world } => {
                if let Some(w) = self.worlds.get().cloned() {
                    w.replay_subs(&world, self);
                }
            }
            Wire::WorldQuery {
                world,
                req,
                from,
                kind,
            } => {
                if let Some(w) = self.worlds.get() {
                    w.answer_query(&world, req, from, &kind, self);
                }
            }
            Wire::WorldQueryReply {
                req,
                node,
                entities,
                ..
            } => {
                if let Some(w) = self.worlds.get() {
                    w.accept_query_reply(req, node, entities);
                }
            }
            Wire::WorldDeleted { world } => {
                if let Some(w) = self.worlds.get() {
                    w.forget_world(&world);
                }
            }
            Wire::WorldCellAssign { world, cell, node } => {
                if let Some(w) = self.worlds.get().cloned() {
                    let prev = w.cell_host_cached(&world, cell);
                    if node == w.node_id() {
                        let me = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) = w.host_cell(&world, cell).await {
                                tracing::warn!(world = %world, ?cell, error = %e, "cell hosting failed");
                                return;
                            }
                            if let Some(p) = prev.filter(|p| *p != w.node_id()) {
                                if let Err(e) = w.adopt_cell_from(&world, cell, p).await {
                                    tracing::warn!(world = %world, ?cell, error = %e, "cell state not adopted");
                                }
                            }
                            me.publish(Wire::WorldSubsRequest { world });
                        });
                    } else if prev == Some(w.node_id()) {
                        let me = self.clone();
                        tokio::spawn(async move {
                            // persist first: the new host merges our shard from the database
                            if let Err(e) = w.snapshot_now(&world).await {
                                tracing::warn!(world = %world, error = %e, "shard snapshot before release failed");
                            }
                            w.release_cell(&world, cell, node, &me);
                            w.forget_adoptions(&world, cell);
                            w.note_cell_host(&world, cell, node);
                            w.drop_idle_shard(&world);
                        });
                    } else {
                        w.forget_adoptions(&world, cell);
                        w.note_cell_host(&world, cell, node);
                    }
                }
            }
            Wire::WorldCmd(env) => {
                if let Some(w) = self.worlds.get().cloned() {
                    let me = self.clone();
                    tokio::spawn(async move {
                        // inputs run on the directory home (which forwards to the owning shard);
                        // subscriptions run on every node keeping a shard of the world
                        let run = match env.cmd {
                            WorldCmd::Input { .. } => {
                                w.home_of(&env.world).await.ok().flatten() == Some(w.node_id())
                            }
                            _ => w.has_shard(&env.world),
                        };
                        if !run {
                            return;
                        }
                        let sid = env.session_id;
                        if let Some(reply) = w.exec(env, &me) {
                            me.session(sid, reply);
                        }
                    });
                }
            }
            Wire::RelayedCmd(env) => {
                if self.relayed.is_home(&env.match_id) {
                    if let Some(reply) = crate::realtime::relayed::exec(
                        &self.relayed,
                        self,
                        &env.me,
                        &env.match_id,
                        env.cmd,
                    ) {
                        if let (
                            Some(social),
                            ServerMessage::Match {
                                match_id,
                                presences,
                                ..
                            },
                        ) = (self.social.get(), &reply)
                        {
                            let social = social.clone();
                            let me = env.me.account_id;
                            let others: Vec<AccountId> =
                                presences.iter().map(|p| p.account_id).collect();
                            let match_id = match_id.clone();
                            tokio::spawn(async move {
                                if let Err(e) = social.recent_seen(me, &others, &match_id).await {
                                    tracing::warn!(error = %e, "recent players not recorded");
                                }
                            });
                        }
                        self.session(env.me.session_id, reply);
                    }
                }
            }
        }
    }

    fn publish(&self, w: Wire) {
        let Some(c) = self.cluster.clone() else {
            return;
        };
        // five lanes, each its own queue, so one kind of traffic cannot starve another: bulk
        // replication (ghost and replica deltas, their acks — ordered, one at a time), user
        // traffic (chat, presence, relayed match input — high volume, published with bounded
        // concurrency) and mesh/session control (handovers, acks, adoption/drop reconciliation,
        // matchmaking — ordered, must not wait behind the other two; the stand saw control
        // messages queue 48 s behind chat in a single queue), and remote-session delivery (world
        // deltas to players on other nodes — batched; on the control lane 1 000 remote players ×
        // 30 Hz queued handovers behind 30 000 frames a second)
        let (lane, concurrency, lane_name): (&Lane, usize, &'static str) = match &w {
            Wire::WorldGhosts { .. } | Wire::WorldRepl { .. } | Wire::WorldReplAck { .. } => {
                (&self.bus_bulk, 1, "bulk")
            }
            // one lane, not several sharded by session: four of them measured the same 7–12 s
            // of wait on the stand — a 64-frame batch costs ~23 ms (136 max) to serialize and
            // push through the shared QUIC connection, and the lanes only queue behind each other
            Wire::Session { .. } => (&self.bus_session, 1, "session"),
            // every player command: inputs, and the subscribe a client re-sends as its interest
            // moves (2 000 players: ~5 000/s on the stand, which on the control lane put the
            // handovers a minute behind); one lane keeps a session's commands in order
            Wire::WorldInput { .. } | Wire::WorldCmd(_) => (&self.bus_input, 1, "input"),
            // a handover ack is small, order-free (late and duplicate acks are handled) and
            // decides whether the handover completes in time: it must not queue behind the
            // handovers and commands of the serial control lane (50–100 s behind on the stand)
            Wire::WorldHandoverAck { .. }
            | Wire::WorldQueryReply { .. }
            | Wire::OrderNack { .. } => (&self.bus_user, 32, "user"),
            Wire::Channel { .. }
            | Wire::Account { .. }
            | Wire::Presence { .. }
            | Wire::Online { .. }
            | Wire::RelayedCmd { .. } => (&self.bus_user, 32, "user"),
            _ => (&self.bus, CONTROL_IN_FLIGHT, "control"),
        };
        metrics::counter!("musubi_fanout_published_total", "lane" => lane_name, "kind" => w.kind())
            .increment(1);
        // control before user before deltas on the shared QUIC connection (see `publish_prio`)
        let priority = match lane_name {
            "control" => 10,
            "user" => 5,
            _ => 0,
        };
        let tx = lane.get_or_init(|| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(std::time::Instant, Wire)>();
            // the high-volume lanes batch what queued while the previous send was in flight;
            // the control lane does not: batched (even 16 handovers a frame) it moved the
            // stand's players-split fps from 25 to 19 without completing more handovers, and
            // its bursts (subscription replays) are batched at the source instead. Only on the
            // QUIC bus: the Postgres NOTIFY fallback caps a message at 7 900 bytes, a batch of
            // deltas is dropped whole there
            let batch = c.bus_addr().is_some()
                && matches!(
                    w,
                    Wire::WorldGhosts { .. }
                        | Wire::WorldRepl { .. }
                        | Wire::Session { .. }
                        | Wire::WorldInput { .. }
                        | Wire::WorldCmd(_)
                );
            let window = self.control_window.clone();
            let inflight = self.session_inflight.clone();
            tokio::spawn(async move {
                let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
                // one "slow" warning per lane per second, not one per message
                let last_warn = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
                let epoch = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(1);
                let mut seq: u64 = 0;
                while let Some((queued, mut w)) = rx.recv().await {
                    metrics::gauge!("musubi_fanout_queue_depth", "lane" => lane_name).decrement(1.0);
                    if batch {
                        // everything already waiting goes out in the same frame: one QUIC send per
                        // peer instead of one per cell per tick
                        let mut items = vec![w];
                        while items.len() < BULK_BATCH_MAX {
                            match rx.try_recv() {
                                Ok((_, next)) => {
                                    metrics::gauge!("musubi_fanout_queue_depth", "lane" => lane_name)
                                        .decrement(1.0);
                                    items.push(next)
                                }
                                Err(_) => break,
                            }
                        }
                        w = if items.len() == 1 {
                            items.pop().unwrap()
                        } else {
                            Wire::WorldBatch(items)
                        };
                    }
                    let sids: Vec<SessionId> = if lane_name == "session" {
                        match &w {
                            Wire::Session { session_id, .. } => vec![*session_id],
                            Wire::WorldBatch(items) => items
                                .iter()
                                .filter_map(|i| match i {
                                    Wire::Session { session_id, .. } => Some(*session_id),
                                    _ => None,
                                })
                                .collect(),
                            _ => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    let Ok(permit) = sem.clone().acquire_owned().await else {
                        break;
                    };
                    if lane_name == "control" {
                        // several in flight: the receiver restores this order
                        w = Wire::Ordered {
                            epoch,
                            seq,
                            inner: Box::new(w),
                        };
                        if let Ok(raw) = serde_json::value::to_raw_value(&w) {
                            let mut win = window.lock().unwrap();
                            win.epoch = epoch;
                            win.frames.push_back((seq, raw));
                            while win.frames.len() > CONTROL_WINDOW {
                                win.frames.pop_front();
                            }
                        }
                        seq += 1;
                    }
                    let c = c.clone();
                    let last_warn = last_warn.clone();
                    let inflight = inflight.clone();
                    let one = async move {
                        let waited = queued.elapsed();
                        let started = std::time::Instant::now();
                        // serialize here, send the bytes: the two halves of a batch's cost are
                        // measured apart (`musubi_fanout_batch_ms{lane,phase}`)
                        match serde_json::value::to_raw_value(&w) {
                            Ok(raw) => {
                                let ser = started.elapsed();
                                metrics::histogram!("musubi_fanout_batch_ms", "lane" => lane_name, "phase" => "serialize")
                                    .record(ser.as_secs_f64() * 1000.0);
                                let send_started = std::time::Instant::now();
                                if let Err(e) = c.publish_prio(TOPIC, &raw, priority).await {
                                    tracing::warn!(error = %e, "fanout publish failed");
                                }
                                metrics::histogram!("musubi_fanout_batch_ms", "lane" => lane_name, "phase" => "send")
                                    .record(send_started.elapsed().as_secs_f64() * 1000.0);
                            }
                            Err(e) => tracing::warn!(error = %e, "fanout frame not serializable"),
                        }
                        Fanout::session_frames_sent(&inflight, &sids);
                        let took = started.elapsed();
                        let slow = waited.as_millis() > 1000 || took.as_millis() > 1000;
                        let report = slow && {
                            let mut l = last_warn.lock().unwrap();
                            let due = l.is_none_or(|t| t.elapsed().as_secs() >= 1);
                            if due {
                                *l = Some(std::time::Instant::now());
                            }
                            due
                        };
                        if report {
                            let kind = serde_json::to_value(&w)
                                .ok()
                                .and_then(|v| v.as_object().and_then(|o| o.keys().next().cloned()))
                                .unwrap_or_default();
                            tracing::warn!(
                                lane = lane_name,
                                kind,
                                waited_ms = waited.as_millis() as u64,
                                publish_ms = took.as_millis() as u64,
                                "fanout bus slow"
                            );
                        }
                        drop(permit);
                    };
                    if concurrency > 1 {
                        tokio::spawn(one);
                    } else {
                        one.await;
                    }
                }
            });
            tx
        });
        metrics::gauge!("musubi_fanout_queue_depth", "lane" => lane_name).increment(1.0);
        let _ = tx.send((std::time::Instant::now(), w));
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn w(n: u64) -> Wire {
        Wire::WorldDeleted {
            world: n.to_string(),
        }
    }

    fn names(v: &[Wire]) -> Vec<u64> {
        v.iter()
            .map(|x| match x {
                Wire::WorldDeleted { world } => world.parse().unwrap(),
                _ => u64::MAX,
            })
            .collect()
    }

    #[test]
    fn frames_ahead_wait_for_the_gap_and_a_nack_asks_for_it() {
        let mut st = OrderState::default();
        let t0 = Instant::now();
        let a = st.accept(1, 0, w(0), t0);
        assert_eq!(names(&a.ready), vec![0]);
        // 1 is lost; 2 and 3 arrive ahead
        let a = st.accept(1, 2, w(2), t0 + Duration::from_millis(10));
        assert!(a.ready.is_empty() && a.nack.is_none(), "too early to ask");
        let a = st.accept(1, 3, w(3), t0 + Duration::from_millis(60));
        assert!(a.ready.is_empty());
        assert_eq!(a.nack, Some((1, 1)), "ask for the one frame missing");
        let a = st.accept(1, 4, w(4), t0 + Duration::from_millis(100));
        assert_eq!(a.nack, None, "one nack per gap per interval");
        let a = st.accept(1, 5, w(5), t0 + Duration::from_millis(270));
        assert_eq!(a.nack, Some((1, 1)), "asked again after the interval");
        // the replay arrives: everything held goes out in order
        let a = st.accept(1, 1, w(1), t0 + Duration::from_millis(300));
        assert_eq!(names(&a.ready), vec![1, 2, 3, 4, 5]);
        assert!(!a.dup);
        // a second copy of the replay is a duplicate
        let a = st.accept(1, 1, w(1), t0 + Duration::from_millis(310));
        assert!(a.ready.is_empty() && a.dup);
    }

    #[test]
    fn a_given_up_gap_still_accepts_its_late_frame_once() {
        let mut st = OrderState::default();
        let t0 = Instant::now();
        st.accept(7, 0, w(0), t0);
        let a = st.accept(7, 2, w(2), t0 + Duration::from_millis(1));
        assert!(a.ready.is_empty());
        let a = st.accept(7, 3, w(3), t0 + Duration::from_millis(600));
        assert_eq!(names(&a.ready), vec![2, 3], "gap skipped after the timeout");
        let a = st.accept(7, 1, w(1), t0 + Duration::from_millis(700));
        assert_eq!(
            names(&a.ready),
            vec![1],
            "the late frame is new, not a duplicate"
        );
        assert!(!a.dup);
        let a = st.accept(7, 1, w(1), t0 + Duration::from_millis(800));
        assert!(a.dup, "but only once");
        let a = st.accept(7, 4, w(4), t0 + Duration::from_millis(900));
        assert_eq!(names(&a.ready), vec![4]);
    }

    #[test]
    fn a_new_epoch_resets_the_sequence() {
        let mut st = OrderState::default();
        let t0 = Instant::now();
        st.accept(1, 10, w(10), t0);
        st.accept(1, 12, w(12), t0);
        let a = st.accept(2, 0, w(0), t0 + Duration::from_millis(5));
        assert_eq!(
            names(&a.ready),
            vec![12, 0],
            "held frames flush, then the new epoch starts"
        );
        let a = st.accept(2, 1, w(1), t0 + Duration::from_millis(6));
        assert_eq!(names(&a.ready), vec![1]);
    }
}
