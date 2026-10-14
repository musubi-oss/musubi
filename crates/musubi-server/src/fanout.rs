//! Deliver realtime messages to sessions on this node and, through the cluster bus, on every other node.

use std::sync::Arc;

use musubi_cluster::{BusMessage, Cluster};
use std::sync::atomic::{AtomicBool, Ordering};

use musubi_core::AccountId;
use musubi_matchmaker::{Matched, MatchedWire, Matchmaker, TicketWire};
use musubi_proto::{ServerMessage, UserPresence};
use serde::{Deserialize, Serialize};

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
}

const TOPIC: &str = "realtime";

#[derive(Clone)]
pub struct Fanout {
    hub: Arc<Hub>,
    cluster: Option<Arc<Cluster>>,
    mm: Arc<Matchmaker>,
    parties: Arc<PartyRegistry>,
    relayed: Arc<RelayedRegistry>,
    /// Only the leader runs matchmaker passes; tickets are replicated to every node.
    leader: Arc<AtomicBool>,
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
        };
        if let Some(c) = &f.cluster {
            let publisher = f.clone();
            f.hub.set_remote(Box::new(move |target, joins, leaves| {
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
        self.hub.send_to_account(account_id, msg.clone());
        self.publish(Wire::Account { account_id, msg });
    }

    pub fn channel(&self, channel_id: &str, msg: ServerMessage) {
        self.hub.broadcast_channel(channel_id, msg.clone());
        self.publish(Wire::Channel {
            channel_id: channel_id.to_string(),
            msg,
        });
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

    pub fn mm_matched(&self, m: &Matched) {
        self.publish(Wire::MmMatched(m.to_wire()));
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
            Ok(Wire::MmAdd(w)) => {
                if let Err(e) = self.mm.add_replica(w) {
                    tracing::warn!(error = %e, "bad replicated ticket");
                }
            }
            Ok(Wire::MmRemove(id)) => {
                self.mm.cancel(&id);
            }
            Ok(Wire::MmMatched(w)) => match Matched::from_wire(w) {
                Ok(matched) => {
                    for t in &matched.tickets {
                        self.mm.cancel(&t.id);
                    }
                    crate::realtime::matchmaking::deliver(
                        &self.hub,
                        &self.parties,
                        &self.relayed,
                        &matched,
                    );
                }
                Err(e) => tracing::warn!(error = %e, "bad matched message"),
            },
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
