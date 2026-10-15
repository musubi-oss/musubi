//! Cluster v0: node registry and a cross-node message bus, both on the shared Postgres
//! (`musubi.nodes` + `LISTEN/NOTIFY`). Zero extra infrastructure, works on any managed Postgres,
//! and gives multi-process presence/chat/notification fan-out today. The gossip membership, Raft
//! control plane and QUIC bus from the design replace these pieces behind the same API.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinSet;

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
    /// QUIC bus endpoint (empty = this node only speaks the Postgres bus).
    #[serde(default)]
    pub bus_addr: String,
    /// Hex DER of the node's self-signed bus certificate; peers trust exactly this.
    #[serde(default)]
    pub bus_cert: String,
}

type NodeRow = (
    Uuid,
    String,
    String,
    Vec<String>,
    String,
    String,
    OffsetDateTime,
    OffsetDateTime,
    String,
    String,
);

pub const HEARTBEAT: Duration = Duration::from_secs(5);
pub const DEAD_AFTER: Duration = Duration::from_secs(15);
const BUS_CHANNEL: &str = "musubi_bus";
/// Postgres NOTIFY payloads are capped at 8000 bytes.
const MAX_BUS_PAYLOAD: usize = 7900;
const BUS_ALPN: &[u8] = b"musubi-bus/1";
/// Largest message accepted over the QUIC bus.
pub const MAX_QUIC_BUS_PAYLOAD: usize = 16 * 1024 * 1024;
const QUIC_SEND_TIMEOUT: Duration = Duration::from_secs(3);

struct QuicBus {
    endpoint: quinn::Endpoint,
    conns: Mutex<HashMap<Uuid, quinn::Connection>>,
}

#[derive(Clone)]
pub struct Cluster {
    db: PgPool,
    me: NodeInfo,
    bus_tx: broadcast::Sender<BusMessage>,
    peers: Arc<RwLock<Vec<NodeInfo>>>,
    bus: Option<Arc<QuicBus>>,
}

/// A message from another node. `topic` routes it; `payload` is topic-specific JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub origin: NodeId,
    pub topic: String,
    pub payload: serde_json::Value,
    /// Set on the Postgres copy of a message that QUIC-capable peers already received directly.
    #[serde(default)]
    pub via_quic: bool,
    /// Non-empty: only these nodes should accept the Postgres copy (targeted redelivery).
    #[serde(default)]
    pub only: Vec<NodeId>,
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
                bus_addr: String::new(),
                bus_cert: String::new(),
            },
            bus_tx,
            peers: Arc::new(RwLock::new(Vec::new())),
            bus: None,
        }
    }

    /// Enable the direct QUIC node-to-node bus on `listen`. Peers discover the address and the
    /// self-signed certificate through `musubi.nodes`, so the only trust root is the database.
    pub fn with_bus(mut self, listen: SocketAddr, advertise: Option<&str>) -> Result<Self> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .map_err(Error::internal)?;
        let der = CertificateDer::from(cert.cert.der().to_vec());
        let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![der.clone()], key)
            .map_err(Error::internal)?;
        tls.alpn_protocols = vec![BUS_ALPN.to_vec()];
        let crypto =
            quinn::crypto::rustls::QuicServerConfig::try_from(tls).map_err(Error::internal)?;
        let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
        let mut transport = quinn::TransportConfig::default();
        transport.max_idle_timeout(Some(
            quinn::IdleTimeout::try_from(Duration::from_secs(60)).map_err(Error::internal)?,
        ));
        transport.keep_alive_interval(Some(Duration::from_secs(15)));
        cfg.transport_config(Arc::new(transport));
        let endpoint = quinn::Endpoint::server(cfg, listen).map_err(Error::internal)?;
        let local = endpoint.local_addr().map_err(Error::internal)?;
        self.me.bus_addr = match advertise {
            Some(a) => a.to_string(),
            None => local.to_string(),
        };
        self.me.bus_cert = hex::encode(der.as_ref());
        self.bus = Some(Arc::new(QuicBus {
            endpoint,
            conns: Mutex::new(HashMap::new()),
        }));
        Ok(self)
    }

    pub fn bus_addr(&self) -> Option<SocketAddr> {
        self.bus
            .as_ref()
            .and_then(|_| self.me.bus_addr.parse().ok())
    }

    /// Re-read the node table into the peer cache used for QUIC fan-out.
    pub async fn refresh_peers(&self) -> Result<()> {
        let nodes = self.nodes().await?;
        *self.peers.write().await = nodes;
        Ok(())
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
            "insert into musubi.nodes (id, name, region, roles, version, advertise, started_at, heartbeat_at, bus_addr, bus_cert) values ($1, $2, $3, $4, $5, $6, $7, now(), $8, $9)",
        )
        .bind(self.me.id.0)
        .bind(&self.me.name)
        .bind(&self.me.region)
        .bind(&self.me.roles)
        .bind(&self.me.version)
        .bind(&self.me.advertise)
        .bind(self.me.started_at)
        .bind(&self.me.bus_addr)
        .bind(&self.me.bus_cert)
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
                if let Err(e) = hb.refresh_peers().await {
                    tracing::warn!(error = %e, "peer refresh failed");
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
                            if relay.accepts_notify(&msg) {
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
        if let Some(bus) = &self.bus {
            let (ep, tx, me) = (bus.endpoint.clone(), self.bus_tx.clone(), self.me.id);
            tokio::spawn(async move {
                while let Some(incoming) = ep.accept().await {
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let Ok(conn) = incoming.await else { return };
                        while let Ok(mut recv) = conn.accept_uni().await {
                            let tx = tx.clone();
                            tokio::spawn(async move {
                                let Ok(buf) = recv.read_to_end(MAX_QUIC_BUS_PAYLOAD).await else {
                                    return;
                                };
                                match serde_json::from_slice::<BusMessage>(&buf) {
                                    Ok(msg) if msg.origin != me => {
                                        let _ = tx.send(msg);
                                    }
                                    Ok(_) => {}
                                    Err(e) => tracing::warn!(error = %e, "bad quic bus frame"),
                                }
                            });
                        }
                    });
                }
            });
        }
        self.refresh_peers().await?;
        Ok(())
    }

    fn accepts_notify(&self, msg: &BusMessage) -> bool {
        if msg.origin == self.me.id {
            return false;
        }
        if !msg.only.is_empty() {
            return msg.only.contains(&self.me.id);
        }
        !(msg.via_quic && self.bus.is_some())
    }

    pub async fn stop(&self) {
        let _ = sqlx::query("delete from musubi.nodes where id = $1")
            .bind(self.me.id.0)
            .execute(&self.db)
            .await;
        if let Some(bus) = &self.bus {
            bus.endpoint.close(0u32.into(), b"stop");
        }
    }

    pub async fn nodes(&self) -> Result<Vec<NodeInfo>> {
        let rows = sqlx::query_as::<_, NodeRow>(
            "select id, name, region, roles, version, advertise, started_at, heartbeat_at, bus_addr, bus_cert from musubi.nodes order by started_at",
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
                bus_addr: r.8,
                bus_cert: r.9,
            })
            .collect())
    }

    /// Broadcast to every other node. Own node never receives its own messages.
    ///
    /// With the QUIC bus enabled, every live peer that advertises a bus address gets the message
    /// over a direct stream; the Postgres copy is only sent for peers without one (flagged
    /// `via_quic` so bus peers drop it) and, targeted, for peers whose QUIC delivery failed.
    pub async fn publish<T: Serialize>(&self, topic: &str, payload: &T) -> Result<()> {
        let mut msg = BusMessage {
            origin: self.me.id,
            topic: topic.to_string(),
            payload: serde_json::to_value(payload)?,
            via_quic: false,
            only: Vec::new(),
        };
        let Some(bus) = &self.bus else {
            return self.notify(&msg).await;
        };
        let peers: Vec<NodeInfo> = self
            .peers
            .read()
            .await
            .iter()
            .filter(|p| p.alive && p.id != self.me.id)
            .cloned()
            .collect();
        let bytes = Arc::new(serde_json::to_vec(&msg)?);
        let mut legacy_peers = false;
        let mut set = JoinSet::new();
        for p in peers {
            if p.bus_addr.is_empty() {
                legacy_peers = true;
                continue;
            }
            let (bus, bytes) = (bus.clone(), bytes.clone());
            set.spawn(async move {
                let r = tokio::time::timeout(QUIC_SEND_TIMEOUT, bus.send(&p, &bytes)).await;
                let ok = matches!(r, Ok(Ok(())));
                if !ok {
                    tracing::warn!(peer = %p.id.0, %p.bus_addr, "quic bus delivery failed; falling back to postgres");
                    bus.conns.lock().await.remove(&p.id.0);
                }
                (p.id, ok)
            });
        }
        let mut failed = Vec::new();
        while let Some(r) = set.join_next().await {
            if let Ok((id, false)) = r {
                failed.push(id);
            }
        }
        msg.via_quic = true;
        if !failed.is_empty() {
            msg.only = failed;
            self.notify(&msg).await?;
            msg.only = Vec::new();
        }
        if legacy_peers {
            if let Err(e) = self.notify(&msg).await {
                tracing::warn!(topic, error = %e, "postgres copy for legacy peers not sent");
            }
        }
        Ok(())
    }

    async fn notify(&self, msg: &BusMessage) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        if text.len() > MAX_BUS_PAYLOAD {
            return Err(Error::ResourceExhausted(format!(
                "bus message {} exceeds {MAX_BUS_PAYLOAD} bytes",
                msg.topic
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

impl QuicBus {
    async fn connection(&self, peer: &NodeInfo) -> Result<quinn::Connection> {
        let mut conns = self.conns.lock().await;
        if let Some(c) = conns.get(&peer.id.0) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        let addr: SocketAddr = peer
            .bus_addr
            .parse()
            .map_err(|_| Error::InvalidArgument(format!("bus_addr {}", peer.bus_addr)))?;
        let der = CertificateDer::from(hex::decode(&peer.bus_cert).map_err(Error::internal)?);
        let mut store = rustls::RootCertStore::empty();
        store.add(der).map_err(Error::internal)?;
        let mut tls = rustls::ClientConfig::builder()
            .with_root_certificates(store)
            .with_no_client_auth();
        tls.alpn_protocols = vec![BUS_ALPN.to_vec()];
        let crypto =
            quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(Error::internal)?;
        let cfg = quinn::ClientConfig::new(Arc::new(crypto));
        let conn = self
            .endpoint
            .connect_with(cfg, addr, "localhost")
            .map_err(Error::internal)?
            .await
            .map_err(Error::internal)?;
        conns.insert(peer.id.0, conn.clone());
        Ok(conn)
    }

    async fn send(&self, peer: &NodeInfo, bytes: &[u8]) -> Result<()> {
        let conn = self.connection(peer).await?;
        let mut stream = conn.open_uni().await.map_err(Error::internal)?;
        stream.write_all(bytes).await.map_err(Error::internal)?;
        stream.finish().map_err(Error::internal)?;
        let _ = stream.stopped().await;
        Ok(())
    }
}
