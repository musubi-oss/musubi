//! Server meshing core (v0): the Replication Layer's data model and rules, independent of
//! transport and engine. One continuous world is split into grid cells; a simulator holds a lease
//! on each cell and is authoritative for the entities inside it; entities near a border are
//! replicated to the neighbouring simulator as read-only ghosts; crossing the border (plus
//! hysteresis) triggers a handover that completes within a few ticks or is rolled back. Clients
//! never talk to simulators: they subscribe to an interest radius and receive versioned deltas.
//!
//! Space is 2D here (x, y); 3D/octree and non-spatial partition keys plug into `CellId` later.

pub mod controller;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type EntityId = Uuid;
pub type SimId = Uuid;
pub type ClientId = Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CellId {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Pos {
    pub x: f64,
    pub y: f64,
}

impl Pos {
    pub fn dist(&self, o: &Pos) -> f64 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Component {
    pub value: serde_json::Value,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entity {
    pub id: EntityId,
    pub pos: Pos,
    pub cell: CellId,
    pub components: BTreeMap<String, Component>,
    /// Survives everyone leaving and cluster restarts (persistent entity streaming).
    pub persistent: bool,
    pub transit: Option<Transit>,
    /// Despawned after this many ticks without a write, interaction or handover (`None` = never).
    #[serde(default)]
    pub decay_ticks: Option<u64>,
    /// World tick of the last write, interaction or handover (drives `decay_ticks`).
    #[serde(default)]
    pub touched_tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transit {
    pub from: SimId,
    pub to: SimId,
    pub to_cell: CellId,
    pub requested_tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lease {
    pub sim: SimId,
    pub expires_tick: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldConfig {
    pub cell_size: f64,
    /// Entities this close to a border are ghosted to the neighbour.
    pub ghost_margin: f64,
    /// Extra distance past the border before a handover starts (prevents flapping).
    pub hysteresis: f64,
    pub lease_ttl_ticks: u64,
    pub handover_timeout_ticks: u64,
    /// Default per-client delta budget per tick (bytes, estimated); a subscription may lower it.
    pub client_budget_bytes: usize,
    /// Positions in client deltas are rounded to this step; smaller moves send nothing.
    pub pos_quantum: f64,
    /// Entities a cell accepts by spawn before new spawns into it are refused (0 = unlimited);
    /// handovers always go through — physics cannot wait at a border.
    pub soft_cap: u32,
    /// A simulator whose last reported tick lags the world by more than this many ticks puts
    /// its cells into time dilation.
    pub tidi_after_ticks: u64,
    /// Lowest time scale a dilated cell is allowed to drop to.
    pub tidi_min: f64,
    /// A spawn that asked to queue for a full cell is dropped after waiting this many ticks
    /// (0 = never).
    pub queue_ttl_ticks: u64,
    /// A leased cell no client has watched for this many ticks hibernates: lease revoked,
    /// ephemeral entities gone, persistent ones kept until interest returns (0 = never).
    pub hibernate_after_ticks: u64,
    /// Ticks of position history kept per entity for lag compensation (`position_at`): a hit
    /// claimed at the shooter's tick is checked against where the target was then.
    pub history_ticks: u64,
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            cell_size: 256.0,
            ghost_margin: 48.0,
            hysteresis: 24.0,
            lease_ttl_ticks: 60,
            handover_timeout_ticks: 3,
            client_budget_bytes: 16 * 1024,
            pos_quantum: 0.01,
            soft_cap: 0,
            tidi_after_ticks: 15,
            tidi_min: 0.1,
            queue_ttl_ticks: 600,
            hibernate_after_ticks: 1800,
            history_ticks: 15,
        }
    }
}

/// What a simulator sees for one of its cells each tick.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SimView {
    pub owned: Vec<Entity>,
    pub ghosts: Vec<Entity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum HandoverOutcome {
    Started {
        entity: EntityId,
        from: SimId,
        to: SimId,
    },
    Completed {
        entity: EntityId,
        to: SimId,
    },
    Cancelled {
        entity: EntityId,
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Delta {
    pub spawned: Vec<Entity>,
    pub updated: Vec<(EntityId, Pos, BTreeMap<String, Component>)>,
    pub despawned: Vec<EntityId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Interest {
    pos: Pos,
    radius: f64,
    /// entity → last component versions the client received (pos is always sent on move).
    known: HashMap<EntityId, (Pos, BTreeMap<String, u64>)>,
    /// Per-tick byte budget override (`None` = world default).
    #[serde(default)]
    budget_bytes: Option<usize>,
    /// Fine filter: every listed component must equal the given value (empty = all entities).
    #[serde(default)]
    filter: BTreeMap<String, serde_json::Value>,
    /// entity → tick of the last delta that carried it (priority grows with age).
    #[serde(default)]
    last_sent: HashMap<EntityId, u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum MeshError {
    #[error("entity not found")]
    NoEntity,
    #[error("cell {0:?} has no live lease")]
    NoLease(CellId),
    #[error("not authoritative: {0}")]
    NotAuthoritative(String),
    #[error("entity already in transit")]
    InTransit,
    #[error("cell {0:?} is full (soft_cap {1})")]
    CellFull(CellId, u32),
}

pub struct World {
    pub cfg: WorldConfig,
    pub tick: u64,
    entities: HashMap<EntityId, Entity>,
    by_cell: HashMap<CellId, HashSet<EntityId>>,
    /// Cells put to sleep by `hibernate_cell`: not orphans, re-leased only when interest returns.
    hibernated: HashSet<CellId>,
    leases: HashMap<CellId, Lease>,
    /// Last simulator granted each cell, kept after the lease lapses (who to tell about it).
    last_owners: HashMap<CellId, SimId>,
    interests: HashMap<ClientId, Interest>,
    /// Inputs that arrived while the target entity was in transit; delivered to the new owner.
    buffered_inputs: HashMap<EntityId, Vec<serde_json::Value>>,
    /// Recent `(tick, pos)` samples per entity, newest last (lag compensation rewind).
    history: HashMap<EntityId, VecDeque<(u64, Pos)>>,
    /// Co-located entities: authoritative cell forced to a partner's cell until `expires_tick`,
    /// whatever the position says (plan 4.6 `co-locate`).
    pins: HashMap<EntityId, (CellId, u64)>,
    /// Cells running slower than real time (EVE-style time dilation); absent = 1.0.
    cell_scale: HashMap<CellId, f64>,
}

impl World {
    pub fn new(cfg: WorldConfig) -> Self {
        Self {
            cfg,
            tick: 0,
            entities: HashMap::new(),
            by_cell: HashMap::new(),
            hibernated: HashSet::new(),
            last_owners: HashMap::new(),
            leases: HashMap::new(),
            interests: HashMap::new(),
            buffered_inputs: HashMap::new(),
            history: HashMap::new(),
            pins: HashMap::new(),
            cell_scale: HashMap::new(),
        }
    }

    /// Whether a spawn into `cell` is allowed under `soft_cap`.
    pub fn cell_has_room(&self, cell: CellId) -> bool {
        self.cfg.soft_cap == 0 || (self.entities_in(cell) as u32) < self.cfg.soft_cap
    }

    /// Time scale of a cell: 1.0 = real time, lower = dilated.
    pub fn time_scale(&self, cell: CellId) -> f64 {
        self.cell_scale.get(&cell).copied().unwrap_or(1.0)
    }

    /// Set a cell's time scale; returns whether it changed.
    pub fn set_time_scale(&mut self, cell: CellId, scale: f64) -> bool {
        let before = self.time_scale(cell);
        if scale >= 1.0 {
            self.cell_scale.remove(&cell);
        } else {
            self.cell_scale.insert(cell, scale);
        }
        (before - self.time_scale(cell)).abs() > f64::EPSILON
    }

    /// Slowest time scale around a subscribed client (1.0 when unknown or undilated).
    pub fn scale_for_client(&self, client: ClientId) -> f64 {
        self.interests
            .get(&client)
            .map_or(1.0, |i| self.min_scale_near(i.pos, i.radius))
    }

    /// Slowest time scale among the cells within `radius` of `pos` (what a client at `pos`
    /// should interpolate against).
    pub fn min_scale_near(&self, pos: Pos, radius: f64) -> f64 {
        self.cell_scale
            .iter()
            .filter(|(c, _)| self.border_distance(**c, pos) > -radius)
            .map(|(_, s)| *s)
            .fold(1.0, f64::min)
    }

    pub fn cell_of(&self, p: Pos) -> CellId {
        CellId {
            x: (p.x / self.cfg.cell_size).floor() as i32,
            y: (p.y / self.cfg.cell_size).floor() as i32,
        }
    }

    pub fn cell_bounds(&self, c: CellId) -> (Pos, Pos) {
        let s = self.cfg.cell_size;
        (
            Pos {
                x: c.x as f64 * s,
                y: c.y as f64 * s,
            },
            Pos {
                x: (c.x + 1) as f64 * s,
                y: (c.y + 1) as f64 * s,
            },
        )
    }

    /// Distance from `p` to the nearest border of cell `c` (negative = outside by that much).
    pub fn border_distance(&self, c: CellId, p: Pos) -> f64 {
        let (lo, hi) = self.cell_bounds(c);
        let dx = (p.x - lo.x).min(hi.x - p.x);
        let dy = (p.y - lo.y).min(hi.y - p.y);
        dx.min(dy)
    }

    // ----- leases -----

    pub fn grant_lease(&mut self, cell: CellId, sim: SimId) {
        self.hibernated.remove(&cell);
        self.last_owners.insert(cell, sim);
        self.leases.insert(
            cell,
            Lease {
                sim,
                expires_tick: self.tick + self.cfg.lease_ttl_ticks,
            },
        );
    }

    /// Simulator heartbeat for every cell it owns.
    pub fn renew_leases(&mut self, sim: SimId) {
        let exp = self.tick + self.cfg.lease_ttl_ticks;
        for l in self.leases.values_mut().filter(|l| l.sim == sim) {
            l.expires_tick = exp;
        }
    }

    pub fn revoke_lease(&mut self, cell: CellId) -> Option<SimId> {
        self.leases.remove(&cell).map(|l| l.sim)
    }

    /// Last holder even if the lease lapsed (None once revoked).
    pub fn lease_holder(&self, cell: CellId) -> Option<SimId> {
        self.leases.get(&cell).map(|l| l.sim)
    }

    /// The simulator the cell was last granted to, even after its lease lapsed or was revoked.
    pub fn last_owner(&self, cell: CellId) -> Option<SimId> {
        self.last_owners.get(&cell).copied()
    }

    pub fn leased_cells(&self) -> Vec<(CellId, SimId)> {
        let mut v: Vec<(CellId, SimId)> = self
            .leases
            .iter()
            .filter(|(_, l)| l.expires_tick > self.tick)
            .map(|(c, l)| (*c, l.sim))
            .collect();
        v.sort();
        v
    }

    /// Remove leases that lapsed (dead simulators). Returns what was dropped.
    pub fn drop_lapsed_leases(&mut self) -> Vec<(CellId, SimId)> {
        let tick = self.tick;
        let lapsed: Vec<(CellId, SimId)> = self
            .leases
            .iter()
            .filter(|(_, l)| l.expires_tick <= tick)
            .map(|(c, l)| (*c, l.sim))
            .collect();
        for (c, _) in &lapsed {
            self.leases.remove(c);
        }
        lapsed
    }

    /// Cells any subscribed client is looking at.
    pub fn interest_cells(&self) -> Vec<CellId> {
        let mut v: Vec<CellId> = self
            .interests
            .values()
            .flat_map(|i| self.cells_in_radius(i.pos, i.radius))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Clients whose interest is centred in `cell` (the "players" term of a cell's cost).
    pub fn players_in(&self, cell: CellId) -> usize {
        self.interests
            .values()
            .filter(|i| self.cell_of(i.pos) == cell)
            .count()
    }

    pub fn entities_in(&self, cell: CellId) -> usize {
        self.by_cell.get(&cell).map_or(0, |s| s.len())
    }

    /// Cells a circle of interest touches (bounding box).
    pub fn cells_in_radius(&self, p: Pos, radius: f64) -> Vec<CellId> {
        let lo = self.cell_of(Pos {
            x: p.x - radius,
            y: p.y - radius,
        });
        let hi = self.cell_of(Pos {
            x: p.x + radius,
            y: p.y + radius,
        });
        let mut v = Vec::new();
        for x in lo.x..=hi.x {
            for y in lo.y..=hi.y {
                v.push(CellId { x, y });
            }
        }
        v
    }

    pub fn owner(&self, cell: CellId) -> Option<SimId> {
        self.leases
            .get(&cell)
            .filter(|l| l.expires_tick > self.tick)
            .map(|l| l.sim)
    }

    /// Cells whose lease lapsed (simulator died): entities there are frozen until re-leased.
    /// Plan 4.9 scale-to-zero for a cell: its lease is revoked and ephemeral entities despawned;
    /// persistent ones stay in memory (and the next snapshot) and the cell is not an orphan — it
    /// wakes when a client's interest reaches it. Returns the former owner and how many
    /// ephemeral entities died.
    pub fn hibernate_cell(&mut self, cell: CellId) -> (Option<SimId>, usize) {
        let ephemeral: Vec<EntityId> = self
            .entities_of(cell)
            .into_iter()
            .filter(|e| !e.persistent)
            .map(|e| e.id)
            .collect();
        for id in &ephemeral {
            self.despawn(*id);
        }
        let owner = self.revoke_lease(cell);
        self.hibernated.insert(cell);
        (owner, ephemeral.len())
    }

    pub fn is_hibernated(&self, cell: CellId) -> bool {
        self.hibernated.contains(&cell)
    }

    pub fn orphaned_cells(&self) -> Vec<CellId> {
        let mut v: Vec<CellId> = self
            .by_cell
            .keys()
            .filter(|c| {
                self.owner(**c).is_none()
                    && !self.by_cell[c].is_empty()
                    && !self.hibernated.contains(*c)
            })
            .copied()
            .collect();
        v.sort();
        v
    }

    pub fn cells_of(&self, sim: SimId) -> Vec<CellId> {
        let mut v: Vec<CellId> = self
            .leases
            .iter()
            .filter(|(_, l)| l.sim == sim && l.expires_tick > self.tick)
            .map(|(c, _)| *c)
            .collect();
        v.sort();
        v
    }

    // ----- entities -----

    pub fn spawn(
        &mut self,
        pos: Pos,
        components: BTreeMap<String, serde_json::Value>,
        persistent: bool,
    ) -> EntityId {
        self.spawn_with_id(Uuid::now_v7(), pos, components, persistent)
    }

    /// Spawn under a known id (journal replay).
    pub fn spawn_with_id(
        &mut self,
        id: EntityId,
        pos: Pos,
        components: BTreeMap<String, serde_json::Value>,
        persistent: bool,
    ) -> EntityId {
        let cell = self.cell_of(pos);
        let e = Entity {
            id,
            pos,
            cell,
            components: components
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        Component {
                            value: v,
                            version: 1,
                        },
                    )
                })
                .collect(),
            persistent,
            transit: None,
            decay_ticks: None,
            touched_tick: self.tick,
        };
        self.by_cell.entry(cell).or_default().insert(id);
        self.entities.insert(id, e);
        self.record_pos(id, pos);
        id
    }

    fn record_pos(&mut self, id: EntityId, pos: Pos) {
        let (tick, keep) = (self.tick, self.cfg.history_ticks.max(1));
        let h = self.history.entry(id).or_default();
        match h.back_mut() {
            Some(last) if last.0 == tick => last.1 = pos,
            _ => h.push_back((tick, pos)),
        }
        while h
            .front()
            .is_some_and(|(t, _)| tick.saturating_sub(*t) > keep)
        {
            h.pop_front();
        }
    }

    /// Where the entity was at the end of `tick`: its newest recorded position at or before it,
    /// `None` when `tick` is older than the kept history (or before the entity existed).
    pub fn position_at(&self, id: EntityId, tick: u64) -> Option<Pos> {
        self.history
            .get(&id)?
            .iter()
            .rev()
            .find(|(t, _)| *t <= tick)
            .map(|(_, p)| *p)
    }

    /// Oldest tick `position_at` can answer for the entity.
    pub fn history_from(&self, id: EntityId) -> Option<u64> {
        self.history.get(&id)?.front().map(|(t, _)| *t)
    }

    /// Source-style rewind check: was the entity within `tolerance` of `at` at `tick`? `None`
    /// when history does not reach back that far.
    pub fn was_at(&self, id: EntityId, tick: u64, at: Pos, tolerance: f64) -> Option<bool> {
        let p = self.position_at(id, tick)?;
        Some(((p.x - at.x).powi(2) + (p.y - at.y).powi(2)).sqrt() <= tolerance)
    }

    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(e) = self.entities.remove(&id) else {
            return false;
        };
        if let Some(set) = self.by_cell.get_mut(&e.cell) {
            set.remove(&id);
        }
        self.buffered_inputs.remove(&id);
        self.history.remove(&id);
        self.pins.remove(&id);
        true
    }

    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(&id)
    }

    /// Entities currently in `cell` (any state).
    pub fn entities_of(&self, cell: CellId) -> Vec<&Entity> {
        self.by_cell
            .get(&cell)
            .into_iter()
            .flatten()
            .filter_map(|id| self.entities.get(id))
            .collect()
    }

    /// Plan 4.9 `decay(ttl)`: the entity disappears after `ticks` without a write, interaction or
    /// handover (`None` = lives until despawned) — abandoned litter cleans itself up.
    pub fn set_decay(&mut self, id: EntityId, ticks: Option<u64>) -> bool {
        let tick = self.tick;
        match self.entities.get_mut(&id) {
            Some(e) => {
                e.decay_ticks = ticks;
                e.touched_tick = tick;
                true
            }
            None => false,
        }
    }

    /// Marks the entity as touched this tick (resets its decay clock).
    pub fn touch(&mut self, id: EntityId) {
        let tick = self.tick;
        if let Some(e) = self.entities.get_mut(&id) {
            e.touched_tick = tick;
        }
    }

    /// Despawns entities whose decay ran out (never one in transit). Returns
    /// `(id, cell, persistent)` so the caller can journal the persistent ones.
    pub fn decay(&mut self) -> Vec<(EntityId, CellId, bool)> {
        let tick = self.tick;
        let mut gone: Vec<(EntityId, CellId, bool)> = self
            .entities
            .values()
            .filter(|e| e.transit.is_none())
            .filter(|e| {
                e.decay_ticks
                    .is_some_and(|d| tick.saturating_sub(e.touched_tick) >= d)
            })
            .map(|e| (e.id, e.cell, e.persistent))
            .collect();
        gone.sort_by_key(|g| g.0);
        for (id, _, _) in &gone {
            self.despawn(*id);
        }
        gone
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Authoritative update from a simulator: only the current lease holder of the entity's cell may
    /// write, and never while the entity is in transit (the old owner keeps simulating but its deltas
    /// after the request are dropped once the new owner acked — see `handover_ack`).
    pub fn apply(
        &mut self,
        sim: SimId,
        id: EntityId,
        pos: Option<Pos>,
        components: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), MeshError> {
        let cell = self.entities.get(&id).ok_or(MeshError::NoEntity)?.cell;
        let owner = self.owner(cell).ok_or(MeshError::NoLease(cell))?;
        if owner != sim {
            return Err(MeshError::NotAuthoritative(format!(
                "cell {cell:?} is owned by {owner}"
            )));
        }
        self.force_apply(id, pos, components)
    }

    /// `apply` without the ownership check (journal replay on a node that holds no leases).
    pub fn force_apply(
        &mut self,
        id: EntityId,
        pos: Option<Pos>,
        components: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), MeshError> {
        let tick = self.tick;
        let e = self.entities.get_mut(&id).ok_or(MeshError::NoEntity)?;
        e.touched_tick = tick;
        if let Some(p) = pos {
            e.pos = p;
        }
        if let Some(p) = pos {
            self.record_pos(id, p);
        }
        let e = self.entities.get_mut(&id).ok_or(MeshError::NoEntity)?;
        for (k, v) in components {
            let c = e.components.entry(k).or_insert(Component {
                value: serde_json::Value::Null,
                version: 0,
            });
            if c.value != v {
                c.value = v;
                c.version += 1;
            }
        }
        Ok(())
    }

    // ----- ghosts & views -----

    pub fn neighbours(c: CellId) -> impl Iterator<Item = CellId> {
        (-1..=1).flat_map(move |dx| {
            (-1..=1).filter_map(move |dy| {
                (dx != 0 || dy != 0).then_some(CellId {
                    x: c.x + dx,
                    y: c.y + dy,
                })
            })
        })
    }

    /// `pos` lies within `ghost_margin` of `cell` (inside it or just outside its border).
    fn within_margin(&self, cell: CellId, pos: Pos) -> bool {
        let (lo, hi) = self.cell_bounds(cell);
        let m = self.cfg.ghost_margin;
        pos.x >= lo.x - m && pos.x <= hi.x + m && pos.y >= lo.y - m && pos.y <= hi.y + m
    }

    /// Entities in cells adjacent to `cell` that lie within `ghost_margin` of its border.
    pub fn ghosts_for(&self, cell: CellId) -> Vec<&Entity> {
        let mut out = Vec::new();
        for n in Self::neighbours(cell) {
            let Some(ids) = self.by_cell.get(&n) else {
                continue;
            };
            for id in ids {
                let e = &self.entities[id];
                if self.within_margin(cell, e.pos) {
                    out.push(e);
                }
            }
        }
        out.sort_by_key(|e| e.id);
        out
    }

    /// Entities of `cell` within `ghost_margin` of its own border: what the hosts of the
    /// neighbouring cells need as ghosts (sent over the bus when those cells live elsewhere).
    pub fn border_entities(&self, cell: CellId) -> Vec<&Entity> {
        let (lo, hi) = self.cell_bounds(cell);
        let m = self.cfg.ghost_margin;
        let mut out: Vec<&Entity> = self
            .by_cell
            .get(&cell)
            .map(|ids| {
                ids.iter()
                    .map(|i| &self.entities[i])
                    .filter(|e| {
                        e.pos.x < lo.x + m
                            || e.pos.x > hi.x - m
                            || e.pos.y < lo.y + m
                            || e.pos.y > hi.y - m
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|e| e.id);
        out
    }

    /// Of `candidates` (entities of a neighbouring cell, possibly received from another node),
    /// those that are ghosts of `cell`.
    pub fn ghosts_among<'a>(
        &self,
        cell: CellId,
        candidates: impl Iterator<Item = &'a Entity>,
    ) -> Vec<&'a Entity> {
        candidates
            .filter(|e| e.cell != cell && self.within_margin(cell, e.pos))
            .collect()
    }

    /// Everything a simulator needs for one tick of one cell.
    pub fn view(&self, cell: CellId) -> SimView {
        let mut owned: Vec<Entity> = self
            .by_cell
            .get(&cell)
            .map(|ids| ids.iter().map(|i| self.entities[i].clone()).collect())
            .unwrap_or_default();
        owned.sort_by_key(|e| e.id);
        SimView {
            owned,
            ghosts: self.ghosts_for(cell).into_iter().cloned().collect(),
        }
    }

    // ----- handover -----

    /// Called by the Replication Layer after applying a tick: entities that left their cell by more
    /// than `hysteresis` start a handover to the cell they are in now (if it has a live lease held
    /// by another simulator) or simply move cells (same simulator owns both).
    /// Co-locate: until `ttl_ticks` pass, `id`'s authoritative cell is `cell` regardless of its
    /// position — the regular handover path moves it there (state and inputs included) and
    /// hands it back once the pin lapses. Returns the tick the pin expires.
    /// The entity need not be here yet: a node pins before the partner arrives from another
    /// node so that it lands in the pinned cell instead of the one its position says.
    pub fn pin(&mut self, id: EntityId, cell: CellId, ttl_ticks: u64) -> u64 {
        let until = self.tick + ttl_ticks.max(1);
        self.pins.insert(id, (cell, until));
        until
    }

    pub fn unpin(&mut self, id: EntityId) -> bool {
        self.pins.remove(&id).is_some()
    }

    pub fn pinned_to(&self, id: EntityId) -> Option<(CellId, u64)> {
        self.pins.get(&id).copied()
    }

    /// Where authority over the entity belongs: its pin while co-located, else its position.
    fn target_cell(&self, id: EntityId, pos: Pos) -> (CellId, bool) {
        match self.pins.get(&id) {
            Some((c, _)) => (*c, true),
            None => (self.cell_of(pos), false),
        }
    }

    pub fn detect_handovers(&mut self) -> Vec<HandoverOutcome> {
        let mut out = Vec::new();
        let ids: Vec<EntityId> = self.entities.keys().copied().collect();
        for id in ids {
            let (cell, pos, in_transit) = {
                let e = &self.entities[&id];
                (e.cell, e.pos, e.transit.is_some())
            };
            if in_transit {
                continue;
            }
            let (target, pinned) = self.target_cell(id, pos);
            if target == cell || (!pinned && -self.border_distance(cell, pos) < self.cfg.hysteresis)
            {
                continue;
            }
            let from = self.owner(cell);
            let to = self.owner(target);
            match (from, to) {
                (Some(a), Some(b)) if a == b => {
                    self.move_cell(id, target);
                }
                (Some(a), Some(b)) => {
                    let e = self.entities.get_mut(&id).unwrap();
                    e.transit = Some(Transit {
                        from: a,
                        to: b,
                        to_cell: target,
                        requested_tick: self.tick,
                    });
                    out.push(HandoverOutcome::Started {
                        entity: id,
                        from: a,
                        to: b,
                    });
                }
                _ => {
                    // target cell has no simulator yet: keep authority where it is (frozen border)
                }
            }
        }
        out
    }

    /// Handovers into cells that have no simulator here but are hosted on another node (see
    /// `remote_host`): the entity is marked in transit towards that node and returned so the
    /// caller can ship its state; `handover_ack`/`expire_handovers` apply as for local ones.
    pub fn detect_remote_handovers(
        &mut self,
        remote_host: impl Fn(CellId) -> Option<SimId>,
    ) -> Vec<(EntityId, CellId, SimId)> {
        let mut out = Vec::new();
        let ids: Vec<EntityId> = self.entities.keys().copied().collect();
        for id in ids {
            let (cell, pos, in_transit) = {
                let e = &self.entities[&id];
                (e.cell, e.pos, e.transit.is_some())
            };
            if in_transit {
                continue;
            }
            let (target, pinned) = self.target_cell(id, pos);
            if target == cell || (!pinned && -self.border_distance(cell, pos) < self.cfg.hysteresis)
            {
                continue;
            }
            if self.owner(target).is_some() {
                continue;
            }
            let Some(from) = self.owner(cell) else {
                continue;
            };
            let Some(host) = remote_host(target) else {
                continue;
            };
            let e = self.entities.get_mut(&id).unwrap();
            e.transit = Some(Transit {
                from,
                to: host,
                to_cell: target,
                requested_tick: self.tick,
            });
            out.push((id, target, host));
        }
        out
    }

    /// Mark `id` in transit towards `host` again (a cross-node handover being retried). False if
    /// the entity is gone or its cell has no owner here.
    pub fn begin_remote_handover(&mut self, id: EntityId, host: SimId) -> bool {
        let tick = self.tick;
        let Some((cell, pos)) = self.entities.get(&id).map(|e| (e.cell, e.pos)) else {
            return false;
        };
        let Some(from) = self.owner(cell) else {
            return false;
        };
        let to_cell = self.cell_of(pos);
        let e = self.entities.get_mut(&id).unwrap();
        e.transit = Some(Transit {
            from,
            to: host,
            to_cell,
            requested_tick: tick,
        });
        true
    }

    /// Inputs buffered while `id` was in transit.
    pub fn take_buffered_inputs(&mut self, id: EntityId) -> Vec<serde_json::Value> {
        self.buffered_inputs.remove(&id).unwrap_or_default()
    }

    /// Drop `id` from every client's known set without reporting a despawn: the entity moved to
    /// another node's shard, whose own deltas carry it from now on.
    pub fn forget_for_clients(&mut self, id: EntityId) {
        for i in self.interests.values_mut() {
            i.known.remove(&id);
        }
    }

    /// Abort a handover in progress; the entity stays with its current owner.
    pub fn cancel_handover(&mut self, id: EntityId) -> Option<Transit> {
        self.entities.get_mut(&id).and_then(|e| e.transit.take())
    }

    fn move_cell(&mut self, id: EntityId, target: CellId) {
        let e = self.entities.get_mut(&id).unwrap();
        let old = e.cell;
        e.cell = target;
        if let Some(s) = self.by_cell.get_mut(&old) {
            s.remove(&id);
        }
        self.by_cell.entry(target).or_default().insert(id);
    }

    /// The new owner confirms it loaded the entity: authority flips, buffered inputs are returned
    /// for the new owner to apply.
    pub fn handover_ack(
        &mut self,
        sim: SimId,
        id: EntityId,
    ) -> Result<(HandoverOutcome, Vec<serde_json::Value>), MeshError> {
        let t = self
            .entities
            .get(&id)
            .ok_or(MeshError::NoEntity)?
            .transit
            .clone()
            .ok_or(MeshError::NotAuthoritative(
                "no handover in progress".into(),
            ))?;
        if t.to != sim {
            return Err(MeshError::NotAuthoritative(format!(
                "handover target is {}",
                t.to
            )));
        }
        self.move_cell(id, t.to_cell);
        {
            let tick = self.tick;
            let e = self.entities.get_mut(&id).unwrap();
            e.transit = None;
            e.touched_tick = tick;
        }
        let inputs = self.buffered_inputs.remove(&id).unwrap_or_default();
        Ok((
            HandoverOutcome::Completed {
                entity: id,
                to: sim,
            },
            inputs,
        ))
    }

    /// Roll back handovers that were not acked within `handover_timeout_ticks`.
    pub fn expire_handovers(&mut self) -> Vec<HandoverOutcome> {
        let timeout = self.cfg.handover_timeout_ticks;
        let tick = self.tick;
        let mut out = Vec::new();
        for e in self.entities.values_mut() {
            let expired = e
                .transit
                .as_ref()
                .is_some_and(|t| tick.saturating_sub(t.requested_tick) > timeout);
            if expired {
                let t = e.transit.take().unwrap();
                out.push(HandoverOutcome::Cancelled {
                    entity: e.id,
                    reason: format!("no ack from {} within {timeout} ticks", t.to),
                });
            }
        }
        out
    }

    /// Route a player input to the entity's current owner; while in transit it is buffered for the
    /// new owner. Returns the simulator that should receive it now (None = buffered).
    pub fn route_input(
        &mut self,
        id: EntityId,
        input: serde_json::Value,
    ) -> Result<Option<SimId>, MeshError> {
        let e = self.entities.get(&id).ok_or(MeshError::NoEntity)?;
        if e.transit.is_some() {
            self.buffered_inputs.entry(id).or_default().push(input);
            return Ok(None);
        }
        Ok(Some(self.owner(e.cell).ok_or(MeshError::NoLease(e.cell))?))
    }

    // ----- interest & client deltas -----

    pub fn subscribe(&mut self, client: ClientId, pos: Pos, radius: f64) {
        self.subscribe_with(client, pos, radius, None, BTreeMap::new());
    }

    /// Subscribe with a per-tick byte budget and a component-equality filter (see `delta_for`).
    pub fn subscribe_with(
        &mut self,
        client: ClientId,
        pos: Pos,
        radius: f64,
        budget_bytes: Option<usize>,
        filter: BTreeMap<String, serde_json::Value>,
    ) {
        let (known, last_sent) = self
            .interests
            .get(&client)
            .map(|i| (i.known.clone(), i.last_sent.clone()))
            .unwrap_or_default();
        self.interests.insert(
            client,
            Interest {
                pos,
                radius,
                known,
                budget_bytes,
                filter,
                last_sent,
            },
        );
    }

    pub fn unsubscribe(&mut self, client: ClientId) {
        self.interests.remove(&client);
    }

    /// Delta since the client's last call: new entities in range, moved/changed ones, and those
    /// that left range or despawned. Transparent to cell ownership and handovers.
    /// What `client` has not seen yet, within its budget. Entities in the radius that pass the
    /// filter are ranked by `relevance × (1 + age)` — relevance falls with distance, age is the
    /// ticks since the client last heard about the entity — and taken in that order until the
    /// estimated bytes exceed the budget (at least one always goes); the rest keep their old
    /// state and come back next tick with a higher priority. Despawns are always delivered.
    /// Positions are quantized to `pos_quantum`, so jitter below it is not an update.
    pub fn delta_for(&mut self, client: ClientId) -> Option<Delta> {
        let tick = self.tick;
        let quantum = self.cfg.pos_quantum;
        let default_budget = self.cfg.client_budget_bytes;
        let interest = self.interests.get_mut(&client)?;
        let budget = interest.budget_bytes.unwrap_or(default_budget);
        let mut seen = HashSet::new();
        struct Cand {
            prio: f64,
            bytes: usize,
            id: EntityId,
            pos: Pos,
            versions: BTreeMap<String, u64>,
            spawn: Option<Entity>,
            update: Option<(EntityId, Pos, BTreeMap<String, Component>)>,
        }
        let mut cands: Vec<Cand> = Vec::new();
        for e in self.entities.values() {
            let dist = e.pos.dist(&interest.pos);
            if dist > interest.radius {
                continue;
            }
            if !interest
                .filter
                .iter()
                .all(|(k, v)| e.components.get(k).is_some_and(|c| &c.value == v))
            {
                continue;
            }
            seen.insert(e.id);
            let qpos = quantize(e.pos, quantum);
            let versions: BTreeMap<String, u64> = e
                .components
                .iter()
                .map(|(k, c)| (k.clone(), c.version))
                .collect();
            let (spawn, update, bytes) = match interest.known.get(&e.id) {
                None => {
                    let mut s = e.clone();
                    s.pos = qpos;
                    let bytes = estimate_components(s.components.iter()) + 64;
                    (Some(s), None, bytes)
                }
                Some((pos, known_versions)) => {
                    let changed: BTreeMap<String, Component> = e
                        .components
                        .iter()
                        .filter(|(k, c)| known_versions.get(*k) != Some(&c.version))
                        .map(|(k, c)| (k.clone(), c.clone()))
                        .collect();
                    if *pos == qpos && changed.is_empty() {
                        continue;
                    }
                    let bytes = estimate_components(changed.iter()) + 56;
                    (None, Some((e.id, qpos, changed)), bytes)
                }
            };
            let age = tick.saturating_sub(interest.last_sent.get(&e.id).copied().unwrap_or(0));
            let relevance = 1.0 / (1.0 + dist / interest.radius.max(f64::EPSILON));
            cands.push(Cand {
                prio: relevance * (1.0 + age as f64),
                bytes,
                id: e.id,
                pos: qpos,
                versions,
                spawn,
                update,
            });
        }
        cands.sort_by(|a, b| {
            b.prio
                .partial_cmp(&a.prio)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
        let mut d = Delta::default();
        let mut used = 0usize;
        for c in cands {
            if used > 0 && used + c.bytes > budget {
                continue;
            }
            used += c.bytes;
            if let Some(s) = c.spawn {
                d.spawned.push(s);
            }
            if let Some(u) = c.update {
                d.updated.push(u);
            }
            interest.known.insert(c.id, (c.pos, c.versions));
            interest.last_sent.insert(c.id, tick);
        }
        let gone: Vec<EntityId> = interest
            .known
            .keys()
            .filter(|id| !seen.contains(id))
            .copied()
            .collect();
        for id in &gone {
            interest.known.remove(id);
            interest.last_sent.remove(id);
        }
        d.despawned = gone;
        d.spawned.sort_by_key(|e| e.id);
        d.updated.sort_by_key(|u| u.0);
        d.despawned.sort();
        Some(d)
    }

    /// Advance the world clock; expire handovers.
    pub fn advance(&mut self) -> Vec<HandoverOutcome> {
        self.tick += 1;
        let tick = self.tick;
        self.pins.retain(|_, (_, until)| *until > tick);
        self.expire_handovers()
    }

    /// Persistent entities only — what survives a restart.
    pub fn config(&self) -> &WorldConfig {
        &self.cfg
    }

    /// Every entity this shard holds, in transit or not (diagnostics).
    pub fn entities_all(&self) -> Vec<Entity> {
        let mut v: Vec<Entity> = self.entities.values().cloned().collect();
        v.sort_by_key(|e| e.id);
        v
    }

    /// Live leases per cell.
    pub fn leases(&self) -> Vec<(CellId, SimId)> {
        let mut v: Vec<(CellId, SimId)> = self
            .leases
            .iter()
            .filter(|(_, l)| l.expires_tick >= self.tick)
            .map(|(c, l)| (*c, l.sim))
            .collect();
        v.sort_by_key(|(c, _)| (c.x, c.y));
        v
    }

    pub fn snapshot_persistent(&self) -> Vec<Entity> {
        let mut v: Vec<Entity> = self
            .entities
            .values()
            .filter(|e| e.persistent)
            .cloned()
            .collect();
        v.sort_by_key(|e| e.id);
        v
    }

    pub fn restore(&mut self, entities: Vec<Entity>) {
        for mut e in entities {
            if let Some(old) = self.entities.get(&e.id) {
                if let Some(set) = self.by_cell.get_mut(&old.cell) {
                    set.remove(&e.id);
                }
            }
            e.transit = None;
            e.cell = match self.pins.get(&e.id) {
                Some((c, _)) => *c,
                None => self.cell_of(e.pos),
            };
            self.by_cell.entry(e.cell).or_default().insert(e.id);
            let (id, pos) = (e.id, e.pos);
            self.entities.insert(e.id, e);
            self.record_pos(id, pos);
        }
    }
}

fn quantize(p: Pos, q: f64) -> Pos {
    if q <= 0.0 {
        return p;
    }
    Pos {
        x: (p.x / q).round() * q,
        y: (p.y / q).round() * q,
    }
}

/// Rough wire size of components in a JSON delta (names, values, version numbers).
fn estimate_components<'a>(it: impl Iterator<Item = (&'a String, &'a Component)>) -> usize {
    it.map(|(k, c)| k.len() + c.value.to_string().len() + 24)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comps(v: serde_json::Value) -> BTreeMap<String, serde_json::Value> {
        v.as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    #[test]
    fn cells_ghosts_and_authority() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            ghost_margin: 10.0,
            hysteresis: 5.0,
            ..Default::default()
        });
        let s1 = Uuid::now_v7();
        let s2 = Uuid::now_v7();
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        w.grant_lease(CellId { x: 1, y: 0 }, s2);
        let a = w.spawn(
            Pos { x: 95.0, y: 50.0 },
            comps(serde_json::json!({"hp": 100})),
            false,
        );
        let b = w.spawn(
            Pos { x: 50.0, y: 50.0 },
            comps(serde_json::json!({"hp": 100})),
            false,
        );
        assert_eq!(w.get(a).unwrap().cell, CellId { x: 0, y: 0 });
        // a is within 10 of the border → ghost for cell (1,0); b is not
        let ghosts: Vec<EntityId> = w
            .ghosts_for(CellId { x: 1, y: 0 })
            .iter()
            .map(|e| e.id)
            .collect();
        assert_eq!(ghosts, vec![a]);
        let v = w.view(CellId { x: 0, y: 0 });
        assert_eq!(v.owned.len(), 2);
        assert!(v.ghosts.is_empty());
        // only s1 may write to a
        assert!(matches!(
            w.apply(s2, a, None, comps(serde_json::json!({"hp": 1}))),
            Err(MeshError::NotAuthoritative(_))
        ));
        w.apply(s1, a, None, comps(serde_json::json!({"hp": 90})))
            .unwrap();
        assert_eq!(w.get(a).unwrap().components["hp"].version, 2);
        let _ = b;
    }

    #[test]
    fn handover_with_buffered_inputs_and_timeout() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            ghost_margin: 10.0,
            hysteresis: 5.0,
            handover_timeout_ticks: 3,
            ..Default::default()
        });
        let s1 = Uuid::now_v7();
        let s2 = Uuid::now_v7();
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        w.grant_lease(CellId { x: 1, y: 0 }, s2);
        let a = w.spawn(Pos { x: 90.0, y: 50.0 }, BTreeMap::new(), false);
        // crossing by less than hysteresis: nothing happens
        w.apply(s1, a, Some(Pos { x: 103.0, y: 50.0 }), BTreeMap::new())
            .unwrap();
        assert!(w.detect_handovers().is_empty());
        w.apply(s1, a, Some(Pos { x: 108.0, y: 50.0 }), BTreeMap::new())
            .unwrap();
        let out = w.detect_handovers();
        assert_eq!(
            out,
            vec![HandoverOutcome::Started {
                entity: a,
                from: s1,
                to: s2
            }]
        );
        // input during transit is buffered, not routed
        assert_eq!(
            w.route_input(a, serde_json::json!({"move": 1})).unwrap(),
            None
        );
        // s1 cannot ack; s2 can, gets the buffered input, authority flips
        assert!(w.handover_ack(s1, a).is_err());
        let (outcome, inputs) = w.handover_ack(s2, a).unwrap();
        assert_eq!(outcome, HandoverOutcome::Completed { entity: a, to: s2 });
        assert_eq!(inputs.len(), 1);
        assert_eq!(w.get(a).unwrap().cell, CellId { x: 1, y: 0 });
        assert_eq!(w.route_input(a, serde_json::json!({})).unwrap(), Some(s2));
        assert!(w.apply(s1, a, None, BTreeMap::new()).is_err());
        // timeout path: move back towards s1, never ack → cancelled after 3 ticks, authority stays with s2
        w.apply(s2, a, Some(Pos { x: 80.0, y: 50.0 }), BTreeMap::new())
            .unwrap();
        assert!(matches!(
            w.detect_handovers()[0],
            HandoverOutcome::Started { .. }
        ));
        for _ in 0..3 {
            assert!(w.advance().is_empty());
        }
        assert!(matches!(w.advance()[0], HandoverOutcome::Cancelled { .. }));
        assert_eq!(w.owner(w.get(a).unwrap().cell), Some(s2));
    }

    #[test]
    fn same_sim_moves_without_handover_and_lease_expiry_orphans_cells() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            hysteresis: 0.0,
            lease_ttl_ticks: 2,
            ..Default::default()
        });
        let s1 = Uuid::now_v7();
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        w.grant_lease(CellId { x: 1, y: 0 }, s1);
        let a = w.spawn(Pos { x: 50.0, y: 50.0 }, BTreeMap::new(), true);
        w.apply(s1, a, Some(Pos { x: 150.0, y: 50.0 }), BTreeMap::new())
            .unwrap();
        assert!(w.detect_handovers().is_empty());
        assert_eq!(w.get(a).unwrap().cell, CellId { x: 1, y: 0 });
        w.advance();
        w.advance();
        assert_eq!(w.owner(CellId { x: 1, y: 0 }), None);
        assert_eq!(w.orphaned_cells(), vec![CellId { x: 1, y: 0 }]);
        let snap = w.snapshot_persistent();
        assert_eq!(snap.len(), 1);
        let mut w2 = World::new(WorldConfig {
            cell_size: 100.0,
            ..Default::default()
        });
        w2.restore(snap);
        assert_eq!(w2.get(a).unwrap().cell, CellId { x: 1, y: 0 });
    }

    #[test]
    fn client_deltas_follow_interest_not_ownership() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            ..Default::default()
        });
        let s1 = Uuid::now_v7();
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        let c = Uuid::now_v7();
        w.subscribe(c, Pos { x: 0.0, y: 0.0 }, 60.0);
        let near = w.spawn(
            Pos { x: 10.0, y: 10.0 },
            comps(serde_json::json!({"hp": 5})),
            false,
        );
        let far = w.spawn(Pos { x: 90.0, y: 90.0 }, BTreeMap::new(), false);
        let d = w.delta_for(c).unwrap();
        assert_eq!(
            d.spawned.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![near]
        );
        assert_eq!(w.delta_for(c).unwrap(), Delta::default());
        w.apply(s1, near, None, comps(serde_json::json!({"hp": 4})))
            .unwrap();
        let d = w.delta_for(c).unwrap();
        assert_eq!(d.updated.len(), 1);
        assert_eq!(d.updated[0].2["hp"].version, 2);
        w.apply(s1, near, Some(Pos { x: 90.0, y: 0.0 }), BTreeMap::new())
            .unwrap();
        let d = w.delta_for(c).unwrap();
        assert_eq!(d.despawned, vec![near]);
        let _ = far;
    }

    #[test]
    fn delta_budget_sends_nearest_first_and_catches_up() {
        let mut w = World::new(WorldConfig {
            cell_size: 1000.0,
            ..Default::default()
        });
        let s1 = Uuid::now_v7();
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        let c = Uuid::now_v7();
        // budget of one byte: exactly one entity per tick, nearest first
        w.subscribe_with(c, Pos { x: 0.0, y: 0.0 }, 500.0, Some(1), BTreeMap::new());
        let far = w.spawn(Pos { x: 300.0, y: 0.0 }, BTreeMap::new(), false);
        let mid = w.spawn(Pos { x: 100.0, y: 0.0 }, BTreeMap::new(), false);
        let near = w.spawn(Pos { x: 10.0, y: 0.0 }, BTreeMap::new(), false);
        let ids = |d: &Delta| d.spawned.iter().map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids(&w.delta_for(c).unwrap()), vec![near]);
        w.advance();
        assert_eq!(ids(&w.delta_for(c).unwrap()), vec![mid]);
        w.advance();
        assert_eq!(ids(&w.delta_for(c).unwrap()), vec![far]);
        w.advance();
        assert_eq!(w.delta_for(c).unwrap(), Delta::default());
        // a stale far entity outranks a fresh near one once it has waited long enough
        w.apply(s1, far, Some(Pos { x: 301.0, y: 0.0 }), BTreeMap::new())
            .unwrap();
        w.apply(s1, near, Some(Pos { x: 11.0, y: 0.0 }), BTreeMap::new())
            .unwrap();
        for _ in 0..10 {
            w.advance();
        }
        let d = w.delta_for(c).unwrap();
        assert_eq!(d.updated.len(), 1);
        // both waited the same: the near one wins; next tick the far one (now older) follows
        assert_eq!(d.updated[0].0, near);
        w.advance();
        let d = w.delta_for(c).unwrap();
        assert_eq!(d.updated.iter().map(|u| u.0).collect::<Vec<_>>(), vec![far]);
    }

    #[test]
    fn delta_filter_and_quantization() {
        let mut w = World::new(WorldConfig {
            cell_size: 1000.0,
            pos_quantum: 0.1,
            ..Default::default()
        });
        let s1 = Uuid::now_v7();
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        let c = Uuid::now_v7();
        let mut filter = BTreeMap::new();
        filter.insert("team".to_string(), serde_json::json!("red"));
        w.subscribe_with(c, Pos { x: 0.0, y: 0.0 }, 500.0, None, filter);
        let red = w.spawn(
            Pos { x: 10.0, y: 0.0 },
            comps(serde_json::json!({"team": "red"})),
            false,
        );
        let _blue = w.spawn(
            Pos { x: 10.0, y: 5.0 },
            comps(serde_json::json!({"team": "blue"})),
            false,
        );
        let d = w.delta_for(c).unwrap();
        assert_eq!(
            d.spawned.iter().map(|e| e.id).collect::<Vec<_>>(),
            vec![red]
        );
        // a move below the quantum is not an update; above it is, and arrives quantized
        w.apply(s1, red, Some(Pos { x: 10.04, y: 0.0 }), BTreeMap::new())
            .unwrap();
        assert_eq!(w.delta_for(c).unwrap(), Delta::default());
        w.apply(s1, red, Some(Pos { x: 10.26, y: 0.0 }), BTreeMap::new())
            .unwrap();
        let d = w.delta_for(c).unwrap();
        assert_eq!(d.updated.len(), 1);
        assert!(
            (d.updated[0].1.x - 10.3).abs() < 1e-9,
            "{:?}",
            d.updated[0].1
        );
    }
}
