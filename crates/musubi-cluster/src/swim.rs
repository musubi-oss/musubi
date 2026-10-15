//! SWIM-style failure detection over the QUIC bus. Direct ping, then indirect ping-requests
//! through other members, then `Suspect` → `Dead`. Membership state rides on every ping/ack as
//! gossip; a node that hears itself suspected refutes by bumping its incarnation. The Postgres
//! heartbeat stays as the slow backstop; SWIM only makes dead detection fast (seconds).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::{Cluster, NodeInfo};

pub const PROBE_INTERVAL: Duration = Duration::from_secs(1);
pub const PING_TIMEOUT: Duration = Duration::from_millis(500);
pub const INDIRECT_TIMEOUT: Duration = Duration::from_millis(1000);
pub const SUSPECT_TIMEOUT: Duration = Duration::from_secs(3);
const INDIRECT_PROBES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Alive,
    Suspect,
    Dead,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Alive => "alive",
            State::Suspect => "suspect",
            State::Dead => "dead",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Member {
    pub state: State,
    pub incarnation: u64,
    pub since: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemberUpdate {
    pub id: Uuid,
    pub state: State,
    pub incarnation: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Msg {
    Ping {
        from: Uuid,
        gossip: Vec<MemberUpdate>,
    },
    PingReq {
        from: Uuid,
        target: Box<NodeInfo>,
    },
    Ack {
        from: Uuid,
        gossip: Vec<MemberUpdate>,
    },
    Nack,
}

impl Cluster {
    fn self_update(&self) -> MemberUpdate {
        MemberUpdate {
            id: self.me.id.0,
            state: State::Alive,
            incarnation: self.incarnation.load(Ordering::Relaxed),
        }
    }

    async fn gossip(&self) -> Vec<MemberUpdate> {
        let m = self.members.read().await;
        let mut v: Vec<MemberUpdate> = m
            .iter()
            .map(|(id, x)| MemberUpdate {
                id: *id,
                state: x.state,
                incarnation: x.incarnation,
            })
            .collect();
        v.push(self.self_update());
        v
    }

    /// SWIM merge rules: Dead wins; Suspect beats Alive at the same incarnation; a higher
    /// incarnation beats anything but Dead. News about ourselves that is not Alive is refuted.
    async fn merge(&self, updates: &[MemberUpdate]) {
        let mut refute = false;
        {
            let mut m = self.members.write().await;
            for u in updates {
                if u.id == self.me.id.0 {
                    if u.state != State::Alive
                        && u.incarnation >= self.incarnation.load(Ordering::Relaxed)
                    {
                        refute = true;
                    }
                    continue;
                }
                let fresh = !m.contains_key(&u.id);
                let e = m.entry(u.id).or_insert(Member {
                    state: u.state,
                    incarnation: u.incarnation,
                    since: Instant::now(),
                });
                if fresh {
                    let _ = self.member_tx.send(crate::MemberEvent {
                        id: u.id,
                        state: u.state.as_str(),
                    });
                }
                let apply = match (e.state, u.state) {
                    (State::Dead, _) => false,
                    (_, State::Dead) => true,
                    (State::Alive, State::Suspect) => u.incarnation >= e.incarnation,
                    _ => u.incarnation > e.incarnation,
                };
                if apply && (e.state != u.state || e.incarnation != u.incarnation) {
                    tracing::info!(node = %u.id, state = ?u.state, incarnation = u.incarnation, "swim: gossip applied");
                    if e.state != u.state {
                        let _ = self.member_tx.send(crate::MemberEvent {
                            id: u.id,
                            state: u.state.as_str(),
                        });
                    }
                    e.state = u.state;
                    e.incarnation = u.incarnation;
                    e.since = Instant::now();
                }
            }
        }
        if refute {
            let inc = self.incarnation.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::info!(incarnation = inc, "swim: refuting suspicion of self");
        }
    }

    pub(crate) async fn set_state(&self, id: Uuid, state: State) {
        let mut m = self.members.write().await;
        let fresh = !m.contains_key(&id);
        let e = m.entry(id).or_insert(Member {
            state: State::Alive,
            incarnation: 0,
            since: Instant::now(),
        });
        if fresh {
            let _ = self.member_tx.send(crate::MemberEvent {
                id,
                state: State::Alive.as_str(),
            });
        }
        if e.state != state && e.state != State::Dead {
            tracing::info!(node = %id, from = ?e.state, to = ?state, "swim: state changed");
            e.state = state;
            e.since = Instant::now();
            let _ = self.member_tx.send(crate::MemberEvent {
                id,
                state: state.as_str(),
            });
        }
    }

    pub async fn member_state(&self, id: Uuid) -> Option<State> {
        self.members.read().await.get(&id).map(|m| m.state)
    }

    pub(crate) async fn handle_swim(&self, msg: Msg) -> Msg {
        match msg {
            Msg::Ping { from, gossip } => {
                self.merge(&gossip).await;
                self.set_state(from, State::Alive).await;
                Msg::Ack {
                    from: self.me.id.0,
                    gossip: self.gossip().await,
                }
            }
            Msg::PingReq { target, .. } => match self.ping(&target).await {
                Ok(g) => {
                    self.merge(&g).await;
                    self.set_state(target.id.0, State::Alive).await;
                    Msg::Ack {
                        from: self.me.id.0,
                        gossip: self.gossip().await,
                    }
                }
                Err(_) => Msg::Nack,
            },
            Msg::Ack { .. } | Msg::Nack => Msg::Nack,
        }
    }

    async fn roundtrip(
        cluster: &Cluster,
        peer: &NodeInfo,
        msg: &Msg,
        timeout: Duration,
    ) -> Result<Msg> {
        let body = serde_json::to_vec(msg)?;
        let resp = cluster
            .bus_request(peer, crate::raft::TAG_SWIM, &body, timeout)
            .await?;
        serde_json::from_slice::<Msg>(&resp).map_err(Into::into)
    }

    async fn ping(&self, target: &NodeInfo) -> Result<Vec<MemberUpdate>> {
        let msg = Msg::Ping {
            from: self.me.id.0,
            gossip: self.gossip().await,
        };
        match Self::roundtrip(self, target, &msg, PING_TIMEOUT).await? {
            Msg::Ack { gossip, .. } => Ok(gossip),
            _ => Err(Error::Internal("nack".into())),
        }
    }

    /// One protocol period: expire suspects, probe the `n`-th live bus peer directly, then
    /// indirectly, and suspect it if nobody can reach it.
    pub(crate) async fn probe_round(&self, n: usize) {
        let Some(bus) = &self.bus else { return };
        {
            let mut m = self.members.write().await;
            for (id, e) in m.iter_mut() {
                if e.state == State::Suspect && e.since.elapsed() > SUSPECT_TIMEOUT {
                    tracing::warn!(node = %id, "swim: suspect timed out, declared dead");
                    let _ = self.member_tx.send(crate::MemberEvent {
                        id: *id,
                        state: State::Dead.as_str(),
                    });
                    e.state = State::Dead;
                    e.since = Instant::now();
                }
            }
        }
        let peers: Vec<NodeInfo> = self
            .peers
            .read()
            .await
            .iter()
            .filter(|p| p.id != self.me.id && !p.bus_addr.is_empty())
            .cloned()
            .collect();
        let mut live = Vec::with_capacity(peers.len());
        for p in peers {
            if self.member_state(p.id.0).await != Some(State::Dead) {
                live.push(p);
            }
        }
        if live.is_empty() {
            return;
        }
        let target = live[n % live.len()].clone();
        if let Ok(g) = self.ping(&target).await {
            self.merge(&g).await;
            self.set_state(target.id.0, State::Alive).await;
            return;
        }
        bus.conns.lock().await.remove(&target.id.0);
        let mut set = JoinSet::new();
        for h in live
            .iter()
            .filter(|p| p.id != target.id)
            .take(INDIRECT_PROBES)
        {
            let (cl, h, target, me) = (
                self.clone(),
                h.clone(),
                Box::new(target.clone()),
                self.me.id.0,
            );
            set.spawn(async move {
                Self::roundtrip(
                    &cl,
                    &h,
                    &Msg::PingReq { from: me, target },
                    INDIRECT_TIMEOUT,
                )
                .await
            });
        }
        while let Some(r) = set.join_next().await {
            if let Ok(Ok(Msg::Ack { gossip, .. })) = r {
                self.merge(&gossip).await;
                self.set_state(target.id.0, State::Alive).await;
                return;
            }
        }
        self.set_state(target.id.0, State::Suspect).await;
    }
}
