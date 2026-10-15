//! Shard Controller (v0, single replication layer): knows which simulators exist and how much they
//! can carry, leases cells to them on demand (preferring a simulator that already owns a neighbour
//! so ghost traffic stays local), re-leases orphaned cells when a simulator's lease lapses and
//! migrates cells off overloaded simulators. Cost model is deliberately simple: entities per cell.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CellId, SimId, World};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimInfo {
    pub id: SimId,
    /// Max cells this simulator accepts.
    pub capacity: u32,
    pub last_seen_tick: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Assignment {
    pub cell: CellId,
    pub from: Option<SimId>,
    pub to: SimId,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ControllerConfig {
    /// A simulator carrying more entity-cost than this sheds cells (if someone else has room).
    pub split_cost: f64,
    /// Simulators unseen for this many ticks are not assigned new cells.
    pub sim_timeout_ticks: u64,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            split_cost: 500.0,
            sim_timeout_ticks: 90,
        }
    }
}

#[derive(Debug, Default)]
pub struct Controller {
    pub cfg: ControllerConfig,
    sims: BTreeMap<SimId, SimInfo>,
}

impl Controller {
    pub fn new(cfg: ControllerConfig) -> Self {
        Self {
            cfg,
            sims: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, id: SimId, capacity: u32, tick: u64) {
        let e = self.sims.entry(id).or_insert(SimInfo {
            id,
            capacity,
            last_seen_tick: tick,
        });
        e.capacity = capacity;
        e.last_seen_tick = tick;
    }

    pub fn seen(&mut self, id: SimId, tick: u64) {
        if let Some(s) = self.sims.get_mut(&id) {
            s.last_seen_tick = tick;
        }
    }

    pub fn unregister(&mut self, id: SimId) -> bool {
        self.sims.remove(&id).is_some()
    }

    pub fn sims(&self) -> Vec<SimInfo> {
        self.sims.values().cloned().collect()
    }

    pub fn is_alive(&self, id: SimId, tick: u64) -> bool {
        self.sims
            .get(&id)
            .is_some_and(|s| tick.saturating_sub(s.last_seen_tick) <= self.cfg.sim_timeout_ticks)
    }

    pub fn load(&self, world: &World, sim: SimId) -> f64 {
        world
            .cells_of(sim)
            .iter()
            .map(|c| world.entities_in(*c) as f64)
            .sum()
    }

    fn has_room(&self, world: &World, sim: SimId) -> bool {
        self.sims
            .get(&sim)
            .is_some_and(|s| (world.cells_of(sim).len() as u32) < s.capacity)
    }

    /// Best simulator for `cell`: alive, with room; owning a neighbour wins, then lowest load.
    pub fn pick(&self, world: &World, cell: CellId, prefer: Option<SimId>) -> Option<SimId> {
        let tick = world.tick;
        if let Some(p) = prefer {
            if self.is_alive(p, tick) && self.has_room(world, p) {
                return Some(p);
            }
        }
        let mut best: Option<(bool, f64, SimId)> = None;
        for s in self.sims.keys().copied() {
            if !self.is_alive(s, tick) || !self.has_room(world, s) {
                continue;
            }
            let adjacent = World::neighbours(cell).any(|n| world.owner(n) == Some(s));
            let load = self.load(world, s);
            let key = (adjacent, load, s);
            best = match best {
                None => Some(key),
                Some(b) if (key.0 && !b.0) || (key.0 == b.0 && key.1 < b.1) => Some(key),
                b => b,
            };
        }
        best.map(|b| b.2)
    }

    /// Lease `cell` if nobody holds it. Returns the assignment made, if any.
    pub fn ensure_leased(
        &self,
        world: &mut World,
        cell: CellId,
        prefer: Option<SimId>,
        reason: &str,
    ) -> Option<Assignment> {
        if world.owner(cell).is_some() {
            return None;
        }
        let to = self.pick(world, cell, prefer)?;
        let from = world.lease_holder(cell).filter(|f| *f != to);
        world.grant_lease(cell, to);
        Some(Assignment {
            cell,
            from,
            to,
            reason: reason.into(),
        })
    }

    /// Periodic pass: re-lease orphaned cells, shed load from overloaded simulators.
    pub fn reconcile(&self, world: &mut World) -> Vec<Assignment> {
        let mut out = Vec::new();
        for c in world.orphaned_cells() {
            if let Some(a) = self.ensure_leased(world, c, None, "orphaned") {
                out.push(a);
            }
        }
        // cells clients are watching must have a simulator too
        for c in world.interest_cells() {
            if let Some(a) = self.ensure_leased(world, c, None, "interest") {
                out.push(a);
            }
        }
        // empty cells of dead simulators simply become free
        world.drop_lapsed_leases();
        let tick = world.tick;
        let sims: Vec<SimId> = self.sims.keys().copied().collect();
        for s in sims {
            if !self.is_alive(s, tick) {
                continue;
            }
            let cells = world.cells_of(s);
            if cells.len() < 2 || self.load(world, s) <= self.cfg.split_cost {
                continue;
            }
            // shed the cell with the fewest neighbours owned by `s` (least ghost coupling)
            let victim = cells
                .iter()
                .copied()
                .min_by_key(|c| {
                    World::neighbours(*c)
                        .filter(|n| world.owner(*n) == Some(s))
                        .count()
                })
                .unwrap();
            let Some(to) = self
                .sims
                .keys()
                .copied()
                .filter(|o| *o != s && self.is_alive(*o, tick) && self.has_room(world, *o))
                .filter(|o| {
                    self.load(world, *o) + world.entities_in(victim) as f64 <= self.cfg.split_cost
                })
                .min_by(|a, b| self.load(world, *a).total_cmp(&self.load(world, *b)))
            else {
                continue;
            };
            world.revoke_lease(victim);
            world.grant_lease(victim, to);
            out.push(Assignment {
                cell: victim,
                from: Some(s),
                to,
                reason: "overloaded".into(),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pos, WorldConfig};
    use std::collections::BTreeMap;
    use uuid::Uuid;

    fn world() -> World {
        World::new(WorldConfig {
            cell_size: 100.0,
            lease_ttl_ticks: 5,
            ..Default::default()
        })
    }

    #[test]
    fn prefers_adjacent_then_least_loaded_and_respects_capacity() {
        let mut w = world();
        let mut c = Controller::new(ControllerConfig::default());
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        c.register(s1, 2, 0);
        c.register(s2, 2, 0);
        let a = c
            .ensure_leased(&mut w, CellId { x: 0, y: 0 }, Some(s1), "spawn")
            .unwrap();
        assert_eq!(a.to, s1);
        // neighbour of s1's cell → s1 again (adjacency beats s2's lower load)
        assert_eq!(c.pick(&w, CellId { x: 1, y: 0 }, None), Some(s1));
        c.ensure_leased(&mut w, CellId { x: 1, y: 0 }, None, "x");
        // s1 is full now → far cell goes to s2
        assert_eq!(c.pick(&w, CellId { x: 2, y: 0 }, None), Some(s2));
        c.ensure_leased(&mut w, CellId { x: 2, y: 0 }, None, "x");
        c.ensure_leased(&mut w, CellId { x: 3, y: 0 }, None, "x");
        assert_eq!(
            c.pick(&w, CellId { x: 9, y: 9 }, None),
            None,
            "everyone is full"
        );
    }

    #[test]
    fn orphaned_cells_are_released_to_a_live_sim_and_overload_sheds_cells() {
        let mut w = world();
        let mut c = Controller::new(ControllerConfig {
            split_cost: 2.0,
            sim_timeout_ticks: 5,
        });
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        c.register(s1, 4, 0);
        c.register(s2, 4, 0);
        w.grant_lease(CellId { x: 0, y: 0 }, s1);
        w.grant_lease(CellId { x: 1, y: 0 }, s1);
        for x in [10.0, 20.0, 110.0, 120.0] {
            w.spawn(Pos { x, y: 10.0 }, BTreeMap::new(), false);
        }
        // s1 carries 4 > split_cost 2 → one cell migrates to s2
        let out = c.reconcile(&mut w);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].from, Some(s1));
        assert_eq!(out[0].to, s2);
        assert_eq!(w.cells_of(s1).len(), 1);
        assert_eq!(w.cells_of(s2).len(), 1);
        // s2 dies: its lease lapses, nobody renews → cell is orphaned → back to s1 (alive)
        for _ in 0..6 {
            w.advance();
            w.renew_leases(s1);
            c.seen(s1, w.tick);
        }
        let orphan = w.cells_of(s2);
        assert!(orphan.is_empty());
        assert_eq!(w.orphaned_cells().len(), 1);
        let out = c.reconcile(&mut w);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].reason, "orphaned");
        assert_eq!(out[0].to, s1);
        assert!(w.orphaned_cells().is_empty());
    }
}
