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
    /// Cell host → cluster, every tick: entities of `cell` near its border, ghosts for the hosts
    /// of the neighbouring cells.
    WorldGhosts {
        world: String,
        cell: musubi_mesh::CellId,
        tick: u64,
        entities: Vec<musubi_mesh::Entity>,
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
    },
    /// Receiving host → `from_node`: handover taken (or refused: no simulator owns the cell).
    WorldHandoverAck {
        world: String,
        entity: musubi_mesh::EntityId,
        from_node: uuid::Uuid,
        accepted: bool,
    },
    /// Input for an entity that lives on another node (e.g. buffered during a handover).
    WorldInput {
        world: String,
        entity: musubi_mesh::EntityId,
        input: serde_json::Value,
    },
    /// Coordinator → cluster: `node` now hosts `cell` of `world` (cells of one world may spread).
    WorldCellAssign {
        world: String,
        cell: musubi_mesh::CellId,
        node: uuid::Uuid,
    },
}

const TOPIC: &str = "realtime";

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
            worlds: Arc::new(OnceLock::new()),
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
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(m) => me.apply(&m),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(dropped = n, "fanout bus lagged");
                        }
                        Err(_) => break,
                    }
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
        if !self.hub.send_to_session(session_id, msg.clone()) {
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
    pub fn world_ghosts(
        &self,
        world: &str,
        cell: musubi_mesh::CellId,
        tick: u64,
        entities: Vec<musubi_mesh::Entity>,
    ) {
        self.publish(Wire::WorldGhosts {
            world: world.to_string(),
            cell,
            tick,
            entities,
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
    ) {
        self.publish(Wire::WorldHandover {
            world: world.to_string(),
            entity,
            from_node,
            inputs,
            forced,
        });
    }

    /// Answer a cross-node handover.
    pub fn world_handover_ack(
        &self,
        world: &str,
        entity: musubi_mesh::EntityId,
        from_node: uuid::Uuid,
        accepted: bool,
    ) {
        self.publish(Wire::WorldHandoverAck {
            world: world.to_string(),
            entity,
            from_node,
            accepted,
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

    /// Announce that `node` hosts `cell` of `world`.
    pub fn cell_assign(&self, world: &str, cell: musubi_mesh::CellId, node: uuid::Uuid) {
        self.publish(Wire::WorldCellAssign {
            world: world.to_string(),
            cell,
            node,
        });
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
                (Some(name), Some(fleets)) => match fleets.get_by_name(&name).await {
                    Ok(f) => match fleets
                        .allocate(
                            f.id,
                            serde_json::json!({ "token": m.token, "users": m.users() }),
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
                            tracing::warn!(fleet = %name, error = %e, "no server for matched group; delivering without one");
                            None
                        }
                    },
                    Err(e) => {
                        tracing::warn!(fleet = %name, error = %e, "matched group asked for an unknown fleet");
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

    pub fn relayed_cmd(&self, env: RelayedCmdEnv) {
        self.publish(Wire::RelayedCmd(env));
    }

    fn apply(&self, m: &BusMessage) {
        if m.topic != TOPIC {
            return;
        }
        match m.decode::<Wire>() {
            Ok(Wire::Account { account_id, msg }) => {
                self.hub.send_to_account(account_id, msg);
            }
            Ok(Wire::Channel { channel_id, msg }) => {
                self.hub.broadcast_channel(&channel_id, msg);
            }
            Ok(Wire::Presence {
                target,
                joins,
                leaves,
            }) => {
                self.hub.notify_followers_local(target, joins, leaves);
            }
            Ok(Wire::SessionSteal(sid)) => {
                if self.hub.steal(sid) {
                    tracing::info!(session = %sid, "parked session taken over by another node");
                }
            }
            Ok(Wire::Online {
                node,
                account,
                delta,
            }) => {
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
            Ok(Wire::MmAdd(w)) => {
                if let Err(e) = self.mm.add_replica(w) {
                    tracing::warn!(error = %e, "bad replicated ticket");
                }
            }
            Ok(Wire::MmRemove(id)) => {
                self.mm.cancel(&id);
            }
            Ok(Wire::MmMatched(w)) => {
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
            Ok(Wire::Session { session_id, msg }) => {
                self.hub.send_to_session(session_id, msg);
            }
            Ok(Wire::PartyCmd(env)) => {
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
            Ok(Wire::SessionGone(sid)) => {
                for t in self.parties.leave_all(self, sid) {
                    self.mm.cancel(&t);
                    self.mm_remove(&t);
                }
                self.relayed.leave_all(self, sid);
                if let Some(w) = self.worlds.get() {
                    w.unsubscribe_all(sid);
                }
            }
            Ok(Wire::WorldGhosts {
                world,
                cell,
                tick,
                entities,
            }) => {
                if let Some(w) = self.worlds.get() {
                    w.accept_ghosts(&world, cell, tick, entities);
                }
            }
            Ok(Wire::WorldHandover {
                world,
                entity,
                from_node,
                inputs,
                forced,
            }) => {
                if let Some(w) = self.worlds.get().cloned() {
                    let id = entity.id;
                    if w.accept_handover(&world, entity, from_node, inputs, forced, self) {
                        let me = self.clone();
                        tokio::spawn(async move {
                            if let Err(e) = w.flush_journal(&world).await {
                                tracing::warn!(world = %world, error = %e, "handover journal flush failed");
                            }
                            me.world_handover_ack(&world, id, from_node, true);
                        });
                    }
                }
            }
            Ok(Wire::WorldHandoverAck {
                world,
                entity,
                from_node,
                accepted,
            }) => {
                if let Some(w) = self.worlds.get() {
                    if from_node == w.node_id() {
                        w.handover_acked(&world, entity, accepted, self);
                    }
                }
            }
            Ok(Wire::WorldInput {
                world,
                entity,
                input,
            }) => {
                if let Some(w) = self.worlds.get() {
                    w.accept_remote_input(&world, entity, input);
                }
            }
            Ok(Wire::WorldCellAssign { world, cell, node }) => {
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
            Ok(Wire::WorldCmd(env)) => {
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
            Ok(Wire::RelayedCmd(env)) => {
                if self.relayed.is_home(&env.match_id) {
                    if let Some(reply) = crate::realtime::relayed::exec(
                        &self.relayed,
                        self,
                        &env.me,
                        &env.match_id,
                        env.cmd,
                    ) {
                        self.session(env.me.session_id, reply);
                    }
                }
            }
            Err(e) => tracing::warn!(error = %e, "bad fanout message"),
        }
    }

    fn publish(&self, w: Wire) {
        let Some(c) = self.cluster.clone() else {
            return;
        };
        tokio::spawn(async move {
            if let Err(e) = c.publish(TOPIC, &w).await {
                tracing::warn!(error = %e, "fanout publish failed");
            }
        });
    }
}
