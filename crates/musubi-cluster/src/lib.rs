//! Cluster v0: node registry and a cross-node message bus, both on the shared Postgres
//! (`musubi.nodes` + `LISTEN/NOTIFY`). Zero extra infrastructure, works on any managed Postgres,
//! and gives multi-process presence/chat/notification fan-out today. The gossip membership, Raft
//! control plane and QUIC bus from the design replace these pieces behind the same API.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinSet;

pub mod log_store;
pub mod raft;
pub mod swim;
pub mod wg;
pub use raft::{DirectoryOp, DirectoryResp, RaftNodeId};
pub use swim::{MemberUpdate, State};

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
    /// SWIM view: `alive` | `suspect` | `dead`.
    #[serde(default)]
    pub state: String,
    /// Raft node id (0 = not raft-capable, i.e. no QUIC bus).
    #[serde(default)]
    pub raft_id: u64,
    /// Independent clusters sharing one Postgres are separated by group.
    #[serde(default = "default_group")]
    pub group: String,
    /// WireGuard mesh: public key, dialable endpoint and overlay address (empty = not meshed).
    #[serde(default)]
    pub wg_pubkey: String,
    #[serde(default)]
    pub wg_endpoint: String,
    #[serde(default)]
    pub wg_addr: String,
}

fn default_group() -> String {
    "default".into()
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
    i64,
    String,
    String,
    String,
    String,
);

pub const HEARTBEAT: Duration = Duration::from_secs(5);
pub const DEAD_AFTER: Duration = Duration::from_secs(15);
/// Bus topic announcing a voluntary departure (payload: node uuid); peers mark it dead at once.
pub const LEAVE_TOPIC: &str = "cluster.leave";
/// Payload: node id asked to start a Raft election (sent by a leaving leader, see `step_down`).
pub const ELECT_TOPIC: &str = "cluster.elect";
const BUS_CHANNEL: &str = "musubi_bus";
/// Postgres NOTIFY payloads are capped at 8000 bytes.
const MAX_BUS_PAYLOAD: usize = 7900;
const BUS_ALPN: &[u8] = b"musubi-bus/1";
/// Largest message accepted over the QUIC bus.
pub const MAX_QUIC_BUS_PAYLOAD: usize = 16 * 1024 * 1024;
const QUIC_SEND_TIMEOUT: Duration = Duration::from_secs(3);
const QUIC_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_BACKOFF: Duration = Duration::from_secs(2);

pub(crate) struct QuicBus {
    pub(crate) endpoint: quinn::Endpoint,
    pub(crate) conns: Mutex<HashMap<Uuid, quinn::Connection>>,
    /// peers whose last connect failed: skipped (fast fail → postgres fallback) for CONNECT_BACKOFF
    conn_backoff: Mutex<HashMap<Uuid, std::time::Instant>>,
    /// One long-lived uni stream per (peer, priority): frames go out length-prefixed on it in
    /// order, and a send returns once the bytes are in the stream (flow control is the only
    /// wait) instead of after the peer has read them — one round trip per frame serialized the
    /// bus lanes (3 ms a session batch on the stand, 48 s of sends in a 40 s run on bulk).
    lane_streams: Mutex<HashMap<(Uuid, i32), LaneStream>>,
}

/// A long-lived outgoing bus stream, written by one sender at a time.
type LaneStream = Arc<tokio::sync::Mutex<quinn::SendStream>>;

/// Transport settings for both ends of a bus connection. The windows are sized for one
/// connection carrying every lane between two busy nodes (~60 MB/s on the stand): quinn's
/// defaults (1.25 MB stream window, 8 MB connection window) made a 64-frame session batch wait
/// 2.7 ms on flow control with 15 µs of work behind it.
fn bus_transport() -> Result<Arc<quinn::TransportConfig>> {
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(Duration::from_secs(60)).map_err(Error::internal)?,
    ));
    transport.keep_alive_interval(Some(Duration::from_secs(15)));
    transport.stream_receive_window(quinn::VarInt::from_u32(16 * 1024 * 1024));
    transport.receive_window(quinn::VarInt::from_u32(64 * 1024 * 1024));
    transport.send_window(64 * 1024 * 1024);
    Ok(Arc::new(transport))
}

#[derive(Clone)]
pub struct Cluster {
    db: PgPool,
    me: NodeInfo,
    bus_tx: broadcast::Sender<BusMessage>,
    pub(crate) peers: Arc<RwLock<Vec<NodeInfo>>>,
    /// (heartbeat, election_min, election_max) in ms for the Raft group.
    pub(crate) raft_timing: (u64, u64, u64),
    pub(crate) bus: Option<Arc<QuicBus>>,
    pub(crate) members: Arc<RwLock<HashMap<Uuid, swim::Member>>>,
    pub(crate) incarnation: Arc<AtomicU64>,
    pub(crate) raft: Arc<std::sync::OnceLock<raft::RaftState>>,
    /// Node data directory: stable node id + Raft storage. `None` = ephemeral node.
    pub(crate) data_dir: Option<std::path::PathBuf>,
    /// Set by `leave()`: stop heartbeating so the deleted row does not come back.
    left: Arc<AtomicBool>,
    pub(crate) member_tx: broadcast::Sender<MemberEvent>,
    /// WireGuard mesh of this node (`[mesh_net]`), `None` = not meshed.
    mesh: Option<Arc<MeshNet>>,
}

/// Live WireGuard mesh state of this node: its key, overlay address, the peer set last derived
/// from the registry and what happened when it was applied to the kernel interface.
pub struct MeshNet {
    pub settings: wg::Settings,
    ident: std::sync::Mutex<wg::Identity>,
    key_path: std::path::PathBuf,
    scratch: std::path::PathBuf,
    pub address: std::net::Ipv4Addr,
    peers: std::sync::Mutex<Vec<wg::Peer>>,
    state: std::sync::Mutex<wg::ApplyState>,
}

/// Peers silent for longer than this drop out of the rendered config even before their row is
/// reaped.
const MESH_PEER_STALE: Duration = Duration::from_secs(600);
const MESH_REFRESH: Duration = Duration::from_secs(10);

impl MeshNet {
    pub fn public_key(&self) -> String {
        self.ident.lock().unwrap().public_b64()
    }

    pub fn peers(&self) -> Vec<wg::Peer> {
        self.peers.lock().unwrap().clone()
    }

    pub fn state(&self) -> wg::ApplyState {
        self.state.lock().unwrap().clone()
    }

    /// `wg-quick` file (`quick`) or `wg setconf` input for the current peer set.
    pub fn render(&self, quick: bool) -> String {
        let peers = self.peers();
        wg::render(
            &self.ident.lock().unwrap(),
            self.address,
            &self.settings.subnet,
            self.settings.listen_port,
            self.settings.keepalive_secs,
            &peers,
            quick,
        )
    }

    /// Overlay `host:port` for a service on this node.
    pub fn overlay_addr(&self, port: u16) -> SocketAddr {
        SocketAddr::from((self.address, port))
    }

    pub fn status(&self) -> serde_json::Value {
        let st = self.state();
        let peers = self.peers();
        serde_json::json!({
            "enabled": true,
            "interface": self.settings.interface,
            "address": self.address.to_string(),
            "subnet": self.settings.subnet.to_string(),
            "listen_port": self.settings.listen_port,
            "endpoint": self.settings.endpoint,
            "public_key": self.public_key(),
            "apply": match self.settings.apply {
                wg::ApplyMode::Auto => "auto",
                wg::ApplyMode::Wg => "wg",
                wg::ApplyMode::None => "none",
            },
            "keepalive_secs": self.settings.keepalive_secs,
            "bind_bus": self.settings.bind_bus,
            "applied": st.applied,
            "tools_missing": st.tools_missing,
            "last_error": st.last_error,
            "applied_at": st.applied_at.map(|t| t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()),
            "generation": st.generation,
            "peers_hash": st.peers_hash,
            "peer_count": peers.len(),
            "alive_peers": peers.iter().filter(|p| p.alive).count(),
            "peers": peers,
        })
    }
}

/// SWIM membership change: `state` is `alive` (new member), `suspect` or `dead`.
#[derive(Debug, Clone)]
pub struct MemberEvent {
    pub id: Uuid,
    pub state: &'static str,
}

/// A message from another node. `topic` routes it; `payload` is topic-specific JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub origin: NodeId,
    pub topic: String,
    /// The message body as it was serialized by the sender: carried through untouched (one
    /// serialization on send, one parse on receive — as a `Value` a frame of 64 deltas was
    /// serialized twice and decoded twice, and the bus lanes ran 15–25 s behind on the stand).
    pub payload: Box<serde_json::value::RawValue>,
    /// Set on the Postgres copy of a message that QUIC-capable peers already received directly.
    #[serde(default)]
    pub via_quic: bool,
    /// Non-empty: only these nodes should accept the Postgres copy (targeted redelivery).
    #[serde(default)]
    pub only: Vec<NodeId>,
    #[serde(default = "default_group")]
    pub group: String,
    /// Sender's wall clock (unix ms) when the message was handed to the bus: the receiver
    /// measures its own queueing from it (`musubi_bus_inbound_lag_ms`).
    #[serde(default)]
    pub sent_ms: u64,
}

/// First object key of a JSON text: `{"Tag": ...}` → `Tag`. For `{"WorldBatch":[{"Tag":...` the
/// caller asks again on the text after the first `[`.
pub fn first_key(text: &str) -> Option<&str> {
    let t = text.trim_start();
    let t = t.strip_prefix('{')?.trim_start();
    let t = t.strip_prefix('"')?;
    let end = t.find('"')?;
    Some(&t[..end])
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Cluster {
    pub fn new(db: PgPool, name: &str, region: &str, roles: Vec<String>, advertise: &str) -> Self {
        let now = OffsetDateTime::now_utc();
        let (bus_tx, _) = broadcast::channel(65536);
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
                state: "alive".into(),
                raft_id: 0,
                group: default_group(),
                wg_pubkey: String::new(),
                wg_endpoint: String::new(),
                wg_addr: String::new(),
            },
            bus_tx,
            peers: Arc::new(RwLock::new(Vec::new())),
            raft_timing: (500, 1500, 2500),
            bus: None,
            members: Arc::new(RwLock::new(HashMap::new())),
            // seeded from the clock so a process restarted with the same identity outranks every
            // incarnation its peers saw from the previous life (and can revive from Dead)
            incarnation: Arc::new(AtomicU64::new(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(1),
            )),
            raft: Arc::new(std::sync::OnceLock::new()),
            data_dir: None,
            left: Arc::new(AtomicBool::new(false)),
            member_tx: broadcast::channel(256).0,
            mesh: None,
        }
    }

    /// Join the WireGuard mesh (`[mesh_net]`, after `with_data_dir`): load or create the key,
    /// claim the sticky overlay address (lowest free in the subnet, kept across restarts in the
    /// data dir) and publish key/endpoint/address so peers can configure us before we start.
    pub async fn with_mesh_net(mut self, cfg: &musubi_core::config::MeshNetConfig) -> Result<Self> {
        if !cfg.enabled {
            return Ok(self);
        }
        let settings = wg::Settings::from_config(cfg, &self.me.advertise)?;
        let dir = self.data_dir.clone().ok_or_else(|| {
            Error::InvalidArgument("mesh_net needs node.data_dir (set with_data_dir first)".into())
        })?;
        let key_path = dir.join("wg_private_key");
        let ident = wg::Identity::load_or_create(&key_path)?;
        let addr_path = dir.join("wg_addr");
        let mut preferred: Option<std::net::Ipv4Addr> = std::fs::read_to_string(&addr_path)
            .ok()
            .and_then(|t| t.trim().parse().ok());
        let pubkey = ident.public_b64();
        let mut address = None;
        for _ in 0..32 {
            let rows: Vec<(String,)> = sqlx::query_as(
                "select wg_addr from musubi.nodes where cluster_group = $1 and wg_addr <> '' and id <> $2",
            )
            .bind(&self.me.group)
            .bind(self.me.id.0)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
            let taken: std::collections::BTreeSet<std::net::Ipv4Addr> =
                rows.into_iter().filter_map(|(a,)| a.parse().ok()).collect();
            let Some(candidate) = wg::allocate(&settings.subnet, &taken, preferred) else {
                return Err(Error::InvalidArgument(format!(
                    "mesh_net.subnet {} has no free address",
                    settings.subnet
                )));
            };
            let claim = sqlx::query(
                "insert into musubi.nodes (id, name, region, roles, version, advertise, cluster_group, wg_pubkey, wg_endpoint, wg_addr) \
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 on conflict (id) do update set wg_pubkey = excluded.wg_pubkey, wg_endpoint = excluded.wg_endpoint, wg_addr = excluded.wg_addr",
            )
            .bind(self.me.id.0)
            .bind(&self.me.name)
            .bind(&self.me.region)
            .bind(&self.me.roles)
            .bind(&self.me.version)
            .bind(&self.me.advertise)
            .bind(&self.me.group)
            .bind(&pubkey)
            .bind(&settings.endpoint)
            .bind(candidate.to_string())
            .execute(&self.db)
            .await;
            match claim {
                Ok(_) => {
                    address = Some(candidate);
                    break;
                }
                Err(e)
                    if e.as_database_error()
                        .is_some_and(|d| d.is_unique_violation()) =>
                {
                    preferred = None;
                }
                Err(e) => return Err(Error::internal(e)),
            }
        }
        let address = address.ok_or_else(|| {
            Error::Internal("mesh_net: could not claim an overlay address".into())
        })?;
        let _ = std::fs::write(&addr_path, format!("{address}\n"));
        self.me.wg_pubkey = pubkey;
        self.me.wg_endpoint = settings.endpoint.clone();
        self.me.wg_addr = address.to_string();
        tracing::info!(address = %address, subnet = %settings.subnet, endpoint = %settings.endpoint, "wireguard mesh joined");
        self.mesh = Some(Arc::new(MeshNet {
            settings,
            ident: std::sync::Mutex::new(ident),
            key_path,
            scratch: dir,
            address,
            peers: std::sync::Mutex::new(Vec::new()),
            state: std::sync::Mutex::new(wg::ApplyState::default()),
        }));
        Ok(self)
    }

    pub fn mesh_net(&self) -> Option<&Arc<MeshNet>> {
        self.mesh.as_ref()
    }

    /// Re-derive the peer set from the registry and, when it changed (or the last apply
    /// failed), load it into the kernel interface.
    pub async fn refresh_mesh(self: &Arc<Self>) -> Result<()> {
        let Some(m) = &self.mesh else {
            return Ok(());
        };
        let now = OffsetDateTime::now_utc();
        let peers: Vec<wg::Peer> = self
            .nodes()
            .await?
            .into_iter()
            .filter(|n| {
                n.id != self.me.id
                    && !n.wg_addr.is_empty()
                    && wg::valid_public_key(&n.wg_pubkey)
                    && now - n.heartbeat_at < MESH_PEER_STALE
            })
            .map(|n| wg::Peer {
                node: n.id.0,
                name: n.name,
                region: n.region,
                public_key: n.wg_pubkey,
                endpoint: n.wg_endpoint,
                address: n.wg_addr,
                alive: n.alive,
            })
            .collect();
        let hash = wg::peers_hash(&peers, m.settings.keepalive_secs);
        let (changed, retry) = {
            let st = m.state.lock().unwrap();
            (
                st.peers_hash != hash,
                st.last_error.is_some() || (st.generation == 0 && !st.applied && !st.tools_missing),
            )
        };
        *m.peers.lock().unwrap() = peers;
        if !changed && !retry {
            return Ok(());
        }
        let conf = m.render(false);
        let res = wg::apply(&m.settings, m.address, &conf, &m.scratch).await;
        let mut st = m.state.lock().unwrap();
        if changed || st.generation == 0 {
            st.generation += 1;
        }
        st.peers_hash = hash;
        match res {
            Ok(true) => {
                st.applied = true;
                st.tools_missing = false;
                st.last_error = None;
                st.applied_at = Some(now);
            }
            Ok(false) => {
                st.applied = false;
                st.tools_missing = m.settings.apply == wg::ApplyMode::Auto;
                st.last_error = None;
            }
            Err(e) => {
                tracing::warn!(error = %e, "wireguard apply failed");
                st.applied = false;
                st.tools_missing = false;
                st.last_error = Some(e);
            }
        }
        Ok(())
    }

    /// New key pair: saved, published, and pushed to the interface on the next refresh (which
    /// is forced). Peers pick the new key up from the registry within one refresh interval.
    pub async fn rotate_mesh_key(self: &Arc<Self>) -> Result<String> {
        let Some(m) = &self.mesh else {
            return Err(Error::InvalidArgument("mesh_net is not enabled".into()));
        };
        let ident = wg::Identity::generate();
        ident.save(&m.key_path)?;
        let pubkey = ident.public_b64();
        sqlx::query("update musubi.nodes set wg_pubkey = $2 where id = $1")
            .bind(self.me.id.0)
            .bind(&pubkey)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        *m.ident.lock().unwrap() = ident;
        m.state.lock().unwrap().peers_hash.clear();
        self.refresh_mesh().await?;
        Ok(pubkey)
    }

    /// Membership scope: nodes only see, gossip with and replicate to their own group.
    pub fn with_group(mut self, group: &str) -> Self {
        self.me.group = group.to_string();
        self
    }

    /// Raft heartbeat / election timing (ms); see `NodeConfig::raft_heartbeat_ms`.
    pub fn with_raft_timing(
        mut self,
        heartbeat_ms: u64,
        election_min_ms: u64,
        election_max_ms: u64,
    ) -> Self {
        self.raft_timing = (heartbeat_ms, election_min_ms, election_max_ms);
        self
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
        cfg.transport_config(bus_transport()?);
        let endpoint = quinn::Endpoint::server(cfg, listen).map_err(Error::internal)?;
        let local = endpoint.local_addr().map_err(Error::internal)?;
        self.me.bus_addr = match advertise {
            Some(a) => a.to_string(),
            None => local.to_string(),
        };
        self.me.bus_cert = hex::encode(der.as_ref());
        self.me.raft_id = raft::raft_id(self.me.id.0);
        self.bus = Some(Arc::new(QuicBus {
            endpoint,
            conns: Mutex::new(HashMap::new()),
            conn_backoff: Mutex::new(HashMap::new()),
            lane_streams: Mutex::new(HashMap::new()),
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
        self.members
            .write()
            .await
            .retain(|id, _| nodes.iter().any(|n| n.id.0 == *id));
        let mut peers = self.peers.write().await;
        // a node seen for the first time (new row in `musubi.nodes`) is a join
        for n in nodes.iter().filter(|n| n.alive && n.id != self.me.id) {
            if !peers.iter().any(|p| p.id == n.id) {
                let _ = self.member_tx.send(MemberEvent {
                    id: n.id.0,
                    state: swim::State::Alive.as_str(),
                });
            }
        }
        *peers = nodes;
        Ok(())
    }

    /// Close the QUIC endpoint but keep heartbeating: simulates a node whose network died.
    pub fn close_bus(&self) {
        if let Some(bus) = &self.bus {
            bus.endpoint.close(1u32.into(), b"closed");
        }
    }

    pub fn me(&self) -> &NodeInfo {
        &self.me
    }

    pub fn node_id(&self) -> NodeId {
        self.me.id
    }

    /// Give the node a home on disk: its id is read from (or written to) `<dir>/node_id`, so the
    /// same identity — and Raft membership, worlds, matches — survives a restart. Raft log and
    /// state machine live under `<dir>/raft/<group>/`.
    pub fn with_data_dir(mut self, dir: impl Into<std::path::PathBuf>) -> Result<Self> {
        let dir: std::path::PathBuf = dir.into();
        std::fs::create_dir_all(&dir).map_err(Error::internal)?;
        let id_path = dir.join("node_id");
        let id = match std::fs::read_to_string(&id_path) {
            Ok(text) => Uuid::parse_str(text.trim())
                .map_err(|_| Error::Internal(format!("{} is not a uuid", id_path.display())))?,
            Err(_) => {
                std::fs::write(&id_path, self.me.id.0.to_string()).map_err(Error::internal)?;
                self.me.id.0
            }
        };
        self.me.id = NodeId(id);
        if self.bus.is_some() {
            self.me.raft_id = raft::raft_id(id);
        }
        self.data_dir = Some(dir);
        Ok(self)
    }

    /// Insert our row, then keep heartbeating and relaying bus notifications until dropped.
    /// Boot epoch of this process (the SWIM incarnation it started with): rows a previous
    /// life of the same node id owned carry a smaller epoch and may be reclaimed at once.
    pub fn epoch(&self) -> i64 {
        self.incarnation.load(Ordering::Relaxed) as i64
    }

    pub async fn start(self: &Arc<Self>) -> Result<()> {
        if self.left.load(Ordering::Relaxed) {
            return Ok(());
        }
        sqlx::query(
            "insert into musubi.nodes (id, name, region, roles, version, advertise, started_at, heartbeat_at, bus_addr, bus_cert, raft_id, cluster_group, epoch) values ($1, $2, $3, $4, $5, $6, $7, now(), $8, $9, $10, $11, $12) on conflict (id) do update set name = excluded.name, region = excluded.region, roles = excluded.roles, version = excluded.version, advertise = excluded.advertise, started_at = excluded.started_at, heartbeat_at = now(), bus_addr = excluded.bus_addr, bus_cert = excluded.bus_cert, raft_id = excluded.raft_id, cluster_group = excluded.cluster_group, epoch = excluded.epoch",
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
        .bind(self.me.raft_id as i64)
        .bind(&self.me.group)
        .bind(self.epoch())
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
        if self.mesh.is_some() {
            // first peer set synchronously, so the interface is up before anything dials it
            if let Err(e) = self.refresh_mesh().await {
                tracing::warn!(error = %e, "wireguard mesh initial refresh failed");
            }
            let mesh = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(MESH_REFRESH);
                let mut events = mesh.member_events();
                loop {
                    tokio::select! {
                        _ = tick.tick() => {}
                        ev = events.recv() => if ev.is_err() { tokio::time::sleep(MESH_REFRESH).await; }
                    }
                    if mesh.left.load(Ordering::Relaxed) {
                        break;
                    }
                    if let Err(e) = mesh.refresh_mesh().await {
                        tracing::warn!(error = %e, "wireguard mesh refresh failed");
                    }
                }
            });
        }
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
                                metrics::counter!("musubi_bus_messages_total", "transport" => "postgres", "dir" => "rx", "ok" => "true").increment(1);
                                metrics::counter!("musubi_bus_bytes_total", "transport" => "postgres", "dir" => "rx").increment(n.payload().len() as u64);
                                relay.note_leave(&msg).await;
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
            let (ep, cl) = (bus.endpoint.clone(), self.clone());
            tokio::spawn(async move {
                while let Some(incoming) = ep.accept().await {
                    let cl = cl.clone();
                    tokio::spawn(async move {
                        let Ok(conn) = incoming.await else { return };
                        let (c_uni, conn_uni) = (cl.clone(), conn.clone());
                        tokio::spawn(async move {
                            while let Ok(mut recv) = conn_uni.accept_uni().await {
                                let c = c_uni.clone();
                                tokio::spawn(async move {
                                    // a lane stream carries length-prefixed frames for the
                                    // life of the connection; a legacy peer opens one stream per
                                    // frame and starts with the JSON itself
                                    let mut first = [0u8; 1];
                                    if recv.read_exact(&mut first).await.is_err() {
                                        return;
                                    }
                                    if first[0] == b'{' {
                                        let Ok(rest) = recv.read_to_end(MAX_QUIC_BUS_PAYLOAD).await
                                        else {
                                            return;
                                        };
                                        let mut buf = Vec::with_capacity(rest.len() + 1);
                                        buf.push(first[0]);
                                        buf.extend_from_slice(&rest);
                                        c.ingest_quic_frame(buf).await;
                                        return;
                                    }
                                    let mut len = [0u8; 4];
                                    len[0] = first[0];
                                    if recv.read_exact(&mut len[1..]).await.is_err() {
                                        return;
                                    }
                                    // the reader only moves bytes off the stream (releasing
                                    // the sender's flow-control window); a worker parses and
                                    // dispatches the frames in the same order on another task
                                    let (tx, mut rx) =
                                        tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
                                    let worker = c.clone();
                                    tokio::spawn(async move {
                                        while let Some(buf) = rx.recv().await {
                                            let started = std::time::Instant::now();
                                            worker.ingest_quic_frame(buf).await;
                                            metrics::histogram!("musubi_bus_ingest_ms")
                                                .record(started.elapsed().as_secs_f64() * 1000.0);
                                        }
                                    });
                                    loop {
                                        let n = u32::from_be_bytes(len) as usize;
                                        if n == 0 || n > MAX_QUIC_BUS_PAYLOAD {
                                            tracing::warn!(len = n, "bad quic bus frame length");
                                            return;
                                        }
                                        let mut buf = vec![0u8; n];
                                        if recv.read_exact(&mut buf).await.is_err() {
                                            return;
                                        }
                                        if tx.send(buf).is_err() {
                                            return;
                                        }
                                        if recv.read_exact(&mut len).await.is_err() {
                                            return;
                                        }
                                    }
                                });
                            }
                        });
                        while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                            let c = cl.clone();
                            tokio::spawn(async move {
                                let Ok(buf) = recv.read_to_end(raft::MAX_RAFT_FRAME).await else {
                                    return;
                                };
                                let resp = match buf.split_first() {
                                    Some((&raft::TAG_SWIM, body)) => {
                                        let Ok(msg) = serde_json::from_slice::<swim::Msg>(body)
                                        else {
                                            return;
                                        };
                                        serde_json::to_vec(&c.handle_swim(msg).await)
                                            .unwrap_or_default()
                                    }
                                    Some((&raft::TAG_RAFT, body)) => c.handle_raft(body).await,
                                    Some((&raft::TAG_WRITE, body)) => {
                                        c.handle_forwarded_write(body).await
                                    }
                                    Some((&raft::TAG_READ, body)) => {
                                        c.handle_forwarded_read(body).await
                                    }
                                    _ => return,
                                };
                                let _ = send.write_all(&resp).await;
                                let _ = send.finish();
                                let _ = send.stopped().await;
                            });
                        }
                    });
                }
            });
            let probe = self.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(swim::PROBE_INTERVAL);
                let mut n = 0usize;
                loop {
                    tick.tick().await;
                    probe.probe_round(n).await;
                    n = n.wrapping_add(1);
                }
            });
            self.start_raft().await?;
        }
        self.refresh_peers().await?;
        Ok(())
    }

    fn accepts_notify(&self, msg: &BusMessage) -> bool {
        if msg.origin == self.me.id || msg.group != self.me.group {
            return false;
        }
        if !msg.only.is_empty() {
            return msg.only.contains(&self.me.id);
        }
        !(msg.via_quic && self.bus.is_some())
    }

    async fn ingest_quic_frame(&self, buf: Vec<u8>) {
        match serde_json::from_slice::<BusMessage>(&buf) {
            Ok(msg) if msg.origin != self.me.id => {
                metrics::counter!("musubi_bus_messages_total", "transport" => "quic", "dir" => "rx", "ok" => "true").increment(1);
                metrics::counter!("musubi_bus_bytes_total", "transport" => "quic", "dir" => "rx")
                    .increment(buf.len() as u64);
                self.note_leave(&msg).await;
                let _ = self.bus_tx.send(msg);
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "bad quic bus frame"),
        }
    }

    async fn note_leave(&self, msg: &BusMessage) {
        if msg.topic == LEAVE_TOPIC {
            if let Ok(id) = serde_json::from_str::<Uuid>(msg.payload.get()) {
                self.set_state(id, swim::State::Dead).await;
            }
        } else if msg.topic == ELECT_TOPIC {
            if let Ok(id) = serde_json::from_str::<Uuid>(msg.payload.get()) {
                if id == self.me.id.0 {
                    if let Some(r) = self.raft_state() {
                        if let Err(e) = r.raft.trigger().elect().await {
                            tracing::debug!(error = %e, "raft elect on request");
                        }
                    }
                }
            }
        }
    }

    /// Voluntary departure: tell peers (they mark us dead immediately instead of waiting for
    /// SWIM), drop every lease we hold and our node row, and stop heartbeating. Callers hand off
    /// matches/worlds before this so the failover leader finds them orphaned.
    pub async fn leave(&self) {
        self.step_down().await;
        self.left.store(true, Ordering::Relaxed);
        let _ = sqlx::query("delete from musubi.leases where holder = $1")
            .bind(self.me.id.0)
            .execute(&self.db)
            .await;
        let _ = sqlx::query("delete from musubi.nodes where id = $1")
            .bind(self.me.id.0)
            .execute(&self.db)
            .await;
        // announce last: by now the database already shows us gone
        if let Err(e) = self.publish(LEAVE_TOPIC, &self.me.id.0).await {
            tracing::warn!(error = %e, "leave broadcast failed");
        }
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
            "select id, name, region, roles, version, advertise, started_at, heartbeat_at, bus_addr, bus_cert, raft_id, cluster_group, wg_pubkey, wg_endpoint, wg_addr from musubi.nodes where cluster_group = $1 order by started_at",
        )
        .bind(&self.me.group)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let now = OffsetDateTime::now_utc();
        let members = self.members.read().await.clone();
        Ok(rows
            .into_iter()
            .map(|r| {
                let db_alive = now - r.7 < DEAD_AFTER;
                let swim = members.get(&r.0).map(|m| m.state);
                let state = match swim {
                    Some(State::Dead) => "dead",
                    Some(State::Suspect) if db_alive => "suspect",
                    _ if db_alive => "alive",
                    _ => "dead",
                };
                NodeInfo {
                    id: NodeId(r.0),
                    name: r.1,
                    region: r.2,
                    roles: r.3,
                    version: r.4,
                    advertise: r.5,
                    started_at: r.6,
                    heartbeat_at: r.7,
                    alive: db_alive && swim != Some(State::Dead),
                    bus_addr: r.8,
                    bus_cert: r.9,
                    state: state.into(),
                    raft_id: r.10 as u64,
                    group: r.11,
                    wg_pubkey: r.12,
                    wg_endpoint: r.13,
                    wg_addr: r.14,
                }
            })
            .collect())
    }

    /// Broadcast to every other node. Own node never receives its own messages.
    ///
    /// With the QUIC bus enabled, every live peer that advertises a bus address gets the message
    /// over a direct stream; the Postgres copy is only sent for peers without one (flagged
    /// `via_quic` so bus peers drop it) and, targeted, for peers whose QUIC delivery failed.
    pub async fn publish<T: Serialize>(&self, topic: &str, payload: &T) -> Result<()> {
        self.publish_prio(topic, payload, 0).await
    }

    /// `publish` with a QUIC stream priority: all lanes of a peer share one connection and its
    /// flow control, and a send completes when the peer has read the frame — so a small control
    /// message (handover, ack) queued behind megabytes of deltas took 60–120 s on the stand.
    /// Higher goes first; deltas use 0.
    pub async fn publish_prio<T: Serialize>(
        &self,
        topic: &str,
        payload: &T,
        priority: i32,
    ) -> Result<()> {
        let mut msg = BusMessage {
            origin: self.me.id,
            topic: topic.to_string(),
            payload: serde_json::value::to_raw_value(payload)?,
            via_quic: false,
            only: Vec::new(),
            group: self.me.group.clone(),
            sent_ms: unix_ms(),
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
                let r =
                    tokio::time::timeout(QUIC_SEND_TIMEOUT, bus.send(&p, &bytes, priority)).await;
                let ok = matches!(r, Ok(Ok(())));
                // a peer that already failed to connect (backoff) or that SWIM holds suspect is
                // almost surely down: the first failure got its Postgres copy, repeating that for
                // every message turns a dead node into a NOTIFY storm against the live ones (the
                // stand saw ~4000 in 8 s and the bus "resource exhausted"); it catches up from
                // snapshots and journals when it is back
                let backoff = matches!(&r, Ok(Err(Error::Unavailable(m))) if m.ends_with("(backoff)"));
                if backoff || (!ok && p.state == "suspect") {
                    tracing::debug!(peer = %p.id.0, state = %p.state, "quic bus delivery skipped: peer down");
                    return (p.id, true);
                }
                metrics::counter!("musubi_bus_messages_total", "transport" => "quic", "dir" => "tx", "ok" => if ok { "true" } else { "false" }).increment(1);
                metrics::counter!("musubi_bus_bytes_total", "transport" => "quic", "dir" => "tx").increment(bytes.len() as u64);
                if !ok {
                    tracing::warn!(peer = %p.id.0, %p.bus_addr, result = ?r, "quic bus delivery failed; falling back to postgres");
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
        metrics::counter!("musubi_bus_messages_total", "transport" => "postgres", "dir" => "tx", "ok" => "true").increment(1);
        metrics::counter!("musubi_bus_bytes_total", "transport" => "postgres", "dir" => "tx")
            .increment(text.len() as u64);
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

    /// Membership changes seen by SWIM (bus clusters only).
    pub fn member_events(&self) -> broadcast::Receiver<MemberEvent> {
        self.member_tx.subscribe()
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
                // With Raft running, the elected leader owns every duty; the Postgres CAS lease
                // is only the fallback for clusters without the QUIC bus. Leadership is a local
                // read, so poll it every second to follow elections promptly.
                if c.raft.get().is_some() {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    tick.tick().await;
                }
                let held = if c.raft.get().is_some() {
                    c.is_raft_leader()
                } else {
                    match c.try_lease(&l.name, ttl).await {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(lease = %l.name, error = %e, "lease renewal failed");
                            false
                        }
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
        serde_json::from_str(self.payload.get()).map_err(Into::into)
    }

    /// The payload parsed into a tree (for the small control messages that inspect it).
    pub fn payload_value(&self) -> serde_json::Value {
        serde_json::from_str(self.payload.get()).unwrap_or(serde_json::Value::Null)
    }

    /// First key of an object payload (an externally tagged enum's variant), without parsing.
    pub fn payload_tag(&self) -> Option<&str> {
        first_key(self.payload.get())
    }
}

impl Cluster {
    /// Tagged request/response on a QUIC bi-stream (SWIM, Raft RPC, forwarded writes).
    pub(crate) async fn bus_request(
        &self,
        peer: &NodeInfo,
        tag: u8,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let bus = self
            .bus
            .as_ref()
            .ok_or_else(|| Error::Internal("bus disabled".into()))?;
        let r = tokio::time::timeout(timeout, async {
            let conn = bus.connection(peer).await?;
            let (mut send, mut recv) = conn.open_bi().await.map_err(Error::internal)?;
            let mut frame = Vec::with_capacity(body.len() + 1);
            frame.push(tag);
            frame.extend_from_slice(body);
            send.write_all(&frame).await.map_err(Error::internal)?;
            send.finish().map_err(Error::internal)?;
            recv.read_to_end(raft::MAX_RAFT_FRAME)
                .await
                .map_err(Error::internal)
        })
        .await
        .map_err(|_| Error::Internal("bus request timeout".into()))?;
        if r.is_err() {
            bus.conns.lock().await.remove(&peer.id.0);
        }
        r
    }
}

impl QuicBus {
    /// Cached connection to a peer. The handshake runs outside the cache lock: a dead peer must
    /// not stall sends to live ones (that starved Raft votes after a kill), and a peer that just
    /// failed to connect is skipped for CONNECT_BACKOFF so publishers fall back fast.
    pub(crate) async fn connection(&self, peer: &NodeInfo) -> Result<quinn::Connection> {
        if let Some(c) = self.conns.lock().await.get(&peer.id.0) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        if let Some(at) = self.conn_backoff.lock().await.get(&peer.id.0) {
            if at.elapsed() < CONNECT_BACKOFF {
                return Err(Error::Unavailable(format!(
                    "bus peer {} unreachable (backoff)",
                    peer.id.0
                )));
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
        let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
        cfg.transport_config(bus_transport()?);
        let connecting = self
            .endpoint
            .connect_with(cfg, addr, "localhost")
            .map_err(Error::internal)?;
        let conn = match tokio::time::timeout(QUIC_CONNECT_TIMEOUT, connecting).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                self.conn_backoff
                    .lock()
                    .await
                    .insert(peer.id.0, std::time::Instant::now());
                return Err(Error::internal(e));
            }
            Err(_) => {
                self.conn_backoff
                    .lock()
                    .await
                    .insert(peer.id.0, std::time::Instant::now());
                return Err(Error::Unavailable(format!(
                    "bus connect to {} timed out",
                    peer.bus_addr
                )));
            }
        };
        self.conn_backoff.lock().await.remove(&peer.id.0);
        let mut conns = self.conns.lock().await;
        // another task may have connected meanwhile: keep one connection per peer
        if let Some(c) = conns.get(&peer.id.0) {
            if c.close_reason().is_none() {
                return Ok(c.clone());
            }
        }
        conns.insert(peer.id.0, conn.clone());
        Ok(conn)
    }

    async fn send(&self, peer: &NodeInfo, bytes: &[u8], priority: i32) -> Result<()> {
        let key = (peer.id.0, priority);
        let stream = {
            let existing = self.lane_streams.lock().await.get(&key).cloned();
            match existing {
                Some(s) => s,
                None => {
                    let conn = self.connection(peer).await?;
                    let st = conn.open_uni().await.map_err(Error::internal)?;
                    let _ = st.set_priority(priority);
                    let st = Arc::new(tokio::sync::Mutex::new(st));
                    self.lane_streams.lock().await.insert(key, st.clone());
                    st
                }
            }
        };
        let mut st = stream.lock().await;
        let len = u32::try_from(bytes.len()).map_err(Error::internal)?;
        let r = async {
            st.write_all(&len.to_be_bytes()).await?;
            st.write_all(bytes).await
        }
        .await;
        drop(st);
        if let Err(e) = r {
            // the stream (and its connection) is gone: the next send opens a fresh one
            self.lane_streams.lock().await.remove(&key);
            return Err(Error::internal(e));
        }
        Ok(())
    }
}
