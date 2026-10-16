//! Worlds: one `musubi_mesh::World` per named world on this node, ticked at 30 Hz. Simulators
//! drive it through the server-key HTTP API (lease, spawn, apply, view, events, handover ack);
//! clients subscribe by interest over the socket and receive `WorldDelta`s. A world lives on one
//! home node (directory: `musubi.worlds`); clients on other nodes reach it over the bus
//! (`WorldCmd`), simulators are redirected to the home node's HTTP address.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::{DashMap, DashSet};
use musubi_core::{Error, Result, SessionId};
use musubi_mesh::controller::{Assignment, Controller, ControllerConfig, SimInfo};
use musubi_mesh::{CellId, Delta, Entity, EntityId, HandoverOutcome, Pos, World, WorldConfig};
use musubi_proto::realtime::{WorldComponent, WorldEntity};
use musubi_proto::ServerMessage;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::fanout::Fanout;

pub const TICK_HZ: u64 = 30;
pub const MAX_SIM_EVENTS: usize = 4096;
/// Persistent entities are snapshotted this often (ticks); 30 Hz → every 5 s.
const SNAPSHOT_EVERY: u64 = 150;
/// Controller pass every N ticks (1 s at 30 Hz).
pub const CONTROL_EVERY: u64 = 30;

struct WorldState {
    world: World,
    controller: Controller,
    /// session → interest client id
    subs: HashMap<SessionId, Uuid>,
    /// simulator → pending events (handover requests, inputs, …)
    sim_events: HashMap<Uuid, Vec<Value>>,
    /// Persistent-entity ops since the last journal flush (see `musubi.world_journal`).
    journal: Vec<WorldOp>,
    /// Border entities of cells hosted on other nodes, as last received over the bus.
    remote_ghosts: HashMap<CellId, (std::time::Instant, Vec<Entity>)>,
    /// This shard exists only for cells assigned to this node (it is not the world's home).
    hosted_only: bool,
    /// The world's home node as of shard creation: cells without an explicit host live there.
    home_node: Option<Uuid>,
    /// Cross-node handovers in flight: entity → (target node, attempts so far).
    remote_transit: HashMap<EntityId, (Uuid, u32)>,
    /// Per-target sequence of delivered interactions (this node is the single point of order
    /// for the entities it owns; the counter restarts when an entity changes owner).
    interaction_seq: HashMap<EntityId, u64>,
    /// Spawns waiting for room in a full cell (`soft_cap`), per cell, in arrival order.
    admission: HashMap<CellId, CellQueue>,
}

/// A spawn refused by `soft_cap` that asked to wait; admitted when its cell makes room.
struct Queued {
    ticket: Uuid,
    sim: Uuid,
    pos: Pos,
    components: std::collections::BTreeMap<String, Value>,
    persistent: bool,
    decay_ticks: Option<u64>,
    enqueued_tick: u64,
    /// (position, eta) last told to the simulator, to report only changes.
    reported: (usize, Option<u64>),
}

#[derive(Default)]
struct CellQueue {
    entries: std::collections::VecDeque<Queued>,
    /// Entity count after the previous tick's admissions, to measure departures.
    prev_count: Option<usize>,
    /// Departures per tick, exponentially smoothed; drives the ETA.
    departures: f64,
}

impl CellQueue {
    fn eta_ticks(&self, position: usize) -> Option<u64> {
        (self.departures > 1e-6).then(|| (position as f64 / self.departures).ceil() as u64)
    }
}

/// What a simulator asks to spawn.
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub pos: Pos,
    pub components: Value,
    pub persistent: bool,
    /// Despawn after this many ticks without a write, interaction or handover (litter).
    pub decay_ticks: Option<u64>,
    /// Wait in the cell's admission queue instead of failing on `soft_cap`.
    pub queue: bool,
}

/// A simulator's interaction with an entity it may not own; `at_tick`/`at` let the owner check
/// where the target really was when the actor acted (lag compensation).
#[derive(Debug, Clone)]
pub struct InteractSpec {
    pub kind: String,
    pub payload: Value,
    /// The actor's world tick when it acted.
    pub at_tick: Option<u64>,
    /// Where the actor saw the target at that tick.
    pub at: Option<Pos>,
    /// Distance within which `at` counts as a hit (default 1.0).
    pub tolerance: Option<f64>,
}

/// Result of a spawn that may wait for room.
#[derive(Debug)]
pub enum SpawnOutcome {
    Spawned(WorldEntity),
    Queued {
        ticket: Uuid,
        cell: CellId,
        position: usize,
        eta_ticks: Option<u64>,
    },
}

/// A cross-node handover still unacknowledged after this many re-sends is logged (it keeps going).
const REMOTE_HANDOVER_RETRIES: u32 = 5;

/// Remote ghosts older than this are stale (their host stopped publishing or moved).
const REMOTE_GHOST_TTL: std::time::Duration = std::time::Duration::from_secs(2);

const JOURNAL_EVERY: u64 = 5;

/// One journaled mutation of a persistent entity; replayed on top of a snapshot when a world is
/// adopted by another node, so a rehome loses at most `JOURNAL_EVERY` ticks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldOp {
    pub tick: u64,
    pub op: WorldMutation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorldMutation {
    Spawn {
        id: Uuid,
        pos: Pos,
        components: Value,
        #[serde(default)]
        decay_ticks: Option<u64>,
    },
    Despawn {
        id: Uuid,
    },
    Apply {
        id: Uuid,
        pos: Option<Pos>,
        components: Value,
    },
}

/// Replays `node`'s journal over a world merged from several shards. `src` remembers which node's
/// snapshot or Spawn produced each entity's current copy: a Despawn/Apply from another node is a
/// stale "it left my shard" record and must not touch a copy that came from elsewhere.
fn replay(
    world: &mut World,
    ops: &[WorldOp],
    from_tick: u64,
    node: Uuid,
    src: &mut HashMap<musubi_mesh::EntityId, Uuid>,
) -> usize {
    let mut n = 0;
    // ops at the snapshot's own tick may or may not be in it; every op is idempotent, so replay them
    for o in ops.iter().filter(|o| o.tick >= from_tick) {
        let comps = |v: &Value| -> std::collections::BTreeMap<String, Value> {
            v.as_object()
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        match &o.op {
            WorldMutation::Spawn {
                id,
                pos,
                components,
                decay_ticks,
            } => {
                if world.get(*id).is_none() {
                    world.spawn_with_id(*id, *pos, comps(components), true);
                    world.set_decay(*id, *decay_ticks);
                    src.insert(*id, node);
                }
            }
            WorldMutation::Despawn { id } => {
                if src.get(id).is_none_or(|s| *s == node) {
                    world.despawn(*id);
                    src.remove(id);
                }
            }
            WorldMutation::Apply {
                id,
                pos,
                components,
            } => {
                if src.get(id).is_none_or(|s| *s == node) {
                    let _ = world.force_apply(*id, *pos, comps(components));
                }
            }
        }
        if o.tick > world.tick {
            world.tick = o.tick;
        }
        n += 1;
    }
    n
}

pub struct WorldRegistry {
    node_id: Uuid,
    /// `node.cluster_group`: worlds of other clusters sharing the database are invisible here.
    group: String,
    db: Option<PgPool>,
    worlds: DashMap<String, Arc<Mutex<WorldState>>>,
    /// world → home node (cache of `musubi.worlds`)
    homes: DashMap<String, Uuid>,
    /// Fires after every registry tick with the tick number; streaming simulator sessions wake on it.
    ticks: tokio::sync::broadcast::Sender<u64>,
    tick_no: std::sync::atomic::AtomicU64,
    /// Raft directory (`world:<name>` → home) when the cluster runs the QUIC bus.
    cluster: std::sync::OnceLock<Arc<musubi_cluster::Cluster>>,
    /// Outbox for `world.rehomed`.
    events: std::sync::OnceLock<Arc<crate::events::Events>>,
    /// (world, cell) → host node, cached from the directory.
    cell_hosts: DashMap<(String, CellId), Uuid>,
    /// (world, cell, previous host) shards already merged after a hand-back, so a later
    /// reconciliation round does not resurrect entities despawned since.
    adopted_from: DashSet<(String, CellId, Uuid)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorldCmd {
    Subscribe {
        x: f64,
        y: f64,
        radius: f64,
        #[serde(default)]
        budget_bytes: Option<u32>,
        #[serde(default)]
        filter: Option<Value>,
    },
    Unsubscribe,
    Input {
        entity: String,
        input: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldCmdEnv {
    pub world: String,
    pub session_id: SessionId,
    pub cmd: WorldCmd,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldInfo {
    pub name: String,
    pub tick: u64,
    pub entities: usize,
    pub subscribers: usize,
    pub cells: Vec<(CellId, Uuid)>,
    pub orphaned: Vec<CellId>,
    pub sims: Vec<SimInfo>,
    pub config: WorldConfig,
}

pub fn to_wire(e: &Entity) -> WorldEntity {
    WorldEntity {
        id: e.id.to_string(),
        x: e.pos.x,
        y: e.pos.y,
        components: e
            .components
            .iter()
            .map(|(k, c)| WorldComponent {
                name: k.clone(),
                value: c.value.clone(),
                version: c.version,
            })
            .collect(),
    }
}

fn parse_entity(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad entity id".into()))
}

fn mesh_err(e: musubi_mesh::MeshError) -> Error {
    use musubi_mesh::MeshError::*;
    match e {
        NoEntity => Error::NotFound("entity".into()),
        NoLease(c) => Error::Conflict(format!("cell {c:?} has no live lease")),
        NotAuthoritative(m) => Error::PermissionDenied(m),
        InTransit => Error::Conflict("entity in transit".into()),
        CellFull(c, cap) => {
            Error::ResourceExhausted(format!("cell {c:?} is full (soft_cap {cap})"))
        }
    }
}

impl WorldRegistry {
    pub fn new(node_id: Uuid, db: Option<PgPool>) -> Self {
        Self {
            node_id,
            group: "default".into(),
            db,
            worlds: DashMap::new(),
            homes: DashMap::new(),
            ticks: tokio::sync::broadcast::channel(16).0,
            tick_no: std::sync::atomic::AtomicU64::new(0),
            cell_hosts: DashMap::new(),
            adopted_from: DashSet::new(),
            cluster: std::sync::OnceLock::new(),
            events: std::sync::OnceLock::new(),
        }
    }

    pub fn set_events(&self, events: Arc<crate::events::Events>) {
        let _ = self.events.set(events);
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = group.to_string();
        self
    }

    pub fn set_cluster(&self, cluster: Arc<musubi_cluster::Cluster>) {
        let _ = self.cluster.set(cluster);
    }

    fn directory_key(name: &str) -> String {
        format!("world:{name}")
    }

    /// Directory key of one cell's host node: cells of a world may live on different nodes.
    fn cell_key(name: &str, cell: CellId) -> String {
        format!("world:{name}:cell:{},{}", cell.x, cell.y)
    }

    /// Record (or clear) which node hosts `cell` of `name`.
    /// The directory (Raft) is the fence: without a successful write nothing changes locally,
    /// so a node cut off from the quorum cannot take cells for itself.
    pub async fn publish_cell_host(
        &self,
        name: &str,
        cell: CellId,
        host: Option<Uuid>,
    ) -> Result<()> {
        if let Some(c) = self.cluster.get() {
            if c.raft_id() != 0 {
                let key = Self::cell_key(name, cell);
                let op = match host {
                    Some(h) => musubi_cluster::DirectoryOp::Set {
                        key,
                        value: serde_json::json!({ "node_id": h }),
                    },
                    None => musubi_cluster::DirectoryOp::Delete { key },
                };
                // without a Raft leader a write would wait indefinitely; failover must not hang on it
                match tokio::time::timeout(Duration::from_secs(5), c.directory_write(op)).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        return Err(Error::Unavailable(format!(
                            "cell directory write failed: {e}"
                        )))
                    }
                    Err(_) => {
                        return Err(Error::Unavailable(
                            "cell directory write timed out (no raft leader?)".into(),
                        ))
                    }
                }
            }
        }
        match host {
            Some(h) => {
                self.cell_hosts.insert((name.to_string(), cell), h);
            }
            None => {
                self.cell_hosts.remove(&(name.to_string(), cell));
            }
        }
        Ok(())
    }

    /// `cell` now belongs to `node`: if this node still holds entities of it (the cell was moved
    /// away, or we were wrongly declared dead), ship them to the new host and drop the local
    /// simulator's lease on the cell. Returns how many entities left.
    pub fn release_cell(&self, name: &str, cell: CellId, node: Uuid, fanout: &Fanout) -> usize {
        let Some(w) = self.worlds.get(name) else {
            return 0;
        };
        let mut st = w.lock();
        let st = &mut *st;
        let owned = st.world.view(cell).owned;
        let n = owned.len();
        for mut e in owned {
            let id = e.id;
            tracing::debug!(world = name, %id, %node, "release: entity shipped");
            let inputs = st.world.take_buffered_inputs(id);
            st.world.forget_for_clients(id);
            st.world.despawn(id);
            e.transit = None;
            fanout.world_handover(name, e, self.node_id, inputs, true);
        }
        if let Some(sim) = st.world.revoke_lease(cell) {
            push_event(
                st,
                sim,
                json!({"type": "cell_released", "cell": cell, "node": node, "entities": n}),
            );
        }
        if n > 0 {
            tracing::info!(world = name, ?cell, %node, entities = n, "cell released to its new host");
        }
        n
    }

    /// A hosted-only shard that gave up its last cell must stop existing here: left ticking, it
    /// would flush its journal (Despawns of the entities just handed back) and an empty periodic
    /// snapshot after the new host merged and pruned our rows, and the next adoption of the world
    /// would replay those Despawns over the fresh state.
    pub fn drop_idle_shard(&self, name: &str) -> bool {
        let idle = match self.worlds.get(name) {
            Some(w) => {
                w.lock().hosted_only
                    && !self
                        .cell_hosts
                        .iter()
                        .any(|r| r.key().0 == name && *r.value() == self.node_id)
            }
            None => false,
        };
        if idle {
            if let Some((_, w)) = self.worlds.remove(name) {
                w.lock().journal.clear();
            }
            self.homes.remove(name);
            tracing::info!(world = name, "idle hosted shard dropped");
        }
        idle
    }

    /// Cell containing `pos` under this world's grid (needs a local copy of the world).
    pub fn cell_of(&self, name: &str, pos: Pos) -> Result<CellId> {
        let w = self.get(name)?;
        let st = w.lock();
        Ok(st.world.cell_of(pos))
    }

    /// Remember another node's cell assignment (no directory write; the coordinator did that).
    pub fn note_cell_host(&self, name: &str, cell: CellId, node: Uuid) {
        self.cell_hosts.insert((name.to_string(), cell), node);
    }

    /// This node takes `cell` of `name`: make sure a local shard of the world exists (config from
    /// `musubi.worlds`, defaults without a database); simulators lease the cell here as usual.
    pub async fn host_cell(&self, name: &str, cell: CellId) -> Result<Vec<CellId>> {
        let home = self.home_of(name).await?;
        if !self.worlds.contains_key(name) {
            let (cfg, ctl) = match &self.db {
                Some(db) => {
                    let row: Option<(Value, Value)> = sqlx::query_as(
                        "select config, controller from musubi.worlds where name = $1 and cluster_group = $2",
                    )
                    .bind(name)
                    .bind(&self.group)
                    .fetch_optional(db)
                    .await
                    .map_err(Error::internal)?;
                    let (c, k) = row.ok_or_else(|| Error::NotFound(format!("world {name}")))?;
                    (
                        serde_json::from_value(c).map_err(Error::internal)?,
                        serde_json::from_value(k).map_err(Error::internal)?,
                    )
                }
                None => (WorldConfig::default(), ControllerConfig::default()),
            };
            self.worlds.entry(name.to_string()).or_insert_with(|| {
                Arc::new(Mutex::new(WorldState {
                    world: World::new(cfg),
                    controller: Controller::new(ctl),
                    subs: HashMap::new(),
                    sim_events: HashMap::new(),
                    journal: Vec::new(),
                    remote_ghosts: HashMap::new(),
                    hosted_only: true,
                    home_node: home,
                    remote_transit: HashMap::new(),
                    interaction_seq: HashMap::new(),
                    admission: HashMap::new(),
                }))
            });
        }
        self.cell_hosts
            .insert((name.to_string(), cell), self.node_id);
        Ok(vec![cell])
    }

    /// Boot: cells the directory still assigns to this node (we restarted before the home
    /// re-homed them) go back to their home, which merges our persisted shard from the database.
    /// Resuming them from memory would race the home's dead-host recovery into two live owners.
    pub async fn recover_hosted(&self, fanout: &Fanout) -> Result<Vec<(String, CellId)>> {
        let Some(c) = self.cluster.get() else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for (key, v) in c.directory_prefix("world:").await {
            let Some((name, rest)) = key
                .strip_prefix("world:")
                .and_then(|k| k.split_once(":cell:"))
            else {
                continue;
            };
            let Some((x, y)) = rest.split_once(',') else {
                continue;
            };
            let (Ok(x), Ok(y)) = (x.parse::<i32>(), y.parse::<i32>()) else {
                continue;
            };
            if v["node_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) != Some(self.node_id) {
                continue;
            }
            let cell = CellId { x, y };
            let home = match self.home_of(name).await {
                Ok(Some(h)) => h,
                _ => continue,
            };
            let target = (home != self.node_id).then_some(home);
            if let Err(e) = self.publish_cell_host(name, cell, target).await {
                tracing::warn!(world = name, ?cell, error = %e, "boot: hosted cell not handed back");
                continue;
            }
            if let Some(h) = target {
                fanout.cell_assign(name, cell, h);
                // a fresh process has no cached assignment, so the bus handler would not
                // recognise us as the previous host: ship and drop the shard here, or it keeps
                // ticking and persisting stale rows the next adoption of the world replays
                self.release_cell(name, cell, h, fanout);
                self.note_cell_host(name, cell, h);
                self.drop_idle_shard(name);
            } else {
                self.cell_hosts.remove(&(name.to_string(), cell));
            }
            tracing::info!(world = name, ?cell, home = %home, "boot: hosted cell handed back");
            out.push((name.to_string(), cell));
        }
        Ok(out)
    }

    /// Cached host of `cell` (no directory lookup).
    pub fn cell_host_cached(&self, name: &str, cell: CellId) -> Option<Uuid> {
        self.cell_hosts.get(&(name.to_string(), cell)).map(|h| *h)
    }

    /// A cell just moved to us from `prev`: its persisted state (snapshot + journal of that
    /// node's shard) merges into our copy — the forced handovers over the bus are the fast path,
    /// this is the one that cannot drop entities.
    pub async fn adopt_cell_from(&self, name: &str, cell: CellId, prev: Uuid) -> Result<usize> {
        let Some(db) = self.db.clone() else {
            return Ok(0);
        };
        let w = self.get(name)?;
        let cfg = w.lock().world.config().clone();
        let mut tmp = World::new(cfg);
        let replayed = Self::load_shards(&db, name, &[prev], true, &mut tmp).await?;
        let ents = tmp.snapshot_persistent();
        let n = {
            let mut st = w.lock();
            let ents: Vec<_> = ents
                .into_iter()
                .filter(|e| st.world.cell_of(e.pos) == cell && st.world.get(e.id).is_none())
                .collect();
            let n = ents.len();
            st.world.restore(ents);
            n
        };
        self.adopted_from.insert((name.to_string(), cell, prev));
        if n > 0 || replayed > 0 {
            tracing::info!(world = name, ?cell, %prev, entities = n, ops = replayed, "cell adopted from previous host");
        }
        let mut keep = self.assigned_hosts(name).await;
        if !keep.contains(&prev) {
            keep.push(self.node_id);
            if let Err(e) = self.prune_shards(name, &keep).await {
                tracing::warn!(world = name, %prev, error = %e, "stale shard rows not pruned");
            }
        }
        Ok(n)
    }

    /// The cell left us: a future hand-back from the same node must merge again.
    pub fn forget_adoptions(&self, name: &str, cell: CellId) {
        self.adopted_from
            .retain(|(n, c, _)| !(n == name && *c == cell));
    }

    /// Re-read the directory home of every world we only host cells of (the home moves on
    /// failover and our handovers into unassigned cells must follow it).
    pub async fn refresh_homes(&self) {
        let hosted: Vec<String> = self
            .worlds
            .iter()
            .filter(|w| w.value().lock().hosted_only)
            .map(|w| w.key().clone())
            .collect();
        for name in hosted {
            if self.home_fresh(&name).await.is_none() {
                let _ = self.home_of(&name).await;
            }
        }
    }

    /// Home side of a hand-back that did not reach us over the bus: every cell the directory
    /// assigns to us on a home world merges the persisted shard of any node that is not hosting
    /// this world any more (once per hand-back).
    pub async fn reconcile_handed_back(&self) -> Result<usize> {
        let Some(db) = self.db.clone() else {
            return Ok(0);
        };
        let mine: Vec<(String, CellId)> = self
            .cell_hosts
            .iter()
            .filter(|e| *e.value() == self.node_id)
            .map(|e| e.key().clone())
            .collect();
        let mut merged = 0;
        for (name, cell) in mine {
            let is_home = self
                .worlds
                .get(&name)
                .is_some_and(|w| !w.lock().hosted_only);
            if !is_home {
                continue;
            }
            let rows: Vec<(Uuid,)> = sqlx::query_as(
                "select node_id from musubi.world_snapshots where name = $1 and node_id <> $2",
            )
            .bind(&name)
            .bind(self.node_id)
            .fetch_all(&db)
            .await
            .map_err(Error::internal)?;
            for (prev,) in rows {
                let still_hosts = self
                    .cell_hosts
                    .iter()
                    .any(|e| e.key().0 == name && *e.value() == prev);
                if still_hosts || self.adopted_from.contains(&(name.clone(), cell, prev)) {
                    continue;
                }
                match self.adopt_cell_from(&name, cell, prev).await {
                    Ok(n) => merged += n,
                    Err(e) => {
                        tracing::warn!(world = %name, ?cell, %prev, error = %e, "hand-back not merged")
                    }
                }
            }
        }
        Ok(merged)
    }

    /// Node hosting `cell`: an explicit assignment wins, otherwise the world's home node.
    pub async fn cell_host_of(&self, name: &str, cell: CellId) -> Result<Option<Uuid>> {
        let key = (name.to_string(), cell);
        if let Some(c) = self.cluster.get() {
            if c.raft_id() != 0 {
                // the replicated directory is the truth; the cache only mirrors it (a bus
                // announcement can be lost, an unassignment is never announced)
                let h = c
                    .directory_get(&Self::cell_key(name, cell))
                    .await
                    .and_then(|v| v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()));
                return match h {
                    Some(h) => {
                        self.cell_hosts.insert(key, h);
                        Ok(Some(h))
                    }
                    None => {
                        // our own fresh assignment may not have reached this replica yet
                        if self
                            .cell_hosts
                            .get(&key)
                            .is_some_and(|h| *h != self.node_id)
                        {
                            self.cell_hosts.remove(&key);
                        }
                        self.home_of(name).await
                    }
                };
            }
        }
        if let Some(h) = self.cell_hosts.get(&key) {
            return Ok(Some(*h));
        }
        self.home_of(name).await
    }

    /// Publish the world's home to the replicated directory (best effort; Postgres stays truth).
    async fn publish_home(&self, name: &str, home: Option<Uuid>) {
        let Some(c) = self.cluster.get() else { return };
        if c.raft_id() == 0 {
            return;
        }
        let key = Self::directory_key(name);
        let op = match home {
            Some(h) => musubi_cluster::DirectoryOp::Set {
                key,
                value: serde_json::json!({ "node_id": h }),
            },
            None => musubi_cluster::DirectoryOp::Delete { key },
        };
        if let Err(e) = c.directory_write(op).await {
            tracing::warn!(world = name, error = %e, "world directory write failed");
        }
    }

    /// Snapshot of this node's shard of `name` (cells of a world may live on several nodes, each
    /// writes its own row) and the journal chunks it supersedes.
    async fn write_snapshot(
        db: &PgPool,
        name: &str,
        node: Uuid,
        entities: Vec<musubi_mesh::Entity>,
        tick: u64,
    ) -> Result<()> {
        sqlx::query(
            "insert into musubi.world_snapshots (name, node_id, entities, tick, updated_at) values ($1, $2, $3, $4, now())
             on conflict (name, node_id) do update set entities = excluded.entities, tick = excluded.tick, updated_at = now()",
        )
        .bind(name)
        .bind(node)
        .bind(serde_json::to_value(&entities)?)
        .bind(tick as i64)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.world_history (name, node_id, tick, entities) values ($1, $2, $3, $4)
             on conflict (name, node_id, tick) do nothing",
        )
        .bind(name)
        .bind(node)
        .bind(tick as i64)
        .bind(serde_json::to_value(&entities)?)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        // the journal is kept back to the oldest retained history row, so the world can be rebuilt
        // as of any tick in between (`entities_at`)
        sqlx::query(
            "delete from musubi.world_journal where name = $1 and (node_id = $2 or node_id is null)
             and tick_to < (select coalesce(min(tick), $3) from musubi.world_history where name = $1 and node_id = $2)",
        )
        .bind(name)
        .bind(node)
        .bind(tick as i64)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Plan 4.9 time machine: the world's persistent entities as of the end of `tick`, rebuilt from each
    /// shard's newest history snapshot at or before it plus the journal ops up to it (a shard whose
    /// journal was pruned because it left the world contributes its last snapshot only).
    pub async fn entities_at(&self, name: &str, tick: u64) -> Result<Value> {
        let Some(db) = &self.db else {
            return Err(Error::Internal(
                "no database: world history is not kept".into(),
            ));
        };
        let range: Option<(Option<i64>, Option<i64>)> =
            sqlx::query_as("select min(tick), max(tick) from musubi.world_history where name = $1")
                .bind(name)
                .fetch_optional(db)
                .await
                .map_err(Error::internal)?;
        let (oldest, newest) = match range {
            Some((Some(a), Some(b))) => (a as u64, b as u64),
            _ => {
                return Err(Error::NotFound(format!(
                    "world {name}: no history snapshots yet"
                )))
            }
        };
        if tick < oldest {
            return Err(Error::NotFound(format!(
                "world {name}: history starts at tick {oldest}, asked for {tick}"
            )));
        }
        let snaps: Vec<(Uuid, Value, i64)> = sqlx::query_as(
            "select distinct on (node_id) node_id, entities, tick from musubi.world_history
             where name = $1 and tick <= $2 order by node_id, tick desc",
        )
        .bind(name)
        .bind(tick as i64)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let cfg = self
            .worlds
            .get(name)
            .map(|w| w.lock().world.cfg.clone())
            .unwrap_or_default();
        let mut world = World::new(cfg);
        let mut since: HashMap<Uuid, u64> = HashMap::new();
        let mut src: HashMap<musubi_mesh::EntityId, Uuid> = HashMap::new();
        let mut from = Vec::new();
        for (node, ents, t) in snaps {
            let ents: Vec<musubi_mesh::Entity> = serde_json::from_value(ents)?;
            for e in &ents {
                src.insert(e.id, node);
            }
            world.restore(ents);
            since.insert(node, t as u64);
            from.push(json!({"node_id": node, "tick": t}));
        }
        let rows: Vec<(Option<Uuid>, Value)> = sqlx::query_as(
            "select node_id, entries from musubi.world_journal where name = $1 and tick_from <= $2 order by tick_from",
        )
        .bind(name)
        .bind(tick as i64)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut by_node: HashMap<Uuid, Vec<WorldOp>> = HashMap::new();
        for (node, v) in rows {
            if let Ok(mut o) = serde_json::from_value::<Vec<WorldOp>>(v) {
                by_node
                    .entry(node.unwrap_or(Uuid::nil()))
                    .or_default()
                    .append(&mut o);
            }
        }
        let mut replayed = 0;
        for (node, mut ops) in by_node {
            ops.retain(|o| o.tick <= tick);
            ops.sort_by_key(|o| o.tick);
            let from_tick = since.get(&node).copied().unwrap_or(0);
            replayed += replay(&mut world, &ops, from_tick, node, &mut src);
        }
        Ok(json!({
            "world": name, "tick": tick,
            "history": {"oldest_tick": oldest, "newest_tick": newest},
            "from_snapshots": from, "replayed": replayed,
            "entities": world.snapshot_persistent(),
        }))
    }

    /// Entities of the shards of `name`: with `only` the shards kept by `nodes` (plus legacy
    /// whole-world rows), otherwise every shard except those of `nodes` (live cell hosts). The
    /// journal ops written after each shard's snapshot are replayed on top.
    async fn load_shards(
        db: &PgPool,
        name: &str,
        nodes: &[Uuid],
        only: bool,
        world: &mut World,
    ) -> Result<usize> {
        let mut wanted: Vec<Uuid> = nodes.to_vec();
        if only {
            wanted.push(Uuid::nil());
        }
        let snaps: Vec<(Uuid, Value, i64)> = sqlx::query_as(if only {
            "select node_id, entities, tick from musubi.world_snapshots where name = $1 and node_id = any($2) order by updated_at"
        } else {
            "select node_id, entities, tick from musubi.world_snapshots where name = $1 and node_id <> all($2) order by updated_at"
        })
        .bind(name)
        .bind(&wanted)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut since: HashMap<Uuid, u64> = HashMap::new();
        let mut src: HashMap<musubi_mesh::EntityId, Uuid> = HashMap::new();
        for (node, ents, tick) in snaps {
            let ents: Vec<musubi_mesh::Entity> = serde_json::from_value(ents)?;
            for e in &ents {
                src.insert(e.id, node);
            }
            world.restore(ents);
            world.tick = world.tick.max(tick as u64);
            since.insert(node, tick as u64);
        }
        let rows: Vec<(Option<Uuid>, Value)> = sqlx::query_as(if only {
            "select node_id, entries from musubi.world_journal where name = $1 and (node_id = any($2) or node_id is null) order by tick_from"
        } else {
            "select node_id, entries from musubi.world_journal where name = $1 and (node_id <> all($2) or node_id is null) order by tick_from"
        })
        .bind(name)
        .bind(&wanted)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut by_node: HashMap<Uuid, Vec<WorldOp>> = HashMap::new();
        for (node, v) in rows {
            match serde_json::from_value::<Vec<WorldOp>>(v) {
                Ok(mut o) => by_node
                    .entry(node.unwrap_or(Uuid::nil()))
                    .or_default()
                    .append(&mut o),
                Err(e) => {
                    tracing::warn!(world = %name, error = %e, "bad world journal chunk skipped")
                }
            }
        }
        let mut replayed = 0;
        for (node, mut ops) in by_node {
            // chunks arrive in tick order, ops inside a chunk may not: a Despawn replayed after
            // the Spawn that came back later would erase the entity
            ops.sort_by_key(|o| o.tick);
            let from = since.get(&node).copied().unwrap_or(0);
            replayed += replay(world, &ops, from, node, &mut src);
        }
        Ok(replayed)
    }

    /// Write the world's persistent entities to the snapshot store now (the ticker also does it
    /// every `SNAPSHOT_EVERY` ticks).
    /// Shard rows of nodes that no longer hold any part of `name` were merged into this node's
    /// state: persist that state first, then drop them so a later recovery cannot resurrect
    /// stale entities (journal rows without a node are legacy single-node chunks).
    async fn prune_shards(&self, name: &str, keep: &[Uuid]) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        self.snapshot_now(name).await?;
        sqlx::query("delete from musubi.world_snapshots where name = $1 and node_id <> all($2)")
            .bind(name)
            .bind(keep)
            .execute(db)
            .await
            .map_err(Error::internal)?;
        sqlx::query("delete from musubi.world_journal where name = $1 and node_id <> all($2)")
            .bind(name)
            .bind(keep)
            .execute(db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Nodes the directory currently assigns cells of `name` to.
    fn epoch(&self) -> i64 {
        self.cluster.get().map(|c| c.epoch()).unwrap_or(0)
    }

    async fn assigned_hosts(&self, name: &str) -> Vec<Uuid> {
        let Some(c) = self.cluster.get() else {
            return vec![];
        };
        // confirmed through the leader: the local copy may still list a host that just gave the
        // cell up, and keeping its rows means replaying them over fresher state later
        let mut out = Vec::new();
        for (key, _) in c.directory_prefix(&format!("world:{name}:cell:")).await {
            if let Some(h) = c
                .directory_get_fresh(&key)
                .await
                .and_then(|v| v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()))
            {
                out.push(h);
            }
        }
        out
    }

    pub async fn snapshot_now(&self, name: &str) -> Result<u64> {
        let (entities, tick) = {
            let w = self.get(name)?;
            let st = w.lock();
            (st.world.snapshot_persistent(), st.world.tick)
        };
        if let Some(db) = &self.db {
            Self::write_snapshot(db, name, self.node_id, entities, tick).await?;
        }
        Ok(tick)
    }

    /// Adopt worlds whose home node is dead (or gone from the node table): claim the directory row
    /// atomically, rebuild the world from its config and the last snapshot. Returns adopted names.
    pub async fn recover(&self, dead_nodes: &[Uuid]) -> Result<Vec<String>> {
        let Some(db) = &self.db else {
            return Ok(vec![]);
        };
        let rows: Vec<(String, Value, Value)> = sqlx::query_as(
            "update musubi.worlds w set node_id = $2, node_epoch = $4
             where w.cluster_group = $3
               and (w.region = '' or w.region = $5 or not exists
                    (select 1 from musubi.nodes n where n.region = w.region
                       and n.heartbeat_at > now() - interval '10 seconds'))
               and (w.node_id is null or w.node_id = any($1)
                    or not exists (select 1 from musubi.nodes n where n.id = w.node_id)
                    or exists (select 1 from musubi.nodes n where n.id = w.node_id and n.epoch > w.node_epoch
                                 and n.started_at < now() - interval '10 seconds'))
               and (w.node_id is null or not (w.node_id = $2 and $2 <> all($1) and w.node_epoch = $4))
               and (w.node_id is null or w.node_id = $2 or not exists
                    (select 1 from musubi.nodes n where n.id = w.node_id
                       and n.heartbeat_at > now() - interval '10 seconds'
                       and (n.epoch <= w.node_epoch or n.started_at > now() - interval '10 seconds')))
             returning name, config, controller",
        )
        .bind(dead_nodes)
        .bind(self.node_id)
        .bind(&self.group)
        .bind(self.epoch())
        .bind(self.region())
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        // shards of hosts that left the cluster are persisted, not live: load them too
        let alive: Option<std::collections::HashSet<Uuid>> = match self.cluster.get() {
            Some(c) => c
                .nodes()
                .await
                .ok()
                .map(|ns| ns.iter().filter(|n| n.alive).map(|n| n.id.0).collect()),
            None => None,
        };
        let alive = &alive;
        // which nodes host cells of each world, by the replicated directory (the local cache
        // can lag or hold an assignment the host has since given up)
        let mut assigned: HashMap<String, Vec<Uuid>> = HashMap::new();
        if let Some(c) = self.cluster.get() {
            for (key, v) in c.directory_prefix("world:").await {
                if let Some((n, _)) = key
                    .strip_prefix("world:")
                    .and_then(|k| k.split_once(":cell:"))
                {
                    let Some(h) = v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok())
                    else {
                        continue;
                    };
                    // a host counted as live keeps its shard out of the restore, so a stale local
                    // copy of the directory (the drained node deleted its self-assignment on the
                    // leader a moment ago) must not hide its fresh snapshot: confirm linearizably
                    let fresh = match alive {
                        Some(a) if a.contains(&h) && !dead_nodes.contains(&h) => {
                            c.directory_get_fresh(&key).await.and_then(|v| {
                                v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok())
                            })
                        }
                        _ => Some(h),
                    };
                    match fresh {
                        Some(h) => assigned.entry(n.to_string()).or_default().push(h),
                        None => {
                            tracing::info!(key, "stale cell assignment skipped during adoption")
                        }
                    }
                }
            }
        }
        let assigned = &assigned;
        let mut out = Vec::new();
        // adopt concurrently: each world costs a snapshot + journal read and a Raft directory
        // write, and a dead node may have owned many
        let adopted = futures::future::join_all(rows.into_iter().map(|(name, cfg, ctl)| async move {
            let is_live = |h: &Uuid| match alive {
                Some(a) => a.contains(h),
                None => true,
            };
            if let Some(w) = self.worlds.get(&name) {
                if !w.lock().hosted_only {
                    return Ok(None);
                }
                // we host some of its cells: promote the shard to home — the dead home's
                // (and other dead hosts') persisted cells merge into it, ours stay live
                let cfg: WorldConfig = serde_json::from_value(cfg)?;
                let mut live: Vec<Uuid> = assigned.get(&name).cloned().unwrap_or_default().into_iter().filter(|h| !dead_nodes.contains(h) && is_live(h)).collect();
                if !dead_nodes.contains(&self.node_id) {
                    live.push(self.node_id);
                }
                let mut tmp = World::new(cfg);
                let replayed = Self::load_shards(db, &name, &live, false, &mut tmp).await?;
                let (ents, tick) = (tmp.snapshot_persistent(), tmp.tick);
                {
                    let mut st = w.lock();
                    let ents: Vec<_> = ents
                        .into_iter()
                        .filter(|e| st.world.get(e.id).is_none())
                        .collect();
                    let n = ents.len();
                    st.world.restore(ents);
                    st.hosted_only = false;
                    st.home_node = None;
                    tracing::info!(world = %name, merged = n, ops = replayed, home_tick = tick, "hosted shard promoted to home");
                }
                self.homes.insert(name.clone(), self.node_id);
                self.publish_home(&name, Some(self.node_id)).await;
                if let Some(ev) = self.events.get() {
                    ev.emit(
                        "world.rehomed",
                        None,
                        serde_json::json!({ "name": name, "node_id": self.node_id }),
                    )
                    .await;
                }
                return Ok(Some(name));
            }
            let cfg: WorldConfig = serde_json::from_value(cfg)?;
            let ctl: ControllerConfig = serde_json::from_value(ctl)?;
            let mut world = World::new(cfg);
            // shards of cell hosts that are still alive keep living there
            let live: Vec<Uuid> = assigned.get(&name).cloned().unwrap_or_default().into_iter().filter(|h| !dead_nodes.contains(h) && is_live(h)).collect();
            let replayed = Self::load_shards(db, &name, &live, false, &mut world).await?;
            if replayed > 0 {
                tracing::info!(world = %name, ops = replayed, tick = world.tick, "world journal replayed");
            }
            self.worlds.insert(
                name.clone(),
                Arc::new(Mutex::new(WorldState {
                    world,
                    controller: Controller::new(ctl),
                    subs: HashMap::new(),
                    sim_events: HashMap::new(),
                    journal: Vec::new(),
                remote_ghosts: HashMap::new(),
                    hosted_only: false,
                    home_node: None,
                    remote_transit: HashMap::new(),
            interaction_seq: HashMap::new(),
            admission: HashMap::new(),
                })),
            );
            self.homes.insert(name.clone(), self.node_id);
            self.publish_home(&name, Some(self.node_id)).await;
            let mut keep = live.clone();
            keep.push(self.node_id);
            if let Err(e) = self.prune_shards(&name, &keep).await {
                tracing::warn!(world = %name, error = %e, "stale shard rows not pruned");
            }
            if let Some(ev) = self.events.get() {
                ev.emit(
                    "world.rehomed",
                    None,
                    serde_json::json!({ "name": name, "node_id": self.node_id }),
                )
                .await;
            }
            Ok::<Option<String>, Error>(Some(name))
        }))
        .await;
        for r in adopted {
            if let Some(name) = r? {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Drain: cells this node hosts go back to their worlds' homes — directory first, then the
    /// entities (forced handover; the home takes them even without a simulator on the cell).
    pub async fn release_hosted(&self, fanout: &Fanout) -> Vec<(String, CellId)> {
        let mine: Vec<(String, CellId)> = self
            .cell_hosts
            .iter()
            .filter(|e| *e.value() == self.node_id)
            .map(|e| e.key().clone())
            .collect();
        let mut out = Vec::new();
        for (name, cell) in mine {
            let cached = match self.home_fresh(&name).await {
                Some(h) => Some(h),
                None => self.worlds.get(&name).and_then(|w| w.lock().home_node),
            };
            let home = match cached {
                Some(h) if h != self.node_id => h,
                _ => match self.home_of(&name).await {
                    Ok(Some(h)) if h != self.node_id => h,
                    _ => {
                        // we are its home as well (promoted shard): the assignment goes away and
                        // the full home snapshot carries the cell to the next home
                        if let Err(e) = self.publish_cell_host(&name, cell, None).await {
                            tracing::warn!(world = %name, ?cell, error = %e, "drain: cell not unassigned");
                            continue;
                        }
                        self.cell_hosts.remove(&(name.clone(), cell));
                        out.push((name, cell));
                        continue;
                    }
                },
            };
            if let Err(e) = self.snapshot_now(&name).await {
                tracing::warn!(world = %name, error = %e, "drain: shard snapshot failed");
            }
            if let Err(e) = self.publish_cell_host(&name, cell, Some(home)).await {
                tracing::warn!(world = %name, ?cell, error = %e, "drain: cell not handed back");
                continue;
            }
            fanout.cell_assign(&name, cell, home);
            self.release_cell(&name, cell, home, fanout);
            self.note_cell_host(&name, cell, home);
            self.drop_idle_shard(&name);
            out.push((name, cell));
        }
        if !out.is_empty() {
            // give the bus a moment to deliver the handovers before the node leaves
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        out
    }

    /// Drain: snapshot every world we host and disown it so the failover leader re-homes it.
    pub async fn release_all(&self) -> Result<Vec<String>> {
        let Some(db) = &self.db else {
            return Ok(vec![]);
        };
        let mut out = Vec::new();
        for name in self.list() {
            if let Err(e) = self.snapshot_now(&name).await {
                tracing::warn!(world = %name, error = %e, "drain snapshot failed");
            }
            let r = sqlx::query(
                "update musubi.worlds set node_id = null where name = $1 and node_id = $2",
            )
            .bind(&name)
            .bind(self.node_id)
            .execute(db)
            .await
            .map_err(Error::internal)?;
            if r.rows_affected() > 0 {
                // the snapshot is now the only authoritative copy: a world left ticking here would
                // start handing its entities over to the adopter the moment it publishes itself as
                // home (journaling Despawns the adopter replays over that snapshot) and the node
                // exits before those handovers land
                self.worlds.remove(&name);
                self.homes.remove(&name);
                out.push(name);
            }
        }
        Ok(out)
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Home node of a world: local registry first, then the directory. `None` = unknown world.
    /// Linearizable read of a world's home from the directory; refreshes the caches a hosted
    /// shard keeps (its home may have been adopted by another node since the cell was assigned).
    async fn home_fresh(&self, name: &str) -> Option<Uuid> {
        let c = self.cluster.get()?;
        let v = c.directory_get_fresh(&Self::directory_key(name)).await?;
        let h = v["node_id"]
            .as_str()
            .and_then(|x| Uuid::parse_str(x).ok())?;
        self.homes.insert(name.to_string(), h);
        if let Some(w) = self.worlds.get(name) {
            let mut st = w.lock();
            if st.hosted_only && h != self.node_id {
                st.home_node = Some(h);
            }
        }
        Some(h)
    }

    pub async fn home_of(&self, name: &str) -> Result<Option<Uuid>> {
        // a local shard makes us the home only if it is not merely hosting assigned cells
        if let Some(w) = self.worlds.get(name) {
            let st = w.lock();
            if !st.hosted_only {
                return Ok(Some(self.node_id));
            }
            if let Some(h) = st.home_node {
                return Ok(Some(h));
            }
        }
        if let Some(h) = self.homes.get(name) {
            return Ok(Some(*h));
        }
        if let Some(c) = self.cluster.get() {
            if let Some(v) = c.directory_get_fresh(&Self::directory_key(name)).await {
                if let Some(h) = v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()) {
                    self.homes.insert(name.to_string(), h);
                    if let Some(w) = self.worlds.get(name) {
                        let mut st = w.lock();
                        if st.hosted_only && h != self.node_id {
                            st.home_node = Some(h);
                        }
                    }
                    return Ok(Some(h));
                }
            }
        }
        let Some(db) = &self.db else { return Ok(None) };
        let row: Option<Uuid> =
            sqlx::query_scalar("select node_id from musubi.worlds where name = $1")
                .bind(name)
                .fetch_optional(db)
                .await
                .map_err(Error::internal)?;
        if let Some(h) = row {
            self.homes.insert(name.to_string(), h);
            if let Some(w) = self.worlds.get(name) {
                let mut st = w.lock();
                if st.hosted_only && h != self.node_id {
                    st.home_node = Some(h);
                }
            }
        }
        Ok(row)
    }

    /// This node keeps a shard of `name` (its home shard or an assigned cell's).
    pub fn has_shard(&self, name: &str) -> bool {
        self.worlds.contains_key(name)
    }

    /// Run a client command on the home node; errors go back to the session as messages. An input
    /// for an entity that lives in a cell hosted elsewhere is forwarded over the bus.
    pub fn exec(&self, env: WorldCmdEnv, fanout: &Fanout) -> Option<ServerMessage> {
        let r = match env.cmd {
            WorldCmd::Subscribe {
                x,
                y,
                radius,
                budget_bytes,
                filter,
            } => self.subscribe(
                &env.world,
                env.session_id,
                Pos { x, y },
                radius,
                budget_bytes.map(|b| b as usize),
                filter,
            ),
            WorldCmd::Unsubscribe => self.unsubscribe(&env.world, env.session_id),
            WorldCmd::Input { entity, input } => {
                match self.input(&env.world, env.session_id, &entity, input.clone()) {
                    Err(Error::NotFound(_)) if self.is_split(&env.world) => {
                        match parse_entity(&entity) {
                            Ok(id) => {
                                let ev = json!({"type": "input", "entity": id, "session": env.session_id, "input": input});
                                fanout.world_input(&env.world, id, ev);
                                Ok(())
                            }
                            Err(e) => Err(e),
                        }
                    }
                    r => r,
                }
            }
        };
        r.err().map(|e| ServerMessage::error(&e))
    }

    /// Cells whose host died: the home node takes them back (directory + bus) and restores the
    /// dead host's shard snapshot/journal into its own shard. Returns (world, cell) pairs re-homed.
    pub async fn recover_cells(
        &self,
        dead_nodes: &[Uuid],
        fanout: &Fanout,
    ) -> Result<Vec<(String, CellId)>> {
        let orphaned: Vec<(String, CellId, Uuid)> = self
            .cell_hosts
            .iter()
            .filter(|e| dead_nodes.contains(e.value()))
            .map(|e| (e.key().0.clone(), e.key().1, *e.value()))
            .collect();
        let mut out = Vec::new();
        let mut restored: Vec<(String, Uuid)> = Vec::new();
        for (name, cell, dead) in orphaned {
            if self.home_of(&name).await? != Some(self.node_id) {
                continue;
            }
            if let Err(e) = self
                .publish_cell_host(&name, cell, Some(self.node_id))
                .await
            {
                tracing::warn!(world = %name, ?cell, error = %e, "cell not re-homed: no directory quorum");
                continue;
            }
            // the local cache must follow at once: with it still naming the dead node, every
            // entity entering the cell would start a cross-node handover to a dead host and sit
            // in transit (invisible to views) until that node came back
            self.cell_hosts.insert((name.clone(), cell), self.node_id);
            fanout.cell_assign(&name, cell, self.node_id);
            // entities we were handing to the dead host never got their ack: the cell is ours
            // now, so they simply stay here instead of retrying against a dead node
            if let Ok(w) = self.get(&name) {
                let mut st = w.lock();
                let st = &mut *st;
                let stuck: Vec<EntityId> = st
                    .remote_transit
                    .iter()
                    .filter(|(_, (h, _))| *h == dead)
                    .map(|(id, _)| *id)
                    .collect();
                for id in stuck {
                    st.remote_transit.remove(&id);
                    if let Some(t) = st.world.cancel_handover(id) {
                        push_event(
                            st,
                            t.from,
                            json!({"type": "handover_cancelled", "entity": id, "reason": "target node died; cell re-homed"}),
                        );
                    }
                }
            }
            if !restored.contains(&(name.clone(), dead)) {
                if let (Some(db), Ok(w)) = (&self.db, self.get(&name)) {
                    let cfg = w.lock().world.cfg.clone();
                    let mut shard = World::new(cfg);
                    let n = Self::load_shards(db, &name, &[dead], true, &mut shard).await?;
                    let mut ents = shard.snapshot_persistent();
                    tracing::info!(world = %name, node = %dead, tick = shard.tick, entities = ents.len(), "dead host shard loaded");
                    {
                        let mut st = w.lock();
                        // an entity that already lives here (handed over before the host died, or
                        // journaled on both sides) keeps its live copy
                        ents.retain(|e| st.world.get(e.id).is_none());
                        st.world.restore(ents);
                    }
                    tracing::info!(world = %name, node = %dead, replayed = n, "dead cell host's shard restored");
                    sqlx::query(
                        "delete from musubi.world_snapshots where name = $1 and node_id = $2",
                    )
                    .bind(&name)
                    .bind(dead)
                    .execute(db)
                    .await
                    .map_err(Error::internal)?;
                }
                restored.push((name.clone(), dead));
            }
            out.push((name, cell));
        }
        Ok(out)
    }

    fn get(&self, name: &str) -> Result<Arc<Mutex<WorldState>>> {
        self.worlds
            .get(name)
            .map(|w| w.clone())
            .ok_or_else(|| Error::NotFound(format!("world {name}")))
    }

    /// This node's region (`node.region`), the default placement for the worlds it creates.
    fn region(&self) -> String {
        self.cluster
            .get()
            .map(|c| c.me().region.clone())
            .unwrap_or_default()
    }

    /// Creates the world here, or — when `region` names another region with a live node — as a
    /// pending row (`node_id` null) that a node of that region adopts on its next failover round;
    /// `None` means pending. Without a live node there the world is placed here.
    pub async fn create(
        &self,
        name: &str,
        cfg: WorldConfig,
        ctl: ControllerConfig,
        region: &str,
    ) -> Result<Option<WorldInfo>> {
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidArgument("world name 1..=64 chars".into()));
        }
        if cfg.cell_size <= 0.0 || cfg.ghost_margin < 0.0 || cfg.hysteresis < 0.0 {
            return Err(Error::InvalidArgument("bad world config".into()));
        }
        if self.worlds.contains_key(name) {
            return Err(Error::Conflict("world exists".into()));
        }
        if let Some(db) = &self.db {
            let elsewhere = !region.is_empty() && region != self.region();
            let remote_alive: bool = if elsewhere {
                sqlx::query_scalar(
                    "select exists(select 1 from musubi.nodes where region = $1 and heartbeat_at > now() - interval '10 seconds')",
                )
                .bind(region)
                .fetch_one(db)
                .await
                .map_err(Error::internal)?
            } else {
                false
            };
            if remote_alive {
                sqlx::query("insert into musubi.worlds (name, node_id, config, controller, cluster_group, node_epoch, region) values ($1, null, $2, $3, $4, 0, $5)")
                    .bind(name)
                    .bind(serde_json::to_value(&cfg)?)
                    .bind(serde_json::to_value(&ctl)?)
                    .bind(&self.group)
                    .bind(region)
                    .execute(db)
                    .await
                    .map_err(|e| match e {
                        sqlx::Error::Database(d) if d.is_unique_violation() => Error::Conflict("world exists on another node".into()),
                        e => Error::internal(e),
                    })?;
                tracing::info!(
                    world = name,
                    region,
                    "world placed for another region, pending adoption"
                );
                return Ok(None);
            }
            sqlx::query("insert into musubi.worlds (name, node_id, config, controller, cluster_group, node_epoch, region) values ($1, $2, $3, $4, $5, $6, $7)")
                .bind(name)
                .bind(self.node_id)
                .bind(serde_json::to_value(&cfg)?)
                .bind(serde_json::to_value(&ctl)?)
                .bind(&self.group)
                .bind(self.epoch())
                .bind(region)
                .execute(db)
                .await
                .map_err(|e| match e {
                    sqlx::Error::Database(d) if d.is_unique_violation() => Error::Conflict("world exists on another node".into()),
                    e => Error::internal(e),
                })?;
        }
        self.worlds.insert(
            name.to_string(),
            Arc::new(Mutex::new(WorldState {
                world: World::new(cfg),
                controller: Controller::new(ctl),
                subs: HashMap::new(),
                sim_events: HashMap::new(),
                journal: Vec::new(),
                remote_ghosts: HashMap::new(),
                hosted_only: false,
                home_node: None,
                remote_transit: HashMap::new(),
                interaction_seq: HashMap::new(),
                admission: HashMap::new(),
            })),
        );
        self.publish_home(name, Some(self.node_id)).await;
        self.info(name).map(Some)
    }

    pub async fn delete(&self, name: &str) -> Result<()> {
        self.worlds
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| Error::NotFound(format!("world {name}")))?;
        self.homes.remove(name);
        if let Some(db) = &self.db {
            sqlx::query("delete from musubi.worlds where name = $1 and node_id = $2")
                .bind(name)
                .bind(self.node_id)
                .execute(db)
                .await
                .map_err(Error::internal)?;
        }
        self.publish_home(name, None).await;
        Ok(())
    }

    pub fn info(&self, name: &str) -> Result<WorldInfo> {
        let w = self.get(name)?;
        let st = w.lock();
        let cells = st.world.leased_cells();
        Ok(WorldInfo {
            name: name.to_string(),
            tick: st.world.tick,
            entities: st.world.len(),
            subscribers: st.subs.len(),
            cells,
            orphaned: st.world.orphaned_cells(),
            sims: st.controller.sims(),
            config: st.world.cfg.clone(),
        })
    }

    pub fn list(&self) -> Vec<String> {
        let mut v: Vec<String> = self.worlds.iter().map(|e| e.key().clone()).collect();
        v.sort();
        v
    }

    // ----- simulator side -----

    pub fn lease(&self, name: &str, sim: Uuid, cells: &[CellId]) -> Result<Vec<CellId>> {
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        for c in cells {
            if let Some(owner) = st.world.owner(*c) {
                if owner != sim {
                    return Err(Error::Conflict(format!("cell {c:?} is leased by {owner}")));
                }
            }
            st.world.grant_lease(*c, sim);
        }
        st.sim_events.entry(sim).or_default();
        Ok(st.world.cells_of(sim))
    }

    /// A simulator announces itself (and its capacity); the controller may then lease it cells.
    /// What this node's shard of `name` holds right now (operators: "where is entity X?").
    pub fn local_entities(&self, name: &str) -> Result<Value> {
        let w = self.get(name)?;
        let st = w.lock();
        let ents: Vec<Value> = st
            .world
            .entities_all()
            .into_iter()
            .map(|e| {
                json!({
                    "id": e.id, "cell": e.cell, "pos": e.pos, "persistent": e.persistent,
                    "transit": e.transit,
                    "remote_to": st.remote_transit.get(&e.id).map(|(h, _)| *h),
                })
            })
            .collect();
        Ok(json!({
            "world": name,
            "node_id": self.node_id,
            "hosted_only": st.hosted_only,
            "home_node": st.home_node,
            "tick": st.world.tick,
            "leases": st.world.leases().into_iter().map(|(c, s)| json!({"cell": c, "sim": s})).collect::<Vec<_>>(),
            "entities": ents,
        }))
    }

    pub fn register_sim(&self, name: &str, sim: Uuid, capacity: u32) -> Result<Vec<SimInfo>> {
        if capacity == 0 {
            return Err(Error::InvalidArgument("capacity >= 1".into()));
        }
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let tick = st.world.tick;
        st.controller.register(sim, capacity, tick);
        st.sim_events.entry(sim).or_default();
        Ok(st.controller.sims())
    }

    pub fn unregister_sim(&self, name: &str, sim: Uuid) -> Result<()> {
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        st.controller.unregister(sim);
        for c in st.world.cells_of(sim) {
            st.world.revoke_lease(c);
        }
        Ok(())
    }

    pub fn renew(&self, name: &str, sim: Uuid) -> Result<Vec<CellId>> {
        self.renew_at(name, sim, None)
    }

    /// Heartbeat plus, optionally, the last world tick the simulator has fully processed —
    /// the input of time dilation.
    pub fn renew_at(&self, name: &str, sim: Uuid, done: Option<u64>) -> Result<Vec<CellId>> {
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        st.world.renew_leases(sim);
        let tick = st.world.tick;
        st.controller.seen(sim, tick);
        if let Some(t) = done {
            st.controller.progress(sim, t.min(tick));
        }
        Ok(st.world.cells_of(sim))
    }

    pub fn time_scale(&self, name: &str, cell: CellId) -> Result<f64> {
        Ok(self.get(name)?.lock().world.time_scale(cell))
    }

    pub fn spawn(
        &self,
        name: &str,
        sim: Uuid,
        pos: Pos,
        components: Value,
        persistent: bool,
    ) -> Result<WorldEntity> {
        let spec = SpawnSpec {
            pos,
            components,
            persistent,
            decay_ticks: None,
            queue: false,
        };
        match self.spawn_or_queue(name, sim, spec)? {
            SpawnOutcome::Spawned(e) => Ok(e),
            SpawnOutcome::Queued { .. } => {
                Err(Error::Internal("spawn queued without asking".into()))
            }
        }
    }

    /// Spawns, or with `queue` joins the cell's admission queue when `soft_cap` refuses: the
    /// simulator gets `queued` now and `queue_admitted` with the entity once there is room
    /// (`queue_update` as its position/ETA change, `queue_expired` on `queue_ttl_ticks` or a lost
    /// lease).
    pub fn spawn_or_queue(&self, name: &str, sim: Uuid, spec: SpawnSpec) -> Result<SpawnOutcome> {
        let SpawnSpec {
            pos,
            components,
            persistent,
            decay_ticks,
            queue,
        } = spec;
        let comps: std::collections::BTreeMap<String, Value> = components
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let cell = st.world.cell_of(pos);
        if st.world.owner(cell).is_none() {
            if let Some(a) = st
                .controller
                .ensure_leased(&mut st.world, cell, Some(sim), "spawn")
            {
                announce(st, &a);
            }
        }
        if !st.world.cell_has_room(cell) && !queue {
            push_event(
                st,
                sim,
                json!({"type": "cell_full", "cell": cell, "soft_cap": st.world.cfg.soft_cap}),
            );
            return Err(mesh_err(musubi_mesh::MeshError::CellFull(
                cell,
                st.world.cfg.soft_cap,
            )));
        }
        match st.world.owner(cell) {
            Some(o) if o == sim => {}
            Some(o) => {
                return Err(Error::PermissionDenied(format!(
                    "cell {cell:?} is owned by {o}"
                )))
            }
            None => return Err(Error::Conflict(format!("cell {cell:?} has no live lease"))),
        }
        if !st.world.cell_has_room(cell) {
            let ticket = Uuid::now_v7();
            let soft_cap = st.world.cfg.soft_cap;
            let tick = st.world.tick;
            let q = st.admission.entry(cell).or_default();
            let position = q.entries.len() + 1;
            let eta_ticks = q.eta_ticks(position);
            q.entries.push_back(Queued {
                ticket,
                sim,
                pos,
                components: comps,
                persistent,
                decay_ticks,
                enqueued_tick: tick,
                reported: (position, eta_ticks),
            });
            push_event(
                st,
                sim,
                json!({"type": "queued", "ticket": ticket, "cell": cell, "position": position,
                       "eta_ticks": eta_ticks, "soft_cap": soft_cap}),
            );
            return Ok(SpawnOutcome::Queued {
                ticket,
                cell,
                position,
                eta_ticks,
            });
        }
        let id = spawn_entity(st, pos, comps, persistent, decay_ticks);
        Ok(SpawnOutcome::Spawned(to_wire(st.world.get(id).unwrap())))
    }

    /// Co-locate (plan 4.6): `sim`, owning `entity`'s cell, asks for `with` to be simulated next
    /// to it — `with` is pinned into that cell for `ttl_ticks` and the regular handover brings it
    /// over (its owner gets `handover_request`, `sim` acks); when the pin lapses it goes back to
    /// where its position says. Both entities must be on this shard.
    /// Bus side of a cross-node co-locate: pin `id` into `cell` (hosted by the requester) so
    /// `detect_remote_handovers` ships it there; its owner hears `co_locate`.
    pub fn accept_remote_co_locate(
        &self,
        name: &str,
        id: EntityId,
        cell: CellId,
        by: Uuid,
        ttl_ticks: u64,
    ) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        let Some(e) = st.world.get(id).cloned() else {
            return;
        };
        let until = st.world.pin(id, cell, ttl_ticks);
        if let Some(owner) = st.world.owner(e.cell) {
            push_event(
                st,
                owner,
                json!({"type": "co_locate", "entity": id, "cell": cell, "by": by, "until_tick": until}),
            );
        }
    }

    pub fn co_locate(
        &self,
        name: &str,
        sim: Uuid,
        entity: &str,
        with: &str,
        ttl_ticks: u64,
        fanout: &Fanout,
    ) -> Result<Value> {
        let a = parse_entity(entity)?;
        let b = parse_entity(with)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let cell = st
            .world
            .get(a)
            .ok_or_else(|| Error::NotFound(format!("entity {a} is not on this shard")))?
            .cell;
        match st.world.owner(cell) {
            Some(o) if o == sim => {}
            Some(o) => {
                return Err(Error::PermissionDenied(format!(
                    "cell {cell:?} is owned by {o}"
                )))
            }
            None => return Err(Error::Conflict(format!("cell {cell:?} has no live lease"))),
        }
        if st.world.get(b).is_none() {
            if !self.is_split(name) {
                return Err(Error::NotFound(format!("entity {b} is not in this world")));
            }
            // the partner lives on the node hosting its cell: pin here so it lands in `cell`
            // when it arrives, and ask that node to pin and ship it
            let until = st.world.pin(b, cell, ttl_ticks);
            fanout.world_co_locate(name, b, cell, sim, ttl_ticks);
            return Ok(
                json!({"pinned": b, "with": a, "cell": cell, "until_tick": until, "forwarded": true}),
            );
        }
        let until = st.world.pin(b, cell, ttl_ticks);
        if let Some(owner) = st.world.owner(st.world.get(b).unwrap().cell) {
            push_event(
                st,
                owner,
                json!({"type": "co_locate", "entity": b, "with": a, "cell": cell, "by": sim, "until_tick": until}),
            );
        }
        Ok(json!({"pinned": b, "with": a, "cell": cell, "until_tick": until}))
    }

    /// Lag compensation query: where `entity` was at `tick` (and whether `at` was within
    /// `tolerance` of it). Answered by the shard holding the entity.
    pub fn rewind(
        &self,
        name: &str,
        entity: &str,
        tick: u64,
        at: Option<Pos>,
        tolerance: f64,
    ) -> Result<Value> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let st = w.lock();
        if st.world.get(id).is_none() {
            return Err(mesh_err(musubi_mesh::MeshError::NoEntity));
        }
        Ok(rewind_json(&st.world, id, tick, at, tolerance))
    }

    pub fn despawn(&self, name: &str, sim: Uuid, entity: &str) -> Result<()> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let cell = st
            .world
            .get(id)
            .ok_or_else(|| Error::NotFound("entity".into()))?
            .cell;
        if st.world.owner(cell) != Some(sim) {
            return Err(Error::PermissionDenied("not the owner".into()));
        }
        let persistent = st.world.get(id).is_some_and(|e| e.persistent);
        st.world.despawn(id);
        if persistent {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                op: WorldMutation::Despawn { id },
            });
        }
        Ok(())
    }

    pub fn apply(
        &self,
        name: &str,
        sim: Uuid,
        entity: &str,
        pos: Option<Pos>,
        components: Value,
    ) -> Result<()> {
        let id = parse_entity(entity)?;
        let comps: std::collections::BTreeMap<String, Value> = components
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        st.world
            .apply(sim, id, pos, comps.clone())
            .map_err(mesh_err)?;
        if st.world.get(id).is_some_and(|e| e.persistent) {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                op: WorldMutation::Apply {
                    id,
                    pos,
                    components: Value::Object(comps.into_iter().collect()),
                },
            });
        }
        Ok(())
    }

    /// Persist the pending journal of a world now (the ticker does it every `JOURNAL_EVERY` ticks).
    pub async fn flush_journal(&self, name: &str) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let (ops, tick) = {
            let w = self.get(name)?;
            let mut st = w.lock();
            (std::mem::take(&mut st.journal), st.world.tick)
        };
        Self::write_journal(db, name, self.node_id, ops, tick).await
    }

    async fn write_journal(
        db: &PgPool,
        name: &str,
        node: Uuid,
        ops: Vec<WorldOp>,
        tick: u64,
    ) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let from = ops.first().map(|o| o.tick).unwrap_or(tick);
        sqlx::query(
            "insert into musubi.world_journal (name, tick_from, tick_to, entries, node_id) values ($1, $2, $3, $4, $5)
             on conflict (name, node_id, tick_from) do update set tick_to = greatest(musubi.world_journal.tick_to, excluded.tick_to), entries = musubi.world_journal.entries || excluded.entries",
        )
        .bind(name)
        .bind(from as i64)
        .bind(tick as i64)
        .bind(serde_json::to_value(&ops)?)
        .bind(node)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub fn view(&self, name: &str, cell: CellId) -> Result<Value> {
        let w = self.get(name)?;
        let st = w.lock();
        let mut v = st.world.view(cell);
        for n in World::neighbours(cell) {
            let Some((at, es)) = st.remote_ghosts.get(&n) else {
                continue;
            };
            if at.elapsed() > REMOTE_GHOST_TTL {
                continue;
            }
            for g in st.world.ghosts_among(cell, es.iter()) {
                if !v.ghosts.iter().any(|e| e.id == g.id) {
                    v.ghosts.push(g.clone());
                }
            }
        }
        v.ghosts.sort_by_key(|e| e.id);
        Ok(json!({
            "tick": st.world.tick,
            "owner": st.world.owner(cell),
            "owned": v.owned.iter().map(to_wire).collect::<Vec<_>>(),
            "ghosts": v.ghosts.iter().map(to_wire).collect::<Vec<_>>(),
        }))
    }

    /// Ghosts from another node: border entities of `cell`, which lives there, not here.
    pub fn accept_ghosts(&self, name: &str, cell: CellId, tick: u64, entities: Vec<Entity>) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        if self
            .cell_hosts
            .get(&(name.to_string(), cell))
            .is_some_and(|h| *h == self.node_id)
        {
            return;
        }
        let mut st = w.lock();
        if st.world.owner(cell).is_some() {
            return;
        }
        let _ = tick;
        st.remote_ghosts
            .insert(cell, (std::time::Instant::now(), entities));
    }

    /// An entity handed over from another node into a cell hosted here. Taken when a local
    /// simulator owns the cell (it gets a `handover_arrived` event with the state and the inputs
    /// buffered so far); refused otherwise. Nodes that do not host the cell stay silent.
    pub fn accept_handover(
        &self,
        name: &str,
        entity: Entity,
        from_node: Uuid,
        inputs: Vec<Value>,
        forced: bool,
        fanout: &Fanout,
    ) -> bool {
        let Some(w) = self.worlds.get(name) else {
            return false;
        };
        let mut st = w.lock();
        let st = &mut *st;
        // a co-located partner lands in the cell it was pinned to, not where its position says
        let cell = st
            .world
            .pinned_to(entity.id)
            .map(|(c, _)| c)
            .unwrap_or_else(|| st.world.cell_of(entity.pos));
        // ours if assigned to us, or unassigned and we are the home
        let here = match self.cell_hosts.get(&(name.to_string(), cell)) {
            Some(h) => *h == self.node_id,
            None => !st.hosted_only,
        };
        if !here {
            return false;
        }
        let id = entity.id;
        if st.world.get(id).is_some() {
            // already taken earlier; the ack was lost — answer again, keep our copy
            tracing::warn!(world = name, %id, %from_node, "handover re-acked (duplicate delivery)");
            fanout.world_handover_ack(name, id, from_node, true);
            return false;
        }
        let owner = st.world.owner(cell);
        if owner.is_none() && !forced {
            fanout.world_handover_ack(name, id, from_node, false);
            return false;
        }
        if entity.persistent {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                op: WorldMutation::Spawn {
                    id,
                    pos: entity.pos,
                    decay_ticks: entity.decay_ticks,
                    components: Value::Object(
                        entity
                            .components
                            .iter()
                            .map(|(k, c)| (k.clone(), c.value.clone()))
                            .collect(),
                    ),
                },
            });
        }
        let durable = entity.persistent;
        tracing::debug!(world = name, %id, %from_node, forced, "handover: entity arrived");
        st.world.restore(vec![entity]);
        if let Some(owner) = owner {
            let state = st.world.get(id).map(to_wire);
            push_event(
                st,
                owner,
                json!({"type": "handover_arrived", "entity": id, "from_node": from_node, "state": state, "buffered_inputs": inputs}),
            );
        }
        if durable {
            // the sender journals our ack as its Despawn: ours must hit the journal first,
            // or a crash in between loses the entity — the caller flushes, then acks
            return true;
        }
        fanout.world_handover_ack(name, id, from_node, true);
        false
    }

    /// The other node answered our handover: on success the entity leaves this shard (inputs
    /// buffered meanwhile follow it over the bus), on refusal it stays with its owner.
    pub fn handover_acked(&self, name: &str, id: EntityId, accepted: bool, fanout: &Fanout) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        let Some(t) = st.world.get(id).and_then(|e| e.transit.clone()) else {
            // a late ack for a handover we already cancelled: if the receiver took the entity
            // and its cell is not ours, our copy is the stale one — drop it instead of keeping
            // two owners
            if accepted {
                if let Some(e) = st.world.get(id).cloned() {
                    let cell = st.world.cell_of(e.pos);
                    let elsewhere = match self.cell_hosts.get(&(name.to_string(), cell)) {
                        Some(h) => *h != self.node_id,
                        None => st.hosted_only,
                    };
                    if elsewhere {
                        tracing::info!(world = name, %id, "late handover ack: stale local copy dropped");
                        st.remote_transit.remove(&id);
                        st.world.forget_for_clients(id);
                        st.world.despawn(id);
                        if e.persistent {
                            st.journal.push(WorldOp {
                                tick: st.world.tick,
                                op: WorldMutation::Despawn { id },
                            });
                        }
                    }
                }
            }
            return;
        };
        st.remote_transit.remove(&id);
        if accepted {
            let inputs = st.world.take_buffered_inputs(id);
            let persistent = st.world.get(id).is_some_and(|e| e.persistent);
            tracing::debug!(world = name, %id, to = %t.to, "handover: entity left");
            st.world.forget_for_clients(id);
            st.world.despawn(id);
            if persistent {
                st.journal.push(WorldOp {
                    tick: st.world.tick,
                    op: WorldMutation::Despawn { id },
                });
            }
            for i in inputs {
                fanout.world_input(name, id, i);
            }
            push_event(
                st,
                t.from,
                json!({"type": "handover_completed", "entity": id, "to": t.to, "node": t.to}),
            );
        } else {
            st.world.cancel_handover(id);
            push_event(
                st,
                t.from,
                json!({"type": "handover_cancelled", "entity": id, "reason": format!("node {} refused: no simulator owns the cell", t.to)}),
            );
        }
    }

    /// Input forwarded from another node; dropped unless the entity lives here.
    pub fn accept_remote_input(&self, name: &str, id: EntityId, input: Value) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        if let Ok(Some(sim)) = st.world.route_input(id, input.clone()) {
            let ev = if input["type"] == "interaction" {
                stamp_interaction(st, id, input)
            } else {
                input
            };
            push_event(st, sim, ev);
        }
    }

    /// A simulator acts on an entity it does not own (a hit on a ghost across a cell or node
    /// border): the effect is delivered as an `interaction` event to the simulator that owns the
    /// target, which alone applies it — one winner per conflict, in per-entity sequence order.
    /// In transit the interaction travels with the entity's buffered inputs; an entity living in
    /// a cell hosted elsewhere is forwarded over the bus.
    pub fn interact(
        &self,
        name: &str,
        sim: Uuid,
        entity: &str,
        spec: InteractSpec,
        fanout: &Fanout,
    ) -> Result<Value> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let ev = json!({
            "type": "interaction", "entity": id, "kind": spec.kind, "payload": spec.payload,
            "from_sim": sim, "at_tick": spec.at_tick, "at": spec.at, "tolerance": spec.tolerance,
        });
        match st.world.route_input(id, ev.clone()) {
            Ok(Some(owner)) => {
                let ev = stamp_interaction(st, id, ev);
                let seq = ev["seq"].clone();
                push_event(st, owner, ev);
                Ok(json!({ "delivered": "owner", "seq": seq }))
            }
            Ok(None) => Ok(json!({ "delivered": "in_transit" })),
            Err(musubi_mesh::MeshError::NoEntity) if self.is_split(name) => {
                fanout.world_input(name, id, ev);
                Ok(json!({ "delivered": "forwarded" }))
            }
            Err(e) => Err(mesh_err(e)),
        }
    }

    /// Are the cells of `name` spread over several nodes? Any explicit cell assignment means
    /// so: the home node sees the foreign hosts, an assigned node sees at least its own entry.
    pub fn is_split(&self, name: &str) -> bool {
        self.cell_hosts.iter().any(|e| e.key().0 == name)
    }

    /// Drain pending events for a simulator (handover requests/results, routed inputs).
    pub fn events(&self, name: &str, sim: Uuid) -> Result<Vec<Value>> {
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        Ok(std::mem::take(st.sim_events.entry(sim).or_default()))
    }

    pub fn handover_ack(&self, name: &str, sim: Uuid, entity: &str) -> Result<Value> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let from = st
            .world
            .get(id)
            .and_then(|e| e.transit.as_ref().map(|t| t.from));
        let (outcome, inputs) = st.world.handover_ack(sim, id).map_err(mesh_err)?;
        if let Some(from) = from {
            push_event(
                st,
                from,
                json!({"type": "handover_completed", "entity": id, "to": sim}),
            );
        }
        Ok(json!({ "outcome": outcome, "buffered_inputs": inputs }))
    }

    // ----- client side -----

    pub fn subscribe(
        &self,
        name: &str,
        sid: SessionId,
        pos: Pos,
        radius: f64,
        budget_bytes: Option<usize>,
        filter: Option<Value>,
    ) -> Result<()> {
        if !(0.0..=100_000.0).contains(&radius) {
            return Err(Error::InvalidArgument("radius 0..=100000".into()));
        }
        let filter: std::collections::BTreeMap<String, Value> = match filter {
            None | Some(Value::Null) => Default::default(),
            Some(Value::Object(m)) => m.into_iter().collect(),
            Some(_) => {
                return Err(Error::InvalidArgument(
                    "filter must be an object of component → value".into(),
                ))
            }
        };
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let client = *st.subs.entry(sid).or_insert_with(Uuid::now_v7);
        st.world
            .subscribe_with(client, pos, radius, budget_bytes, filter);
        let hosted_only = st.hosted_only;
        for c in st.world.cells_in_radius(pos, radius) {
            // only cells this shard is responsible for get a simulator from here
            let here = match self.cell_hosts.get(&(name.to_string(), c)) {
                Some(h) => *h == self.node_id,
                None => !hosted_only,
            };
            if !here {
                continue;
            }
            if let Some(a) = st
                .controller
                .ensure_leased(&mut st.world, c, None, "interest")
            {
                announce(st, &a);
            }
        }
        Ok(())
    }

    pub fn unsubscribe(&self, name: &str, sid: SessionId) -> Result<()> {
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        if let Some(c) = st.subs.remove(&sid) {
            st.world.unsubscribe(c);
        }
        Ok(())
    }

    pub fn unsubscribe_all(&self, sid: SessionId) {
        for w in self.worlds.iter() {
            let mut st = w.lock();
            let st = &mut *st;
            if let Some(c) = st.subs.remove(&sid) {
                st.world.unsubscribe(c);
            }
        }
    }

    /// Route a player input to the entity's current owner (buffered while in transit).
    pub fn input(&self, name: &str, sid: SessionId, entity: &str, input: Value) -> Result<()> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let ev = json!({"type": "input", "entity": id, "session": sid, "input": input});
        if let Some(sim) = st.world.route_input(id, ev.clone()).map_err(mesh_err)? {
            push_event(st, sim, ev);
        }
        Ok(())
    }

    // ----- ticking -----

    /// One tick of every world: clock, handover detection/expiry, client deltas.
    pub fn tick(&self, fanout: &Fanout) {
        for w in self.worlds.iter() {
            let name = w.key().clone();
            let started = std::time::Instant::now();
            let mut st = w.lock();
            let st = &mut *st;
            let mut outcomes = st.world.advance();
            outcomes.extend(st.world.detect_handovers());
            drain_admission(st);
            decay_entities(st);
            if self.is_split(&name) {
                let (hosted_only, home_node) = (st.hosted_only, st.home_node);
                let remote = st.world.detect_remote_handovers(|c| {
                    self.cell_hosts
                        .get(&(name.clone(), c))
                        .map(|h| *h)
                        .or(if hosted_only {
                            // the home may have moved since this shard was created
                            self.homes.get(&name).map(|h| *h).or(home_node)
                        } else {
                            None
                        })
                        .filter(|h| *h != self.node_id)
                });
                for (id, _, host) in remote {
                    st.remote_transit.insert(id, (host, 1));
                    if let Some(e) = st.world.get(id).cloned() {
                        let inputs = st.world.take_buffered_inputs(id);
                        let from = e.transit.as_ref().map(|t| t.from);
                        let mut shipped = e;
                        shipped.transit = None;
                        fanout.world_handover(&name, shipped, self.node_id, inputs, false);
                        if let Some(from) = from {
                            push_event(
                                st,
                                from,
                                json!({"type": "handover_started", "entity": id, "to": host, "node": host}),
                            );
                        }
                    }
                }
                for (cell, _) in st.world.leased_cells() {
                    let border: Vec<Entity> = st
                        .world
                        .border_entities(cell)
                        .into_iter()
                        .cloned()
                        .collect();
                    fanout.world_ghosts(&name, cell, st.world.tick, border);
                }
                st.remote_ghosts
                    .retain(|_, (at, _)| at.elapsed() <= REMOTE_GHOST_TTL);
            }
            if st.world.tick % JOURNAL_EVERY == 0 && !st.journal.is_empty() {
                if let Some(db) = self.db.clone() {
                    let (ops, tick, n, node) = (
                        std::mem::take(&mut st.journal),
                        st.world.tick,
                        name.clone(),
                        self.node_id,
                    );
                    tokio::spawn(async move {
                        if let Err(e) = Self::write_journal(&db, &n, node, ops, tick).await {
                            tracing::warn!(world = %n, error = %e, "world journal failed");
                        }
                    });
                }
            }
            if st.world.tick % SNAPSHOT_EVERY == 0 {
                if let Some(db) = self.db.clone() {
                    let (entities, tick, n, node) = (
                        st.world.snapshot_persistent(),
                        st.world.tick,
                        name.clone(),
                        self.node_id,
                    );
                    tokio::spawn(async move {
                        if let Err(e) = Self::write_snapshot(&db, &n, node, entities, tick).await {
                            tracing::warn!(world = %n, error = %e, "world snapshot failed");
                        }
                    });
                }
            }
            st.controller.observe(&st.world);
            if st.world.tick % CONTROL_EVERY == 0 {
                let moves = st.controller.reconcile(&mut st.world);
                for a in &moves {
                    announce(st, a);
                }
                for (cell, sim, ephemeral) in st.controller.hibernate(&mut st.world) {
                    if let Some(sim) = sim {
                        push_event(
                            st,
                            sim,
                            json!({"type": "cell_hibernated", "cell": cell, "ephemeral_despawned": ephemeral,
                                   "persistent_kept": st.world.entities_in(cell)}),
                        );
                    }
                    tracing::info!(world = %name, ?cell, ephemeral, "cell hibernated");
                }
            }
            for (cell, scale) in st.controller.dilate(&mut st.world) {
                if let Some(owner) = st.world.owner(cell) {
                    push_event(
                        st,
                        owner,
                        json!({"type": "time_scale", "cell": cell, "scale": scale}),
                    );
                }
                tracing::info!(world = %name, ?cell, scale, "cell time scale changed");
            }
            for o in outcomes {
                match o {
                    HandoverOutcome::Started { entity, from, to } => {
                        let full = st.world.get(entity).map(to_wire);
                        push_event(
                            st,
                            to,
                            json!({"type": "handover_request", "entity": entity, "from": from, "state": full}),
                        );
                        push_event(
                            st,
                            from,
                            json!({"type": "handover_started", "entity": entity, "to": to}),
                        );
                    }
                    HandoverOutcome::Completed { .. } => {}
                    HandoverOutcome::Cancelled { entity, .. }
                        if st.remote_transit.contains_key(&entity) =>
                    {
                        // no ack from the other node yet: ship it again (the receiver re-acks if it
                        // already has it). Never give up: the entity sits in a cell another node
                        // hosts, and a copy kept alive here would become a second owner once the
                        // real one moves on (the stand saw 5 of 20 entities duplicated that way).
                        let Some((stale, n)) = st.remote_transit.get(&entity).copied() else {
                            continue;
                        };
                        // the target may have moved since the first attempt (host died and the
                        // cell was re-homed, home adopted by another node): resolve it again
                        let fresh = st
                            .world
                            .get(entity)
                            .map(|e| st.world.cell_of(e.pos))
                            .and_then(|c| {
                                self.cell_hosts
                                    .get(&(name.clone(), c))
                                    .map(|h| *h)
                                    .or(if st.hosted_only {
                                        self.homes.get(&name).map(|h| *h).or(st.home_node)
                                    } else {
                                        None
                                    })
                                    .filter(|h| *h != self.node_id)
                            });
                        let Some(host) = fresh else {
                            // the cell is ours now: the entity simply stays
                            st.remote_transit.remove(&entity);
                            st.world.cancel_handover(entity);
                            tracing::info!(world = %name, %entity, was = %stale, "cross-node handover dropped: target cell is local now");
                            continue;
                        };
                        if host != stale {
                            tracing::info!(world = %name, %entity, from = %stale, to = %host, "cross-node handover re-targeted");
                        }
                        if n + 1 == REMOTE_HANDOVER_RETRIES {
                            tracing::warn!(world = %name, %entity, %host, tries = n + 1, "cross-node handover still unacknowledged; keeps retrying");
                        }
                        st.remote_transit.insert(entity, (host, n + 1));
                        if st.world.begin_remote_handover(entity, host) {
                            if let Some(mut e) = st.world.get(entity).cloned() {
                                e.transit = None;
                                fanout.world_handover(&name, e, self.node_id, Vec::new(), false);
                            }
                        }
                    }
                    HandoverOutcome::Cancelled { entity, reason } => {
                        st.remote_transit.remove(&entity);
                        let sims: Vec<Uuid> = st.sim_events.keys().copied().collect();
                        for s in sims {
                            push_event(
                                st,
                                s,
                                json!({"type": "handover_cancelled", "entity": entity, "reason": reason}),
                            );
                        }
                    }
                }
            }
            let tick = st.world.tick;
            metrics::histogram!("musubi_world_tick_duration_seconds", "world" => name.clone())
                .record(started.elapsed().as_secs_f64());
            metrics::gauge!("musubi_world_entities", "world" => name.clone())
                .set(st.world.len() as f64);
            let subs: Vec<(SessionId, Uuid)> = st.subs.iter().map(|(s, c)| (*s, *c)).collect();
            for (sid, client) in subs {
                if let Some(d) = st.world.delta_for(client) {
                    if d != Delta::default() {
                        let scale = st.world.scale_for_client(client);
                        fanout.session(sid, delta_msg(&name, tick, scale, d));
                    }
                }
            }
        }
    }

    /// Tick numbers as they complete (lagging receivers skip ahead, never block the ticker).
    pub fn subscribe_ticks(&self) -> tokio::sync::broadcast::Receiver<u64> {
        self.ticks.subscribe()
    }

    pub fn spawn_ticker(self: Arc<Self>, fanout: Fanout) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(1000 / TICK_HZ));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                self.tick(&fanout);
                let t = self
                    .tick_no
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                let _ = self.ticks.send(t);
            }
        })
    }
}

fn announce(st: &mut WorldState, a: &Assignment) {
    if let Some(from) = a.from {
        push_event(
            st,
            from,
            json!({"type": "cell_revoked", "cell": a.cell, "to": a.to, "reason": a.reason}),
        );
    }
    push_event(
        st,
        a.to,
        json!({"type": "cell_assigned", "cell": a.cell, "from": a.from, "reason": a.reason}),
    );
}

fn stamp_interaction(st: &mut WorldState, id: EntityId, mut ev: Value) -> Value {
    st.world.touch(id);
    // the owner holds the target's history: answer "where was it when the actor acted" here,
    // whether the interaction came from a local simulator or over the bus
    if let Some(t) = ev["at_tick"].as_u64() {
        let at = serde_json::from_value::<Option<Pos>>(ev["at"].clone()).unwrap_or(None);
        let tol = ev["tolerance"].as_f64().unwrap_or(1.0);
        ev["rewind"] = rewind_json(&st.world, id, t, at, tol);
    }
    let seq = st.interaction_seq.entry(id).or_insert(0);
    *seq += 1;
    ev["seq"] = json!(*seq);
    ev["tick"] = json!(st.world.tick);
    ev
}

fn spawn_entity(
    st: &mut WorldState,
    pos: Pos,
    comps: std::collections::BTreeMap<String, Value>,
    persistent: bool,
    decay_ticks: Option<u64>,
) -> EntityId {
    let id = st.world.spawn(pos, comps.clone(), persistent);
    st.world.set_decay(id, decay_ticks);
    if persistent {
        st.journal.push(WorldOp {
            tick: st.world.tick,
            op: WorldMutation::Spawn {
                id,
                pos,
                components: Value::Object(comps.into_iter().collect()),
                decay_ticks,
            },
        });
    }
    id
}

/// `{tick, pos, hit, history_from}`: where `id` was at `tick` and whether `at` matches it.
fn rewind_json(world: &World, id: EntityId, tick: u64, at: Option<Pos>, tolerance: f64) -> Value {
    let pos = world.position_at(id, tick);
    json!({
        "tick": tick,
        "pos": pos,
        "hit": at.and_then(|a| world.was_at(id, tick, a, tolerance)),
        "history_from": world.history_from(id),
    })
}

/// Plan 4.9: entities whose `decay_ticks` ran out leave the world; persistent ones are
/// journaled so no shard resurrects them, and the cell's simulator hears `decayed`.
fn decay_entities(st: &mut WorldState) {
    for (id, cell, persistent) in st.world.decay() {
        if persistent {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                op: WorldMutation::Despawn { id },
            });
        }
        if let Some(owner) = st.world.owner(cell) {
            push_event(
                st,
                owner,
                json!({"type": "decayed", "entity": id, "cell": cell}),
            );
        }
    }
}

/// Admits queued spawns in arrival order as their cells make room; drops tickets past
/// `queue_ttl_ticks` or whose simulator no longer owns the cell. ETA = position / smoothed
/// departures per tick (unknown until the cell has seen entities leave).
fn drain_admission(st: &mut WorldState) {
    if st.admission.is_empty() {
        return;
    }
    let tick = st.world.tick;
    let ttl = st.world.cfg.queue_ttl_ticks;
    let cells: Vec<CellId> = st.admission.keys().copied().collect();
    let mut events = Vec::new();
    for cell in cells {
        let owner = st.world.owner(cell);
        let count = st.world.entities_in(cell);
        let Some(q) = st.admission.get_mut(&cell) else {
            continue;
        };
        if let Some(prev) = q.prev_count {
            let left = prev.saturating_sub(count) as f64;
            q.departures = q.departures * 0.9 + left * 0.1;
        }
        q.entries.retain(|e| {
            let expired = ttl > 0 && tick.saturating_sub(e.enqueued_tick) >= ttl;
            let lost = owner != Some(e.sim);
            if expired || lost {
                events.push((
                    e.sim,
                    json!({"type": "queue_expired", "ticket": e.ticket, "cell": cell,
                           "reason": if lost { "lease_lost" } else { "timeout" }}),
                ));
            }
            !(expired || lost)
        });
        while st.world.cell_has_room(cell) {
            let Some(e) = q.entries.pop_front() else {
                break;
            };
            let id = st.world.spawn(e.pos, e.components.clone(), e.persistent);
            st.world.set_decay(id, e.decay_ticks);
            if e.persistent {
                st.journal.push(WorldOp {
                    tick,
                    op: WorldMutation::Spawn {
                        id,
                        pos: e.pos,
                        components: Value::Object(e.components.into_iter().collect()),
                        decay_ticks: e.decay_ticks,
                    },
                });
            }
            events.push((
                e.sim,
                json!({"type": "queue_admitted", "ticket": e.ticket, "cell": cell,
                       "entity": to_wire(st.world.get(id).unwrap())}),
            ));
        }
        let departures = q.departures;
        for (i, e) in q.entries.iter_mut().enumerate() {
            let position = i + 1;
            let eta = (departures > 1e-6).then(|| (position as f64 / departures).ceil() as u64);
            if e.reported != (position, eta) {
                e.reported = (position, eta);
                events.push((
                    e.sim,
                    json!({"type": "queue_update", "ticket": e.ticket, "cell": cell,
                           "position": position, "eta_ticks": eta}),
                ));
            }
        }
        q.prev_count = Some(st.world.entities_in(cell));
        if q.entries.is_empty() {
            st.admission.remove(&cell);
        }
    }
    for (sim, ev) in events {
        push_event(st, sim, ev);
    }
}

fn push_event(st: &mut WorldState, sim: Uuid, ev: Value) {
    let q = st.sim_events.entry(sim).or_default();
    if q.len() < MAX_SIM_EVENTS {
        q.push(ev);
    }
}

fn delta_msg(world: &str, tick: u64, scale: f64, d: Delta) -> ServerMessage {
    ServerMessage::WorldDelta {
        world: world.to_string(),
        tick,
        time_scale: scale,
        spawned: d.spawned.iter().map(to_wire).collect(),
        updated: d
            .updated
            .into_iter()
            .map(|(id, pos, comps)| WorldEntity {
                id: id.to_string(),
                x: pos.x,
                y: pos.y,
                components: comps
                    .into_iter()
                    .map(|(k, c)| WorldComponent {
                        name: k,
                        value: c.value,
                        version: c.version,
                    })
                    .collect(),
            })
            .collect(),
        despawned: d.despawned.iter().map(|i| i.to_string()).collect(),
    }
}
