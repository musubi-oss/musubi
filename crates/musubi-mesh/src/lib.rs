//! Server meshing core (v0): the Replication Layer's data model and rules, independent of
//! transport and engine. One continuous world is split into grid cells; a simulator holds a lease
//! on each cell and is authoritative for the entities inside it; entities near a border are
//! replicated to the neighbouring simulator as read-only ghosts; crossing the border (plus
//! hysteresis) triggers a handover that completes within a few ticks or is rolled back. Clients
//! never talk to simulators: they subscribe to an interest radius and receive versioned deltas.
//!
//! Space is 2D here (x, y); 3D/octree and non-spatial partition keys plug into `CellId` later.

pub mod controller;

use std::collections::{BTreeMap, HashMap, HashSet};

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
}

impl Default for WorldConfig {
    fn default() -> Self {
        Self {
            cell_size: 256.0,
            ghost_margin: 48.0,
            hysteresis: 24.0,
            lease_ttl_ticks: 60,
            handover_timeout_ticks: 3,
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
}

pub struct World {
    pub cfg: WorldConfig,
    pub tick: u64,
    entities: HashMap<EntityId, Entity>,
    by_cell: HashMap<CellId, HashSet<EntityId>>,
    leases: HashMap<CellId, Lease>,
    interests: HashMap<ClientId, Interest>,
    /// Inputs that arrived while the target entity was in transit; delivered to the new owner.
    buffered_inputs: HashMap<EntityId, Vec<serde_json::Value>>,
}

impl World {
    pub fn new(cfg: WorldConfig) -> Self {
        Self {
            cfg,
            tick: 0,
            entities: HashMap::new(),
            by_cell: HashMap::new(),
            leases: HashMap::new(),
            interests: HashMap::new(),
            buffered_inputs: HashMap::new(),
        }
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
    pub fn orphaned_cells(&self) -> Vec<CellId> {
        let mut v: Vec<CellId> = self
            .by_cell
            .keys()
            .filter(|c| self.owner(**c).is_none() && !self.by_cell[c].is_empty())
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
        };
        self.by_cell.entry(cell).or_default().insert(id);
        self.entities.insert(id, e);
        id
    }

    pub fn despawn(&mut self, id: EntityId) -> bool {
        let Some(e) = self.entities.remove(&id) else {
            return false;
        };
        if let Some(set) = self.by_cell.get_mut(&e.cell) {
            set.remove(&id);
        }
        self.buffered_inputs.remove(&id);
        true
    }

    pub fn get(&self, id: EntityId) -> Option<&Entity> {
        self.entities.get(&id)
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
        let e = self.entities.get_mut(&id).ok_or(MeshError::NoEntity)?;
        if let Some(p) = pos {
            e.pos = p;
        }
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
            let target = self.cell_of(pos);
            if target == cell || -self.border_distance(cell, pos) < self.cfg.hysteresis {
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
        self.entities.get_mut(&id).unwrap().transit = None;
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
        let known = self
            .interests
            .get(&client)
            .map(|i| i.known.clone())
            .unwrap_or_default();
        self.interests
            .insert(client, Interest { pos, radius, known });
    }

    pub fn unsubscribe(&mut self, client: ClientId) {
        self.interests.remove(&client);
    }

    /// Delta since the client's last call: new entities in range, moved/changed ones, and those
    /// that left range or despawned. Transparent to cell ownership and handovers.
    pub fn delta_for(&mut self, client: ClientId) -> Option<Delta> {
        let interest = self.interests.get_mut(&client)?;
        let mut d = Delta::default();
        let mut seen = HashSet::new();
        for e in self.entities.values() {
            if e.pos.dist(&interest.pos) > interest.radius {
                continue;
            }
            seen.insert(e.id);
            let versions: BTreeMap<String, u64> = e
                .components
                .iter()
                .map(|(k, c)| (k.clone(), c.version))
                .collect();
            match interest.known.get(&e.id) {
                None => d.spawned.push(e.clone()),
                Some((pos, known_versions)) => {
                    let changed: BTreeMap<String, Component> = e
                        .components
                        .iter()
                        .filter(|(k, c)| known_versions.get(*k) != Some(&c.version))
                        .map(|(k, c)| (k.clone(), c.clone()))
                        .collect();
                    if *pos != e.pos || !changed.is_empty() {
                        d.updated.push((e.id, e.pos, changed));
                    }
                }
            }
            interest.known.insert(e.id, (e.pos, versions));
        }
        let gone: Vec<EntityId> = interest
            .known
            .keys()
            .filter(|id| !seen.contains(id))
            .copied()
            .collect();
        for id in &gone {
            interest.known.remove(id);
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
        self.expire_handovers()
    }

    /// Persistent entities only — what survives a restart.
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
            e.transit = None;
            e.cell = self.cell_of(e.pos);
            self.by_cell.entry(e.cell).or_default().insert(e.id);
            self.entities.insert(e.id, e);
        }
    }
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
}
