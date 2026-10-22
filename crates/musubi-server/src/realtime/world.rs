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
use musubi_mesh::controller::{Assignment, Controller, ControllerConfig, SimInfo, Strategy};
use musubi_mesh::SimEvent;
use musubi_mesh::{
    CellId, ClientId, Delta, Entity, EntityId, HandoverOutcome, Interest, Pos, World, WorldConfig,
};
use musubi_proto::realtime::{WorldComponent, WorldEntity};
use musubi_proto::ServerMessage;
use musubi_runtime::CallContext;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::fanout::Fanout;

pub const TICK_HZ: u64 = 30;
/// Ticks between re-reads of the alive peer set that cell replicas are picked from.
const PEER_REFRESH_TICKS: u64 = 30;
/// Postgres-bus mode: how often `musubi.world_cell_hosts` is re-read against the cache.
const CELL_HOST_RECONCILE_TICKS: u64 = 90;
pub const MAX_SIM_EVENTS: usize = 4096;

/// What a simulator stream in delta mode was last sent for one cell: entity → (rev,
/// changed_tick, is_ghost).
pub type SimKnown = HashMap<EntityId, (u64, u64, bool)>;
/// Persistent entities are snapshotted this often (ticks); 30 Hz → every 5 s.
const SNAPSHOT_EVERY: u64 = 150;
/// Controller pass every N ticks (1 s at 30 Hz).
pub const CONTROL_EVERY: u64 = 30;

/// One write of a cross-cell transaction: components to set on `entity`, guarded by the
/// component versions the caller read (`expect`) so a concurrent write aborts the whole thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxOp {
    pub entity: EntityId,
    #[serde(default)]
    pub components: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    pub expect: std::collections::BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TxState {
    Pending,
    Committed,
    Aborted { reason: String },
}

/// Coordinator-side record of a cross-cell transaction.
struct TxRecord {
    ops: Vec<TxOp>,
    /// Entities whose host has not voted yet.
    pending: std::collections::HashSet<EntityId>,
    state: TxState,
    deadline_tick: u64,
    /// Local tick after which the record is forgotten (finished ones stay for idempotent reads).
    forget_tick: u64,
}

/// Default ticks a transaction may stay in prepare before the coordinator aborts it.
const TX_TTL_TICKS: u64 = 30;
/// Finished transactions are kept this many ticks for `GET …/tx/{id}` and duplicate begins.
const TX_KEEP_TICKS: u64 = 30 * 600;

fn tx_json(tx: Uuid, s: &TxState, replayed: bool) -> Value {
    let mut v = json!({
        "tx": tx,
        "state": match s {
            TxState::Pending => "pending",
            TxState::Committed => "committed",
            TxState::Aborted { .. } => "aborted",
        },
    });
    if let TxState::Aborted { reason } = s {
        v["reason"] = json!(reason);
    }
    if replayed {
        v["replayed"] = json!(true);
    }
    v
}

/// Participant-side check of the ops whose entities live here: present, not in transit, not held
/// by another transaction, every expected component version still current. Returns those ids.
fn tx_check_local(
    st: &WorldState,
    tx: Uuid,
    ops: &[TxOp],
) -> std::result::Result<Vec<EntityId>, String> {
    let mut mine = Vec::new();
    for op in ops {
        let Some(e) = st.world.get(op.entity) else {
            continue;
        };
        if e.transit.is_some() {
            return Err(format!("entity {} is in transit", op.entity));
        }
        if let Some((holder, _)) = st.tx_locks.get(&op.entity) {
            if *holder != tx {
                return Err(format!(
                    "entity {} is held by transaction {holder}",
                    op.entity
                ));
            }
        }
        for (k, v) in &op.expect {
            let have = e.components.get(k).map(|c| c.version).unwrap_or(0);
            if have != *v {
                return Err(format!(
                    "entity {} component {k}: expected version {v}, have {have}",
                    op.entity
                ));
            }
        }
        mine.push(op.entity);
    }
    Ok(mine)
}

fn tx_lock_local(st: &mut WorldState, tx: Uuid, ids: &[EntityId], until_tick: u64) {
    for id in ids {
        st.tx_locks.insert(*id, (tx, until_tick));
        st.world.defer_handover(*id, until_tick);
    }
}

fn tx_release(st: &mut WorldState, tx: Uuid) -> Vec<EntityId> {
    let ids: Vec<EntityId> = st
        .tx_locks
        .iter()
        .filter(|(_, (t, _))| *t == tx)
        .map(|(id, _)| *id)
        .collect();
    for id in &ids {
        st.tx_locks.remove(id);
    }
    ids
}

/// Commit the ops whose entities live here: write, journal persistent ones, tell the owning
/// simulator. An entity that left meanwhile is skipped — its new host applies the same commit.
fn tx_apply_local(st: &mut WorldState, tx: Uuid, ops: &[TxOp]) -> Vec<EntityId> {
    let mut applied = Vec::new();
    for op in ops {
        if st.world.get(op.entity).is_none()
            || st
                .world
                .force_apply(op.entity, None, op.components.clone())
                .is_err()
        {
            continue;
        }
        let (persistent, cell) = match st.world.get(op.entity) {
            Some(e) => (e.persistent, e.cell),
            None => continue,
        };
        if persistent {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                source: Some(Provenance {
                    by: "tx".into(),
                    sim: None,
                }),
                op: WorldMutation::Apply {
                    id: op.entity,
                    pos: None,
                    components: Value::Object(op.components.clone().into_iter().collect()),
                },
            });
        }
        let owner = st.world.owner(cell);
        if let Some(owner) = owner {
            push_event(
                st,
                owner,
                SimEvent::<WorldEntity>::TxCommitted {
                    tx,
                    entity: op.entity,
                },
            );
        }
        applied.push(op.entity);
    }
    applied
}

/// Ship an entity to the node hosting its new cell. When the neighbours already hold it as a
/// ghost, only the components newer than the revision they have travel (`base_rev`); the
/// receiver merges them onto its ghost. After a `resend_full` refusal the full state goes.
/// One entity in sixteen gets a timeline in the log (`hotrace`): ship, arrival, ack, apply,
/// expiry — with the sender's and receiver's clocks, to see where a cross-node handover
/// spends its time.
/// One step of an entity's handover into the post-mortem journal (`realtime::hojournal`).
fn hoj(
    name: &str,
    id: EntityId,
    tick: u64,
    event: &'static str,
    path: &'static str,
    detail: String,
) {
    super::hojournal::record(name, id, tick, event, path, detail);
}

fn traced(id: EntityId) -> bool {
    id.to_string().ends_with('0')
}

fn ship_handover(
    st: &mut WorldState,
    fanout: &Fanout,
    name: &str,
    node_id: Uuid,
    mut e: Entity,
    inputs: Vec<Value>,
) {
    let id = e.id;
    // a retry ships everything: the receiver consumed its ghost on the attempt whose ack never
    // came back (or never had one), and a delta it cannot build on costs a refusal round trip
    // on top — the stand saw 43 k such refusals in 30 s
    // Always the full entity. The delta path (only components newer than the ghost the
    // neighbours hold) saved bytes, but `ghost_send` knows what was sent to neighbouring cells,
    // not whether the receiving node still holds that ghost — on the stand one ship in five was
    // refused ("no ghost to build on") and cost a round trip plus a re-send under the timeout.
    st.full_resend.remove(&id);
    let base: Option<u64> = None;
    if let Some(r) = base {
        std::sync::Arc::make_mut(&mut e.components).retain(|_, c| c.changed_rev > r);
    }
    if traced(id) {
        let n = st.remote_transit.get(&id).map(|(_, n, _)| *n).unwrap_or(0);
        tracing::debug!(world = name, %id, attempt = n, ms = musubi_cluster::unix_ms(), "hotrace: ship");
    }
    fanout.world_handover(name, e, node_id, inputs, false, base);
}

struct WorldState {
    world: World,
    controller: Controller,
    /// session → interest client id
    subs: HashMap<SessionId, Uuid>,
    /// interest client id → delivery options
    sub_opts: HashMap<Uuid, SubOpts>,
    /// session → its last subscribe (home shard only): replayed to a node that starts hosting a
    /// cell after the player subscribed, so the new shard knows whom to send deltas to
    sub_cmds: HashMap<SessionId, WorldCmd>,
    /// simulator → pending events (handover requests, inputs, …)
    sim_events: HashMap<Uuid, Vec<Value>>,
    /// Persistent-entity ops since the last journal flush (see `musubi.world_journal`).
    journal: Vec<WorldOp>,
    /// Ghosts streamed by the hosts of cells on other nodes, merged from their deltas.
    remote_ghosts: HashMap<CellId, musubi_mesh::GhostCache>,
    /// Per leased cell: what the neighbours already hold, so each tick sends only the changes.
    ghost_send: HashMap<CellId, musubi_mesh::GhostSendState>,
    /// Entities whose next handover must carry the full state (the receiver had no ghost of them).
    full_resend: std::collections::HashSet<EntityId>,
    /// Per hosted cell: what its standby replicas hold (replication stream, see `repl_delta`).
    repl_send: HashMap<CellId, musubi_mesh::GhostSendState>,
    /// Per hosted cell: the replicas it streams to; a change restarts the stream with a keyframe.
    repl_targets: HashMap<CellId, Vec<Uuid>>,
    /// Per hosted cell: replica node → last tick it acknowledged.
    repl_acks: HashMap<CellId, HashMap<Uuid, u64>>,
    /// Cross-cell transactions this node coordinates (by tx id).
    txs: HashMap<Uuid, TxRecord>,
    /// Entities held by a prepared transaction: no writes until commit, abort or the deadline.
    tx_locks: HashMap<EntityId, (Uuid, u64)>,
    /// Hibernated cells and the tick they fell asleep (drives `unload_after_ticks`).
    hibernated_since: HashMap<CellId, u64>,
    /// Cells whose cold-cell row is being written or read right now.
    loading: std::collections::HashSet<CellId>,
    /// This shard exists only for cells assigned to this node (it is not the world's home).
    hosted_only: bool,
    /// The world's home node as of shard creation: cells without an explicit host live there.
    home_node: Option<Uuid>,
    /// Cross-node handovers in flight: entity → (target node, attempts so far).
    remote_transit: HashMap<EntityId, (Uuid, u32, CellId)>,
    /// Target cell of each handover we cancelled and kept the entity of: a late ack from the
    /// node that did take it must still make us drop our copy.
    kept_after_cancel: HashMap<EntityId, CellId>,
    /// Per-target sequence of delivered interactions (this node is the single point of order
    /// for the entities it owns; the counter restarts when an entity changes owner).
    interaction_seq: HashMap<EntityId, u64>,
    /// Cross-border contacts by pair: (last tick seen, consecutive-tick streak). Dedupes the
    /// same contact reported from both sides of a cell border and drives co-location of a
    /// pair that keeps touching across it.
    contacts: HashMap<(EntityId, EntityId), (u64, u32)>,
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
    global: bool,
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
    /// Always relevant: every subscriber of the world receives it whatever their interest.
    pub global: bool,
    /// Despawn after this many ticks without a write, interaction or handover (litter).
    pub decay_ticks: Option<u64>,
    /// Wait in the cell's admission queue instead of failing on `soft_cap`.
    pub queue: bool,
}

/// Contact pairs not seen for this long are forgotten.
const CONTACT_WINDOW_TICKS: u64 = 60;

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

const JOURNAL_EVERY: u64 = 5;

/// One journaled mutation of a persistent entity; replayed on top of a snapshot when a world is
/// adopted by another node, so a rehome loses at most `JOURNAL_EVERY` ticks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldOp {
    pub tick: u64,
    pub op: WorldMutation,
    /// Who caused the op (debugging meshing: which simulator / mechanism wrote what, when).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Provenance>,
}

/// Provenance of a journaled op: `by` is `sim` (a simulator's spawn/apply/despawn/attach/detach),
/// `tx` (world transaction commit), `handover` (entity moved between cells/nodes), `spawn`
/// (server-side spawn), `decay`, `queue` (soft-cap admission) or `rewind`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Provenance {
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sim: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorldMutation {
    Spawn {
        id: Uuid,
        pos: Pos,
        components: Value,
        #[serde(default)]
        decay_ticks: Option<u64>,
        #[serde(default)]
        global: bool,
    },
    Despawn {
        id: Uuid,
    },
    Apply {
        id: Uuid,
        pos: Option<Pos>,
        components: Value,
    },
    Attach {
        id: Uuid,
        root: Uuid,
        #[serde(default)]
        contained: bool,
    },
    Detach {
        id: Uuid,
    },
}

fn op_entity(op: &WorldMutation) -> Uuid {
    match op {
        WorldMutation::Spawn { id, .. }
        | WorldMutation::Despawn { id }
        | WorldMutation::Apply { id, .. }
        | WorldMutation::Attach { id, .. }
        | WorldMutation::Detach { id } => *id,
    }
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
                global,
            } => {
                if world.get(*id).is_none() {
                    world.spawn_with_id(*id, *pos, comps(components), true);
                    world.set_decay(*id, *decay_ticks);
                    world.set_global(*id, *global);
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
            WorldMutation::Attach {
                id,
                root,
                contained,
            } => {
                if src.get(id).is_none_or(|s| *s == node) {
                    let _ = world.attach_with(*id, *root, *contained);
                }
            }
            WorldMutation::Detach { id } => {
                if src.get(id).is_none_or(|s| *s == node) {
                    world.detach(*id);
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

/// What a hierarchy query asks for (`WorldRegistry::query`, `GET …/entities*`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryKind {
    Entity { id: EntityId },
    Children { id: EntityId, depth: usize },
    Find { q: musubi_mesh::EntityQuery },
}

/// How long a cross-shard query waits for the other cell hosts.
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

#[derive(Clone)]
struct PendingInterest {
    sid: SessionId,
    pos: Pos,
    radius: f64,
    budget_bytes: Option<usize>,
    filter: std::collections::BTreeMap<String, Value>,
    every: u64,
    max_hz: Option<u32>,
    unreliable: bool,
    inside: Option<musubi_mesh::EntityId>,
}

/// Per-client delivery options kept outside the mesh `Interest`.
#[derive(Default)]
struct SubOpts {
    /// Position-only updates as unreliable sequenced frames.
    unreliable: bool,
    /// Entities whose latest position went out unreliably; re-sent reliably on the next keyframe.
    unconfirmed: std::collections::HashSet<musubi_mesh::EntityId>,
}

/// Of two copies of one entity exactly one wins. A copy a node owns live (its simulator put it
/// there, or a handover delivered it) beats a *provisional* one — restored from a dead host's
/// shard or kept after a cancelled handover, i.e. the ones that get announced. Two provisional
/// copies go to the lower node id. Both sides evaluate the same order, so neither a mutual drop
/// nor a mutual keep can happen.
fn copy_wins(mine_provisional: bool, me: Uuid, theirs_provisional: bool, them: Uuid) -> bool {
    match (mine_provisional, theirs_provisional) {
        (false, true) => true,
        (true, false) => false,
        _ => me < them,
    }
}

/// See `WorldRegistry::requeue_journal`: ops of a failed (not fenced) flush go back to the front
/// of the tail.
fn requeue_journal(w: &Mutex<WorldState>, ops: Vec<WorldOp>, err: &Error) {
    if matches!(err, Error::Unavailable(_)) {
        return;
    }
    let mut st = w.lock();
    let tail = std::mem::take(&mut st.journal);
    st.journal = ops;
    st.journal.extend(tail);
}

/// Registry ticks an adopted cell keeps re-announcing the entity ids restored from a dead shard.
const ADOPT_ANNOUNCE_TICKS: u32 = 60;
/// Every N of those ticks an announcement goes out (60, 40, 20 ticks left).
const ADOPT_ANNOUNCE_EVERY: u32 = 20;

pub struct WorldRegistry {
    /// Runtime the tick threads hand async work to (cell offload directory writes).
    rt: std::sync::OnceLock<tokio::runtime::Handle>,
    me: std::sync::OnceLock<std::sync::Weak<WorldRegistry>>,
    /// Control rounds left before a world may offload another cell.
    offload_cooldown: DashMap<String, u32>,
    node_id: Uuid,
    /// `[worlds] enabled`: off, every world operation is refused.
    enabled: std::sync::atomic::AtomicBool,
    /// This node has the `sim` role: it may home worlds, host cells and accept simulators.
    hosting: std::sync::atomic::AtomicBool,
    /// `node.cluster_group`: worlds of other clusters sharing the database are invisible here.
    group: String,
    db: Option<PgPool>,
    worlds: DashMap<String, Arc<Mutex<WorldState>>>,
    /// world → home node (cache of `musubi.worlds`)
    homes: DashMap<String, Uuid>,
    /// Fires after every registry tick with the tick number; streaming simulator sessions wake on it.
    ticks: tokio::sync::broadcast::Sender<u64>,
    tick_no: std::sync::atomic::AtomicU64,
    /// worlds that have a ticker thread (see `ensure_tickers`)
    ticking: Mutex<std::collections::HashSet<String>>,
    /// (hosted_only, home_node) of each local shard, mirrored so that `home_of` — called on
    /// every world subscription — does not queue behind a world's tick for two bools
    shard_home: DashMap<String, (bool, Option<Uuid>)>,
    /// interest updates waiting for the world's next tick (see `subscribe`)
    interest: DashMap<String, Mutex<Vec<PendingInterest>>>,
    /// Raft directory (`world:<name>` → home) when the cluster runs the QUIC bus.
    cluster: std::sync::OnceLock<Arc<musubi_cluster::Cluster>>,
    /// `node.region` when set explicitly (`set_region`); otherwise taken from the cluster handle.
    region: std::sync::OnceLock<String>,
    /// Outbox for `world.rehomed`.
    events: std::sync::OnceLock<Arc<crate::events::Events>>,
    /// Module runtime, for the `hook_mesh_place` placement hook of the `custom` strategy.
    runtime: std::sync::OnceLock<Arc<musubi_runtime::Runtime>>,
    /// (world, cell) → host node, cached from the directory.
    cell_hosts: DashMap<(String, CellId), Uuid>,
    /// Open cross-shard queries: req → where the shard answers are collected.
    queries: DashMap<Uuid, tokio::sync::mpsc::UnboundedSender<(Uuid, Vec<Value>)>>,
    /// Fencing term this node holds per world (`musubi.world_hosts`); absent = may not persist.
    host_terms: DashMap<String, i64>,
    /// Standby mirrors of cells hosted elsewhere: this node is one of their replicas.
    mirrors: DashMap<(String, CellId), Mirror>,
    /// Alive nodes other than this one (refreshed by the ticker); replicas are picked among them.
    peers: std::sync::RwLock<Vec<Uuid>>,
    /// (world, cell, previous host) shards already merged after a hand-back, so a later
    /// reconciliation round does not resurrect entities despawned since.
    adopted_from: DashSet<(String, CellId, Uuid)>,
    /// Entities restored from a dead host's shard, not yet announced to the other hosts of the
    /// world (they drop duplicates of copies that had handed over before the crash).
    adopted_pending: DashMap<(String, CellId), (Vec<EntityId>, u32)>,
    /// Entities this node holds provisionally (see `copy_wins`), for as long as they are announced.
    provisional: DashSet<(String, EntityId)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorldCmd {
    Subscribe {
        x: f64,
        y: f64,
        radius: f64,
        #[serde(default)]
        max_hz: Option<u32>,
        #[serde(default)]
        budget_bytes: Option<u32>,
        #[serde(default)]
        filter: Option<Value>,
        #[serde(default)]
        unreliable: bool,
        #[serde(default)]
        inside: Option<String>,
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

/// A standby copy of one cell hosted on `from`, mirrored from its replication stream.
struct Mirror {
    from: Uuid,
    /// Incarnation of `from` when it streamed: an older host (restarted node, late packets of a
    /// replaced host) cannot overwrite a newer mirror.
    epoch: i64,
    cache: musubi_mesh::GhostCache,
    /// Host tick the mirror is complete up to.
    tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReplicaInfo {
    pub cell: CellId,
    pub node: Uuid,
    /// Host tick the replica has acknowledged.
    pub tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldInfo {
    pub name: String,
    pub tick: u64,
    pub entities: usize,
    pub subscribers: usize,
    pub cells: Vec<(CellId, Uuid)>,
    pub orphaned: Vec<CellId>,
    /// Hibernated cells whose persistent entities live in `world_cold_cells`, not in memory.
    pub cold_cells: Vec<CellId>,
    /// Standby replicas of the cells hosted here and how far they have caught up.
    #[serde(default)]
    pub replicas: Vec<ReplicaInfo>,
    /// Cells hosted elsewhere that this node mirrors (`node` = their host).
    #[serde(default)]
    pub mirrors: Vec<ReplicaInfo>,
    pub sims: Vec<SimInfo>,
    pub config: WorldConfig,
}

pub fn to_wire(e: &Entity) -> WorldEntity {
    WorldEntity {
        id: e.id,
        x: e.pos.x,
        y: e.pos.y,
        components: e
            .components
            .iter()
            .map(|(k, c)| WorldComponent {
                name: k.as_str().into(),
                value: musubi_proto::JsonText(c.json()),
                version: c.version,
            })
            .collect(),
    }
}

fn ref_to_wire(e: musubi_mesh::EntityRef) -> WorldEntity {
    WorldEntity {
        id: e.id,
        x: e.pos.x,
        y: e.pos.y,
        components: e
            .components
            .into_iter()
            .map(|c| WorldComponent {
                name: c.name,
                value: musubi_proto::JsonText(c.json),
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
        super::hojournal::init(node_id);
        Self {
            node_id,
            enabled: std::sync::atomic::AtomicBool::new(true),
            hosting: std::sync::atomic::AtomicBool::new(true),
            group: "default".into(),
            db,
            worlds: DashMap::new(),
            homes: DashMap::new(),
            ticks: tokio::sync::broadcast::channel(16).0,
            tick_no: std::sync::atomic::AtomicU64::new(0),
            ticking: Mutex::new(Default::default()),
            shard_home: DashMap::new(),
            interest: DashMap::new(),
            cell_hosts: DashMap::new(),
            queries: DashMap::new(),
            mirrors: DashMap::new(),
            peers: std::sync::RwLock::new(Vec::new()),
            rt: std::sync::OnceLock::new(),
            me: std::sync::OnceLock::new(),
            offload_cooldown: DashMap::new(),
            host_terms: DashMap::new(),
            adopted_from: DashSet::new(),
            adopted_pending: DashMap::new(),
            provisional: DashSet::new(),
            cluster: std::sync::OnceLock::new(),
            region: std::sync::OnceLock::new(),
            events: std::sync::OnceLock::new(),
            runtime: std::sync::OnceLock::new(),
        }
    }

    pub fn set_events(&self, events: Arc<crate::events::Events>) {
        let _ = self.events.set(events);
    }

    pub fn set_runtime(&self, runtime: Arc<musubi_runtime::Runtime>) {
        let _ = self.runtime.set(runtime);
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = group.to_string();
        self
    }

    /// `[worlds] enabled` and whether this node carries the `sim` role.
    pub fn with_modes(self, enabled: bool, hosting: bool) -> Self {
        self.enabled
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
        self.hosting
            .store(hosting, std::sync::atomic::Ordering::Relaxed);
        self
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Homes worlds / hosts cells here only with the `sim` role.
    pub fn hosting(&self) -> bool {
        self.enabled() && self.hosting.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn check_enabled(&self) -> Result<()> {
        if self.enabled() {
            Ok(())
        } else {
            Err(Error::Unavailable(
                "worlds are disabled on this cluster ([worlds] enabled = false)".into(),
            ))
        }
    }

    fn check_hosting(&self) -> Result<()> {
        self.check_enabled()?;
        if self.hosting() {
            Ok(())
        } else {
            Err(Error::Unavailable(
                "this node has no `sim` role: it routes world traffic but hosts no cells".into(),
            ))
        }
    }

    pub fn set_cluster(&self, cluster: Arc<musubi_cluster::Cluster>) {
        let _ = self.cluster.set(cluster);
    }

    fn directory_key(name: &str) -> String {
        format!("world:{name}")
    }

    /// Directory key of one cell's host node: cells of a world may live on different nodes.
    fn cell_key(name: &str, cell: CellId) -> String {
        // base-grid cells keep the historical key; deeper quadtree cells carry their level
        if cell.level == 0 {
            format!("world:{name}:cell:{},{}", cell.x, cell.y)
        } else {
            format!("world:{name}:cell:{},{}@{}", cell.x, cell.y, cell.level)
        }
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
        if self.cell_hosts_in_db() {
            self.persist_cell_host(name, cell, host).await;
        }
        match host {
            Some(h) => {
                self.cell_hosts.insert((name.to_string(), cell), h);
                if h == self.node_id && !self.host_terms.contains_key(name) {
                    self.enroll_host(name).await?;
                }
            }
            None => {
                self.cell_hosts.remove(&(name.to_string(), cell));
            }
        }
        Ok(())
    }

    /// Postgres-bus mode (a cluster without the raft directory): `musubi.world_cell_hosts` is the
    /// record a cell assignment is recovered from when its one-shot bus announcement is lost.
    fn cell_hosts_in_db(&self) -> bool {
        self.db.is_some() && self.cluster.get().is_some_and(|c| c.raft_id() == 0)
    }

    async fn persist_cell_host(&self, name: &str, cell: CellId, host: Option<Uuid>) {
        let Some(db) = &self.db else { return };
        let r = match host {
            Some(h) => sqlx::query(
                "insert into musubi.world_cell_hosts (name, x, y, level, node_id)
                 values ($1, $2, $3, $4, $5)
                 on conflict (name, x, y, level)
                 do update set node_id = excluded.node_id, updated_at = now()",
            )
            .bind(name)
            .bind(cell.x)
            .bind(cell.y)
            .bind(cell.level as i16)
            .bind(h)
            .execute(db)
            .await,
            None => sqlx::query(
                "delete from musubi.world_cell_hosts where name = $1 and x = $2 and y = $3 and level = $4",
            )
            .bind(name)
            .bind(cell.x)
            .bind(cell.y)
            .bind(cell.level as i16)
            .execute(db)
            .await,
        };
        if let Err(e) = r {
            tracing::warn!(world = name, ?cell, error = %e, "cell host not persisted");
        }
    }

    async fn cell_host_from_db(&self, name: &str, cell: CellId) -> Option<Uuid> {
        let db = self.db.as_ref()?;
        for probe in std::iter::once(cell).chain(cell.ancestors()) {
            let r: Option<(Uuid,)> = sqlx::query_as(
                "select node_id from musubi.world_cell_hosts where name = $1 and x = $2 and y = $3 and level = $4",
            )
            .bind(name)
            .bind(probe.x)
            .bind(probe.y)
            .bind(probe.level as i16)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
            if let Some((h,)) = r {
                return Some(h);
            }
        }
        None
    }

    /// Postgres-bus mode: re-read `musubi.world_cell_hosts` for the worlds this node knows and
    /// the cells assigned to it, and apply what the bus announcement would have. `pg_notify` is
    /// fire-and-forget — a listener reconnect loses it — and a restarted process has no cache at
    /// all; without this the cell stays unhosted and every lease for it is redirected to the home.
    /// The cache is snapshotted before the read: an announcement that lands in between wins.
    pub async fn reconcile_cell_hosts(&self, fanout: &Fanout) {
        if !self.cell_hosts_in_db() {
            return;
        }
        let Some(db) = &self.db else { return };
        let mut known: Vec<String> = self.worlds.iter().map(|e| e.key().clone()).collect();
        known.extend(self.cell_hosts.iter().map(|e| e.key().0.clone()));
        known.sort();
        known.dedup();
        let before: HashMap<(String, CellId), Uuid> = self
            .cell_hosts
            .iter()
            .map(|e| (e.key().clone(), *e.value()))
            .collect();
        let rows: Vec<(String, i32, i32, i16, Uuid)> = match sqlx::query_as(
            "select name, x, y, level, node_id from musubi.world_cell_hosts
             where node_id = $1 or name = any($2)",
        )
        .bind(self.node_id)
        .bind(&known)
        .fetch_all(db)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "cell hosts not reconciled");
                return;
            }
        };
        for (name, x, y, level, node) in rows {
            let cell = CellId {
                x,
                y,
                level: level as u8,
                z: 0,
            };
            let key = (name.clone(), cell);
            let seen = before.get(&key).copied();
            if seen == Some(node) || self.cell_host_cached(&name, cell) != seen {
                continue;
            }
            metrics::counter!("musubi_world_cell_hosts_reconciled_total").increment(1);
            tracing::info!(world = %name, ?cell, %node, "cell assignment recovered from the database");
            fanout.apply_cell_assign(name, cell, node);
        }
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
            hoj(
                name,
                id,
                st.world.tick,
                "shipped",
                "forced",
                format!("cell {cell:?} to node {node}"),
            );
            let inputs = st.world.take_buffered_inputs(id);
            st.world.forget_for_clients(id);
            // Keep our copy in transit: the new host's ack despawns it and the tick re-ships
            // it on timeout, so a shipment the receiver drops is not lost.
            if st.world.begin_remote_handover(id, node) {
                st.remote_transit.insert(id, (node, 1, cell));
            } else {
                st.world.despawn(id);
            }
            e.transit = None;
            fanout.world_handover(name, e, self.node_id, inputs, true, None);
        }
        if let Some(sim) = st.world.revoke_lease(cell) {
            push_event(
                st,
                sim,
                SimEvent::<WorldEntity>::CellReleased {
                    cell,
                    node,
                    entities: n,
                },
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
                let st = w.lock();
                st.hosted_only
                    && st.remote_transit.is_empty()
                    && !self
                        .cell_hosts
                        .iter()
                        .any(|r| r.key().0 == name && *r.value() == self.node_id)
            }
            None => false,
        };
        if idle {
            self.shard_home.remove(name);
            self.interest.remove(name);
            if let Some((_, w)) = self.worlds.remove(name) {
                w.lock().journal.clear();
            }
            self.homes.remove(name);
            tracing::info!(world = name, "idle hosted shard dropped");
        }
        idle
    }

    /// Cell containing `pos` under this world's grid (needs a local copy of the world).
    /// The cell a handed-over entity lands in: a pin this node holds for it, else the cell the
    /// sender's transit named (a pin on the sender's side — a co-located body whose position
    /// is still in another cell), else the cell its position is in.
    pub fn handover_target_cell(&self, name: &str, e: &Entity) -> Result<CellId> {
        let w = self.get(name)?;
        let st = w.lock();
        Ok(st
            .world
            .pinned_to(e.id)
            .map(|(c, _)| c)
            .or_else(|| e.transit.as_ref().map(|t| t.to_cell))
            .unwrap_or_else(|| st.world.cell_of(e.pos)))
    }

    pub fn cell_of(&self, name: &str, pos: Pos) -> Result<CellId> {
        let w = self.get(name)?;
        let st = w.lock();
        Ok(st.world.cell_of(pos))
    }

    /// `cell_of` for a world this node may not hold a shard of: the grid comes from
    /// `musubi.worlds` then. A simulator redirected here for a cell the directory assigned to this
    /// node can arrive before (or without) the bus announcement; answering "not my world, go
    /// home" sent it round in circles, resolving the cell lets `ensure_hosting` take the cell.
    pub async fn cell_of_any(&self, name: &str, pos: Pos) -> Result<CellId> {
        if let Ok(c) = self.cell_of(name, pos) {
            return Ok(c);
        }
        let Some(db) = &self.db else {
            return Err(Error::NotFound(format!("world {name}")));
        };
        let row: Option<(Value,)> = sqlx::query_as(
            "select config from musubi.worlds where name = $1 and cluster_group = $2",
        )
        .bind(name)
        .bind(&self.group)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?;
        let (c,) = row.ok_or_else(|| Error::NotFound(format!("world {name}")))?;
        let cfg: WorldConfig = serde_json::from_value(c).map_err(Error::internal)?;
        Ok(World::new(cfg).cell_of(pos))
    }

    /// Remember another node's cell assignment (no directory write; the coordinator did that).
    pub fn note_cell_host(&self, name: &str, cell: CellId, node: Uuid) {
        self.cell_hosts.insert((name.to_string(), cell), node);
    }

    /// This node takes `cell` of `name`: make sure a local shard of the world exists (config from
    /// `musubi.worlds`, defaults without a database); simulators lease the cell here as usual.
    /// The directory names this node as `cell`'s host: make sure the shard is up and the cell
    /// hosted before serving a request for it. The bus announcement (`WorldCellAssign`) does the
    /// same asynchronously; a simulator redirected here can arrive first, and answering it with
    /// a redirect back to the home (which redirects here again) was an endless loop.
    pub async fn ensure_hosting(&self, name: &str, cell: CellId) -> Result<()> {
        let hosted = self
            .worlds
            .get(name)
            .is_some_and(|w| self.hosts_cell(&w.lock(), name, cell));
        if hosted {
            // the shard may be up before its hosting finished enrolling (the assignment is
            // cached first): a lease served here must be able to persist, so enrol now
            if self.db.is_some() && !self.host_terms.contains_key(name) {
                self.enroll_host(name).await?;
            }
            return Ok(());
        }
        self.host_cell(name, cell).await.map(|_| ())
    }

    pub async fn host_cell(&self, name: &str, cell: CellId) -> Result<Vec<CellId>> {
        self.check_hosting()?;
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
                    ghost_send: HashMap::new(),
                    repl_send: HashMap::new(),
                    repl_targets: HashMap::new(),
                    repl_acks: HashMap::new(),
                    full_resend: std::collections::HashSet::new(),
                    txs: HashMap::new(),
                    tx_locks: HashMap::new(),
                    hibernated_since: HashMap::new(),
                    loading: std::collections::HashSet::new(),
                    sub_opts: HashMap::new(),
                    sub_cmds: HashMap::new(),
                    hosted_only: true,
                    home_node: home,
                    remote_transit: HashMap::new(),
                    kept_after_cancel: HashMap::new(),
                    interaction_seq: HashMap::new(),
                    contacts: HashMap::new(),
                    admission: HashMap::new(),
                }))
            });
            self.note_home(name, true, home);
        }
        self.cell_hosts
            .insert((name.to_string(), cell), self.node_id);
        if !self.host_terms.contains_key(name) {
            self.enroll_host(name).await?;
        }
        Ok(vec![cell])
    }

    /// Boot: cells the directory still assigns to this node (we restarted before the home
    /// re-homed them) go back to their home, which merges our persisted shard from the database.
    /// Resuming them from memory would race the home's dead-host recovery into two live owners.
    pub async fn recover_hosted(&self, fanout: &Fanout) -> Result<Vec<(String, CellId)>> {
        if !self.hosting() {
            return Ok(Vec::new());
        }
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
            let (y, level) = y.split_once('@').unwrap_or((y, "0"));
            let (Ok(x), Ok(y), Ok(level)) =
                (x.parse::<i32>(), y.parse::<i32>(), level.parse::<u8>())
            else {
                continue;
            };
            if v["node_id"].as_str().and_then(|s| Uuid::parse_str(s).ok()) != Some(self.node_id) {
                continue;
            }
            let cell = CellId { x, y, level, z: 0 };
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

    /// Whether this node hosts `cell` by the cached directory: an assigned host, or the home for
    /// an unassigned cell.
    pub fn hosts_cell_cached(&self, name: &str, cell: CellId) -> bool {
        match self.cell_host_cached(name, cell) {
            Some(h) => h == self.node_id,
            None => self.worlds.get(name).is_some_and(|w| !w.lock().hosted_only),
        }
    }

    /// Cached host of `cell` (no directory lookup).
    pub fn cell_host_cached(&self, name: &str, cell: CellId) -> Option<Uuid> {
        self.cell_hosts.get(&(name.to_string(), cell)).map(|h| *h)
    }

    /// A cell just moved to us from `prev`: its persisted state (snapshot + journal of that
    /// node's shard) merges into our copy — the forced handovers over the bus are the fast path,
    /// this is the one that cannot drop entities.
    pub async fn adopt_cell_from(&self, name: &str, cell: CellId, prev: Uuid) -> Result<usize> {
        self.check_hosting()?;
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
            let ids: Vec<EntityId> = ents.iter().map(|e| e.id).collect();
            st.world.restore(ents);
            if !ids.is_empty() {
                self.queue_adopted(name, ids.iter().map(|id| (*id, cell)));
            }
            n
        };
        self.adopted_from.insert((name.to_string(), cell, prev));
        if n > 0 || replayed > 0 {
            tracing::info!(world = name, ?cell, %prev, entities = n, ops = replayed, "cell adopted from previous host");
        }
        let mut keep = self.assigned_hosts(name).await;
        if !keep.contains(&prev) {
            keep.push(self.node_id);
            if let Err(e) = self.fence_hosts(name, &keep).await {
                tracing::warn!(world = name, %prev, error = %e, "previous host not fenced");
            }
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
                let mut h = None;
                for probe in std::iter::once(cell).chain(cell.ancestors()) {
                    h = c
                        .directory_get(&Self::cell_key(name, probe))
                        .await
                        .and_then(|v| v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()));
                    if h.is_some() {
                        break;
                    }
                }
                if h.is_none() && !c.is_raft_leader() {
                    // a follower's replica can lag the assignment the home just wrote: the home
                    // redirects a simulator here, a stale "no host" sends it back to the home,
                    // and so on until the client gives up ("too many redirects") — ask the leader
                    h = c
                        .directory_get_fresh(&Self::cell_key(name, cell))
                        .await
                        .and_then(|v| v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()));
                }
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
        if self.cell_hosts_in_db() {
            if let Some(h) = self.cell_host_from_db(name, cell).await {
                self.cell_hosts.insert(key, h);
                return Ok(Some(h));
            }
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

    /// Take (or keep) this node's fencing term for `name`. Called only where ownership was just
    /// won through the quorum (world creation, a directory write that assigned us a cell, a home
    /// claim in `recover`), never lazily: a fenced-out node must not re-enroll itself.
    async fn enroll_host(&self, name: &str) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let (term,): (i64,) = sqlx::query_as(
            "insert into musubi.world_hosts (name, node_id, term)
             values ($1, $2, coalesce((select max(term) from musubi.world_hosts where name = $1), 0) + 1)
             on conflict (name, node_id) do update set term = musubi.world_hosts.term
             returning term",
        )
        .bind(name)
        .bind(self.node_id)
        .fetch_one(db)
        .await
        .map_err(Error::internal)?;
        self.host_terms.insert(name.to_string(), term);
        Ok(())
    }

    /// Fence every other host of `name` that the directory no longer assigns anything to: their
    /// later snapshot / journal writes are rejected. `live` are the nodes still hosting cells.
    async fn fence_hosts(&self, name: &str, live: &[Uuid]) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let n = sqlx::query(
            "delete from musubi.world_hosts where name = $1 and node_id <> $2 and node_id <> all($3)",
        )
        .bind(name)
        .bind(self.node_id)
        .bind(live)
        .execute(db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n > 0 {
            tracing::info!(world = name, fenced = n, "previous hosts fenced");
        }
        Ok(())
    }

    /// The term to persist under, or an error when this node is not an enrolled host of `name`.
    fn host_term(&self, name: &str) -> Result<i64> {
        self.host_terms
            .get(name)
            .map(|t| *t)
            .ok_or_else(|| Error::Unavailable(format!("not an enrolled host of world {name}")))
    }

    /// Snapshot of this node's shard of `name` (cells of a world may live on several nodes, each
    /// writes its own row) and the journal chunks it supersedes.
    async fn write_snapshot(
        db: &PgPool,
        name: &str,
        node: Uuid,
        term: i64,
        entities: Vec<musubi_mesh::Entity>,
        tick: u64,
    ) -> Result<()> {
        // `where exists` is the fence: once the row in world_hosts is gone (or carries a newer
        // term) nothing of this node's reaches the table any more
        let n = sqlx::query(
            "insert into musubi.world_snapshots (name, node_id, entities, tick, updated_at, node_term)
             select $1, $2, $3, $4, now(), $5
             where exists (select 1 from musubi.world_hosts h where h.name = $1 and h.node_id = $2 and h.term = $5)
             on conflict (name, node_id) do update set entities = excluded.entities, tick = excluded.tick, updated_at = now(), node_term = excluded.node_term",
        )
        .bind(name)
        .bind(node)
        .bind(serde_json::to_value(&entities)?)
        .bind(tick as i64)
        .bind(term)
        .execute(db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::Unavailable(format!(
                "fenced: node {node} no longer hosts world {name} (term {term})"
            )));
        }
        sqlx::query(
            "insert into musubi.world_history (name, node_id, tick, entities, node_term) values ($1, $2, $3, $4, $5)
             on conflict (name, node_id, tick) do nothing",
        )
        .bind(name)
        .bind(node)
        .bind(tick as i64)
        .bind(serde_json::to_value(&entities)?)
        .bind(term)
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

    /// Journaled ops of a world (flushed rows plus this node's unflushed tail) in `[from, to]`,
    /// optionally for one entity, each with the node that wrote it and its provenance — the
    /// audit trail for "who moved this entity and when" while debugging meshing.
    pub async fn history(
        &self,
        name: &str,
        entity: Option<Uuid>,
        from: u64,
        to: u64,
        limit: usize,
    ) -> Result<Value> {
        let Some(db) = &self.db else {
            return Err(Error::Internal(
                "no database: world history is not kept".into(),
            ));
        };
        let rows: Vec<(Option<Uuid>, Value)> = sqlx::query_as(
            "select node_id, entries from musubi.world_journal where name = $1 and tick_to >= $2 and tick_from <= $3 order by tick_from",
        )
        .bind(name)
        .bind(from.min(i64::MAX as u64) as i64)
        .bind(to.min(i64::MAX as u64) as i64)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut ops: Vec<(u64, Uuid, WorldOp)> = Vec::new();
        for (node, v) in rows {
            if let Ok(o) = serde_json::from_value::<Vec<WorldOp>>(v) {
                let node = node.unwrap_or(Uuid::nil());
                ops.extend(o.into_iter().map(|op| (op.tick, node, op)));
            }
        }
        if let Ok(w) = self.get(name) {
            let st = w.lock();
            ops.extend(
                st.journal
                    .iter()
                    .cloned()
                    .map(|op| (op.tick, self.node_id, op)),
            );
        }
        ops.retain(|(t, _, op)| {
            *t >= from && *t <= to && entity.is_none_or(|e| op_entity(&op.op) == e)
        });
        ops.sort_by_key(|(t, _, _)| *t);
        let total = ops.len();
        ops.truncate(limit.clamp(1, 5000));
        let ops: Vec<Value> = ops
            .into_iter()
            .map(|(tick, node, op)| {
                json!({ "tick": tick, "node_id": node, "entity": op_entity(&op.op), "source": op.source, "op": op.op })
            })
            .collect();
        Ok(json!({ "world": name, "from_tick": from, "to_tick": to, "total": total, "ops": ops }))
    }

    /// Determinism check: flush this node's journal, then rebuild the world at the current tick
    /// from snapshots + journal (`entities_at`) and diff it against the live persistent
    /// entities. Any difference means an unjournaled mutation or a non-deterministic replay.
    pub async fn verify(&self, name: &str) -> Result<Value> {
        let Some(db) = &self.db else {
            return Err(Error::Internal(
                "no database: world history is not kept".into(),
            ));
        };
        let term = self.host_term(name)?;
        let (ops, tick, live) = {
            let w = self.get(name)?;
            let mut st = w.lock();
            (
                std::mem::take(&mut st.journal),
                st.world.tick,
                st.world.snapshot_persistent(),
            )
        };
        if let Err(e) = Self::write_journal(db, name, self.node_id, term, &ops, tick).await {
            self.requeue_journal(name, ops, &e);
            return Err(e);
        }
        let rebuilt = self.entities_at(name, tick).await?;
        let rebuilt: Vec<musubi_mesh::Entity> =
            serde_json::from_value(rebuilt["entities"].clone()).map_err(Error::internal)?;
        let live_by: HashMap<Uuid, Value> = live
            .iter()
            .map(|e| (e.id, serde_json::to_value(e).unwrap_or(Value::Null)))
            .collect();
        let rebuilt_by: HashMap<Uuid, Value> = rebuilt
            .iter()
            .map(|e| (e.id, serde_json::to_value(e).unwrap_or(Value::Null)))
            .collect();
        let mut missing_in_rebuilt: Vec<Uuid> = live_by
            .keys()
            .filter(|id| !rebuilt_by.contains_key(id))
            .copied()
            .collect();
        let mut missing_in_live: Vec<Uuid> = rebuilt_by
            .keys()
            .filter(|id| !live_by.contains_key(id))
            .copied()
            .collect();
        missing_in_rebuilt.sort();
        missing_in_live.sort();
        let mut differing: Vec<Value> = live_by
            .iter()
            .filter_map(|(id, l)| {
                let r = rebuilt_by.get(id)?;
                (l != r).then(|| json!({ "id": id, "live": l, "rebuilt": r }))
            })
            .collect();
        differing.sort_by_key(|d| d["id"].as_str().unwrap_or("").to_string());
        let ok =
            missing_in_rebuilt.is_empty() && missing_in_live.is_empty() && differing.is_empty();
        Ok(json!({
            "world": name, "tick": tick, "ok": ok,
            "live": live_by.len(), "rebuilt": rebuilt_by.len(),
            "missing_in_rebuilt": missing_in_rebuilt, "missing_in_live": missing_in_live,
            "differing": differing,
        }))
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
        let cold: Vec<(i32, i32, i32)> =
            sqlx::query_as("select x, y, level from musubi.world_cold_cells where name = $1")
                .bind(name)
                .fetch_all(db)
                .await
                .map_err(Error::internal)?;
        for (x, y, level) in cold {
            world.mark_unloaded(CellId {
                x,
                y,
                level: level as u8,
                z: 0,
            });
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
        if c.raft_id() == 0 {
            // Postgres-bus mode: the rows are the record (written before any announcement);
            // the local cache may not have seen an assignment yet and must not fence its host
            let Some(db) = &self.db else { return vec![] };
            return sqlx::query_scalar::<_, Uuid>(
                "select distinct node_id from musubi.world_cell_hosts where name = $1",
            )
            .bind(name)
            .fetch_all(db)
            .await
            .unwrap_or_default();
        }
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
            Self::write_snapshot(
                db,
                name,
                self.node_id,
                self.host_term(name)?,
                entities,
                tick,
            )
            .await?;
        }
        Ok(tick)
    }

    /// Adopt worlds whose home node is dead (or gone from the node table): claim the directory row
    /// atomically, rebuild the world from its config and the last snapshot. Returns adopted names.
    pub async fn recover(&self, dead_nodes: &[Uuid]) -> Result<Vec<String>> {
        if !self.hosting() {
            return Ok(Vec::new());
        }
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
                    or (w.node_id = $2 and w.node_epoch <> $4)
                    or not exists (select 1 from musubi.nodes n where n.id = w.node_id)
                    or exists (select 1 from musubi.nodes n where n.id = w.node_id and n.epoch > w.node_epoch
                                 and n.started_at < now() - interval '10 seconds'))
               and (w.node_id is null or not (w.node_id = $2 and $2 <> all($1) and w.node_epoch = $4))
               and (w.node_id is null or w.node_id = $2 or w.node_id = any($1) or not exists
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
                    self.queue_adopted(&name, ents.iter().map(|e| (e.id, e.cell)));
                    st.world.restore(ents);
                    st.hosted_only = false;
                    st.home_node = None;
                    self.note_home(&name, false, None);
                    tracing::info!(world = %name, merged = n, ops = replayed, home_tick = tick, "hosted shard promoted to home");
                }
                self.homes.insert(name.clone(), self.node_id);
                self.publish_home(&name, Some(self.node_id)).await;
                self.enroll_host(&name).await?;
                let live: Vec<Uuid> = assigned.get(&name).cloned().unwrap_or_default().into_iter().filter(|h| is_live(h) && !dead_nodes.contains(h)).collect();
                self.fence_hosts(&name, &live).await?;
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
            // everything here came from shards of nodes that are gone: a live host may still own
            // an entity that handed over before the crash
            self.queue_adopted(&name, world.snapshot_persistent().iter().map(|e| (e.id, e.cell)));
            if replayed > 0 {
                tracing::info!(world = %name, ops = replayed, tick = world.tick, "world journal replayed");
            }
            self.note_home(&name, false, None);
            self.worlds.insert(
                name.clone(),
                Arc::new(Mutex::new(WorldState {
                    world,
                    controller: Controller::new(ctl),
                    subs: HashMap::new(),
                    sim_events: HashMap::new(),
                    journal: Vec::new(),
                remote_ghosts: HashMap::new(),
                ghost_send: HashMap::new(),
                repl_send: HashMap::new(),
                repl_targets: HashMap::new(),
                repl_acks: HashMap::new(),
                full_resend: std::collections::HashSet::new(),
                txs: HashMap::new(),
                tx_locks: HashMap::new(),
                hibernated_since: HashMap::new(),
                loading: std::collections::HashSet::new(),
            sub_opts: HashMap::new(),
                    sub_cmds: HashMap::new(),
                    hosted_only: false,
                    home_node: None,
                    remote_transit: HashMap::new(),
 kept_after_cancel: HashMap::new(),
            interaction_seq: HashMap::new(),
            contacts: HashMap::new(),
            admission: HashMap::new(),
                })),
            );
            self.homes.insert(name.clone(), self.node_id);
            self.publish_home(&name, Some(self.node_id)).await;
            self.enroll_host(&name).await?;
            self.fence_hosts(&name, &live).await?;
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
                self.shard_home.remove(&name);
                self.interest.remove(&name);
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
                self.note_home(name, true, Some(h));
            }
        }
        Some(h)
    }

    fn note_home(&self, name: &str, hosted_only: bool, home: Option<Uuid>) {
        self.shard_home
            .insert(name.to_string(), (hosted_only, home));
    }

    pub async fn home_of(&self, name: &str) -> Result<Option<Uuid>> {
        // a local shard makes us the home only if it is not merely hosting assigned cells
        if let Some(m) = self.shard_home.get(name) {
            let (hosted_only, home) = *m;
            if !hosted_only {
                return Ok(Some(self.node_id));
            }
            if let Some(h) = home {
                return Ok(Some(h));
            }
        } else if let Some(w) = self.worlds.get(name) {
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
                            self.note_home(name, true, Some(h));
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
                    self.note_home(name, true, Some(h));
                }
            }
        }
        Ok(row)
    }

    /// This node keeps a shard of `name` (its home shard or an assigned cell's).
    /// Re-publish every live subscription of a world this node is home of (`WorldCmd::Subscribe`
    /// per session): a shard that came up on another node after the players subscribed applies
    /// them and starts sending deltas for the cells it hosts.
    pub fn replay_subs(&self, name: &str, fanout: &Fanout) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let cmds: Vec<WorldCmdEnv> = {
            let st = w.lock();
            if st.hosted_only {
                return;
            }
            st.sub_cmds
                .iter()
                .map(|(sid, cmd)| WorldCmdEnv {
                    world: name.to_string(),
                    session_id: *sid,
                    cmd: cmd.clone(),
                })
                .collect()
        };
        // one bus message, not one per player: with 2 000 subscribers a replay per offloaded
        // cell on the control lane put the handovers seconds behind it
        fanout.world_cmds(cmds);
    }

    pub fn has_shard(&self, name: &str) -> bool {
        self.worlds.contains_key(name)
    }

    /// Run a client command on the home node; errors go back to the session as messages. An input
    /// for an entity that lives in a cell hosted elsewhere is forwarded over the bus.
    pub fn exec(&self, env: WorldCmdEnv, fanout: &Fanout) -> Option<ServerMessage> {
        if let Err(e) = self.check_enabled() {
            return Some(ServerMessage::error(&e));
        }
        let r = match env.cmd {
            WorldCmd::Subscribe {
                x,
                y,
                radius,
                max_hz,
                budget_bytes,
                filter,
                unreliable,
                inside,
            } => self.subscribe(
                &env.world,
                env.session_id,
                Pos { x, y, z: 0.0 },
                radius,
                budget_bytes.map(|b| b as usize),
                filter,
                max_hz,
                unreliable,
                inside,
            ),
            WorldCmd::Unsubscribe => self.unsubscribe(&env.world, env.session_id),
            WorldCmd::Input { entity, input } => {
                match self.input(&env.world, env.session_id, &entity, input.clone()) {
                    Err(Error::NotFound(_)) if self.is_split(&env.world) => {
                        match parse_entity(&entity) {
                            Ok(id) => {
                                let ev = SimEvent::<WorldEntity>::Input {
                                    entity: id,
                                    session: json!(env.session_id),
                                    input,
                                }
                                .to_value();
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
        if !self.hosting() {
            return Ok(Vec::new());
        }
        let orphaned: Vec<(String, CellId, Uuid)> = self
            .cell_hosts
            .iter()
            .filter(|e| dead_nodes.contains(e.value()))
            .map(|e| (e.key().0.clone(), e.key().1, *e.value()))
            .collect();
        let mut out = Vec::new();
        let mut restored: Vec<(String, Uuid)> = Vec::new();
        let mut mirrored_cells: Vec<(String, CellId, u64)> = Vec::new();
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
                    .filter(|(_, (h, _, _))| *h == dead)
                    .map(|(id, _)| *id)
                    .collect();
                for id in stuck {
                    st.remote_transit.remove(&id);
                    if let Some(t) = st.world.cancel_handover(id) {
                        push_event(
                            st,
                            t.from,
                            SimEvent::<WorldEntity>::HandoverCancelled {
                                entity: id,
                                reason: "target node died; cell re-homed".into(),
                            },
                        );
                    }
                }
            }
            // a mirror of the cell fed by the dead host up to its last tick beats the shard
            // restore: nothing to read, no ticks lost since the last journal flush, ephemeral
            // entities included
            let mut mirrored = None;
            if let Some((_, mut m)) = self.mirrors.remove(&(name.clone(), cell)) {
                if m.from == dead {
                    if let Ok(w) = self.get(&name) {
                        let mut st = w.lock();
                        let mut ents = m.cache.drain();
                        ents.retain(|e| st.world.get(e.id).is_none());
                        let n = ents.len();
                        let adopted: Vec<EntityId> = ents.iter().map(|e| e.id).collect();
                        st.world.restore(ents);
                        if !adopted.is_empty() {
                            self.queue_adopted(&name, adopted.iter().map(|id| (*id, cell)));
                        }
                        tracing::info!(world = %name, ?cell, node = %dead, tick = m.tick, entities = n, "cell taken over from its replica mirror");
                        mirrored = Some(m.tick);
                    }
                }
            }
            if let Some(tick) = mirrored {
                mirrored_cells.push((name.clone(), cell, tick));
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
                        // a cell restored from a mirror at least as fresh as the shard is
                        // complete; a mirror that fell behind the host's last snapshot misses
                        // the entities the host accepted after the mirror's tick — those come
                        // from the shard (and go through the duplicate check like the rest)
                        ents.retain(|e| {
                            !mirrored_cells
                                .iter()
                                .any(|(n, c, t)| *n == name && *c == e.cell && *t >= shard.tick)
                        });
                        let adopted: Vec<(EntityId, CellId)> =
                            ents.iter().map(|e| (e.id, e.cell)).collect();
                        st.world.restore(ents);
                        if !adopted.is_empty() {
                            self.queue_adopted(&name, adopted.iter().copied());
                        }
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
        if let Some(r) = self.region.get() {
            return r.clone();
        }
        self.cluster
            .get()
            .map(|c| c.me().region.clone())
            .unwrap_or_default()
    }

    /// Pin this node's region (nodes without a cluster handle, e.g. tests); first call wins.
    pub fn set_region(&self, region: &str) {
        let _ = self.region.set(region.to_string());
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
        self.check_enabled()?;
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
            let elsewhere = !self.hosting() || (!region.is_empty() && region != self.region());
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
            self.enroll_host(name).await?;
        }
        self.note_home(name, false, None);
        self.worlds.insert(
            name.to_string(),
            Arc::new(Mutex::new(WorldState {
                world: World::new(cfg),
                controller: Controller::new(ctl),
                subs: HashMap::new(),
                sim_events: HashMap::new(),
                journal: Vec::new(),
                remote_ghosts: HashMap::new(),
                ghost_send: HashMap::new(),
                repl_send: HashMap::new(),
                repl_targets: HashMap::new(),
                repl_acks: HashMap::new(),
                full_resend: std::collections::HashSet::new(),
                txs: HashMap::new(),
                tx_locks: HashMap::new(),
                hibernated_since: HashMap::new(),
                loading: std::collections::HashSet::new(),
                sub_opts: HashMap::new(),
                sub_cmds: HashMap::new(),
                hosted_only: false,
                home_node: None,
                remote_transit: HashMap::new(),
                kept_after_cancel: HashMap::new(),
                interaction_seq: HashMap::new(),
                contacts: HashMap::new(),
                admission: HashMap::new(),
            })),
        );
        self.publish_home(name, Some(self.node_id)).await;
        self.info(name).map(Some)
    }

    pub async fn delete(&self, name: &str, fanout: Option<&Fanout>) -> Result<()> {
        self.worlds
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| Error::NotFound(format!("world {name}")))?;
        self.homes.remove(name);
        // the shards hosting this world's cells on other nodes only learn of the deletion from
        // us: the stand's peer kept the shards of every deleted bench world (198 of them),
        // ticking and answering handovers for worlds the home no longer had
        let cells: Vec<CellId> = self
            .cell_hosts
            .iter()
            .filter(|e| e.key().0 == name)
            .map(|e| e.key().1)
            .collect();
        self.cell_hosts.retain(|k, _| k.0 != name);
        if let Some(c) = self.cluster.get() {
            if c.raft_id() != 0 {
                for cell in cells {
                    let _ = c
                        .directory_write(musubi_cluster::DirectoryOp::Delete {
                            key: Self::cell_key(name, cell),
                        })
                        .await;
                }
            }
        }
        if let Some(f) = fanout {
            f.world_deleted(name);
        }
        if let Some(db) = &self.db {
            sqlx::query("delete from musubi.worlds where name = $1 and node_id = $2")
                .bind(name)
                .bind(self.node_id)
                .execute(db)
                .await
                .map_err(Error::internal)?;
            sqlx::query("delete from musubi.world_cold_cells where name = $1")
                .bind(name)
                .execute(db)
                .await
                .map_err(Error::internal)?;
        }
        self.publish_home(name, None).await;
        Ok(())
    }

    /// The home deleted `name`: drop our shard of it and everything we remember about its cells.
    pub fn forget_world(&self, name: &str) {
        if self.worlds.remove(name).is_some() {
            tracing::info!(world = name, "shard dropped: world deleted by its home");
        }
        self.homes.remove(name);
        self.cell_hosts.retain(|k, _| k.0 != name);
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
            cold_cells: st.world.unloaded_cells(),
            replicas: {
                let mut v: Vec<ReplicaInfo> = st
                    .repl_acks
                    .iter()
                    .flat_map(|(c, acks)| {
                        acks.iter().map(|(n, t)| ReplicaInfo {
                            cell: *c,
                            node: *n,
                            tick: *t,
                        })
                    })
                    .collect();
                v.sort_by_key(|r| (r.cell.x, r.cell.y, r.node));
                v
            },
            mirrors: {
                let mut v: Vec<ReplicaInfo> = self
                    .mirrors
                    .iter()
                    .filter(|m| m.key().0 == name)
                    .map(|m| ReplicaInfo {
                        cell: m.key().1,
                        node: m.value().from,
                        tick: m.value().tick,
                    })
                    .collect();
                v.sort_by_key(|r| (r.cell.x, r.cell.y));
                v
            },
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
        self.check_hosting()?;
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
                    "remote_to": st.remote_transit.get(&e.id).map(|(h, _, _)| *h),
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

    fn entity_json(e: &musubi_mesh::Entity, remote_to: Option<Uuid>) -> Value {
        json!({
            "id": e.id, "cell": e.cell, "pos": e.pos, "persistent": e.persistent,
            "global": e.global, "attached_to": e.attached_to, "contained": e.contained,
            "rev": e.rev, "transit": e.transit, "remote_to": remote_to,
            "components": e.components.iter().map(|(k, c)| (k.clone(), c.value.clone())).collect::<serde_json::Map<_, _>>(),
        })
    }

    /// Answer `kind` against one mesh (a live shard or cold cells loaded into a scratch mesh):
    /// `Entity` → zero or one record `{entity, path_to_root, children, contents, capacity}`,
    /// `Children`/`Find` → one record per entity.
    fn answer_in(
        world: &musubi_mesh::World,
        kind: &QueryKind,
        remote_to: &HashMap<EntityId, Uuid>,
    ) -> Vec<Value> {
        let ej = |e: &musubi_mesh::Entity| Self::entity_json(e, remote_to.get(&e.id).copied());
        match kind {
            QueryKind::Entity { id } => {
                let Some(e) = world.get(*id) else {
                    return Vec::new();
                };
                let children: Vec<Value> = world
                    .children_of(*id, 1)
                    .into_iter()
                    .filter_map(|(c, _)| world.get(c))
                    .map(ej)
                    .collect();
                vec![json!({
                    "entity": ej(e),
                    "path_to_root": world.path_to_root(*id),
                    "children": children,
                    "contents": world.contents(*id),
                    "capacity": world.container_capacity(*id),
                })]
            }
            QueryKind::Children { id, depth } => world
                .children_of(*id, *depth)
                .into_iter()
                .filter_map(|(c, d)| world.get(c).map(|e| (e, d)))
                .map(|(e, d)| {
                    let mut v = ej(e);
                    v["depth"] = json!(d);
                    v
                })
                .collect(),
            QueryKind::Find { q } => world.find(q).into_iter().map(ej).collect(),
        }
    }

    /// What this shard knows (`None` = no shard of the world here).
    fn local_answer(&self, name: &str, kind: &QueryKind) -> Option<Vec<Value>> {
        let w = self.worlds.get(name)?;
        let st = w.lock();
        let remote: HashMap<EntityId, Uuid> = st
            .remote_transit
            .iter()
            .map(|(id, (h, _, _))| (*id, *h))
            .collect();
        Some(Self::answer_in(&st.world, kind, &remote))
    }

    /// Persistent entities of hibernated cells, answered from the database through a scratch
    /// mesh with the world's own config (so cells match); empty without a database.
    async fn cold_answer(&self, name: &str, kind: &QueryKind) -> Result<Vec<Value>> {
        let Some(db) = self.db.as_ref() else {
            return Ok(Vec::new());
        };
        let cfg = match self.worlds.get(name) {
            Some(w) => w.lock().world.cfg.clone(),
            None => musubi_mesh::WorldConfig::default(),
        };
        let rows: Vec<(Value,)> =
            sqlx::query_as("select entities from musubi.world_cold_cells where name = $1")
                .bind(name)
                .fetch_all(db)
                .await
                .map_err(Error::internal)?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut scratch = musubi_mesh::World::new(cfg);
        for (v,) in rows {
            let ents: Vec<Entity> = serde_json::from_value(v)?;
            scratch.restore(ents);
        }
        Ok(Self::answer_in(&scratch, kind, &HashMap::new()))
    }

    /// A shard was asked (`WorldQuery`): answer with what it holds, unless it is the asker.
    pub fn answer_query(
        &self,
        name: &str,
        req: Uuid,
        from: Uuid,
        kind: &QueryKind,
        fanout: &Fanout,
    ) {
        if from == self.node_id {
            return;
        }
        if let Some(ents) = self.local_answer(name, kind) {
            fanout.world_query_reply(name, req, self.node_id, ents);
        }
    }

    pub fn accept_query_reply(&self, req: Uuid, node: Uuid, entities: Vec<Value>) {
        if let Some(tx) = self.queries.get(&req) {
            let _ = tx.send((node, entities));
        }
    }

    /// Nodes other than this one that host cells of `name` (the shards a query must reach).
    fn other_shard_nodes(&self, name: &str) -> std::collections::HashSet<Uuid> {
        self.cell_hosts
            .iter()
            .filter(|e| e.key().0 == name)
            .map(|e| *e.value())
            .filter(|n| *n != self.node_id)
            .collect()
    }

    /// Hierarchy / filter query over the whole world: this shard, every other cell host (asked
    /// over the bus, `QUERY_TIMEOUT` to answer — a shard that stays silent is reported in
    /// `shards_missing`), and the cold cells in the database. Records are merged by entity id,
    /// live shards win over cold rows; `Find.limit` applies to the merged set.
    pub async fn query(&self, name: &str, kind: QueryKind, fanout: &Fanout) -> Result<Value> {
        let mut records: Vec<Value> = Vec::new();
        let mut asked = 0usize;
        let mut missing: Vec<Uuid> = Vec::new();
        let local = self.local_answer(name, &kind);
        if local.is_none() && !self.homes.contains_key(name) && !self.is_split(name) {
            return Err(Error::NotFound("world".into()));
        }
        records.extend(local.unwrap_or_default());
        let others = self.other_shard_nodes(name);
        if !others.is_empty() {
            let req = Uuid::now_v7();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            self.queries.insert(req, tx);
            fanout.world_query(name, req, self.node_id, kind.clone());
            let mut pending = others;
            asked = pending.len();
            let deadline = tokio::time::Instant::now() + QUERY_TIMEOUT;
            while !pending.is_empty() {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Some((node, ents))) => {
                        if pending.remove(&node) {
                            records.extend(ents);
                        }
                    }
                    _ => break,
                }
            }
            self.queries.remove(&req);
            missing = pending.into_iter().collect();
            metrics::counter!("musubi_world_query_shards_total", "outcome" => "answered")
                .increment((asked - missing.len()) as u64);
            metrics::counter!("musubi_world_query_shards_total", "outcome" => "missing")
                .increment(missing.len() as u64);
        }
        let cold = self.cold_answer(name, &kind).await?;
        let id_of = |v: &Value| -> Option<Uuid> {
            let s = match &kind {
                QueryKind::Entity { .. } => v["entity"]["id"].as_str(),
                _ => v["id"].as_str(),
            }?;
            s.parse().ok()
        };
        let mut seen: std::collections::HashSet<Uuid> = records.iter().filter_map(id_of).collect();
        for v in cold {
            if let Some(id) = id_of(&v) {
                if seen.insert(id) {
                    records.push(v);
                }
            }
        }
        records.sort_by_key(id_of);
        if let QueryKind::Find { q } = &kind {
            if q.limit > 0 {
                records.truncate(q.limit);
            }
        }
        Ok(json!({
            "world": name,
            "node_id": self.node_id,
            "shards_asked": asked,
            "shards_missing": missing,
            "records": records,
        }))
    }

    pub fn register_sim(&self, name: &str, sim: Uuid, capacity: u32) -> Result<Vec<SimInfo>> {
        self.check_hosting()?;
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
            global: false,
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
            global,
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
                SimEvent::<WorldEntity>::CellFull {
                    cell,
                    soft_cap: st.world.cfg.soft_cap,
                },
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
                global,
                decay_ticks,
                enqueued_tick: tick,
                reported: (position, eta_ticks),
            });
            push_event(
                st,
                sim,
                SimEvent::<WorldEntity>::Queued {
                    ticket,
                    cell,
                    position,
                    eta_ticks,
                    soft_cap,
                },
            );
            return Ok(SpawnOutcome::Queued {
                ticket,
                cell,
                position,
                eta_ticks,
            });
        }
        let id = spawn_entity(st, pos, comps, persistent, global, decay_ticks);
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
                SimEvent::<WorldEntity>::CoLocate {
                    entity: id,
                    with: None,
                    cell,
                    by,
                    until_tick: until,
                },
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
        let a_cell = st
            .world
            .get(a)
            .ok_or_else(|| Error::NotFound(format!("entity {a} is not on this shard")))?
            .cell;
        // the reporter may itself be pinned to a partner's cell: the whole contact group
        // gathers there, not in the cell its position happens to name
        let cell = st.world.pinned_to(a).map(|(c, _)| c).unwrap_or(a_cell);
        let b_cell = st
            .world
            .pinned_to(b)
            .map(|(c, _)| c)
            .or_else(|| st.world.get(b).map(|e| e.cell));
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
            let followers =
                self.co_locate_followers(st, name, sim, b, b_cell, cell, ttl_ticks, fanout);
            return Ok(
                json!({"pinned": b, "with": a, "cell": cell, "until_tick": until, "forwarded": true, "followers": followers}),
            );
        }
        let until = st.world.pin(b, cell, ttl_ticks);
        let followers = self.co_locate_followers(st, name, sim, b, b_cell, cell, ttl_ticks, fanout);
        if let Some(owner) = st.world.owner(st.world.get(b).unwrap().cell) {
            push_event(
                st,
                owner,
                SimEvent::<WorldEntity>::CoLocate {
                    entity: b,
                    with: Some(a),
                    cell,
                    by: sim,
                    until_tick: until,
                },
            );
        }
        Ok(
            json!({"pinned": b, "with": a, "cell": cell, "until_tick": until, "followers": followers}),
        )
    }

    /// Bodies pinned next to `b` because they were touching it (A–B–C over two borders) move
    /// with it: a pair-by-pair pin otherwise pulls `b` into one partner's cell and the third body
    /// into `b`'s old one, and the next contact report pulls them back — every co-location a
    /// handover. Followers are `b`'s recent contact partners whose pin named `b`'s cell.
    #[allow(clippy::too_many_arguments)]
    fn co_locate_followers(
        &self,
        st: &mut WorldState,
        name: &str,
        sim: Uuid,
        b: EntityId,
        b_cell: Option<CellId>,
        anchor: CellId,
        ttl_ticks: u64,
        fanout: &Fanout,
    ) -> Vec<EntityId> {
        let Some(b_cell) = b_cell else {
            return Vec::new();
        };
        if b_cell == anchor {
            return Vec::new();
        }
        let now = st.world.tick;
        let partners: Vec<EntityId> = st
            .contacts
            .iter()
            .filter(|(_, (t, _))| now.saturating_sub(*t) <= CONTACT_WINDOW_TICKS)
            .filter_map(|((x, y), _)| {
                if *x == b {
                    Some(*y)
                } else if *y == b {
                    Some(*x)
                } else {
                    None
                }
            })
            .filter(|x| st.world.pinned_to(*x).is_some_and(|(c, _)| c == b_cell))
            .collect();
        let mut moved = Vec::new();
        for x in partners {
            let until = st.world.pin(x, anchor, ttl_ticks);
            match st.world.get(x).map(|e| e.cell) {
                Some(c) => {
                    if let Some(owner) = st.world.owner(c) {
                        push_event(
                            st,
                            owner,
                            SimEvent::<WorldEntity>::CoLocate {
                                entity: x,
                                with: Some(b),
                                cell: anchor,
                                by: sim,
                                until_tick: until,
                            },
                        );
                    }
                }
                None if self.is_split(name) => {
                    fanout.world_co_locate(name, x, anchor, sim, ttl_ticks)
                }
                None => {}
            }
            metrics::counter!("musubi_world_contacts_total", "path" => "colocated_follower")
                .increment(1);
            moved.push(x);
        }
        moved
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
                source: Some(Provenance {
                    by: "sim".into(),
                    sim: Some(sim),
                }),
                op: WorldMutation::Despawn { id },
            });
        }
        Ok(())
    }

    /// Flags an owned entity always-relevant (coherence "Is Global"): every subscriber of the
    /// world receives it regardless of interest radius or cell, until flagged back.
    pub fn set_global(&self, name: &str, sim: Uuid, entity: &str, global: bool) -> Result<Value> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let e = st
            .world
            .get(id)
            .ok_or_else(|| Error::NotFound("entity".into()))?;
        if st.world.owner(e.cell) != Some(sim) {
            return Err(Error::PermissionDenied("not the owner".into()));
        }
        let (persistent, pos, decay_ticks, components) = (
            e.persistent,
            e.pos,
            e.decay_ticks,
            Value::Object(
                e.components
                    .iter()
                    .map(|(k, c)| (k.clone(), c.value.clone()))
                    .collect(),
            ),
        );
        st.world.set_global(id, global);
        if persistent {
            // re-journal the spawn so a replay restores the flag
            st.journal.push(WorldOp {
                tick: st.world.tick,
                source: Some(Provenance {
                    by: "sim".into(),
                    sim: Some(sim),
                }),
                op: WorldMutation::Spawn {
                    id,
                    pos,
                    components,
                    decay_ticks,
                    global,
                },
            });
        }
        Ok(json!({ "entity": id, "global": global, "globals": st.world.globals().len() }))
    }

    /// `entity` rides with `root` from now on (plan 4.5): both must be owned by `sim`.
    pub fn attach(
        &self,
        name: &str,
        sim: Uuid,
        entity: &str,
        root: &str,
        contained: bool,
    ) -> Result<()> {
        let id = parse_entity(entity)?;
        let root = parse_entity(root)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        for e in [id, root] {
            let cell = st
                .world
                .get(e)
                .ok_or_else(|| Error::NotFound("entity".into()))?
                .cell;
            if st.world.owner(cell) != Some(sim) {
                return Err(Error::PermissionDenied("not the owner".into()));
            }
        }
        st.world
            .attach_with(id, root, contained)
            .map_err(|e| Error::InvalidArgument(e.to_string()))?;
        if st.world.get(id).is_some_and(|e| e.persistent) {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                source: Some(Provenance {
                    by: "sim".into(),
                    sim: Some(sim),
                }),
                op: WorldMutation::Attach {
                    id,
                    root,
                    contained,
                },
            });
        }
        Ok(())
    }

    pub fn detach(&self, name: &str, sim: Uuid, entity: &str) -> Result<()> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let e = st
            .world
            .get(id)
            .ok_or_else(|| Error::NotFound("entity".into()))?;
        let (cell, persistent) = (e.cell, e.persistent);
        if st.world.owner(cell) != Some(sim) {
            return Err(Error::PermissionDenied("not the owner".into()));
        }
        if !st.world.detach(id) {
            return Err(Error::InvalidArgument("entity is not attached".into()));
        }
        if persistent {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                source: Some(Provenance {
                    by: "sim".into(),
                    sim: Some(sim),
                }),
                op: WorldMutation::Detach { id },
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
        if let Some((tx, _)) = st.tx_locks.get(&id) {
            return Err(Error::Conflict(format!(
                "entity {id} is held by transaction {tx}"
            )));
        }
        st.world
            .apply(sim, id, pos, comps.clone())
            .map_err(mesh_err)?;
        if st.world.get(id).is_some_and(|e| e.persistent) {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                source: Some(Provenance {
                    by: "sim".into(),
                    sim: Some(sim),
                }),
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
        let term = self.host_term(name)?;
        let (ops, tick) = {
            let w = self.get(name)?;
            let mut st = w.lock();
            (std::mem::take(&mut st.journal), st.world.tick)
        };
        if let Err(e) = Self::write_journal(db, name, self.node_id, term, &ops, tick).await {
            self.requeue_journal(name, ops, &e);
            return Err(e);
        }
        Ok(())
    }

    /// A flush that failed for any reason but fencing keeps its ops: back to the front of the
    /// tail, the next flush retries them. A fenced node no longer owns the world — its ops die
    /// with it, as they would have in a crash.
    fn requeue_journal(&self, name: &str, ops: Vec<WorldOp>, err: &Error) {
        if let Ok(w) = self.get(name) {
            requeue_journal(&w, ops, err);
        }
    }

    async fn write_journal(
        db: &PgPool,
        name: &str,
        node: Uuid,
        term: i64,
        ops: &[WorldOp],
        tick: u64,
    ) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let from = ops.first().map(|o| o.tick).unwrap_or(tick);
        let n = sqlx::query(
            "insert into musubi.world_journal (name, tick_from, tick_to, entries, node_id, node_term)
             select $1, $2, $3, $4, $5, $6
             where exists (select 1 from musubi.world_hosts h where h.name = $1 and h.node_id = $5 and h.term = $6)
             on conflict (name, node_id, tick_from) do update set tick_to = greatest(musubi.world_journal.tick_to, excluded.tick_to), entries = musubi.world_journal.entries || excluded.entries",
        )
        .bind(name)
        .bind(from as i64)
        .bind(tick as i64)
        .bind(serde_json::to_value(ops)?)
        .bind(node)
        .bind(term)
        .execute(db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::Unavailable(format!(
                "fenced: node {node} no longer hosts world {name} (term {term})"
            )));
        }
        Ok(())
    }

    pub fn view(&self, name: &str, cell: CellId) -> Result<Value> {
        let w = self.get(name)?;
        let st = w.lock();
        let v = self.cell_view(&st, name, cell)?;
        Ok(json!({
            "tick": st.world.tick,
            "owner": st.world.owner(cell),
            "owned": v.owned.iter().map(to_wire).collect::<Vec<_>>(),
            "ghosts": v.ghosts.iter().map(to_wire).collect::<Vec<_>>(),
            "globals": st.world.globals(),
        }))
    }

    /// `view` as a delta against what this simulator stream was last sent for the cell:
    /// `owned`/`ghosts` carry only entities that changed (or moved between the two lists) since,
    /// `gone` the ids that left the view, `full: true` when nothing was known (first frame of a
    /// cell). A full frame of a 500-entity cell is ~100 KB of JSON thirty times a second per
    /// simulator; the stand's bots spent 70 % of a core parsing it.
    pub fn view_delta(&self, name: &str, cell: CellId, known: &mut SimKnown) -> Result<Value> {
        let w = self.get(name)?;
        let st = w.lock();
        let v = self.cell_view(&st, name, cell)?;
        let full = known.is_empty();
        let mut seen: std::collections::HashSet<EntityId> =
            std::collections::HashSet::with_capacity(v.owned.len() + v.ghosts.len());
        let mut changed = |e: &Entity, ghost: bool| -> bool {
            seen.insert(e.id);
            let now = (e.rev, e.changed_tick, ghost);
            known.insert(e.id, now) != Some(now)
        };
        let owned: Vec<WorldEntity> = v
            .owned
            .iter()
            .filter(|e| changed(e, false))
            .map(to_wire)
            .collect();
        let ghosts: Vec<WorldEntity> = v
            .ghosts
            .iter()
            .filter(|e| changed(e, true))
            .map(to_wire)
            .collect();
        let mut gone: Vec<EntityId> = Vec::new();
        known.retain(|id, _| {
            let keep = seen.contains(id);
            if !keep {
                gone.push(*id);
            }
            keep
        });
        gone.sort();
        metrics::counter!("musubi_world_sim_frame_entities_total", "mode" => "delta")
            .increment((owned.len() + ghosts.len()) as u64);
        metrics::counter!("musubi_world_sim_frame_entities_total", "mode" => "full")
            .increment((v.owned.len() + v.ghosts.len()) as u64);
        Ok(json!({
            "tick": st.world.tick,
            "owner": st.world.owner(cell),
            "full": full,
            "owned": owned,
            "ghosts": ghosts,
            "gone": gone,
            "globals": st.world.globals(),
        }))
    }

    fn cell_view(&self, st: &WorldState, name: &str, cell: CellId) -> Result<musubi_mesh::SimView> {
        // only the cell's host has its authoritative state; a node that merely knows the world
        // (home of a hosted-only world, a node that lost the cell) must not answer with an empty
        // or stale view as if it were the truth
        if !self.hosts_cell(st, name, cell) {
            let host = self
                .cell_hosts
                .get(&(name.to_string(), cell))
                .map(|h| format!("; hosted by {}", *h))
                .unwrap_or_default();
            return Err(Error::NotFound(format!(
                "cell ({}, {}, level {}) of world {name} is not hosted here{host}",
                cell.x, cell.y, cell.level
            )));
        }
        let mut v = st.world.view(cell);
        for n in st.world.neighbours(cell) {
            let Some(g) = st.remote_ghosts.get(&n) else {
                continue;
            };
            if g.stale(st.world.tick, st.world.config().ghost_stale_ticks) {
                continue;
            }
            for g in st.world.ghosts_among(cell, g.entities()) {
                if !v.ghosts.iter().any(|e| e.id == g.id) {
                    v.ghosts.push(g.clone());
                }
            }
        }
        v.ghosts.sort_by_key(|e| e.id);
        Ok(v)
    }

    /// Ghosts from another node: border entities of `cell`, which lives there, not here.
    pub fn accept_ghosts(&self, name: &str, delta: musubi_mesh::GhostDelta) {
        let cell = delta.cell;
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
        let local = st.world.tick;
        st.remote_ghosts
            .entry(cell)
            .or_default()
            .apply(delta, local);
    }

    /// Replica side of the replication stream: a cell host sent this tick's delta of the whole
    /// cell to the nodes in `replicas`. A listed node merges it into its mirror and acks the
    /// tick; a keyframe from a new host replaces a mirror of the old one; everyone else ignores it.
    pub fn accept_repl(
        &self,
        name: &str,
        from_node: Uuid,
        epoch: i64,
        replicas: &[Uuid],
        delta: musubi_mesh::GhostDelta,
        fanout: &Fanout,
    ) {
        if from_node == self.node_id || !replicas.contains(&self.node_id) {
            return;
        }
        let key = (name.to_string(), delta.cell);
        // fenced by the cell directory: only the node it names as host may feed the mirror
        // (an unknown cell is accepted — the directory may lag a fresh assignment by a tick)
        if self.cell_hosts.get(&key).is_some_and(|h| *h != from_node) {
            return;
        }
        let (cell, tick) = (delta.cell, delta.tick);
        let mut m = self.mirrors.entry(key).or_insert_with(|| Mirror {
            from: from_node,
            epoch,
            cache: musubi_mesh::GhostCache::default(),
            tick: 0,
        });
        // fenced by epoch: a replaced or restarted host streams from a newer incarnation and
        // must start with a keyframe; anything older than the mirror's source is dropped
        if epoch < m.epoch {
            return;
        }
        if m.from != from_node || epoch > m.epoch {
            if !delta.keyframe {
                return;
            }
            *m = Mirror {
                from: from_node,
                epoch,
                cache: musubi_mesh::GhostCache::default(),
                tick: 0,
            };
        }
        if m.cache.apply(delta, tick) {
            m.tick = tick;
            drop(m);
            fanout.world_repl_ack(name, cell, self.node_id, tick);
        }
    }

    /// Host side: a replica reports its mirror of `cell` complete up to `tick`.
    pub fn accept_repl_ack(&self, name: &str, cell: CellId, node: Uuid, tick: u64) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        if !st
            .repl_targets
            .get(&cell)
            .is_some_and(|t| t.contains(&node))
        {
            return;
        }
        let t = st
            .repl_acks
            .entry(cell)
            .or_default()
            .entry(node)
            .or_default();
        *t = (*t).max(tick);
    }

    /// The replicas of `cell`: `n` alive peers, spread by hash so neighbouring cells do not all
    /// land on the same node, stable while the peer set is.
    fn pick_replicas(peers: &[Uuid], name: &str, cell: CellId, n: usize) -> Vec<Uuid> {
        if n == 0 || peers.is_empty() {
            return Vec::new();
        }
        let mut ranked: Vec<(u64, Uuid)> = peers
            .iter()
            .map(|p| {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (name, cell.x, cell.y, p).hash(&mut h);
                (h.finish(), *p)
            })
            .collect();
        ranked.sort();
        ranked.into_iter().take(n).map(|(_, p)| p).collect()
    }

    /// An entity handed over from another node into a cell hosted here. Taken when a local
    /// simulator owns the cell (it gets a `handover_arrived` event with the state and the inputs
    /// buffered so far); refused otherwise. Nodes that do not host the cell stay silent.
    #[allow(clippy::too_many_arguments)]
    pub fn accept_handover(
        &self,
        name: &str,
        entity: Entity,
        from_node: Uuid,
        inputs: Vec<Value>,
        forced: bool,
        base_rev: Option<u64>,
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
            .or_else(|| entity.transit.as_ref().map(|t| t.to_cell))
            .unwrap_or_else(|| st.world.cell_of(entity.pos));
        // ours if assigned to us, or unassigned and we are the home
        let here = match self.cell_hosts.get(&(name.to_string(), cell)) {
            Some(h) => *h == self.node_id,
            None => !st.hosted_only,
        };
        if !here {
            metrics::counter!("musubi_world_handover_recv_total", "path" => "not_here")
                .increment(1);
            hoj(
                name,
                entity.id,
                st.world.tick,
                "received",
                "not_here",
                format!("cell {cell:?} from {from_node}"),
            );
            return false;
        }
        let id = entity.id;
        if st.world.get(id).is_some() {
            metrics::counter!("musubi_world_handover_recv_total", "path" => "have_it").increment(1);
            hoj(
                name,
                id,
                st.world.tick,
                "received",
                "have_it",
                format!("from {from_node}"),
            );
            // we already hold this entity. Usually the ack was lost and this is the same transfer
            // again: answer again, keep our copy. But if our copy is itself on its way to the
            // sender (both sides restored or kept one and each ships it over the border at
            // once), two mutual "accepted" acks would make both sides despawn and the entity
            // vanish — the stand lost one exactly so. The lower node id keeps its copy and
            // accepts; the higher one declines and waits for the ack of its own transfer.
            let outgoing = st
                .remote_transit
                .get(&id)
                .is_some_and(|(h, _, _)| *h == from_node);
            if outgoing && self.node_id > from_node {
                tracing::info!(world = name, %id, %from_node, "handover collision: declined, our copy is on its way there");
                hoj(
                    name,
                    id,
                    st.world.tick,
                    "received",
                    "collision_declined",
                    format!("from {from_node}"),
                );
                fanout.world_handover_ack(name, id, from_node, false, false);
                return false;
            }
            if outgoing {
                st.remote_transit.remove(&id);
                st.world.cancel_handover(id);
                tracing::info!(world = name, %id, %from_node, "handover collision: kept our copy, theirs is dropped");
                hoj(
                    name,
                    id,
                    st.world.tick,
                    "received",
                    "collision_kept",
                    format!("from {from_node}"),
                );
            } else if self.provisional.remove(&(name.to_string(), id)).is_some() {
                // our copy was restored from a dead host's shard; the sender owned the entity
                // live and has now handed it to us — ours is the live one from here on
                tracing::info!(world = name, %id, %from_node, "handover delivered onto our restored copy: it is live now");
                hoj(
                    name,
                    id,
                    st.world.tick,
                    "received",
                    "onto_restored",
                    format!("from {from_node}"),
                );
            } else {
                tracing::warn!(world = name, %id, %from_node, "handover re-acked (duplicate delivery)");
                hoj(
                    name,
                    id,
                    st.world.tick,
                    "received",
                    "duplicate",
                    format!("from {from_node}"),
                );
            }
            fanout.world_handover_ack(name, id, from_node, true, false);
            return false;
        }
        let entity = match base_rev {
            None => entity,
            Some(base) => match st.remote_ghosts.values_mut().find_map(|c| c.take(id)) {
                Some(mut g) if g.rev >= base => {
                    std::sync::Arc::make_mut(&mut g.components)
                        .extend((*entity.components).clone());
                    g.pos = entity.pos;
                    g.cell = entity.cell;
                    g.rev = entity.rev;
                    g.changed_tick = entity.changed_tick;
                    g.persistent = entity.persistent;
                    g.attached_to = entity.attached_to;
                    g.decay_ticks = entity.decay_ticks;
                    g.touched_tick = entity.touched_tick;
                    g.handover_attempts = entity.handover_attempts;
                    g.handover_retry_tick = entity.handover_retry_tick;
                    g.transit = None;
                    g
                }
                _ => {
                    metrics::counter!("musubi_world_handover_recv_total", "path" => "no_ghost")
                        .increment(1);
                    tracing::info!(world = name, %id, %from_node, base, "handover: no ghost to build on, asking for the full state");
                    hoj(
                        name,
                        id,
                        st.world.tick,
                        "received",
                        "no_ghost",
                        format!("from {from_node}"),
                    );
                    fanout.world_handover_ack(name, id, from_node, false, true);
                    return false;
                }
            },
        };
        // A hosted cell without a simulator lease still takes the entity: on one node an
        // unleased cell holds its entities until a simulator leases it, and the same holds
        // here. Refusing made the sender re-offer every tick (~60 ms) for as long as the
        // simulator took to follow the offload redirect — the stand's largest handover loop.
        let owner = st.world.owner(cell);
        if owner.is_none() {
            metrics::counter!("musubi_world_handover_recv_total", "path" => "unowned_cell")
                .increment(1);
        }
        hoj(
            name,
            id,
            st.world.tick,
            "received",
            "accepted",
            format!("cell {cell:?} from {from_node} owner {owner:?}"),
        );
        if entity.persistent {
            st.journal.push(WorldOp {
                tick: st.world.tick,
                source: Some(Provenance {
                    by: "handover".into(),
                    sim: None,
                }),
                op: WorldMutation::Spawn {
                    id,
                    pos: entity.pos,
                    decay_ticks: entity.decay_ticks,
                    global: entity.global,
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
        if traced(id) {
            tracing::debug!(world = name, %id, ms = musubi_cluster::unix_ms(), "hotrace: arrived, acking");
        }
        st.world.restore(vec![entity]);
        if let Some(owner) = owner {
            let state = st.world.get(id).map(to_wire);
            push_event(
                st,
                owner,
                SimEvent::HandoverArrived {
                    entity: id,
                    from_node,
                    state,
                    buffered_inputs: inputs,
                },
            );
        }
        if durable {
            // the sender journals our ack as its Despawn: ours must hit the journal first,
            // or a crash in between loses the entity — the caller flushes, then acks
            return true;
        }
        fanout.world_handover_ack(name, id, from_node, true, false);
        false
    }

    /// The other node answered our handover: on success the entity leaves this shard (inputs
    /// buffered meanwhile follow it over the bus), on refusal it stays with its owner.
    pub fn handover_acked(
        &self,
        name: &str,
        id: EntityId,
        acker: Uuid,
        accepted: bool,
        resend_full: bool,
        fanout: &Fanout,
    ) {
        let Some(w) = self.worlds.get(name) else {
            metrics::counter!("musubi_world_handover_acks_total", "path" => "no_world", "accepted" => accepted.to_string(), "world" => name.to_string())
                .increment(1);
            hoj(
                name,
                id,
                0,
                "ack",
                "no_world",
                format!("from {acker} accepted {accepted}"),
            );
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        let Some(t) = st.world.get(id).and_then(|e| e.transit.clone()) else {
            let gone = st.world.get(id).is_none();
            metrics::counter!("musubi_world_handover_acks_total", "path" => if gone { "late_gone" } else { "late_kept" }, "accepted" => accepted.to_string())
                .increment(1);
            hoj(
                name,
                id,
                st.world.tick,
                "ack",
                if gone { "late_gone" } else { "late_kept" },
                format!("from {acker} accepted {accepted}"),
            );
            if traced(id) {
                tracing::debug!(world = name, %id, %acker, accepted, gone, in_remote_transit = st.remote_transit.contains_key(&id), ms = musubi_cluster::unix_ms(), "hotrace: ack late");
            }
            metrics::counter!("musubi_world_handover_acks_total", "path" => "late", "accepted" => accepted.to_string())
                .increment(1);
            // a late ack for a handover we already cancelled: if the receiver took the entity
            // and its cell is not ours, our copy is the stale one — drop it instead of keeping
            // two owners
            if accepted {
                if let Some(e) = st.world.get(id).cloned() {
                    let cell = st
                        .kept_after_cancel
                        .remove(&id)
                        .unwrap_or_else(|| st.world.cell_of(e.pos));
                    // only believed from the node the directory names as the cell's host: a
                    // late ack from a node that has since died (or lost the cell) must not make
                    // us drop the only live copy
                    // the node we are still shipping it to says it has it: that is the completion,
                    // wherever our stale copy's position falls (an entity offered from inside the
                    // border margin still sits in our cell, and judging by the cell alone left the
                    // ack ignored and the entity re-shipped every timeout — 7 000 duplicate
                    // deliveries a run on the stand)
                    let shipping_there = st
                        .remote_transit
                        .get(&id)
                        .is_some_and(|(h, _, _)| *h == acker);
                    let elsewhere = shipping_there
                        || (acker != self.node_id
                            && match self.cell_hosts.get(&(name.to_string(), cell)) {
                                Some(h) => *h == acker,
                                // an unassigned cell is the home's
                                None => self.homes.get(name).is_some_and(|h| *h == acker),
                            });
                    if elsewhere {
                        tracing::info!(world = name, %id, "late handover ack: stale local copy dropped");
                        hoj(
                            name,
                            id,
                            st.world.tick,
                            "ended",
                            "late_dropped",
                            format!("acker {acker} hosts {cell:?}"),
                        );
                        metrics::counter!("musubi_world_handover_acks_total", "path" => "late_dropped", "accepted" => "true")
                            .increment(1);
                        metrics::counter!("musubi_world_handovers_total", "world" => name.to_string())
                            .increment(1);
                        metrics::counter!("musubi_world_handover_ended_total", "path" => "completed")
                            .increment(1);
                        st.remote_transit.remove(&id);
                        st.world.forget_for_clients(id);
                        st.world.despawn(id);
                        if e.persistent {
                            st.journal.push(WorldOp {
                                tick: st.world.tick,
                                source: Some(Provenance {
                                    by: "handover".into(),
                                    sim: None,
                                }),
                                op: WorldMutation::Despawn { id },
                            });
                        }
                    }
                }
            }
            return;
        };
        metrics::counter!("musubi_world_handover_acks_total", "path" => "in_transit", "accepted" => accepted.to_string())
            .increment(1);
        hoj(
            name,
            id,
            st.world.tick,
            "ack",
            if accepted {
                "accepted"
            } else if resend_full {
                "refused_resend_full"
            } else {
                "refused"
            },
            format!("from {acker} to {}", t.to),
        );
        if traced(id) {
            tracing::debug!(world = name, %id, %acker, accepted, ms = musubi_cluster::unix_ms(), "hotrace: ack in transit");
        }
        st.remote_transit.remove(&id);
        st.kept_after_cancel.remove(&id);
        if !accepted && resend_full {
            st.full_resend.insert(id);
        }
        if accepted {
            // a cross-node handover completes here, not in the simulator's ack path
            metrics::counter!("musubi_world_handovers_total", "world" => name.to_string())
                .increment(1);
            metrics::counter!("musubi_world_handover_ended_total", "path" => "completed")
                .increment(1);
            let inputs = st.world.take_buffered_inputs(id);
            let persistent = st.world.get(id).is_some_and(|e| e.persistent);
            tracing::debug!(world = name, %id, to = %t.to, "handover: entity left");
            hoj(
                name,
                id,
                st.world.tick,
                "ended",
                "completed",
                format!("left for {} on {acker}", t.to),
            );
            st.world.forget_for_clients(id);
            st.world.despawn(id);
            if persistent {
                st.journal.push(WorldOp {
                    tick: st.world.tick,
                    source: Some(Provenance {
                        by: "handover".into(),
                        sim: None,
                    }),
                    op: WorldMutation::Despawn { id },
                });
            }
            for i in inputs {
                fanout.world_input(name, id, i);
                metrics::counter!("musubi_world_handovers_total", "world" => name.to_string())
                    .increment(1);
                metrics::counter!("musubi_world_handover_ended_total", "path" => "completed")
                    .increment(1);
            }
            push_event(
                st,
                t.from,
                SimEvent::<WorldEntity>::HandoverCompleted {
                    entity: id,
                    to: t.to,
                    node: Some(t.to),
                },
            );
        } else {
            st.world.cancel_handover(id);
            push_event(
                st,
                t.from,
                SimEvent::<WorldEntity>::HandoverCancelled {
                    entity: id,
                    reason: format!("node {} refused: no simulator owns the cell", t.to),
                },
            );
        }
    }

    /// Begin a cross-cell transaction: every op is applied on every host involved, or none is.
    /// All entities here → applied now. Otherwise the entities here are held (no writes, no
    /// handover) and the other hosts are asked to prepare (`WorldTxPrepare`); their votes decide
    /// (`accept_tx_vote`), a missing vote aborts at the deadline. Idempotent by `tx`.
    pub fn begin_tx(
        &self,
        name: &str,
        tx: Option<Uuid>,
        ops: Vec<TxOp>,
        ttl_ticks: Option<u64>,
        fanout: &Fanout,
    ) -> Result<Value> {
        if ops.is_empty() {
            return Err(Error::InvalidArgument("ops: at least one".into()));
        }
        if ops.len() > 64 {
            return Err(Error::InvalidArgument("ops: at most 64".into()));
        }
        let mut seen = std::collections::HashSet::new();
        for op in &ops {
            if !seen.insert(op.entity) {
                return Err(Error::InvalidArgument(format!(
                    "entity {} listed twice",
                    op.entity
                )));
            }
        }
        let tx = tx.unwrap_or_else(Uuid::now_v7);
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        if let Some(r) = st.txs.get(&tx) {
            return Ok(tx_json(tx, &r.state, true));
        }
        let now = st.world.tick;
        let ttl = ttl_ticks.unwrap_or(TX_TTL_TICKS).clamp(1, 3000);
        let deadline = now + ttl;
        let record = |state: TxState, pending, ops| TxRecord {
            ops,
            pending,
            state,
            deadline_tick: deadline,
            forget_tick: deadline + TX_KEEP_TICKS,
        };
        let mine = match tx_check_local(st, tx, &ops) {
            Ok(m) => m,
            Err(reason) => {
                let state = TxState::Aborted { reason };
                st.txs
                    .insert(tx, record(state.clone(), Default::default(), ops));
                return Ok(tx_json(tx, &state, false));
            }
        };
        let remote: std::collections::HashSet<EntityId> = ops
            .iter()
            .map(|o| o.entity)
            .filter(|id| !mine.contains(id))
            .collect();
        if !remote.is_empty() && !self.is_split(name) {
            let mut missing: Vec<String> = remote.iter().map(|i| i.to_string()).collect();
            missing.sort();
            let state = TxState::Aborted {
                reason: format!("unknown entities: {}", missing.join(", ")),
            };
            st.txs
                .insert(tx, record(state.clone(), Default::default(), ops));
            return Ok(tx_json(tx, &state, false));
        }
        if remote.is_empty() {
            tx_apply_local(st, tx, &ops);
            st.txs
                .insert(tx, record(TxState::Committed, Default::default(), ops));
            return Ok(tx_json(tx, &TxState::Committed, false));
        }
        tx_lock_local(st, tx, &mine, deadline);
        st.txs
            .insert(tx, record(TxState::Pending, remote, ops.clone()));
        fanout.world_tx_prepare(name, tx, self.node_id, ops, ttl);
        Ok(tx_json(tx, &TxState::Pending, false))
    }

    pub fn tx_status(&self, name: &str, tx: Uuid) -> Result<Value> {
        let w = self.get(name)?;
        let st = w.lock();
        match st.txs.get(&tx) {
            Some(r) => Ok(tx_json(tx, &r.state, false)),
            None => Err(Error::NotFound(format!("transaction {tx}"))),
        }
    }

    /// Another node coordinates `tx`: hold the entities of `ops` that live here and vote. Nodes
    /// holding none of them stay silent.
    pub fn accept_tx_prepare(
        &self,
        name: &str,
        tx: Uuid,
        coordinator: Uuid,
        ops: Vec<TxOp>,
        ttl_ticks: u64,
        fanout: &Fanout,
    ) {
        if coordinator == self.node_id {
            return;
        }
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        if !ops.iter().any(|o| st.world.get(o.entity).is_some()) {
            return;
        }
        match tx_check_local(st, tx, &ops) {
            Ok(mine) => {
                // the coordinator aborts after its ttl; hold a little longer so its commit lands
                let until = st.world.tick + ttl_ticks + 5;
                tx_lock_local(st, tx, &mine, until);
                fanout.world_tx_vote(name, tx, coordinator, self.node_id, mine, None);
            }
            Err(reason) => {
                fanout.world_tx_vote(
                    name,
                    tx,
                    coordinator,
                    self.node_id,
                    Vec::new(),
                    Some(reason),
                );
            }
        }
    }

    /// A vote for a transaction this node coordinates: a refusal aborts it everywhere, the vote
    /// covering the last missing entity commits it.
    pub fn accept_tx_vote(
        &self,
        name: &str,
        tx: Uuid,
        coordinator: Uuid,
        entities: Vec<EntityId>,
        reason: Option<String>,
        fanout: &Fanout,
    ) {
        if coordinator != self.node_id {
            return;
        }
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        let decision = {
            let Some(r) = st.txs.get_mut(&tx) else {
                return;
            };
            if r.state != TxState::Pending {
                return;
            }
            match reason {
                Some(reason) => {
                    r.state = TxState::Aborted {
                        reason: reason.clone(),
                    };
                    Err(reason)
                }
                None => {
                    for id in entities {
                        r.pending.remove(&id);
                    }
                    if !r.pending.is_empty() {
                        return;
                    }
                    r.state = TxState::Committed;
                    Ok(r.ops.clone())
                }
            }
        };
        match decision {
            Err(reason) => {
                tx_release(st, tx);
                fanout.world_tx_abort(name, tx, reason);
            }
            Ok(ops) => {
                tx_release(st, tx);
                tx_apply_local(st, tx, &ops);
                fanout.world_tx_commit(name, tx, ops);
            }
        }
    }

    /// Commit from the coordinator: apply the ops whose entities live here and release them.
    pub fn accept_tx_commit(&self, name: &str, tx: Uuid, ops: Vec<TxOp>) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        if st.txs.contains_key(&tx) {
            return; // we coordinate it: applied when the last vote came in
        }
        tx_release(st, tx);
        tx_apply_local(st, tx, &ops);
    }

    /// Abort from the coordinator: release the held entities, tell their simulators.
    pub fn accept_tx_abort(&self, name: &str, tx: Uuid, reason: String) {
        let Some(w) = self.worlds.get(name) else {
            return;
        };
        let mut st = w.lock();
        let st = &mut *st;
        for id in tx_release(st, tx) {
            let owner = st.world.get(id).and_then(|e| st.world.owner(e.cell));
            if let Some(owner) = owner {
                push_event(
                    st,
                    owner,
                    SimEvent::<WorldEntity>::TxAborted {
                        tx,
                        entity: id,
                        reason: reason.to_string(),
                    },
                );
            }
        }
    }

    /// Does this node host `cell` of `name` (assigned to it, or unassigned on the home)?
    fn hosts_cell(&self, st: &WorldState, name: &str, cell: CellId) -> bool {
        match self.cell_hosts.get(&(name.to_string(), cell)) {
            Some(h) => *h == self.node_id,
            None => !st.hosted_only,
        }
    }

    /// Hibernated cells idle for `unload_after_ticks` more: their persistent entities are written
    /// to `world_cold_cells` first and dropped from memory once the row is there.
    fn unload_cold_cells(&self, st: &mut WorldState, name: &str) {
        let after = st.world.cfg.unload_after_ticks;
        let Some(db) = self.db.clone() else {
            return;
        };
        if after == 0 {
            st.hibernated_since.clear();
            return;
        }
        let now = st.world.tick;
        let world = &st.world;
        st.hibernated_since.retain(|c, _| world.is_hibernated(*c));
        let due: Vec<CellId> = st
            .hibernated_since
            .iter()
            .filter(|(c, since)| {
                now.saturating_sub(**since) >= after
                    && world.entities_in(**c) > 0
                    && !world.is_unloaded(**c)
                    && !st.loading.contains(*c)
            })
            .map(|(c, _)| *c)
            .collect();
        for cell in due {
            if !self.hosts_cell(st, name, cell) {
                continue;
            }
            let mut snapshot: Vec<Entity> = st
                .world
                .entities_of(cell)
                .into_iter()
                .filter(|e| e.persistent)
                .cloned()
                .collect();
            snapshot.sort_by_key(|e| e.id);
            st.hibernated_since.remove(&cell);
            if snapshot.is_empty() {
                continue;
            }
            let Ok(w) = self.get(name) else {
                continue;
            };
            st.loading.insert(cell);
            let (db, n) = (db.clone(), name.to_string());
            tokio::spawn(async move {
                let written = Self::write_cold_cell(&db, &n, cell, &snapshot, now).await;
                // decide under the lock, talk to the database outside it
                let cold: Option<Vec<Entity>> = {
                    let mut st = w.lock();
                    st.loading.remove(&cell);
                    if let Err(e) = written {
                        tracing::warn!(world = %n, ?cell, error = %e, "cold cell write failed, kept in memory");
                        return;
                    }
                    if st.world.is_hibernated(cell) && !st.world.is_unloaded(cell) {
                        Some(st.world.unload_cell(cell))
                    } else {
                        None
                    }
                };
                let Some(cold) = cold else {
                    // woke up while the row was being written: the row would be stale
                    if let Err(e) = Self::delete_cold_cell(&db, &n, cell).await {
                        tracing::warn!(world = %n, ?cell, error = %e, "stale cold cell row not removed");
                    }
                    return;
                };
                let same = cold.len() == snapshot.len()
                    && cold
                        .iter()
                        .zip(&snapshot)
                        .all(|(a, b)| a.id == b.id && a.rev == b.rev && a.pos == b.pos);
                if !same {
                    // something arrived or changed between the write and the unload
                    if let Err(e) = Self::write_cold_cell(&db, &n, cell, &cold, now).await {
                        tracing::error!(world = %n, ?cell, error = %e, "cold cell rewrite failed after unload");
                    }
                }
                tracing::info!(world = %n, ?cell, entities = cold.len(), "cell unloaded");
            });
        }
    }

    /// Unloaded cells that are watched, leased or spawned into again get their entities back.
    fn reload_cold_cells(&self, st: &mut WorldState, name: &str) {
        let Some(db) = self.db.clone() else {
            return;
        };
        let unloaded = st.world.unloaded_cells();
        if unloaded.is_empty() {
            return;
        }
        let watched: std::collections::HashSet<CellId> =
            st.world.interest_cells().into_iter().collect();
        for cell in unloaded {
            let wanted = watched.contains(&cell)
                || st.world.owner(cell).is_some()
                || st.world.entities_in(cell) > 0;
            if !wanted || st.loading.contains(&cell) || !self.hosts_cell(st, name, cell) {
                continue;
            }
            let Ok(w) = self.get(name) else {
                continue;
            };
            st.loading.insert(cell);
            let (db, n) = (db.clone(), name.to_string());
            tokio::spawn(async move {
                let loaded = Self::read_cold_cell(&db, &n, cell).await;
                let count = {
                    let mut st = w.lock();
                    st.loading.remove(&cell);
                    match loaded {
                        Ok(Some(entities)) => {
                            let count = entities.len();
                            st.world.reload_cell(cell, entities);
                            if let Some(owner) = st.world.owner(cell) {
                                push_event(
                                    &mut st,
                                    owner,
                                    SimEvent::<WorldEntity>::CellReloaded {
                                        cell,
                                        entities: count,
                                    },
                                );
                            }
                            count
                        }
                        Ok(None) => {
                            st.world.reload_cell(cell, Vec::new());
                            return;
                        }
                        Err(e) => {
                            tracing::warn!(world = %n, ?cell, error = %e, "cold cell read failed");
                            return;
                        }
                    }
                };
                tracing::info!(world = %n, ?cell, entities = count, "cell reloaded");
                if let Err(e) = Self::delete_cold_cell(&db, &n, cell).await {
                    tracing::warn!(world = %n, ?cell, error = %e, "cold cell row not removed after reload");
                }
            });
        }
    }

    async fn write_cold_cell(
        db: &PgPool,
        name: &str,
        cell: CellId,
        entities: &[Entity],
        tick: u64,
    ) -> Result<()> {
        sqlx::query(
            "insert into musubi.world_cold_cells (name, x, y, level, entities, tick) values ($1, $2, $3, $6, $4, $5)
             on conflict (name, x, y, level) do update set entities = excluded.entities, tick = excluded.tick, unloaded_at = now()",
        )
        .bind(name)
        .bind(cell.x)
        .bind(cell.y)
        .bind(serde_json::to_value(entities)?)
        .bind(tick as i64)
        .bind(i32::from(cell.level))
        .execute(db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    async fn read_cold_cell(db: &PgPool, name: &str, cell: CellId) -> Result<Option<Vec<Entity>>> {
        let row: Option<(Value,)> = sqlx::query_as(
            "select entities from musubi.world_cold_cells where name = $1 and x = $2 and y = $3 and level = $4",
        )
        .bind(name)
        .bind(cell.x)
        .bind(cell.y)
        .bind(i32::from(cell.level))
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?;
        match row {
            Some((v,)) => Ok(Some(serde_json::from_value(v)?)),
            None => Ok(None),
        }
    }

    async fn delete_cold_cell(db: &PgPool, name: &str, cell: CellId) -> Result<()> {
        sqlx::query(
            "delete from musubi.world_cold_cells where name = $1 and x = $2 and y = $3 and level = $4",
        )
        .bind(name)
        .bind(cell.x)
        .bind(cell.y)
        .bind(i32::from(cell.level))
            .execute(db)
            .await
            .map_err(Error::internal)?;
        Ok(())
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
        let mut guard = w.lock();
        let st = &mut *guard;
        let (colocate_after, colocate_ttl) = (
            st.world.cfg.contact_colocate_after_ticks,
            st.world.cfg.contact_colocate_ttl_ticks,
        );
        // `contact`: a physical contact with a ghost. The owner of the target applies the
        // impulse (as any interaction); the same contact seen from both sides of the border is
        // applied once; a pair that keeps touching across the border is co-located so one
        // simulator resolves it from then on.
        let mut colocate: Option<EntityId> = None;
        if spec.kind == "contact" {
            let other = spec.payload["other"]
                .as_str()
                .ok_or_else(|| Error::InvalidArgument("contact payload needs `other`".into()))
                .and_then(parse_entity)?;
            let tick = spec.at_tick.unwrap_or(st.world.tick);
            let key = (id.min(other), id.max(other));
            let now = st.world.tick;
            if now % 30 == 0 {
                st.contacts
                    .retain(|_, (t, _)| now.saturating_sub(*t) <= CONTACT_WINDOW_TICKS);
            }
            let e = st.contacts.entry(key).or_insert((tick, 0));
            if e.0 == tick && e.1 > 0 {
                metrics::counter!("musubi_world_contacts_total", "path" => "dup").increment(1);
                return Ok(json!({ "delivered": "dup", "tick": tick }));
            }
            e.1 = if tick.saturating_sub(e.0) <= 1 {
                e.1 + 1
            } else {
                1
            };
            e.0 = tick;
            let streak = e.1;
            let across = match (st.world.get(id), st.world.get(other)) {
                (Some(a), Some(b)) => a.cell != b.cell,
                _ => true,
            };
            if across && colocate_after > 0 && streak >= colocate_after {
                e.1 = 0;
                colocate = Some(other);
            }
            metrics::counter!("musubi_world_contacts_total", "path" => "applied").increment(1);
        }
        let ev = SimEvent::<WorldEntity>::Interaction {
            entity: id,
            kind: spec.kind,
            payload: spec.payload,
            from_sim: sim,
            at_tick: spec.at_tick,
            at: spec.at,
            tolerance: spec.tolerance,
            rewind: None,
            seq: None,
            tick: None,
        }
        .to_value();
        if let Some(other) = colocate {
            drop(guard);
            // the pair keeps touching across a border: pin the target into the reporter's cell
            let pinned = self
                .co_locate(name, sim, &other.to_string(), entity, colocate_ttl, fanout)
                .ok();
            metrics::counter!("musubi_world_contacts_total", "path" => "colocated").increment(1);
            let w = self.get(name)?;
            let mut st = w.lock();
            let st = &mut *st;
            let mut r = self.deliver_interaction(name, st, id, ev, fanout)?;
            r["colocated"] = json!(pinned.is_some());
            // bodies that were pinned next to the moved one and came along (see `co_locate`)
            if let Some(f) = pinned.as_ref().and_then(|p| p.get("followers")) {
                if f.as_array().is_some_and(|a| !a.is_empty()) {
                    r["followers"] = f.clone();
                }
            }
            return Ok(r);
        }
        self.deliver_interaction(name, st, id, ev, fanout)
    }

    fn deliver_interaction(
        &self,
        name: &str,
        st: &mut WorldState,
        id: EntityId,
        ev: Value,
        fanout: &Fanout,
    ) -> Result<Value> {
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
            .and_then(|e| e.transit.as_ref().map(|t| (t.from, t.requested_tick)));
        let (outcome, inputs) = st.world.handover_ack(sim, id).map_err(mesh_err)?;
        if let Some((from, asked)) = from {
            metrics::counter!("musubi_world_handovers_total", "world" => name.to_string())
                .increment(1);
            hoj(
                name,
                id,
                st.world.tick,
                "ended",
                "completed",
                format!("sim {sim} acked, from {from}, asked at tick {asked}"),
            );
            // how long the simulator's answer took: the handover timeout should sit above this
            metrics::histogram!("musubi_world_handover_ack_latency_ticks")
                .record(st.world.tick.saturating_sub(asked) as f64);
            metrics::counter!("musubi_world_handover_ended_total", "path" => "completed")
                .increment(1);
            push_event(
                st,
                from,
                SimEvent::<WorldEntity>::HandoverCompleted {
                    entity: id,
                    to: sim,
                    node: None,
                },
            );
        }
        Ok(json!({ "outcome": outcome, "buffered_inputs": inputs }))
    }

    // ----- client side -----

    #[allow(clippy::too_many_arguments)]
    pub fn subscribe(
        &self,
        name: &str,
        sid: SessionId,
        pos: Pos,
        radius: f64,
        budget_bytes: Option<usize>,
        filter: Option<Value>,
        max_hz: Option<u32>,
        unreliable: bool,
        inside: Option<String>,
    ) -> Result<()> {
        if !(0.0..=100_000.0).contains(&radius) {
            return Err(Error::InvalidArgument("radius 0..=100000".into()));
        }
        let inside = inside.map(|i| parse_entity(&i)).transpose()?;
        let filter: std::collections::BTreeMap<String, Value> = match filter {
            None | Some(Value::Null) => Default::default(),
            Some(Value::Object(m)) => m.into_iter().collect(),
            Some(_) => {
                return Err(Error::InvalidArgument(
                    "filter must be an object of component → value".into(),
                ))
            }
        };
        let every = match max_hz {
            Some(hz) if hz > 0 => TICK_HZ.div_ceil(hz as u64),
            _ => 1,
        };
        self.get(name)?;
        // applied by the world's own tick thread: at 10k players re-sending interest a few
        // times a second, taking the world lock here parked the runtime behind every tick
        self.interest
            .entry(name.to_string())
            .or_default()
            .lock()
            .push(PendingInterest {
                sid,
                pos,
                radius,
                budget_bytes,
                filter,
                every,
                max_hz,
                unreliable,
                inside,
            });
        Ok(())
    }

    fn drain_interest(&self, name: &str, st: &mut WorldState) {
        let pending = match self.interest.get(name) {
            Some(q) => std::mem::take(&mut *q.lock()),
            None => return,
        };
        for p in pending {
            self.apply_interest(name, st, p);
        }
    }

    fn apply_interest(&self, name: &str, st: &mut WorldState, p: PendingInterest) {
        let PendingInterest {
            sid,
            pos,
            radius,
            budget_bytes,
            filter,
            every,
            max_hz,
            unreliable,
            inside,
        } = p;
        if !st.hosted_only {
            st.sub_cmds.insert(
                sid,
                WorldCmd::Subscribe {
                    x: pos.x,
                    y: pos.y,
                    radius,
                    max_hz,
                    budget_bytes: budget_bytes.map(|b| b as _),
                    filter: (!filter.is_empty())
                        .then(|| Value::Object(filter.clone().into_iter().collect())),
                    unreliable,
                    inside: inside.map(|i| i.to_string()),
                },
            );
        }
        let client = *st.subs.entry(sid).or_insert_with(Uuid::now_v7);
        st.sub_opts.entry(client).or_default().unreliable = unreliable;
        st.world
            .subscribe_with(client, pos, radius, budget_bytes, filter, every);
        st.world.look_inside(client, inside);
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
    }

    /// Whether `sid` is subscribed to `name` on this node (an interest still queued for the
    /// next tick counts).
    pub fn is_subscribed(&self, name: &str, sid: SessionId) -> bool {
        self.worlds
            .get(name)
            .is_some_and(|w| w.lock().subs.contains_key(&sid))
            || self
                .interest
                .get(name)
                .is_some_and(|q| q.lock().iter().any(|p| p.sid == sid))
    }

    pub fn unsubscribe(&self, name: &str, sid: SessionId) -> Result<()> {
        let w = self.get(name)?;
        if let Some(q) = self.interest.get(name) {
            q.lock().retain(|p| p.sid != sid);
        }
        let mut st = w.lock();
        let st = &mut *st;
        if let Some(c) = st.subs.remove(&sid) {
            st.sub_cmds.remove(&sid);
            st.sub_opts.remove(&c);
            st.world.unsubscribe(c);
        }
        Ok(())
    }

    pub fn unsubscribe_all(&self, sid: SessionId) {
        for q in self.interest.iter() {
            q.lock().retain(|p| p.sid != sid);
        }
        for w in self.worlds.iter() {
            let mut st = w.lock();
            let st = &mut *st;
            if let Some(c) = st.subs.remove(&sid) {
                st.sub_cmds.remove(&sid);
                st.sub_opts.remove(&c);
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
        let ev = SimEvent::<WorldEntity>::Input {
            entity: id,
            session: json!(sid),
            input,
        }
        .to_value();
        if let Some(sim) = st.world.route_input(id, ev.clone()).map_err(mesh_err)? {
            push_event(st, sim, ev);
        }
        Ok(())
    }

    // ----- ticking -----

    /// One tick of every world: clock, handover detection/expiry, client deltas.
    /// One tick of every world here, one after another (tests and single-world nodes); the
    /// ticker runs worlds in parallel through `tick_world`.
    pub fn tick(&self, fanout: &Fanout) {
        let names: Vec<String> = self.worlds.iter().map(|w| w.key().clone()).collect();
        for name in names {
            self.tick_world(&name, fanout);
        }
    }

    /// Remember ids restored from the shards of hosts that are gone, per cell, so the next
    /// registry ticks announce them and a live owner elsewhere can answer with a drop.
    fn queue_adopted(&self, name: &str, ents: impl IntoIterator<Item = (EntityId, CellId)>) {
        for (id, cell) in ents {
            self.provisional.insert((name.to_string(), id));
            let mut e = self
                .adopted_pending
                .entry((name.to_string(), cell))
                .or_insert((Vec::new(), 0));
            e.0.push(id);
            e.1 = ADOPT_ANNOUNCE_TICKS;
        }
    }

    /// Of `ids`, the persistent entities this node holds (owned, or in transit towards a host
    /// that may be the dead one — the transfer is cancelled and the entity stays here): the
    /// adopter that just restored them from a dead shard must drop its duplicates.
    pub fn duplicates_of(
        &self,
        name: &str,
        from_node: Uuid,
        ids: &[EntityId],
    ) -> Vec<(EntityId, CellId, bool)> {
        let Ok(w) = self.get(name) else {
            return Vec::new();
        };
        let st = w.lock();
        // the directory decides who hosts the cell, not a local lease: a host that lost the cell
        // (or a zombie that still believes it has it) must not make the new owner drop its copy
        let hosted_here = |cell: CellId| match self.cell_hosts.get(&(name.to_string(), cell)) {
            Some(h) => *h == self.node_id,
            None => !st.hosted_only,
        };
        ids.iter()
            .filter_map(|id| {
                let e = st.world.get(*id)?;
                let mine = self.provisional.contains(&(name.to_string(), *id));
                // the announcer's copy is provisional by definition
                (e.persistent
                    && hosted_here(e.cell)
                    && copy_wins(mine, self.node_id, true, from_node))
                .then_some((*id, e.cell, mine))
            })
            .collect()
    }

    /// Drop restored copies another host still owns live (see `duplicates_of`).
    pub fn drop_duplicates(
        &self,
        name: &str,
        from_node: Uuid,
        ids: &[(EntityId, CellId, bool)],
    ) -> usize {
        let Ok(w) = self.get(name) else {
            return 0;
        };
        let mut st = w.lock();
        let st = &mut *st;
        let mut n = 0;
        let mut dropped_ids: Vec<EntityId> = Vec::new();
        for (id, cell, theirs_provisional) in ids {
            // only the node the directory names as host of the cell it claims the entity in
            // may make us drop ours: a zombie that lost the cell (or has not noticed its
            // replica took over) is not believed; a lease we have not learned about yet is
            // covered by the repeated announcements
            if self.cell_hosts.get(&(name.to_string(), *cell)).map(|h| *h) != Some(from_node) {
                tracing::debug!(world = name, %id, ?cell, %from_node, "drop request ignored: sender is not the cell's host");
                continue;
            }
            if st.world.get(*id).is_none() {
                continue;
            }
            let mine = self.provisional.contains(&(name.to_string(), *id));
            // a drop is only ever aimed at a provisional copy: if ours has since become the live
            // one (the requester handed the entity to us after it computed the drop), the request
            // is stale — honouring it would despawn the last copy (the stand lost one exactly so)
            if !mine || copy_wins(true, self.node_id, *theirs_provisional, from_node) {
                // ours wins (live beats provisional, or the tie goes to us): the sender drops
                continue;
            }
            self.provisional.remove(&(name.to_string(), *id));
            st.world.forget_for_clients(*id);
            if st.world.despawn(*id) {
                n += 1;
                dropped_ids.push(*id);
                st.journal.push(WorldOp {
                    tick: st.world.tick,
                    op: WorldMutation::Despawn { id: *id },
                    source: Some(Provenance {
                        by: "dedupe".into(),
                        sim: None,
                    }),
                });
            }
        }
        if n > 0 {
            tracing::info!(
                world = name,
                dropped = n,
                ids = ?dropped_ids,
                "duplicate copies restored from a dead shard dropped"
            );
        }
        n
    }

    /// One tick of one world: simulation step, handovers, admission, decay, then the per-client
    /// delta fan-out. Worlds are independent, so the ticker runs these concurrently.
    /// Stream every hosted cell to its standby replicas (`WorldConfig::replicas`): one delta per
    /// tick per cell, a keyframe when the replica set changes.
    fn ship_replication(&self, name: &str, st: &mut WorldState, fanout: &Fanout) {
        let n = st.world.cfg.replicas as usize;
        if n == 0 {
            st.repl_send.clear();
            st.repl_targets.clear();
            st.repl_acks.clear();
            return;
        }
        let peers = self.peers.read().map(|p| p.clone()).unwrap_or_default();
        let hosted: Vec<CellId> = st
            .world
            .occupied_cells()
            .into_iter()
            .filter(|c| self.hosts_cell(st, name, *c))
            .collect();
        st.repl_send.retain(|c, _| hosted.contains(c));
        st.repl_targets.retain(|c, _| hosted.contains(c));
        st.repl_acks.retain(|c, _| hosted.contains(c));
        for cell in hosted {
            let targets = Self::pick_replicas(&peers, name, cell, n);
            let mut gs = st.repl_send.remove(&cell).unwrap_or_default();
            if st.repl_targets.get(&cell) != Some(&targets) {
                gs.reset();
                st.repl_acks.remove(&cell);
                st.repl_targets.insert(cell, targets.clone());
            }
            if targets.is_empty() {
                continue;
            }
            if let Some(acks) = st.repl_acks.get_mut(&cell) {
                acks.retain(|n, _| targets.contains(n));
            }
            if let Some(d) = st.world.repl_delta(cell, &mut gs) {
                fanout.world_repl(name, self.node_id, self.epoch(), targets, d);
            }
            st.repl_send.insert(cell, gs);
        }
    }

    /// Entities restored from a dead shard (or kept after a cancelled handover) are announced so a
    /// host already owning a live copy answers with a drop; see `queue_adopted`.
    fn announce_adopted(&self, name: &str, fanout: &Fanout) {
        // announce what we restored from a dead shard: a host already owning a live copy (the
        // entity handed over before the crash, the Despawn never reached the journal) answers
        // with a drop
        // announced several times over ADOPT_ANNOUNCE_TICKS: a transfer from the dead host may
        // still be in flight to its target, or the target may still hold the entity in transit
        // towards the dead host and only claim it back once it notices the death
        let mut announce: Vec<((String, CellId), Vec<EntityId>)> = Vec::new();
        self.adopted_pending.retain(|key, (ids, left)| {
            if key.0 != name {
                return true;
            }
            if *left % ADOPT_ANNOUNCE_EVERY == 0 {
                announce.push((key.clone(), ids.clone()));
            }
            *left = left.saturating_sub(1);
            if *left == 0 {
                for id in ids.iter() {
                    self.provisional.remove(&(key.0.clone(), *id));
                }
            }
            *left > 0
        });
        for ((name, cell), ids) in announce {
            tracing::info!(world = %name, ?cell, ?ids, "announcing entities adopted from a dead shard");
            fanout.world_adopted(&name, cell, self.node_id, ids);
        }
    }

    pub fn tick_world(&self, name: &str, fanout: &Fanout) {
        self.announce_adopted(name, fanout);
        let Some(w) = self.worlds.get(name).map(|w| w.clone()) else {
            return;
        };
        let name = name.to_string();
        {
            let started = std::time::Instant::now();
            let mut guard = w.lock();
            let t_lock = started.elapsed();
            let st = &mut *guard;
            self.drain_interest(&name, st);
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
                for (id, cell, host) in remote {
                    st.remote_transit.insert(id, (host, 1, cell));
                    if let Some(e) = st.world.get(id).cloned() {
                        let inputs = st.world.take_buffered_inputs(id);
                        let from = e.transit.as_ref().map(|t| t.from);
                        let group = e
                            .transit
                            .as_ref()
                            .map(|t| t.group.clone())
                            .unwrap_or_default();
                        let mut shipped = e;
                        shipped.transit = None;
                        ship_handover(&mut *st, fanout, &name, self.node_id, shipped, inputs);
                        // attached entities travel with their root (plan 4.5)
                        for m in group {
                            if let Some(mut part) = st.world.get(m).cloned() {
                                let part_inputs = st.world.take_buffered_inputs(m);
                                part.transit = None;
                                ship_handover(
                                    &mut *st,
                                    fanout,
                                    &name,
                                    self.node_id,
                                    part,
                                    part_inputs,
                                );
                            }
                        }
                        if let Some(from) = from {
                            push_event(
                                st,
                                from,
                                SimEvent::<WorldEntity>::HandoverStarted {
                                    entity: id,
                                    to: host,
                                    node: Some(host),
                                    group: None,
                                },
                            );
                        }
                    }
                }
                let leased: Vec<CellId> = st
                    .world
                    .leased_cells()
                    .into_iter()
                    .map(|(c, _)| c)
                    .collect();
                st.ghost_send.retain(|c, _| leased.contains(c));
                for cell in leased {
                    let mut gs = st.ghost_send.remove(&cell).unwrap_or_default();
                    if let Some(d) = st.world.ghost_delta(cell, &mut gs) {
                        fanout.world_ghosts(&name, d);
                    }
                    st.ghost_send.insert(cell, gs);
                }
                let (now, stale) = (st.world.tick, st.world.config().ghost_stale_ticks);
                st.remote_ghosts.retain(|_, g| !g.stale(now, stale));
                self.ship_replication(&name, st, fanout);
                st.tx_locks.retain(|_, (_, until)| *until > now);
                let timed_out: Vec<Uuid> = st
                    .txs
                    .iter()
                    .filter(|(_, r)| r.state == TxState::Pending && r.deadline_tick <= now)
                    .map(|(t, _)| *t)
                    .collect();
                for tx in timed_out {
                    if let Some(r) = st.txs.get_mut(&tx) {
                        r.state = TxState::Aborted {
                            reason: "timeout: not every host voted".into(),
                        };
                        r.forget_tick = now + TX_KEEP_TICKS;
                    }
                    tx_release(&mut *st, tx);
                    fanout.world_tx_abort(&name, tx, "timeout: not every host voted".into());
                }
                st.txs.retain(|_, r| r.forget_tick > now);
            }
            if st.world.tick % JOURNAL_EVERY == 0 && !st.journal.is_empty() {
                if let Some(db) = self.db.clone() {
                    let (ops, tick, n, node, term) = (
                        std::mem::take(&mut st.journal),
                        st.world.tick,
                        name.clone(),
                        self.node_id,
                        self.host_terms.get(&name).map(|t| *t),
                    );
                    let world = w.clone();
                    tokio::spawn(async move {
                        let Some(term) = term else {
                            tracing::warn!(world = %n, "world journal skipped: not an enrolled host");
                            return;
                        };
                        if let Err(e) = Self::write_journal(&db, &n, node, term, &ops, tick).await {
                            tracing::warn!(world = %n, error = %e, "world journal failed");
                            requeue_journal(&world, ops, &e);
                        }
                    });
                }
            }
            if st.world.tick % SNAPSHOT_EVERY == 0 {
                if let Some(db) = self.db.clone() {
                    let (entities, tick, n, node, term) = (
                        st.world.snapshot_persistent(),
                        st.world.tick,
                        name.clone(),
                        self.node_id,
                        self.host_terms.get(&name).map(|t| *t),
                    );
                    tokio::spawn(async move {
                        let Some(term) = term else {
                            tracing::warn!(world = %n, "world snapshot skipped: not an enrolled host");
                            return;
                        };
                        if let Err(e) =
                            Self::write_snapshot(&db, &n, node, term, entities, tick).await
                        {
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
                self.request_placement_hints(st, &name);
                self.offload_cells(st, &name, fanout);
                for (cell, sim, ephemeral) in st.controller.hibernate(&mut st.world) {
                    if let Some(sim) = sim {
                        push_event(
                            st,
                            sim,
                            SimEvent::<WorldEntity>::CellHibernated {
                                cell,
                                ephemeral_despawned: ephemeral,
                                persistent_kept: st.world.entities_in(cell),
                            },
                        );
                    }
                    tracing::info!(world = %name, ?cell, ephemeral, "cell hibernated");
                    st.hibernated_since.insert(cell, st.world.tick);
                }
                self.unload_cold_cells(&mut *st, &name);
                self.reload_cold_cells(&mut *st, &name);
            }
            for (cell, scale) in st.controller.dilate(&mut st.world) {
                if let Some(owner) = st.world.owner(cell) {
                    push_event(
                        st,
                        owner,
                        SimEvent::<WorldEntity>::TimeScale { cell, scale },
                    );
                }
                tracing::info!(world = %name, ?cell, scale, "cell time scale changed");
            }
            for o in outcomes {
                match o {
                    HandoverOutcome::Started {
                        entity,
                        from,
                        to,
                        group,
                    } => {
                        hoj(
                            &name,
                            entity,
                            st.world.tick,
                            "started",
                            "local",
                            format!("{from} -> {to}, group {}", group.len()),
                        );
                        let full = st.world.get(entity).map(to_wire);
                        let parts: Vec<_> = group
                            .iter()
                            .filter_map(|m| st.world.get(*m).map(to_wire))
                            .collect();
                        push_event(
                            st,
                            to,
                            SimEvent::HandoverRequest {
                                entity,
                                from,
                                state: full,
                                group: parts,
                            },
                        );
                        push_event(
                            st,
                            from,
                            SimEvent::<WorldEntity>::HandoverStarted {
                                entity,
                                to,
                                node: None,
                                group: Some(group),
                            },
                        );
                    }
                    HandoverOutcome::Completed { entity, to } => {
                        metrics::counter!("musubi_world_handover_ended_total", "path" => "completed")
                            .increment(1);
                        hoj(
                            &name,
                            entity,
                            st.world.tick,
                            "ended",
                            "completed",
                            format!("to {to}"),
                        );
                    }
                    HandoverOutcome::Cancelled { entity, .. }
                        if st.remote_transit.contains_key(&entity) =>
                    {
                        metrics::counter!("musubi_world_handover_ended_total", "path" => "remote_reship")
                            .increment(1);
                        hoj(
                            &name,
                            entity,
                            st.world.tick,
                            "ended",
                            "remote_reship",
                            format!(
                                "no ack in time, transit {:?}",
                                st.remote_transit.get(&entity)
                            ),
                        );
                        // no ack from the other node yet: ship it again (the receiver re-acks if it
                        // already has it). Never give up: the entity sits in a cell another node
                        // hosts, and a copy kept alive here would become a second owner once the
                        // real one moves on (the stand saw 5 of 20 entities duplicated that way).
                        let Some((stale, n, target)) = st.remote_transit.get(&entity).copied()
                        else {
                            continue;
                        };
                        // the target may have moved since the first attempt (host died and the
                        // cell was re-homed, home adopted by another node): resolve it again
                        // by the cell the handover was for, not the entity's position: an entity
                        // offered from inside the border margin (or moved back by its client) still
                        // sits in our cell while the other side may already have taken it
                        let fresh = Some(target).and_then(|c| {
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
                            // nobody hosts the target cell yet (its host died, the lease is not
                            // re-issued): keep the entity in transit and retry — the home takes
                            // the cell within the failover round, and if the lease lands on
                            // another node the entity must follow it, not stay here orphaned
                            let ours = self.hosts_cell(st, &name, target);
                            if !ours {
                                continue;
                            }
                            // the cell is ours now: the entity stays — but the copy we shipped may
                            // have been accepted by a host whose ack never arrived, so announce it
                            // like a restored one and let a live owner answer with a drop
                            st.remote_transit.remove(&entity);
                            st.kept_after_cancel.insert(entity, target);
                            st.world.cancel_handover(entity);
                            if let Some(c) = st.world.get(entity).map(|e| e.cell) {
                                self.queue_adopted(&name, std::iter::once((entity, c)));
                            }
                            tracing::info!(world = %name, %entity, was = %stale, cell = ?st.world.get(entity).map(|e| e.cell), "cross-node handover dropped: target cell is local now");
                            continue;
                        };
                        if host != stale {
                            tracing::info!(world = %name, %entity, from = %stale, to = %host, "cross-node handover re-targeted");
                        }
                        if n + 1 == REMOTE_HANDOVER_RETRIES {
                            tracing::warn!(world = %name, %entity, %host, tries = n + 1, "cross-node handover still unacknowledged; keeps retrying");
                        }
                        if traced(entity) {
                            tracing::debug!(world = %name, %entity, attempt = n + 1, ms = musubi_cluster::unix_ms(), "hotrace: expired, re-shipping");
                        }
                        st.remote_transit.insert(entity, (host, n + 1, target));
                        if st.world.begin_remote_handover(entity, host) {
                            let group: Vec<EntityId> = st
                                .world
                                .get(entity)
                                .and_then(|e| e.transit.as_ref().map(|t| t.group.clone()))
                                .unwrap_or_default();
                            for m in std::iter::once(entity).chain(group) {
                                if let Some(mut e) = st.world.get(m).cloned() {
                                    e.transit = None;
                                    ship_handover(
                                        &mut *st,
                                        fanout,
                                        &name,
                                        self.node_id,
                                        e,
                                        Vec::new(),
                                    );
                                }
                            }
                        }
                    }
                    HandoverOutcome::Cancelled { entity, reason } => {
                        metrics::counter!("musubi_world_handover_ended_total", "path" => "cancelled_local")
                            .increment(1);
                        hoj(
                            &name,
                            entity,
                            st.world.tick,
                            "ended",
                            "cancelled_local",
                            reason.clone(),
                        );
                        st.remote_transit.remove(&entity);
                        let sims: Vec<Uuid> = st.sim_events.keys().copied().collect();
                        for s in sims {
                            push_event(
                                st,
                                s,
                                SimEvent::<WorldEntity>::HandoverCancelled {
                                    entity,
                                    reason: reason.clone(),
                                },
                            );
                        }
                    }
                }
            }
            let tick = st.world.tick;
            let t_sim = started.elapsed();
            metrics::histogram!("musubi_world_tick_duration_seconds", "world" => name.clone())
                .record(started.elapsed().as_secs_f64());
            metrics::gauge!("musubi_world_entities", "world" => name.clone())
                .set(st.world.len() as f64);
            // per-client delta build + send: the part of a tick that grows with players, so it
            // is measured apart from the simulation step above. Deltas are built under the lock
            // (they read the world); wire messages are built and sent after it is released.
            let fan_started = std::time::Instant::now();
            let by_client: HashMap<Uuid, SessionId> =
                st.subs.iter().map(|(s, c)| (*c, *s)).collect();
            // the lock is held for a copy of the world, not for the delta build: simulator
            // writes and the sessions' unsubscribes no longer wait behind thousands of clients
            let view = st.world.snapshot();
            let mut interests = st.world.take_interests();
            drop(guard);
            // a remote session whose last frame is still waiting on the session lane gets no
            // new delta this tick: its interest keeps the change and the next delta carries it
            // (building one anyway only grew the lane queue — 7–10 s of wait on the stand)
            let congested: Vec<ClientId> = interests
                .keys()
                .filter(|c| {
                    by_client
                        .get(c)
                        .is_some_and(|sid| fanout.session_congested(*sid))
                })
                .copied()
                .collect();
            let deferred: Vec<(ClientId, Interest)> = congested
                .iter()
                .filter_map(|c| interests.remove_entry(c))
                .collect();
            if !deferred.is_empty() {
                metrics::counter!("musubi_world_deltas_deferred_total")
                    .increment(deferred.len() as u64);
            }
            let pending: Vec<(SessionId, f64, Delta)> = view
                .deltas_all(&mut interests)
                .into_iter()
                .filter_map(|(client, d)| {
                    let sid = *by_client.get(&client)?;
                    let scale = interests.get(&client).map_or(1.0, |i| view.scale_for(i));
                    Some((sid, scale, d))
                })
                .collect();
            interests.extend(deferred);
            let keyframe = tick % view.cfg.keyframe_ticks.max(1) == 0;
            let mut msgs: Vec<(SessionId, ServerMessage)> = Vec::with_capacity(pending.len());
            {
                let mut guard = w.lock();
                let st = &mut *guard;
                let live: std::collections::HashSet<Uuid> = st.subs.values().copied().collect();
                st.world.restore_interests(interests, |c| live.contains(c));
                let mut had_delta = std::collections::HashSet::new();
                for (sid, scale, d) in pending {
                    had_delta.insert(sid);
                    let opts = st.subs.get(&sid).and_then(|c| st.sub_opts.get_mut(c));
                    match opts {
                        Some(o) if o.unreliable => {
                            split_delta(&name, tick, scale, d, o, keyframe, &view, &mut msgs, sid)
                        }
                        _ => msgs.push((sid, delta_msg(&name, tick, scale, d, false))),
                    }
                }
                if keyframe {
                    // clients with nothing new this tick still get their unconfirmed positions
                    for (sid, client) in &st.subs {
                        if had_delta.contains(sid) {
                            continue;
                        }
                        let Some(o) = st.sub_opts.get_mut(client) else {
                            continue;
                        };
                        if !o.unreliable || o.unconfirmed.is_empty() {
                            continue;
                        }
                        let refs = keyframe_refs(o, &std::collections::HashSet::new(), &view);
                        if !refs.is_empty() {
                            let scale = st.world.scale_for_client(*client);
                            msgs.push((
                                *sid,
                                ServerMessage::WorldDelta {
                                    world: name.clone(),
                                    tick,
                                    time_scale: scale,
                                    spawned: Vec::new(),
                                    updated: refs.into_iter().map(ref_to_wire).collect(),
                                    despawned: Vec::new(),
                                    sequenced: false,
                                },
                            ));
                        }
                    }
                }
            }
            drop(view);
            let t_deltas = fan_started.elapsed();
            let frames = msgs.len() as u64;
            let t_build = fan_started.elapsed();
            for (sid, msg) in msgs {
                fanout.session(sid, msg);
            }
            let t_send = fan_started.elapsed();
            if tick % 150 == 0 && std::env::var_os("MUSUBI_TICK_TRACE").is_some() {
                tracing::warn!(
                    world = %name,
                    tick,
                    frames,
                    lock_ms = t_lock.as_secs_f64() * 1e3,
                    sim_ms = (t_sim - t_lock).as_secs_f64() * 1e3,
                    deltas_ms = t_deltas.as_secs_f64() * 1e3,
                    build_ms = (t_build - t_deltas).as_secs_f64() * 1e3,
                    send_ms = (t_send - t_build).as_secs_f64() * 1e3,
                    "tick trace"
                );
            }
            metrics::histogram!("musubi_world_fanout_duration_seconds", "world" => name.clone())
                .record(fan_started.elapsed().as_secs_f64());
            metrics::counter!("musubi_world_frames_total", "world" => name.clone())
                .increment(frames);
        }
    }

    /// Every world ticks on its own thread with its own clock. One ticker that awaited all
    /// worlds ran the node at the pace of its slowest world — and with twenty worlds fighting
    /// the runtime for cores, that pace was a quarter of `TICK_HZ`. A ticker thread exits
    /// when its world leaves this node.
    fn ensure_tickers(self: &Arc<Self>, fanout: &Fanout) {
        let period = Duration::from_millis(1000 / TICK_HZ);
        // the fan-out spawns onto the runtime; a plain thread has no runtime context of its own
        let rt = tokio::runtime::Handle::current();
        let _ = self.rt.set(rt.clone());
        let _ = self.me.set(Arc::downgrade(self));
        for w in self.worlds.iter() {
            let name = w.key().clone();
            if !self.ticking.lock().insert(name.clone()) {
                continue;
            }
            let (this, fanout, rt) = (self.clone(), fanout.clone(), rt.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("tick-{name}"))
                .spawn(move || {
                    let _rt = rt.enter();
                    let mut next = std::time::Instant::now();
                    while this.worlds.contains_key(&name) {
                        this.tick_world(&name, &fanout);
                        next += period;
                        let now = std::time::Instant::now();
                        if next > now {
                            std::thread::sleep(next - now);
                        } else {
                            // behind: do not try to catch up, the next tick is simply now
                            next = now;
                        }
                    }
                    this.ticking.lock().remove(&name);
                });
            if spawned.is_err() {
                self.ticking.lock().remove(w.key());
            }
        }
    }

    /// Tick numbers as they complete (lagging receivers skip ahead, never block the ticker).
    pub fn subscribe_ticks(&self) -> tokio::sync::broadcast::Receiver<u64> {
        self.ticks.subscribe()
    }

    /// Starts a ticker thread for every world on this node (and for worlds that arrive later)
    /// and publishes the node tick number at `TICK_HZ` for simulator streams.
    /// Re-read the alive peers replicas are picked from (`PEER_REFRESH_TICKS`).
    pub async fn refresh_peers(&self) {
        let Some(c) = self.cluster.get() else {
            return;
        };
        let Ok(nodes) = c.nodes().await else {
            return;
        };
        let mut peers: Vec<Uuid> = nodes
            .iter()
            .filter(|n| n.alive && n.id.0 != self.node_id)
            .map(|n| n.id.0)
            .collect();
        peers.sort();
        if let Ok(mut p) = self.peers.write() {
            *p = peers;
        }
    }

    pub fn spawn_ticker(self: Arc<Self>, fanout: Fanout) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(1000 / TICK_HZ));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                self.ensure_tickers(&fanout);
                let t = self
                    .tick_no
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    + 1;
                if t % PEER_REFRESH_TICKS == 1 {
                    self.refresh_peers().await;
                }
                if t % CELL_HOST_RECONCILE_TICKS == 7 {
                    self.reconcile_cell_hosts(&fanout).await;
                }
                if t % TICK_HZ == 3 {
                    if let Some(db) = &self.db {
                        if let Err(e) = super::hojournal::flush(db).await {
                            tracing::warn!(error = %e, "handover journal flush failed");
                        }
                    }
                }
                let _ = self.ticks.send(t);
            }
        })
    }
}

impl WorldRegistry {
    /// `custom` placement strategy: ask the `hook_mesh_place` module export where the free
    /// cells should go — `{world, tick, cells, sims: [{id, capacity, cells, players, cost}]}` →
    /// `{placements: [{cell, sim}]}` — off the tick; the answers steer `pick` from the next
    /// control round on (anything outside the live candidates falls back to adjacency).
    /// Home-node cell offload (the replication layer spread over nodes without an operator):
    /// when the cells this home shard hosts cost more than `offload_above_cost` and a peer with
    /// the `sim` role is alive, the hottest local cell without entities in transit is assigned to
    /// the peer hosting the fewest cells of this world — the same path as
    /// `POST /v1/admin/worlds/{w}/cells/{x}/{y}/host` (directory write, `WorldCellAssign` on the
    /// bus, snapshot here, adoption there). One cell per control round, then a cooldown.
    fn offload_cells(&self, st: &mut WorldState, name: &str, fanout: &Fanout) {
        let threshold = st.controller.cfg.offload_above_cost;
        if st.hosted_only || threshold <= 0.0 {
            return;
        }
        if let Some(mut left) = self.offload_cooldown.get_mut(name) {
            if *left > 0 {
                *left -= 1;
                return;
            }
        }
        let peers: Vec<Uuid> = self.peers.read().map(|p| p.clone()).unwrap_or_default();
        if peers.is_empty() {
            return;
        }
        let Some(rt) = self.rt.get().cloned() else {
            return;
        };
        let mut local: Vec<(f64, CellId)> = st
            .world
            .leased_cells()
            .into_iter()
            .map(|(c, _)| c)
            .filter(|c| {
                self.cell_host_cached(name, *c)
                    .is_none_or(|h| h == self.node_id)
            })
            .map(|c| (st.controller.cell_cost(&st.world, c), c))
            .collect();
        let total: f64 = local.iter().map(|(cost, _)| cost).sum();
        if total <= threshold || local.len() < 2 {
            return;
        }
        local.sort_by(|a, b| b.0.total_cmp(&a.0));
        let Some((cost, cell)) = local
            .into_iter()
            .find(|(_, c)| !st.world.entities_of(*c).iter().any(|e| e.transit.is_some()))
        else {
            return;
        };
        // least-loaded peer: fewest cells of this world assigned to it in the directory cache
        let target = peers
            .iter()
            .copied()
            .min_by_key(|p| {
                self.cell_hosts
                    .iter()
                    .filter(|e| e.key().0 == name && *e.value() == *p)
                    .count()
            })
            .expect("peers non-empty");
        self.offload_cooldown
            .insert(name.to_string(), st.controller.cfg.offload_cooldown_rounds);
        metrics::counter!("musubi_world_cells_offloaded_total", "world" => name.to_string())
            .increment(1);
        tracing::info!(world = %name, ?cell, cost, total, threshold, to = %target, "offloading cell to peer");
        let Some(this) = self.me.get().and_then(|w| w.upgrade()) else {
            return;
        };
        let (fanout, name) = (fanout.clone(), name.to_string());
        rt.spawn(async move {
            // the new host adopts the cell from the database: persist before announcing
            if let Err(e) = this.snapshot_now(&name).await {
                tracing::warn!(world = %name, ?cell, error = %e, "cell offload: shard snapshot failed");
                return;
            }
            if let Err(e) = this.publish_cell_host(&name, cell, Some(target)).await {
                tracing::warn!(world = %name, ?cell, error = %e, "cell offload: directory write failed");
                return;
            }
            fanout.cell_assign(&name, cell, target);
            // the bus does not echo to the publisher: hand the cell over here as the bus handler
            // does on any other previous host
            this.release_cell(&name, cell, target, &fanout);
            this.forget_adoptions(&name, cell);
            this.note_cell_host(&name, cell, target);
        });
    }

    fn request_placement_hints(&self, st: &mut WorldState, name: &str) {
        if !matches!(st.controller.cfg.strategy, Strategy::Custom) {
            return;
        }
        let Some(rt) = self.runtime.get() else {
            return;
        };
        if !rt.has_hook("hook_mesh_place") {
            return;
        }
        let mut cells: Vec<CellId> = st
            .world
            .orphaned_cells()
            .into_iter()
            .chain(st.world.interest_cells())
            .filter(|c| st.world.owner(*c).is_none())
            .collect();
        cells.sort();
        cells.dedup();
        if cells.is_empty() {
            return;
        }
        let sims: Vec<Value> = st
            .controller
            .sims()
            .iter()
            .map(|s| {
                json!({
                    "id": s.id,
                    "capacity": s.capacity,
                    "cells": st.world.cells_of(s.id).len(),
                    "players": st.controller.players_of(&st.world, s.id),
                    "cost": st.controller.cost(&st.world, s.id),
                })
            })
            .collect();
        let sim_ids: Vec<Uuid> = st.controller.sims().iter().map(|s| s.id).collect();
        let payload = json!({ "world": name, "tick": st.world.tick, "cells": cells, "sims": sims });
        let hints = st.controller.hints();
        let rt = rt.clone();
        let world = name.to_string();
        tokio::spawn(async move {
            let ctx = CallContext {
                server: true,
                ..Default::default()
            };
            let bytes = serde_json::to_vec(&payload).unwrap_or_default();
            match rt.hook("hook_mesh_place", ctx, bytes).await {
                Some(Ok(out)) => {
                    let v: Value = serde_json::from_slice(&out).unwrap_or_default();
                    let mut h = hints.lock().unwrap_or_else(|e| e.into_inner());
                    h.clear();
                    for p in v["placements"].as_array().into_iter().flatten() {
                        let cell = serde_json::from_value::<CellId>(p["cell"].clone()).ok();
                        // `sim` by id, or `sim_index` into the `sims` list of the request
                        let sim = p["sim"]
                            .as_str()
                            .and_then(|s| Uuid::parse_str(s).ok())
                            .or_else(|| {
                                p["sim_index"]
                                    .as_u64()
                                    .and_then(|i| sim_ids.get(i as usize).copied())
                            });
                        if let (Some(c), Some(s)) = (cell, sim) {
                            h.insert(c, s);
                        }
                    }
                }
                Some(Err(e)) => tracing::warn!(%world, error = %e, "hook_mesh_place failed"),
                None => {}
            }
        });
    }
}

fn announce(st: &mut WorldState, a: &Assignment) {
    if let Some(from) = a.from {
        push_event(
            st,
            from,
            SimEvent::<WorldEntity>::CellRevoked {
                cell: a.cell,
                to: a.to,
                reason: a.reason.clone(),
            },
        );
    }
    push_event(
        st,
        a.to,
        SimEvent::<WorldEntity>::CellAssigned {
            cell: a.cell,
            from: a.from,
            reason: a.reason.clone(),
        },
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
    global: bool,
    decay_ticks: Option<u64>,
) -> EntityId {
    let id = st.world.spawn(pos, comps.clone(), persistent);
    st.world.set_decay(id, decay_ticks);
    st.world.set_global(id, global);
    if persistent {
        st.journal.push(WorldOp {
            tick: st.world.tick,
            source: Some(Provenance {
                by: "spawn".into(),
                sim: None,
            }),
            op: WorldMutation::Spawn {
                id,
                pos,
                components: Value::Object(comps.into_iter().collect()),
                decay_ticks,
                global,
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
                source: Some(Provenance {
                    by: "decay".into(),
                    sim: None,
                }),
                op: WorldMutation::Despawn { id },
            });
        }
        if let Some(owner) = st.world.owner(cell) {
            push_event(
                st,
                owner,
                SimEvent::<WorldEntity>::Decayed { entity: id, cell },
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
                    SimEvent::<WorldEntity>::QueueExpired {
                        ticket: e.ticket,
                        cell,
                        reason: (if lost { "lease_lost" } else { "timeout" }).into(),
                    }
                    .to_value(),
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
            st.world.set_global(id, e.global);
            if e.persistent {
                st.journal.push(WorldOp {
                    tick,
                    source: Some(Provenance {
                        by: "queue".into(),
                        sim: None,
                    }),
                    op: WorldMutation::Spawn {
                        id,
                        pos: e.pos,
                        components: Value::Object(e.components.into_iter().collect()),
                        decay_ticks: e.decay_ticks,
                        global: e.global,
                    },
                });
            }
            events.push((
                e.sim,
                SimEvent::QueueAdmitted {
                    ticket: e.ticket,
                    cell,
                    entity: to_wire(st.world.get(id).unwrap()),
                }
                .to_value(),
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
                    SimEvent::<WorldEntity>::QueueUpdate {
                        ticket: e.ticket,
                        cell,
                        position,
                        eta_ticks: eta,
                    }
                    .to_value(),
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

fn push_event(st: &mut WorldState, sim: Uuid, ev: impl Into<Value>) {
    let q = st.sim_events.entry(sim).or_default();
    if q.len() < MAX_SIM_EVENTS {
        q.push(ev.into());
    }
}

fn delta_msg(world: &str, tick: u64, scale: f64, d: Delta, sequenced: bool) -> ServerMessage {
    ServerMessage::WorldDelta {
        world: world.to_string(),
        tick,
        time_scale: scale,
        spawned: d.spawned.into_iter().map(ref_to_wire).collect(),
        updated: d.updated.into_iter().map(ref_to_wire).collect(),
        despawned: d.despawned.clone(),
        sequenced,
    }
}

/// A client on unreliable updates gets up to two frames per tick: a reliable one with spawns,
/// despawns and component changes (plus, on a keyframe, the current positions of entities whose
/// last position it only got unreliably), and a sequenced unreliable one with position-only
/// updates. A lost datagram is superseded by the entity's next move or by the keyframe.
/// Current positions of the entities whose last position the client only got unreliably
/// (minus the ones moving again this tick); clears the set.
fn keyframe_refs(
    o: &mut SubOpts,
    fresh: &std::collections::HashSet<musubi_mesh::EntityId>,
    view: &musubi_mesh::WorldView,
) -> Vec<musubi_mesh::EntityRef> {
    o.unconfirmed
        .drain()
        .filter(|id| !fresh.contains(id))
        .filter_map(|id| {
            view.pos_of(id).map(|pos| musubi_mesh::EntityRef {
                id,
                pos,
                rev: 0,
                components: Vec::new(),
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn split_delta(
    world: &str,
    tick: u64,
    scale: f64,
    mut d: Delta,
    o: &mut SubOpts,
    keyframe: bool,
    view: &musubi_mesh::WorldView,
    out: &mut Vec<(SessionId, ServerMessage)>,
    sid: SessionId,
) {
    let (reliable, pos_only): (Vec<_>, Vec<_>) = std::mem::take(&mut d.updated)
        .into_iter()
        .partition(|e| !e.components.is_empty());
    for e in &reliable {
        o.unconfirmed.remove(&e.id);
    }
    for id in &d.despawned {
        o.unconfirmed.remove(id);
    }
    let mut reliable = reliable;
    if keyframe && !o.unconfirmed.is_empty() {
        let fresh: std::collections::HashSet<_> = pos_only.iter().map(|e| e.id).collect();
        reliable.extend(keyframe_refs(o, &fresh, view));
    }
    if !pos_only.is_empty() {
        o.unconfirmed.extend(pos_only.iter().map(|e| e.id));
        out.push((
            sid,
            ServerMessage::WorldDelta {
                world: world.to_string(),
                tick,
                time_scale: scale,
                spawned: Vec::new(),
                updated: pos_only.into_iter().map(ref_to_wire).collect(),
                despawned: Vec::new(),
                sequenced: true,
            },
        ));
    }
    if !d.spawned.is_empty() || !d.despawned.is_empty() || !reliable.is_empty() {
        d.updated = reliable;
        out.push((sid, delta_msg(world, tick, scale, d, false)));
    }
}
