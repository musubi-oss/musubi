//! Cluster v0: node registry and a cross-node message bus, both on the shared Postgres
//! (`musubi.nodes` + `LISTEN/NOTIFY`). Zero extra infrastructure, works on any managed Postgres,
//! and gives multi-process presence/chat/notification fan-out today. The gossip membership, Raft
//! control plane and QUIC bus from the design replace these pieces behind the same API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use musubi_core::{Error, NodeId, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub id: NodeId,
    pub name: String,
    pub region: String,
    pub roles: Vec<String>,
    pub version: String,
    pub advertise: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub heartbeat_at: OffsetDateTime,
    pub alive: bool,
}

pub const HEARTBEAT: Duration = Duration::from_secs(5);
pub const DEAD_AFTER: Duration = Duration::from_secs(15);
const BUS_CHANNEL: &str = "musubi_bus";
/// Postgres NOTIFY payloads are capped at 8000 bytes.
const MAX_BUS_PAYLOAD: usize = 7900;

#[derive(Clone)]
pub struct Cluster {
    db: PgPool,
    me: NodeInfo,
    bus_tx: broadcast::Sender<BusMessage>,
}

/// A message from another node. `topic` routes it; `payload` is topic-specific JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub origin: NodeId,
    pub topic: String,
    pub payload: serde_json::Value,
}

impl Cluster {
    pub fn new(db: PgPool, name: &str, region: &str, roles: Vec<String>, advertise: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        let (bus_tx, _) = broadcast::channel(4096);
        Self {
            db,
            me: NodeInfo {
                id: NodeId::new(),
                name: name.to_string(),
                region: region.to_string(),
                roles,
                version: env!("CARGO_PKG_VERSION").to_string(),
                advertise: advertise.to_string(),
                started_at: now,
                heartbeat_at: now,
                alive: true,
            },
            bus_tx,
        }
    }

    pub fn me(&self) -> &NodeInfo {
        &self.me
    }

    pub fn node_id(&self) -> NodeId {
        self.me.id
    }

    /// Insert our row, then keep heartbeating and relaying bus notifications until dropped.
    pub async fn start(self: &Arc<Self>) -> Result<()> {
        sqlx::query(
            "insert into musubi.nodes (id, name, region, roles, version, advertise, started_at, heartbeat_at) values ($1, $2, $3, $4, $5, $6, $7, now())",
        )
        .bind(self.me.id.0)
        .bind(&self.me.name)
        .bind(&self.me.region)
        .bind(&self.me.roles)
        .bind(&self.me.version)
        .bind(&self.me.advertise)
        .bind(self.me.started_at)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let hb = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(HEARTBEAT);
            loop {
                tick.tick().await;
                if let Err(e) =
                    sqlx::query("update musubi.nodes set heartbeat_at = now() where id = $1")
                        .bind(hb.me.id.0)
                        .execute(&hb.db)
                        .await
                {
                    tracing::warn!(error = %e, "node heartbeat failed");
                }
                // reap rows of nodes that have been silent for a long time
                let _ = sqlx::query(
                    "delete from musubi.nodes where heartbeat_at < now() - interval '1 hour'",
                )
                .execute(&hb.db)
                .await;
            }
        });
        let mut listener = PgListener::connect_with(&self.db)
            .await
            .map_err(Error::internal)?;
        listener
            .listen(BUS_CHANNEL)
            .await
            .map_err(Error::internal)?;
        let relay = self.clone();
        tokio::spawn(async move {
            loop {
                match listener.recv().await {
                    Ok(n) => {
                        if let Ok(msg) = serde_json::from_str::<BusMessage>(n.payload()) {
                            if msg.origin != relay.me.id {
                                let _ = relay.bus_tx.send(msg);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "bus listener error; reconnecting");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        });
        Ok(())
    }

    pub async fn stop(&self) {
        let _ = sqlx::query("delete from musubi.nodes where id = $1")
            .bind(self.me.id.0)
            .execute(&self.db)
            .await;
    }

    pub async fn nodes(&self) -> Result<Vec<NodeInfo>> {
        let rows = sqlx::query_as::<_, (Uuid, String, String, Vec<String>, String, String, OffsetDateTime, OffsetDateTime)>(
            "select id, name, region, roles, version, advertise, started_at, heartbeat_at from musubi.nodes order by started_at",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let now = OffsetDateTime::now_utc();
        Ok(rows
            .into_iter()
            .map(|r| NodeInfo {
                id: NodeId(r.0),
                name: r.1,
                region: r.2,
                roles: r.3,
                version: r.4,
                advertise: r.5,
                started_at: r.6,
                heartbeat_at: r.7,
                alive: now - r.7 < DEAD_AFTER,
            })
            .collect())
    }

    /// Broadcast to every other node. Own node never receives its own messages.
    pub async fn publish<T: Serialize>(&self, topic: &str, payload: &T) -> Result<()> {
        let msg = BusMessage {
            origin: self.me.id,
            topic: topic.to_string(),
            payload: serde_json::to_value(payload)?,
        };
        let text = serde_json::to_string(&msg)?;
        if text.len() > MAX_BUS_PAYLOAD {
            return Err(Error::ResourceExhausted(format!(
                "bus message {topic} exceeds {MAX_BUS_PAYLOAD} bytes"
            )));
        }
        sqlx::query("select pg_notify($1, $2)")
            .bind(BUS_CHANNEL)
            .bind(text)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BusMessage> {
        self.bus_tx.subscribe()
    }
}

/// A named cluster-wide lease: at most one live holder at a time. The holder renews it in the
/// background; `held()` is the authoritative answer for "am I the leader for this duty right now".
pub struct Lease {
    name: String,
    held: AtomicBool,
}

impl Lease {
    pub fn held(&self) -> bool {
        self.held.load(Ordering::Relaxed)
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Cluster {
    /// Compare-and-swap acquire/renew: succeeds if the lease is free, expired, or already ours.
    pub async fn try_lease(&self, name: &str, ttl: Duration) -> Result<bool> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "insert into musubi.leases (name, holder, expires_at)
             values ($1, $2, now() + make_interval(secs => $3))
             on conflict (name) do update set holder = excluded.holder, expires_at = excluded.expires_at
             where musubi.leases.holder = excluded.holder or musubi.leases.expires_at < now()
             returning holder",
        )
        .bind(name)
        .bind(self.me.id.0)
        .bind(ttl.as_secs_f64())
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.is_some())
    }

    pub async fn release_lease(&self, name: &str) -> Result<()> {
        sqlx::query("delete from musubi.leases where name = $1 and holder = $2")
            .bind(name)
            .bind(self.me.id.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Keeps trying to hold `name`, renewing every `ttl / 3`. A failed renewal (DB error or lost
    /// CAS) drops `held()` to false immediately, so a partitioned holder stops acting before the
    /// lease can be taken by someone else.
    pub fn lease(self: &Arc<Self>, name: &str, ttl: Duration) -> Arc<Lease> {
        let lease = Arc::new(Lease {
            name: name.to_string(),
            held: AtomicBool::new(false),
        });
        let (c, l) = (self.clone(), lease.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(ttl / 3);
            loop {
                tick.tick().await;
                let held = match c.try_lease(&l.name, ttl).await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(lease = %l.name, error = %e, "lease renewal failed");
                        false
                    }
                };
                if l.held.swap(held, Ordering::Relaxed) != held {
                    tracing::info!(lease = %l.name, held, "lease state changed");
                }
            }
        });
        lease
    }
}

impl BusMessage {
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_value(self.payload.clone()).map_err(Into::into)
    }
}
