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
        /// Each duplicate with the cell the sender holds it in: the receiver only believes a
        /// sender its directory names as that cell's host.
        ids: Vec<(musubi_mesh::EntityId, musubi_mesh::CellId)>,
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
    /// Several bulk messages (replica/ghost deltas) in one bus frame: the bulk lane drains its
    /// queue into one frame so one QUIC send carries a whole tick's worth of deltas.
    WorldBatch(Vec<Wire>),
}

const TOPIC: &str = "realtime";
/// Most bulk deltas folded into one bus frame.
const BULK_BATCH_MAX: usize = 64;

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
    bus: Arc<OnceLock<tokio::sync::mpsc::UnboundedSender<(std::time::Instant, Wire)>>>,
    bus_bulk: Arc<OnceLock<tokio::sync::mpsc::UnboundedSender<(std::time::Instant, Wire)>>>,
    bus_user: Arc<OnceLock<tokio::sync::mpsc::UnboundedSender<(std::time::Instant, Wire)>>>,
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
                                let id = b
                                    .payload
                                    .as_str()
                                    .and_then(|s| s.parse::<uuid::Uuid>().ok())
                                    .or_else(|| serde_json::from_value(b.payload.clone()).ok());
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
            let (q_tx, mut q_rx) =
                tokio::sync::mpsc::unbounded_channel::<musubi_cluster::BusMessage>();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(m) => {
                            if q_tx.send(m).is_err() {
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

    pub fn channel(&self, channel_id: &str, msg: ServerMessage) {
        self.hub.broadcast_channel(channel_id, msg.clone());
        self.publish(Wire::Channel {
            channel_id: channel_id.to_string(),
            msg,
        });
    }

    /// Deliver to one session wherever it lives: locally if we have it, otherwise via the bus.
    pub fn session(&self, session_id: SessionId, msg: ServerMessage) {
        if let Err(msg) = self.hub.try_send_session(session_id, msg) {
            self.publish(Wire::Session { session_id, msg });
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
        self.publish(Wire::WorldHandoverAck {
            world: world.to_string(),
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
        ids: Vec<(musubi_mesh::EntityId, musubi_mesh::CellId)>,
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

    fn apply(&self, m: &BusMessage) {
        if m.topic != TOPIC {
            return;
        }
        match m.decode::<Wire>() {
            Ok(w) => self.dispatch(w),
            Err(e) => tracing::warn!(error = %e, "bad fanout message"),
        }
    }

    fn dispatch(&self, w: Wire) {
        match w {
            Wire::WorldBatch(items) => {
                for w in items {
                    self.dispatch(w);
                }
            }
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
                    let id = entity.id;
                    if w.accept_handover(&world, entity, from_node, inputs, forced, base_rev, self)
                    {
                        let me = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) = w.flush_journal(&world).await {
                                tracing::warn!(world = %world, error = %e, "handover journal flush failed");
                            }
                            me.world_handover_ack(&world, id, from_node, true, false);
                        });
                    }
                }
            }
            Wire::WorldHandoverAck {
                world,
                entity,
                from_node,
                accepted,
                resend_full,
            } => {
                if let Some(w) = self.worlds.get() {
                    if from_node == w.node_id() {
                        w.handover_acked(&world, entity, accepted, resend_full, self);
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
                        let dups = w.duplicates_of(&world, &ids);
                        tracing::info!(world = %world, from = %from_node, announced = ids.len(), duplicates = dups.len(), "adopted entities announced by another node");
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
                        tracing::info!(world = %world, %from_node, ids = ids.len(), "drop request for duplicate copies");
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
            Wire::WorldCellAssign { world, cell, node } => {
                if let Some(w) = self.worlds.get().cloned() {
                    let prev = w.cell_host_cached(&world, cell);
                    if node == w.node_id() {
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
        // three lanes, each its own queue, so one kind of traffic cannot starve another: bulk
        // replication (ghost and replica deltas, their acks — ordered, one at a time), user
        // traffic (chat, presence, relayed match input — high volume, published with bounded
        // concurrency) and mesh/session control (handovers, acks, adoption/drop reconciliation,
        // matchmaking — ordered, must not wait behind the other two; the stand saw control
        // messages queue 48 s behind chat in a single queue)
        let (lane, concurrency) = match w {
            Wire::WorldGhosts { .. } | Wire::WorldRepl { .. } | Wire::WorldReplAck { .. } => {
                (&self.bus_bulk, 1)
            }
            Wire::Channel { .. }
            | Wire::Account { .. }
            | Wire::Presence { .. }
            | Wire::Online { .. }
            | Wire::RelayedCmd { .. } => (&self.bus_user, 32),
            _ => (&self.bus, 1),
        };
        let tx = lane.get_or_init(|| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<(std::time::Instant, Wire)>();
            let batch = matches!(w, Wire::WorldGhosts { .. } | Wire::WorldRepl { .. });
            tokio::spawn(async move {
                let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
                // one "slow" warning per lane per second, not one per message
                let last_warn = Arc::new(std::sync::Mutex::new(None::<std::time::Instant>));
                while let Some((queued, mut w)) = rx.recv().await {
                    if batch {
                        // everything already waiting goes out in the same frame: one QUIC send per
                        // peer instead of one per cell per tick
                        let mut items = vec![w];
                        while items.len() < BULK_BATCH_MAX {
                            match rx.try_recv() {
                                Ok((_, next)) => items.push(next),
                                Err(_) => break,
                            }
                        }
                        w = if items.len() == 1 {
                            items.pop().unwrap()
                        } else {
                            Wire::WorldBatch(items)
                        };
                    }
                    let Ok(permit) = sem.clone().acquire_owned().await else {
                        break;
                    };
                    let c = c.clone();
                    let last_warn = last_warn.clone();
                    let one = async move {
                        let waited = queued.elapsed();
                        let started = std::time::Instant::now();
                        if let Err(e) = c.publish(TOPIC, &w).await {
                            tracing::warn!(error = %e, "fanout publish failed");
                        }
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
        let _ = tx.send((std::time::Instant::now(), w));
    }
}
