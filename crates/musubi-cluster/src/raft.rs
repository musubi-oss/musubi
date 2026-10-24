//! Raft control plane (openraft) over the QUIC bus: a replicated directory (world → home node,
//! generic key/value) and a single elected control-plane leader. Membership follows the live
//! bus peers automatically (bootstrap guarded by a Postgres lease, dead peers removed by the
//! leader). The log is in memory: on a full-cluster restart the directory is rebuilt from Postgres.
#![allow(clippy::result_large_err)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use musubi_core::{Error, Result};
use openraft::error::{
    InstallSnapshotError, NetworkError, RPCError, RaftError, RemoteError, Unreachable,
};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::storage::{RaftStateMachine, Snapshot};
use openraft::{
    BasicNode, Entry, EntryPayload, LogId, RaftSnapshotBuilder, RaftTypeConfig, SnapshotMeta,
    StorageError, StorageIOError, StoredMembership,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::{Cluster, NodeInfo};

pub type RaftNodeId = u64;

openraft::declare_raft_types!(pub TypeConfig: D = DirectoryOp, R = DirectoryResp);

pub type LogStore = crate::log_store::LogStore<TypeConfig>;
pub type Raft = openraft::Raft<TypeConfig>;

pub(crate) const TAG_SWIM: u8 = 1;
pub(crate) const TAG_RAFT: u8 = 2;
pub(crate) const TAG_WRITE: u8 = 3;
pub(crate) const TAG_READ: u8 = 4;
pub(crate) const MAX_RAFT_FRAME: usize = 64 * 1024 * 1024;
const RPC_TIMEOUT: Duration = Duration::from_secs(3);
const MEMBERSHIP_SYNC: Duration = Duration::from_secs(2);
const INIT_LEASE_TTL: Duration = Duration::from_secs(10);
const BOOTSTRAP_GRACE: time::Duration = time::Duration::seconds(3);

/// Raft node id derived from the cluster node uuid.
pub fn raft_id(id: Uuid) -> RaftNodeId {
    let h = Sha256::digest(id.as_bytes());
    u64::from_be_bytes(h[..8].try_into().expect("8 bytes"))
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum DirectoryOp {
    Set { key: String, value: Value },
    Delete { key: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DirectoryResp {
    pub value: Option<Value>,
}

#[derive(Serialize, Deserialize, Debug, Default, Clone)]
pub struct DirectoryData {
    pub last_applied_log: Option<LogId<RaftNodeId>>,
    pub last_membership: StoredMembership<RaftNodeId, BasicNode>,
    pub data: BTreeMap<String, Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StoredSnapshot {
    pub meta: SnapshotMeta<RaftNodeId, BasicNode>,
    pub data: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct StateMachineStore {
    pub state: RwLock<DirectoryData>,
    snapshot_idx: AtomicU64,
    current_snapshot: RwLock<Option<StoredSnapshot>>,
    /// Durable copy (`sm.json`, `snapshot.json`); `None` = memory only.
    dir: Option<std::path::PathBuf>,
}

fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_data()?;
    }
    std::fs::rename(&tmp, path)
}

impl StateMachineStore {
    /// Open the state machine rooted at `dir`, restoring the applied state and the last snapshot
    /// a previous process wrote there. `None` = in-memory.
    pub fn open(dir: Option<std::path::PathBuf>) -> std::io::Result<Self> {
        let mut me = Self::default();
        if let Some(dir) = dir {
            std::fs::create_dir_all(&dir)?;
            if let Ok(bytes) = std::fs::read(dir.join("sm.json")) {
                let st: DirectoryData = serde_json::from_slice(&bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                me.state = RwLock::new(st);
            }
            if let Ok(bytes) = std::fs::read(dir.join("snapshot.json")) {
                let snap: StoredSnapshot = serde_json::from_slice(&bytes)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                me.current_snapshot = RwLock::new(Some(snap));
            }
            me.dir = Some(dir);
        }
        Ok(me)
    }

    fn persist_state(
        &self,
        st: &DirectoryData,
    ) -> std::result::Result<(), StorageError<RaftNodeId>> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let bytes = serde_json::to_vec(st).map_err(|e| StorageIOError::write_state_machine(&e))?;
        write_atomic(&dir.join("sm.json"), &bytes)
            .map_err(|e| StorageIOError::write_state_machine(&e).into())
    }

    fn persist_snapshot(
        &self,
        snap: &StoredSnapshot,
    ) -> std::result::Result<(), StorageError<RaftNodeId>> {
        let Some(dir) = &self.dir else { return Ok(()) };
        let bytes = serde_json::to_vec(snap)
            .map_err(|e| StorageIOError::write_snapshot(Some(snap.meta.signature()), &e))?;
        write_atomic(&dir.join("snapshot.json"), &bytes)
            .map_err(|e| StorageIOError::write_snapshot(Some(snap.meta.signature()), &e).into())
    }
}

impl RaftSnapshotBuilder<TypeConfig> for Arc<StateMachineStore> {
    async fn build_snapshot(
        &mut self,
    ) -> std::result::Result<Snapshot<TypeConfig>, StorageError<RaftNodeId>> {
        let sm = self.state.read().await;
        let data =
            serde_json::to_vec(&sm.data).map_err(|e| StorageIOError::read_state_machine(&e))?;
        let last_applied_log = sm.last_applied_log;
        let last_membership = sm.last_membership.clone();
        drop(sm);
        let idx = self.snapshot_idx.fetch_add(1, Ordering::Relaxed) + 1;
        let snapshot_id = match last_applied_log {
            Some(last) => format!("{}-{}-{}", last.leader_id, last.index, idx),
            None => format!("--{idx}"),
        };
        let meta = SnapshotMeta {
            last_log_id: last_applied_log,
            last_membership,
            snapshot_id,
        };
        let stored = StoredSnapshot {
            meta: meta.clone(),
            data: data.clone(),
        };
        self.persist_snapshot(&stored)?;
        *self.current_snapshot.write().await = Some(stored);
        Ok(Snapshot {
            meta,
            snapshot: Box::new(Cursor::new(data)),
        })
    }
}

impl RaftStateMachine<TypeConfig> for Arc<StateMachineStore> {
    type SnapshotBuilder = Self;

    async fn applied_state(
        &mut self,
    ) -> std::result::Result<
        (
            Option<LogId<RaftNodeId>>,
            StoredMembership<RaftNodeId, BasicNode>,
        ),
        StorageError<RaftNodeId>,
    > {
        let sm = self.state.read().await;
        Ok((sm.last_applied_log, sm.last_membership.clone()))
    }

    async fn apply<I>(
        &mut self,
        entries: I,
    ) -> std::result::Result<Vec<DirectoryResp>, StorageError<RaftNodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
    {
        let mut res = Vec::new();
        let mut sm = self.state.write().await;
        for entry in entries {
            sm.last_applied_log = Some(entry.log_id);
            match entry.payload {
                EntryPayload::Blank => res.push(DirectoryResp::default()),
                EntryPayload::Normal(ref op) => match op {
                    DirectoryOp::Set { key, value } => {
                        sm.data.insert(key.clone(), value.clone());
                        res.push(DirectoryResp {
                            value: Some(value.clone()),
                        });
                    }
                    DirectoryOp::Delete { key } => {
                        let old = sm.data.remove(key);
                        res.push(DirectoryResp { value: old });
                    }
                },
                EntryPayload::Membership(ref mem) => {
                    sm.last_membership = StoredMembership::new(Some(entry.log_id), mem.clone());
                    res.push(DirectoryResp::default());
                }
            }
        }
        self.persist_state(&sm)?;
        Ok(res)
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> std::result::Result<
        Box<<TypeConfig as RaftTypeConfig>::SnapshotData>,
        StorageError<RaftNodeId>,
    > {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<RaftNodeId, BasicNode>,
        snapshot: Box<<TypeConfig as RaftTypeConfig>::SnapshotData>,
    ) -> std::result::Result<(), StorageError<RaftNodeId>> {
        let new = StoredSnapshot {
            meta: meta.clone(),
            data: snapshot.into_inner(),
        };
        let data: BTreeMap<String, Value> = serde_json::from_slice(&new.data)
            .map_err(|e| StorageIOError::read_snapshot(Some(new.meta.signature()), &e))?;
        let st = DirectoryData {
            last_applied_log: meta.last_log_id,
            last_membership: meta.last_membership.clone(),
            data,
        };
        self.persist_state(&st)?;
        self.persist_snapshot(&new)?;
        *self.state.write().await = st;
        *self.current_snapshot.write().await = Some(new);
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> std::result::Result<Option<Snapshot<TypeConfig>>, StorageError<RaftNodeId>> {
        Ok(self
            .current_snapshot
            .read()
            .await
            .as_ref()
            .map(|s| Snapshot {
                meta: s.meta.clone(),
                snapshot: Box::new(Cursor::new(s.data.clone())),
            }))
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        self.clone()
    }
}

/// Raft RPCs carried on QUIC bi-streams tagged `TAG_RAFT`.
#[derive(Serialize, Deserialize, Debug)]
pub(crate) enum RaftRpc {
    Append(AppendEntriesRequest<TypeConfig>),
    Vote(VoteRequest<RaftNodeId>),
    Snapshot(InstallSnapshotRequest<TypeConfig>),
}

pub(crate) struct RaftState {
    pub raft: Raft,
    pub sm: Arc<StateMachineStore>,
}

pub(crate) struct RaftNet {
    cluster: Cluster,
}

pub(crate) struct RaftConn {
    cluster: Cluster,
    target: RaftNodeId,
}

impl RaftNetworkFactory<TypeConfig> for RaftNet {
    type Network = RaftConn;

    async fn new_client(&mut self, target: RaftNodeId, _node: &BasicNode) -> Self::Network {
        RaftConn {
            cluster: self.cluster.clone(),
            target,
        }
    }
}

impl RaftConn {
    async fn call<Resp, E>(
        &self,
        rpc: RaftRpc,
    ) -> std::result::Result<Resp, RPCError<RaftNodeId, BasicNode, E>>
    where
        Resp: DeserializeOwned,
        E: std::error::Error + DeserializeOwned,
    {
        let peer = self
            .cluster
            .peer_by_raft_id(self.target)
            .await
            .ok_or_else(|| {
                RPCError::Unreachable(Unreachable::new(&std::io::Error::other(format!(
                    "raft peer {} unknown",
                    self.target
                ))))
            })?;
        let body =
            serde_json::to_vec(&rpc).map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        let resp = self
            .cluster
            .bus_request(&peer, TAG_RAFT, &body, RPC_TIMEOUT)
            .await
            .map_err(|e| {
                RPCError::Unreachable(Unreachable::new(&std::io::Error::other(e.to_string())))
            })?;
        let res: std::result::Result<Resp, E> =
            serde_json::from_slice(&resp).map_err(|e| RPCError::Network(NetworkError::new(&e)))?;
        res.map_err(|e| RPCError::RemoteError(RemoteError::new(self.target, e)))
    }
}

impl RaftNetwork<TypeConfig> for RaftConn {
    async fn append_entries(
        &mut self,
        req: AppendEntriesRequest<TypeConfig>,
        _o: RPCOption,
    ) -> std::result::Result<
        AppendEntriesResponse<RaftNodeId>,
        RPCError<RaftNodeId, BasicNode, RaftError<RaftNodeId>>,
    > {
        self.call(RaftRpc::Append(req)).await
    }

    async fn install_snapshot(
        &mut self,
        req: InstallSnapshotRequest<TypeConfig>,
        _o: RPCOption,
    ) -> std::result::Result<
        InstallSnapshotResponse<RaftNodeId>,
        RPCError<RaftNodeId, BasicNode, RaftError<RaftNodeId, InstallSnapshotError>>,
    > {
        self.call(RaftRpc::Snapshot(req)).await
    }

    async fn vote(
        &mut self,
        req: VoteRequest<RaftNodeId>,
        _o: RPCOption,
    ) -> std::result::Result<
        VoteResponse<RaftNodeId>,
        RPCError<RaftNodeId, BasicNode, RaftError<RaftNodeId>>,
    > {
        self.call(RaftRpc::Vote(req)).await
    }
}

impl Cluster {
    pub(crate) async fn peer_by_raft_id(&self, id: RaftNodeId) -> Option<NodeInfo> {
        self.peers
            .read()
            .await
            .iter()
            .find(|p| p.raft_id == id)
            .cloned()
    }

    pub(crate) async fn start_raft(&self) -> Result<()> {
        if self.bus.is_none() {
            return Ok(());
        }
        let config = openraft::Config {
            // followers refuse votes while the leader lease (= election_timeout_min since the
            // last heartbeat) runs, so this also bounds how fast a leaving leader is replaced
            heartbeat_interval: self.raft_timing.0,
            election_timeout_min: self.raft_timing.1,
            election_timeout_max: self.raft_timing.2,
            ..Default::default()
        };
        let config = Arc::new(config.validate().map_err(Error::internal)?);
        let dir = self
            .data_dir
            .as_ref()
            .map(|d| d.join("raft").join(&self.me.group));
        if let Some(d) = &dir {
            tracing::info!(dir = %d.display(), raft_id = self.me.raft_id, "raft storage on disk");
        }
        let sm = Arc::new(StateMachineStore::open(dir.clone()).map_err(Error::internal)?);
        let log = LogStore::open(dir).map_err(Error::internal)?;
        let raft = Raft::new(
            self.me.raft_id,
            config,
            RaftNet {
                cluster: self.clone(),
            },
            log,
            sm.clone(),
        )
        .await
        .map_err(Error::internal)?;
        if self.raft.set(RaftState { raft, sm }).is_err() {
            return Err(Error::Internal("raft already started".into()));
        }
        let c = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(MEMBERSHIP_SYNC);
            loop {
                tick.tick().await;
                c.raft_control_round().await;
            }
        });
        Ok(())
    }

    pub(crate) fn raft_state(&self) -> Option<&RaftState> {
        self.raft.get()
    }

    /// Our raft id (0 when the QUIC bus is disabled).
    pub fn raft_id(&self) -> RaftNodeId {
        self.me.raft_id
    }

    pub fn raft_leader(&self) -> Option<RaftNodeId> {
        self.raft_state()
            .and_then(|r| r.raft.metrics().borrow().current_leader)
    }

    /// Hand Raft leadership to a live voter before leaving: ask one peer to elect itself (its
    /// higher term makes us step down at once) and wait briefly for the term to move on, so the
    /// cluster does not sit through an election timeout while the leaving node's cells and
    /// matches wait for a directory write.
    pub async fn step_down(&self) {
        if !self.is_raft_leader() {
            return;
        }
        let Some(r) = self.raft_state() else { return };
        let voters: Vec<RaftNodeId> = {
            let metrics = r.raft.metrics();
            let m = metrics.borrow();
            m.membership_config.membership().voter_ids().collect()
        };
        let target = self
            .peers
            .read()
            .await
            .iter()
            .find(|p| p.id != self.me.id && voters.contains(&p.raft_id))
            .map(|p| p.id);
        let Some(target) = target else { return };
        if let Err(e) = self.publish(super::ELECT_TOPIC, &target).await {
            tracing::debug!(error = %e, "raft step down: elect request not sent");
            return;
        }
        // openraft 0.9 has no leadership transfer: peers reject the candidate's votes while our
        // lease runs, and every heartbeat renews it, so stop heartbeating and keep asking the
        // peer to run until the lease has expired
        if let Err(e) = r.raft.shutdown().await {
            tracing::debug!(error = %e, "raft shutdown on leave");
        }
        for _ in 0..4 {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let _ = self.publish(super::ELECT_TOPIC, &target).await;
        }
        tracing::info!(?target, "raft leadership handed over before leaving");
    }

    pub fn is_raft_leader(&self) -> bool {
        self.raft_leader().is_some_and(|l| l == self.me.raft_id)
    }

    pub fn raft_voters(&self) -> BTreeSet<RaftNodeId> {
        self.raft_state()
            .map(|r| {
                r.raft
                    .metrics()
                    .borrow()
                    .membership_config
                    .membership()
                    .voter_ids()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Linearizable read: the leader confirms its term with a quorum first, followers ask the
    /// leader (a lagging follower must not answer "who hosts this" with a dead node). Falls back
    /// to the local copy only when no leader answers within the budget.
    pub async fn directory_get_fresh(&self, key: &str) -> Option<Value> {
        const BUDGET: Duration = Duration::from_secs(5);
        const BACKOFF: Duration = Duration::from_millis(200);
        let r = self.raft_state()?;
        let started = Instant::now();
        loop {
            if self.is_raft_leader() {
                // openraft waits for the read index to be applied with no deadline of its own: a
                // leader that lost its quorum mid-change would park every caller for good
                match tokio::time::timeout(RPC_TIMEOUT, r.raft.ensure_linearizable()).await {
                    Ok(Ok(_)) => return r.sm.state.read().await.data.get(key).cloned(),
                    Ok(Err(e)) => tracing::debug!(key, error = %e, "linearizable read failed"),
                    Err(_) => tracing::warn!(key, "linearizable read timed out on the leader"),
                }
            } else if let Some(leader) = self.raft_leader() {
                if let Some(peer) = self.peer_by_raft_id(leader).await {
                    if let Ok(resp) = self
                        .bus_request(&peer, TAG_READ, key.as_bytes(), RPC_TIMEOUT)
                        .await
                    {
                        if let Ok(Ok(v)) = serde_json::from_slice::<
                            std::result::Result<Option<Value>, String>,
                        >(&resp)
                        {
                            return v;
                        }
                    }
                }
            }
            if started.elapsed() + BACKOFF > BUDGET {
                tracing::warn!(
                    key,
                    "directory read: no leader answered; serving local copy"
                );
                return r.sm.state.read().await.data.get(key).cloned();
            }
            tokio::time::sleep(BACKOFF).await;
        }
    }

    /// Follower-forwarded read (`TAG_READ`): answered by the leader after a linearizability check.
    pub(crate) async fn handle_forwarded_read(&self, body: &[u8]) -> Vec<u8> {
        let key = String::from_utf8_lossy(body).to_string();
        let res: std::result::Result<Option<Value>, String> = match self.raft_state() {
            Some(r) if self.is_raft_leader() => {
                match tokio::time::timeout(RPC_TIMEOUT, r.raft.ensure_linearizable()).await {
                    Ok(Ok(_)) => Ok(r.sm.state.read().await.data.get(&key).cloned()),
                    Ok(Err(e)) => Err(format!("linearizable read: {e}")),
                    Err(_) => Err("linearizable read: timed out on the leader".into()),
                }
            }
            Some(_) => Err("not the raft leader".into()),
            None => Err("raft not running".into()),
        };
        serde_json::to_vec(&res).unwrap_or_default()
    }

    /// Local, possibly slightly stale read of the replicated directory.
    pub async fn directory_get(&self, key: &str) -> Option<Value> {
        let r = self.raft_state()?;
        r.sm.state.read().await.data.get(key).cloned()
    }

    pub async fn directory_prefix(&self, prefix: &str) -> Vec<(String, Value)> {
        let Some(r) = self.raft_state() else {
            return Vec::new();
        };
        r.sm.state
            .read()
            .await
            .data
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Replicated write: applied by the leader, forwarded over the bus when we are a follower.
    /// Linearizable directory write. Leader writes locally, followers forward to the leader; an
    /// election window (no leader yet / leader moved) is waited out instead of failing the caller,
    /// since callers act on the verdict (failover, cell hand-off) and retry only on their own slow
    /// cadence.
    pub async fn directory_write(&self, op: DirectoryOp) -> Result<DirectoryResp> {
        const BUDGET: Duration = Duration::from_secs(5);
        const BACKOFF: Duration = Duration::from_millis(200);
        let r = self
            .raft_state()
            .ok_or_else(|| Error::Internal("raft not running (bus disabled)".into()))?;
        let started = Instant::now();
        let mut last = Error::Conflict("raft has no leader yet".into());
        loop {
            if self.is_raft_leader() {
                match tokio::time::timeout(RPC_TIMEOUT, r.raft.client_write(op.clone())).await {
                    Ok(Ok(resp)) => return Ok(resp.data),
                    Ok(Err(e)) => last = Error::Internal(format!("raft write: {e}")),
                    Err(_) => last = Error::Internal("raft write: timed out on the leader".into()),
                }
            } else if let Some(leader) = self.raft_leader() {
                match self.peer_by_raft_id(leader).await {
                    Some(peer) => {
                        let body = serde_json::to_vec(&op)?;
                        match self.bus_request(&peer, TAG_WRITE, &body, RPC_TIMEOUT).await {
                            Ok(resp) => {
                                let res: std::result::Result<DirectoryResp, String> =
                                    serde_json::from_slice(&resp)?;
                                match res {
                                    Ok(v) => return Ok(v),
                                    Err(e) => last = Error::Internal(e),
                                }
                            }
                            Err(e) => last = e,
                        }
                    }
                    None => last = Error::Conflict(format!("raft leader {leader} unknown")),
                }
            }
            if started.elapsed() + BACKOFF > BUDGET {
                return Err(last);
            }
            tracing::debug!(error = %last, "directory write: retrying (election in progress?)");
            tokio::time::sleep(BACKOFF).await;
        }
    }

    /// `peer` is the node the bus connection authenticated: the RPC's vote must be that node's
    /// own (leader or candidate), so no peer can append entries or vote in another's name.
    pub(crate) async fn handle_raft(&self, body: &[u8], peer: Uuid) -> Vec<u8> {
        let Some(r) = self.raft_state() else {
            return b"null".to_vec();
        };
        let Ok(rpc) = serde_json::from_slice::<RaftRpc>(body) else {
            return b"null".to_vec();
        };
        let sender = match &rpc {
            RaftRpc::Append(req) => req.vote.leader_id().voted_for(),
            RaftRpc::Vote(req) => req.vote.leader_id().voted_for(),
            RaftRpc::Snapshot(req) => req.vote.leader_id().voted_for(),
        };
        if sender != Some(raft_id(peer)) {
            tracing::warn!(?sender, peer = %peer, "raft: rpc from a node claiming another raft id; dropped");
            return b"null".to_vec();
        }
        match rpc {
            RaftRpc::Append(req) => serde_json::to_vec(&r.raft.append_entries(req).await),
            RaftRpc::Vote(req) => serde_json::to_vec(&r.raft.vote(req).await),
            RaftRpc::Snapshot(req) => serde_json::to_vec(&r.raft.install_snapshot(req).await),
        }
        .unwrap_or_else(|_| b"null".to_vec())
    }

    pub(crate) async fn handle_forwarded_write(&self, body: &[u8]) -> Vec<u8> {
        let res: std::result::Result<DirectoryResp, String> = match serde_json::from_slice::<
            DirectoryOp,
        >(body)
        {
            Ok(op) if self.is_raft_leader() => match self.raft_state() {
                Some(r) => match tokio::time::timeout(RPC_TIMEOUT, r.raft.client_write(op)).await {
                    Ok(res) => res.map(|x| x.data).map_err(|e| format!("raft write: {e}")),
                    Err(_) => Err("raft write: timed out on the leader".into()),
                },
                None => Err("raft not running".into()),
            },
            Ok(_) => Err("not the raft leader".into()),
            Err(e) => Err(format!("bad write: {e}")),
        };
        serde_json::to_vec(&res).unwrap_or_else(|_| b"null".to_vec())
    }

    /// Bootstrap or, as leader, reconcile raft membership with the live bus peers.
    async fn raft_control_round(&self) {
        let Some(r) = self.raft_state() else { return };
        let (initialized, leader) = {
            let metrics = r.raft.metrics();
            let m = metrics.borrow();
            let initialized = m.membership_config.membership().nodes().next().is_some();
            let leader = m.current_leader;
            (initialized, leader)
        };
        let known: Vec<NodeInfo> = self
            .peers
            .read()
            .await
            .iter()
            .filter(|p| p.id != self.me.id && p.raft_id != 0)
            .cloned()
            .collect();
        let peers: Vec<NodeInfo> = known.iter().filter(|p| p.alive).cloned().collect();
        if !initialized {
            // The oldest live raft-capable node bootstraps; the lease makes it exactly one.
            // Only decide once the peer cache has been loaded (it contains our own row) and we
            // have been registered long enough for simultaneously starting peers to show up.
            let loaded = self.peers.read().await.iter().any(|p| p.id == self.me.id);
            let age = OffsetDateTime::now_utc() - self.me.started_at;
            if !loaded || age < BOOTSTRAP_GRACE {
                return;
            }
            let older = peers.iter().any(|p| p.started_at < self.me.started_at);
            if older {
                return;
            }
            let init_lease = format!("raft-init:{}", self.me.group);
            match self.try_lease(&init_lease, INIT_LEASE_TTL).await {
                Ok(true) => {
                    let mut members = BTreeMap::new();
                    members.insert(self.me.raft_id, BasicNode::new(self.me.bus_addr.clone()));
                    match r.raft.initialize(members).await {
                        Ok(()) => {
                            tracing::info!(raft_id = self.me.raft_id, "raft cluster bootstrapped");
                            let _ = self.release_lease(&init_lease).await;
                        }
                        Err(e) => tracing::debug!(error = %e, "raft initialize"),
                    }
                }
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "raft-init lease failed"),
            }
            return;
        }
        if leader != Some(self.me.raft_id) {
            return;
        }
        let current: BTreeSet<RaftNodeId> = r
            .raft
            .metrics()
            .borrow()
            .membership_config
            .membership()
            .voter_ids()
            .collect();
        // A voter leaves only on a SWIM death verdict (or when its node row is gone): a heartbeat
        // that merely lags under load must not shrink the voter set, otherwise the leader can end
        // up alone in a one-voter config while a live peer keeps the old one and can never win an
        // election or receive entries again.
        let dead: BTreeSet<RaftNodeId> = known
            .iter()
            .filter(|p| p.state == "dead")
            .map(|p| p.raft_id)
            .collect();
        let mut desired: BTreeSet<RaftNodeId> = current
            .iter()
            .copied()
            .filter(|id| {
                *id == self.me.raft_id
                    || (!dead.contains(id) && known.iter().any(|p| p.raft_id == *id))
            })
            .collect();
        desired.extend(peers.iter().map(|p| p.raft_id));
        desired.insert(self.me.raft_id);
        if desired == current {
            return;
        }
        if current.len() > 1 && desired.len() * 2 <= current.len() {
            tracing::warn!(
                ?current,
                ?desired,
                "refusing to shrink raft voters below a majority in one round"
            );
            return;
        }
        for p in peers.iter().filter(|p| !current.contains(&p.raft_id)) {
            if let Err(e) = r
                .raft
                .add_learner(p.raft_id, BasicNode::new(p.bus_addr.clone()), true)
                .await
            {
                tracing::debug!(raft_id = p.raft_id, error = %e, "raft add_learner");
                return;
            }
        }
        match r.raft.change_membership(desired.clone(), false).await {
            Ok(_) => tracing::info!(?desired, "raft membership updated"),
            Err(e) => tracing::debug!(error = %e, "raft change_membership"),
        }
    }
}
