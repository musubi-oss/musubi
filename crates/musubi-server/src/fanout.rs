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
use crate::realtime::world::{WorldCmdEnv, WorldRegistry};
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
    /// Session count change of an account on a node (cluster-wide online set).
    Online {
        node: uuid::Uuid,
        account: AccountId,
        delta: i64,
    },
    RelayedCmd(RelayedCmdEnv),
    WorldCmd(WorldCmdEnv),
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
            Ok(Wire::WorldCmd(env)) => {
                if let Some(w) = self.worlds.get() {
                    if w.is_home(&env.world) {
                        let sid = env.session_id;
                        if let Some(reply) = w.exec(env) {
                            self.session(sid, reply);
                        }
                    }
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
