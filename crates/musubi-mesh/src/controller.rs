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
    /// Last world tick the simulator reported having fully processed (None = never reported).
    #[serde(default)]
    pub acked_tick: Option<u64>,
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
    /// A simulator whose cost (see `Controller::cost`) exceeds this sheds cells (if someone else
    /// has room for them under the same bar).
    pub split_cost: f64,
    /// Simulators unseen for this many ticks are not assigned new cells.
    pub sim_timeout_ticks: u64,
    /// Cell cost weights (plan 4.x `cost = a·entities + b·players + c·tick_lag + d·boundary_traffic`):
    /// per entity in the cell,
    pub cost_entity: f64,
    /// per client whose interest is centred in the cell,
    pub cost_player: f64,
    /// per tick the owning simulator lags the world (charged to the simulator, not a cell),
    pub cost_lag: f64,
    /// per entity in handover out of the cell (smoothed over recent ticks).
    pub cost_traffic: f64,
    /// A simulator whose cost stays under this for `merge_after_rounds` control rounds hands its
    /// cells to the owners of neighbouring cells (co-locate: fewer ghosts and handovers cross
    /// simulators) and is left free to go away.
    pub merge_cost: f64,
    /// Consecutive control rounds under `merge_cost` before merging (hysteresis against flapping).
    pub merge_after_rounds: u32,
    /// Quadtree (plan 4.3): a leaf whose own cost stays above this for `split_cell_after_rounds`
    /// rounds splits into four (needs `WorldConfig::max_depth` > 0); 0 disables.
    pub split_cell_cost: f64,
    pub split_cell_after_rounds: u32,
    /// Four sibling leaves whose summed cost stays under this for `merge_cell_after_rounds`
    /// rounds merge back into their parent (hysteresis against flapping).
    pub merge_cell_cost: f64,
    pub merge_cell_after_rounds: u32,
}

impl Default for ControllerConfig {
    fn default() -> Self {
        Self {
            split_cost: 500.0,
            sim_timeout_ticks: 90,
            cost_entity: 1.0,
            cost_player: 5.0,
            cost_lag: 10.0,
            cost_traffic: 20.0,
            merge_cost: 50.0,
            merge_after_rounds: 30,
            split_cell_cost: 300.0,
            split_cell_after_rounds: 3,
            merge_cell_cost: 60.0,
            merge_cell_after_rounds: 30,
        }
    }
}

/// A merge destination must stay this far under `split_cost`, or the next round splits it back.
const MERGE_HEADROOM: f64 = 0.75;

#[derive(Debug, Default)]
pub struct Controller {
    pub cfg: ControllerConfig,
    sims: BTreeMap<SimId, SimInfo>,
    /// Leased cells no client watches → tick the idleness started (hibernation clock).
    idle_since: BTreeMap<CellId, u64>,
    /// Smoothed count of entities in handover out of each leased cell (boundary traffic).
    traffic: BTreeMap<CellId, f64>,
    /// Control rounds each simulator has spent under `merge_cost` in a row.
    quiet_rounds: BTreeMap<SimId, u32>,
    /// Control rounds each leaf has spent above `split_cell_cost` in a row.
    hot_rounds: BTreeMap<CellId, u32>,
    /// Control rounds each split parent's children have spent under `merge_cell_cost` in a row.
    cold_rounds: BTreeMap<CellId, u32>,
}

impl Controller {
    pub fn new(cfg: ControllerConfig) -> Self {
        Self {
            cfg,
            sims: BTreeMap::new(),
            idle_since: BTreeMap::new(),
            traffic: BTreeMap::new(),
            quiet_rounds: BTreeMap::new(),
            hot_rounds: BTreeMap::new(),
            cold_rounds: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, id: SimId, capacity: u32, tick: u64) {
        let e = self.sims.entry(id).or_insert(SimInfo {
            id,
            capacity,
            last_seen_tick: tick,
            acked_tick: None,
        });
        e.capacity = capacity;
        e.last_seen_tick = tick;
    }

    pub fn seen(&mut self, id: SimId, tick: u64) {
        if let Some(s) = self.sims.get_mut(&id) {
            s.last_seen_tick = tick;
        }
    }

    /// The simulator finished processing `tick` (a tick never goes backwards).
    pub fn progress(&mut self, id: SimId, tick: u64) {
        if let Some(s) = self.sims.get_mut(&id) {
            s.acked_tick = Some(s.acked_tick.map_or(tick, |a| a.max(tick)));
        }
    }

    /// Time dilation: a cell whose owner lags the world by more than `tidi_after_ticks` runs
    /// at `tidi_after_ticks / lag` of real time (not below `tidi_min`); it returns to 1.0 as the
    /// simulator catches up. Returns the cells whose scale changed.
    pub fn dilate(&self, world: &mut World) -> Vec<(CellId, f64)> {
        let after = world.cfg.tidi_after_ticks.max(1);
        let min = world.cfg.tidi_min.clamp(0.01, 1.0);
        let tick = world.tick;
        let mut changed = Vec::new();
        for (cell, sim) in world.leases() {
            let lag = self
                .sims
                .get(&sim)
                .and_then(|s| s.acked_tick)
                .map_or(0, |a| tick.saturating_sub(a));
            let scale = if lag > after {
                (after as f64 / lag as f64).clamp(min, 1.0)
            } else {
                1.0
            };
            if world.set_time_scale(cell, scale) {
                changed.push((cell, scale));
            }
        }
        changed
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

    /// Ticks the simulator's last fully processed tick is behind the world (0 if never reported).
    pub fn lag(&self, sim: SimId, tick: u64) -> u64 {
        self.sims
            .get(&sim)
            .and_then(|s| s.acked_tick)
            .map_or(0, |a| tick.saturating_sub(a))
    }

    /// Per-tick sample of boundary traffic: entities currently in handover out of each cell.
    pub fn observe(&mut self, world: &World) {
        let leased = world.leases();
        self.traffic
            .retain(|c, _| leased.iter().any(|(l, _)| l == c));
        for (cell, _) in leased {
            let moving = world
                .entities_of(cell)
                .iter()
                .filter(|e| e.transit.is_some())
                .count() as f64;
            let t = self.traffic.entry(cell).or_insert(0.0);
            *t = *t * 0.9 + moving * 0.1;
        }
    }

    /// `cost_entity·entities + cost_player·players + cost_traffic·traffic` of one cell.
    pub fn cell_cost(&self, world: &World, cell: CellId) -> f64 {
        self.cfg.cost_entity * world.entities_in(cell) as f64
            + self.cfg.cost_player * world.players_in(cell) as f64
            + self.cfg.cost_traffic * self.traffic.get(&cell).copied().unwrap_or(0.0)
    }

    /// A simulator's cost: its cells plus `cost_lag` per tick it lags the world.
    pub fn cost(&self, world: &World, sim: SimId) -> f64 {
        world
            .cells_of(sim)
            .iter()
            .map(|c| self.cell_cost(world, *c))
            .sum::<f64>()
            + self.cfg.cost_lag * self.lag(sim, world.tick) as f64
    }

    /// Entities across a simulator's cells (the pre-cost-model load figure).
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
            let adjacent = world
                .neighbours(cell)
                .into_iter()
                .any(|n| world.owner(n) == Some(s));
            let load = self.cost(world, s);
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

    /// Plan 4.9 scale-to-zero: a cell no client has watched for `hibernate_after_ticks` — leased
    /// or holding entities under a lapsed lease — with nothing in transit or written to recently
    /// goes to sleep (see `World::hibernate_cell`); interest returning re-leases it through
    /// `reconcile` (`interest`). Returns `(cell, owner at the time, ephemeral entities despawned)`.
    pub fn hibernate(&mut self, world: &mut World) -> Vec<(CellId, Option<SimId>, usize)> {
        let after = world.cfg.hibernate_after_ticks;
        if after == 0 {
            self.idle_since.clear();
            return Vec::new();
        }
        let tick = world.tick;
        let watched: std::collections::BTreeSet<CellId> =
            world.interest_cells().into_iter().collect();
        let mut candidates: Vec<CellId> = world.leases().into_iter().map(|(c, _)| c).collect();
        candidates.extend(world.orphaned_cells());
        candidates.sort();
        candidates.dedup();
        candidates.retain(|c| !watched.contains(c));
        self.idle_since.retain(|c, _| candidates.contains(c));
        let mut out = Vec::new();
        for cell in candidates {
            let since = *self.idle_since.entry(cell).or_insert(tick);
            // something still being simulated (recent writes) or in flight keeps the cell awake
            let active = world
                .entities_of(cell)
                .iter()
                .any(|e| e.transit.is_some() || tick.saturating_sub(e.touched_tick) < after);
            if tick.saturating_sub(since) < after || active {
                continue;
            }
            // after a lapsed lease the last simulator still hears about it
            let last = world.last_owner(cell);
            let (owner, ephemeral) = world.hibernate_cell(cell);
            self.idle_since.remove(&cell);
            out.push((cell, owner.or(last), ephemeral));
        }
        out
    }

    /// Periodic pass: re-lease orphaned cells, shed load from overloaded simulators, merge the
    /// cells of quiet ones onto their neighbours.
    /// Quadtree control (plan 4.3): hot leaves split (the owner keeps all four children — no
    /// handover, the simulator just reports per child from now on), cold sibling groups merge
    /// back once their owners agree; siblings with different owners are first consolidated onto
    /// the one holding most of them, the merge follows a round later.
    fn split_and_merge_cells(&mut self, world: &mut World, out: &mut Vec<Assignment>) {
        if world.cfg.max_depth == 0 {
            return;
        }
        let tick = world.tick;
        if self.cfg.split_cell_cost > 0.0 {
            let mut hot = std::mem::take(&mut self.hot_rounds);
            let leased = world.leased_cells();
            hot.retain(|c, _| leased.iter().any(|(l, _)| l == c));
            for (c, sim) in leased {
                if c.level >= world.cfg.max_depth
                    || self.cell_cost(world, c) <= self.cfg.split_cell_cost
                {
                    hot.remove(&c);
                    continue;
                }
                let rounds = hot.entry(c).or_insert(0);
                *rounds += 1;
                if *rounds < self.cfg.split_cell_after_rounds.max(1) {
                    continue;
                }
                hot.remove(&c);
                if let Ok(kids) = world.split_cell(c) {
                    self.traffic.remove(&c);
                    self.idle_since.remove(&c);
                    for k in kids {
                        out.push(Assignment {
                            cell: k,
                            from: None,
                            to: sim,
                            reason: "split".into(),
                        });
                    }
                }
            }
            self.hot_rounds = hot;
        }
        let mut cold = std::mem::take(&mut self.cold_rounds);
        let parents = world.split_cells();
        cold.retain(|c, _| parents.contains(c));
        for p in parents {
            let kids = p.children();
            if world.is_pinned_split(p) || kids.iter().any(|k| world.is_split(*k)) {
                cold.remove(&p);
                continue;
            }
            let sum: f64 = kids.iter().map(|k| self.cell_cost(world, *k)).sum();
            if sum >= self.cfg.merge_cell_cost {
                cold.remove(&p);
                continue;
            }
            let rounds = cold.entry(p).or_insert(0);
            *rounds += 1;
            if *rounds < self.cfg.merge_cell_after_rounds.max(1) {
                continue;
            }
            let mut owners: BTreeMap<SimId, usize> = BTreeMap::new();
            for k in kids {
                if let Some(o) = world.owner(k) {
                    *owners.entry(o).or_insert(0) += 1;
                }
            }
            if owners.len() > 1 {
                let target = owners
                    .iter()
                    .filter(|(o, _)| self.is_alive(**o, tick))
                    .max_by_key(|(o, n)| (**n, std::cmp::Reverse(self.cost(world, **o) as i64)))
                    .map(|(o, _)| *o);
                let Some(target) = target else {
                    continue;
                };
                for k in kids {
                    let from = world.owner(k);
                    if from == Some(target) || from.is_none() {
                        continue;
                    }
                    if !self.has_room(world, target) {
                        break;
                    }
                    world.revoke_lease(k);
                    world.grant_lease(k, target);
                    out.push(Assignment {
                        cell: k,
                        from,
                        to: target,
                        reason: "consolidate".into(),
                    });
                }
                continue;
            }
            match world.merge_cell(p) {
                Ok(owner) => {
                    cold.remove(&p);
                    for k in kids {
                        self.traffic.remove(&k);
                        self.idle_since.remove(&k);
                    }
                    if let Some(o) = owner {
                        out.push(Assignment {
                            cell: p,
                            from: None,
                            to: o,
                            reason: "merge_cells".into(),
                        });
                    }
                }
                Err(_) => {
                    cold.remove(&p);
                }
            }
        }
        self.cold_rounds = cold;
    }

    pub fn reconcile(&mut self, world: &mut World) -> Vec<Assignment> {
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
        self.split_and_merge_cells(world, &mut out);
        // a simulator that cannot keep up sheds a cell before its cells get dilated (plan 4.8:
        // split first, time dilation only when there is nothing left to move)
        let tidi_after = world.cfg.tidi_after_ticks.max(1);
        let sims: Vec<SimId> = self.sims.keys().copied().collect();
        for s in sims {
            if !self.is_alive(s, tick) {
                continue;
            }
            let cells = world.cells_of(s);
            let lagging = self.lag(s, tick) > tidi_after;
            if cells.len() < 2 || (self.cost(world, s) <= self.cfg.split_cost && !lagging) {
                continue;
            }
            // shed the cell with the fewest neighbours owned by `s` (least ghost coupling)
            let victim = cells
                .iter()
                .copied()
                .min_by_key(|c| {
                    world
                        .neighbours(*c)
                        .into_iter()
                        .filter(|n| world.owner(*n) == Some(s))
                        .count()
                })
                .unwrap();
            let Some(to) = self
                .sims
                .keys()
                .copied()
                .filter(|o| *o != s && self.is_alive(*o, tick) && self.has_room(world, *o))
                .filter(|o| self.lag(*o, tick) <= tidi_after)
                .filter(|o| {
                    lagging
                        || self.cost(world, *o) + self.cell_cost(world, victim)
                            <= self.cfg.split_cost
                })
                .min_by(|a, b| self.cost(world, *a).total_cmp(&self.cost(world, *b)))
            else {
                continue;
            };
            world.revoke_lease(victim);
            world.grant_lease(victim, to);
            out.push(Assignment {
                cell: victim,
                from: Some(s),
                to,
                reason: if lagging { "lagging" } else { "overloaded" }.into(),
            });
        }
        // merge (co-locate): a simulator quiet for `merge_after_rounds` rounds gives every cell
        // to the owner of a neighbouring cell with room well under split_cost — all cells or
        // none, half a merge is just churn — and ends up free
        let mut quiet = std::mem::take(&mut self.quiet_rounds);
        let sims: Vec<SimId> = self.sims.keys().copied().collect();
        for s in sims {
            let cells = world.cells_of(s);
            if cells.is_empty()
                || !self.is_alive(s, tick)
                || self.cost(world, s) >= self.cfg.merge_cost
            {
                quiet.remove(&s);
                continue;
            }
            let rounds = quiet.entry(s).or_insert(0);
            *rounds += 1;
            if *rounds < self.cfg.merge_after_rounds.max(1) {
                continue;
            }
            let mut plan: Vec<(CellId, SimId)> = Vec::new();
            for c in &cells {
                let dest = world
                    .neighbours(*c)
                    .into_iter()
                    .filter_map(|n| world.owner(n))
                    .filter(|o| *o != s && self.is_alive(*o, tick) && self.has_room(world, *o))
                    .filter(|o| self.lag(*o, tick) <= tidi_after)
                    .filter(|o| {
                        self.cost(world, *o) + self.cell_cost(world, *c)
                            <= self.cfg.split_cost * MERGE_HEADROOM
                    })
                    .min_by(|a, b| self.cost(world, *a).total_cmp(&self.cost(world, *b)));
                match dest {
                    Some(d) => plan.push((*c, d)),
                    None => {
                        plan.clear();
                        break;
                    }
                }
            }
            if plan.is_empty() {
                continue;
            }
            for (c, d) in plan {
                world.revoke_lease(c);
                world.grant_lease(c, d);
                out.push(Assignment {
                    cell: c,
                    from: Some(s),
                    to: d,
                    reason: "merge".into(),
                });
            }
            quiet.remove(&s);
        }
        self.quiet_rounds = quiet;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Pos, WorldConfig};

    #[test]
    fn hot_leaf_splits_and_cold_siblings_merge_after_consolidation() {
        let mut world = World::new(WorldConfig {
            cell_size: 100.0,
            max_depth: 1,
            ..WorldConfig::default()
        });
        let mut ctl = Controller::new(ControllerConfig {
            split_cell_cost: 3.0,
            split_cell_after_rounds: 2,
            merge_cell_cost: 2.0,
            merge_cell_after_rounds: 2,
            cost_entity: 1.0,
            cost_player: 0.0,
            cost_traffic: 0.0,
            split_cost: 1e9,
            merge_cost: 0.0,
            ..ControllerConfig::default()
        });
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        ctl.register(s1, 16, 0);
        ctl.register(s2, 16, 0);
        let root = CellId::root(0, 0);
        world.grant_lease(root, s1);
        let mut ids = Vec::new();
        for i in 0..4 {
            ids.push(world.spawn(
                Pos {
                    x: 10.0 + 20.0 * i as f64,
                    y: 10.0,
                },
                std::collections::BTreeMap::new(),
                false,
            ));
        }
        // round 1 is hysteresis, round 2 splits: four children, all to s1, nothing revoked
        assert!(ctl
            .reconcile(&mut world)
            .iter()
            .all(|a| a.reason != "split"));
        assert!(!world.is_split(root));
        let moves = ctl.reconcile(&mut world);
        let splits: Vec<&Assignment> = moves.iter().filter(|a| a.reason == "split").collect();
        assert_eq!(splits.len(), 4, "{moves:?}");
        assert!(splits
            .iter()
            .all(|a| a.to == s1 && a.from.is_none() && a.cell.level == 1));
        assert!(world.is_split(root));
        assert_eq!(
            world.entities_in(CellId {
                x: 0,
                y: 0,
                level: 1
            }),
            2
        );
        assert_eq!(
            world.entities_in(CellId {
                x: 1,
                y: 0,
                level: 1
            }),
            2
        );
        // cool down: despawn three, hand one child to s2 → consolidate first, then merge
        for id in &ids[..3] {
            world.despawn(*id);
        }
        world.revoke_lease(CellId {
            x: 1,
            y: 1,
            level: 1,
        });
        world.grant_lease(
            CellId {
                x: 1,
                y: 1,
                level: 1,
            },
            s2,
        );
        assert!(ctl
            .reconcile(&mut world)
            .iter()
            .all(|a| a.reason != "consolidate"));
        let moves = ctl.reconcile(&mut world);
        let cons: Vec<&Assignment> = moves.iter().filter(|a| a.reason == "consolidate").collect();
        assert_eq!(cons.len(), 1, "{moves:?}");
        assert_eq!((cons[0].from, cons[0].to), (Some(s2), s1));
        assert!(
            world.is_split(root),
            "merge waits a round after consolidation"
        );
        let moves = ctl.reconcile(&mut world);
        assert!(
            moves
                .iter()
                .any(|a| a.reason == "merge_cells" && a.cell == root && a.to == s1),
            "{moves:?}"
        );
        assert!(world.is_leaf(root) && world.owner(root) == Some(s1));
        assert_eq!(world.entities_in(root), 1);
    }

    #[test]
    fn soft_cap_refuses_spawn_but_not_handover_room_check() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            soft_cap: 2,
            ..WorldConfig::default()
        });
        let c = w.cell_of(Pos { x: 10.0, y: 10.0 });
        assert!(w.cell_has_room(c));
        w.spawn(Pos { x: 10.0, y: 10.0 }, Default::default(), false);
        w.spawn(Pos { x: 20.0, y: 10.0 }, Default::default(), false);
        assert!(!w.cell_has_room(c), "two of two slots taken");
        let other = w.cell_of(Pos { x: 150.0, y: 10.0 });
        assert!(w.cell_has_room(other));
        let mut unlimited = World::new(WorldConfig {
            cell_size: 100.0,
            soft_cap: 0,
            ..WorldConfig::default()
        });
        for i in 0..10 {
            unlimited.spawn(
                Pos {
                    x: i as f64,
                    y: 1.0,
                },
                Default::default(),
                false,
            );
        }
        assert!(unlimited.cell_has_room(c));
    }

    /// Plan 4.8: a simulator that cannot keep up first hands a cell to one that can; only what
    /// it still holds afterwards is dilated, and with nobody to take the cell everything is.
    #[test]
    fn lagging_simulator_sheds_a_cell_before_dilating() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            tidi_after_ticks: 10,
            tidi_min: 0.1,
            ..WorldConfig::default()
        });
        let mut ctl = Controller::new(ControllerConfig::default());
        let (slow, fast) = (SimId::now_v7(), SimId::now_v7());
        ctl.register(slow, 4, 0);
        ctl.register(fast, 4, 0);
        let (a, b) = (
            CellId {
                x: 0,
                y: 0,
                level: 0,
            },
            CellId {
                x: 1,
                y: 0,
                level: 0,
            },
        );
        w.grant_lease(a, slow);
        w.grant_lease(b, slow);
        w.spawn(Pos { x: 10.0, y: 10.0 }, Default::default(), false);
        w.spawn(Pos { x: 110.0, y: 10.0 }, Default::default(), false);
        w.tick = 100;
        w.renew_leases(slow);
        w.renew_leases(fast);
        ctl.seen(slow, 100);
        ctl.seen(fast, 100);
        ctl.progress(slow, 50);
        ctl.progress(fast, 99);
        // load 2 is far under split_cost: only the lag makes `slow` shed
        let out = ctl.reconcile(&mut w);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].from, Some(slow));
        assert_eq!(out[0].to, fast);
        assert_eq!(out[0].reason, "lagging");
        assert_eq!(w.cells_of(slow).len(), 1);
        let kept = w.cells_of(slow)[0];
        let moved = w.cells_of(fast)[0];
        // what it still holds is dilated (10/50); the moved cell runs at real time
        assert_eq!(ctl.dilate(&mut w), vec![(kept, 0.2)]);
        assert_eq!(w.time_scale(moved), 1.0);
        // one cell left: nothing more to shed
        let extra = ctl.reconcile(&mut w);
        assert!(extra.is_empty(), "{extra:?}");
        // the world moves on and `fast` falls behind too: a lagging simulator is no destination
        w.tick = 200;
        w.renew_leases(slow);
        w.renew_leases(fast);
        ctl.seen(slow, 200);
        ctl.seen(fast, 200);
        w.grant_lease(moved, slow);
        let extra = ctl.reconcile(&mut w);
        assert!(extra.is_empty(), "{extra:?}");
        // 150 behind: 10/150 is below the floor
        assert_eq!(ctl.dilate(&mut w).len(), 2);
        assert_eq!(w.time_scale(a), 0.1);
        assert_eq!(w.time_scale(b), 0.1);
    }

    /// Plan 4.9: an unwatched cell hibernates after `hibernate_after_ticks` — ephemeral entities
    /// die, persistent ones stay, the lease goes away without the cell becoming an orphan — and a
    /// client's interest wakes it through `reconcile`.
    #[test]
    fn unwatched_cell_hibernates_and_interest_wakes_it() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            hibernate_after_ticks: 5,
            ..WorldConfig::default()
        });
        let mut ctl = Controller::new(ControllerConfig::default());
        let sim = SimId::now_v7();
        ctl.register(sim, 4, 0);
        let c = CellId {
            x: 0,
            y: 0,
            level: 0,
        };
        w.grant_lease(c, sim);
        let keep = w.spawn(Pos { x: 10.0, y: 10.0 }, Default::default(), true);
        let litter = w.spawn(Pos { x: 20.0, y: 10.0 }, Default::default(), false);
        // the idle clock starts at the first unwatched check (tick 1) and fires at 1 + 5
        for _ in 0..5 {
            w.advance();
            w.renew_leases(sim);
            ctl.seen(sim, w.tick);
            assert!(ctl.hibernate(&mut w).is_empty());
        }
        w.advance();
        w.renew_leases(sim);
        ctl.seen(sim, w.tick);
        assert_eq!(ctl.hibernate(&mut w), vec![(c, Some(sim), 1)]);
        assert!(w.is_hibernated(c));
        assert_eq!(w.owner(c), None);
        assert!(w.get(keep).is_some());
        assert!(w.get(litter).is_none());
        assert_eq!(w.snapshot_persistent().len(), 1);
        // asleep is not orphaned: nothing re-leases it while nobody looks
        assert!(w.orphaned_cells().is_empty());
        assert!(ctl.reconcile(&mut w).is_empty());
        // a client arrives: the cell is leased again with its persistent entity in place
        w.subscribe(Uuid::now_v7(), Pos { x: 15.0, y: 15.0 }, 10.0);
        let out = ctl.reconcile(&mut w);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].reason, "interest");
        assert_eq!(w.owner(c), Some(sim));
        assert!(!w.is_hibernated(c));
        assert!(w.get(keep).is_some());
        // watched cells never hibernate
        for _ in 0..10 {
            w.advance();
            w.renew_leases(sim);
            assert!(ctl.hibernate(&mut w).is_empty());
        }
    }

    /// Plan 4.9 `decay(ttl)`: an entity nobody writes to for `decay_ticks` disappears; a write
    /// resets the clock; entities without decay live on.
    #[test]
    fn decay_despawns_untouched_entities() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            ..WorldConfig::default()
        });
        let sim = SimId::now_v7();
        let c = CellId {
            x: 0,
            y: 0,
            level: 0,
        };
        w.grant_lease(c, sim);
        let litter = w.spawn(Pos { x: 10.0, y: 10.0 }, Default::default(), true);
        let forever = w.spawn(Pos { x: 20.0, y: 10.0 }, Default::default(), false);
        assert!(w.set_decay(litter, Some(3)));
        for _ in 0..2 {
            w.advance();
            assert!(w.decay().is_empty());
        }
        // touched at tick 2: the clock restarts
        w.apply(
            sim,
            litter,
            Some(Pos { x: 11.0, y: 10.0 }),
            Default::default(),
        )
        .unwrap();
        for _ in 0..2 {
            w.advance();
            assert!(w.decay().is_empty());
        }
        w.advance();
        assert_eq!(w.decay(), vec![(litter, c, true)]);
        assert!(w.get(litter).is_none());
        assert!(w.get(forever).is_some());
    }

    /// Cost is not just entities: clients watching a cell and handover traffic count, so a
    /// simulator with few entities but a crowd of players sheds its cheap cell to a free one.
    #[test]
    fn players_and_traffic_count_toward_cost() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            ..WorldConfig::default()
        });
        let mut ctl = Controller::new(ControllerConfig {
            split_cost: 40.0,
            ..ControllerConfig::default()
        });
        let (s1, s2) = (SimId::now_v7(), SimId::now_v7());
        ctl.register(s1, 4, 0);
        ctl.register(s2, 4, 0);
        let (a, b) = (
            CellId {
                x: 0,
                y: 0,
                level: 0,
            },
            CellId {
                x: 1,
                y: 0,
                level: 0,
            },
        );
        w.grant_lease(a, s1);
        w.grant_lease(b, s1);
        w.spawn(Pos { x: 10.0, y: 10.0 }, Default::default(), false);
        w.spawn(Pos { x: 110.0, y: 10.0 }, Default::default(), false);
        assert!(ctl.reconcile(&mut w).is_empty(), "2 entities are no load");
        // ten clients crowd cell b: 10 × 5 = 50 > split_cost
        for i in 0..10 {
            w.subscribe(
                Uuid::now_v7(),
                Pos {
                    x: 150.0 + i as f64,
                    y: 50.0,
                },
                10.0,
            );
        }
        assert_eq!(w.players_in(b), 10);
        assert_eq!(ctl.cell_cost(&w, b), 51.0);
        assert_eq!(ctl.cost(&w, s1), 52.0);
        let out = ctl.reconcile(&mut w);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].reason, "overloaded");
        assert_eq!(out[0].cell, a, "the cheap, least-coupled cell moves");
        assert_eq!(out[0].to, s2);
        // traffic: entities in handover out of a cell raise its cost over a few samples
        ctl.observe(&w);
        assert_eq!(ctl.traffic.get(&b).copied().unwrap_or(0.0), 0.0);
    }

    /// Two simulators each holding one quiet neighbouring cell: after `merge_after_rounds`
    /// rounds under `merge_cost` one hands its cell to the other (co-locate) and is left free;
    /// nothing moves back while the merged simulator stays under split_cost.
    #[test]
    fn quiet_simulators_merge_onto_a_neighbour_with_hysteresis() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            ..WorldConfig::default()
        });
        let mut ctl = Controller::new(ControllerConfig {
            merge_cost: 10.0,
            merge_after_rounds: 3,
            ..ControllerConfig::default()
        });
        let (s1, s2) = (SimId::now_v7(), SimId::now_v7());
        ctl.register(s1, 4, 0);
        ctl.register(s2, 4, 0);
        let (a, b) = (
            CellId {
                x: 0,
                y: 0,
                level: 0,
            },
            CellId {
                x: 1,
                y: 0,
                level: 0,
            },
        );
        w.grant_lease(a, s1);
        w.grant_lease(b, s2);
        w.spawn(Pos { x: 10.0, y: 10.0 }, Default::default(), false);
        w.spawn(Pos { x: 110.0, y: 10.0 }, Default::default(), false);
        for round in 0..2 {
            assert!(
                ctl.reconcile(&mut w).is_empty(),
                "round {round} is hysteresis"
            );
        }
        let out = ctl.reconcile(&mut w);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0].reason, "merge");
        let (giver, taker) = (out[0].from.unwrap(), out[0].to);
        assert!(w.cells_of(giver).is_empty());
        assert_eq!(w.cells_of(taker).len(), 2);
        for _ in 0..5 {
            assert!(ctl.reconcile(&mut w).is_empty(), "no flapping");
        }
        // the quiet clock resets when a simulator gets busy
        let mut busy = Controller::new(ControllerConfig {
            merge_cost: 10.0,
            merge_after_rounds: 2,
            ..ControllerConfig::default()
        });
        busy.register(s1, 4, 0);
        busy.register(s2, 4, 0);
        w.revoke_lease(a);
        w.grant_lease(a, s1);
        w.revoke_lease(b);
        w.grant_lease(b, s2);
        assert!(busy.reconcile(&mut w).is_empty());
        for i in 0..3 {
            w.subscribe(
                Uuid::now_v7(),
                Pos {
                    x: 10.0 + i as f64,
                    y: 10.0,
                },
                5.0,
            );
        }
        // s1 is busy now (clock reset); the still-quiet s2 merges its cell onto busy s1 instead
        let out = busy.reconcile(&mut w);
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(
            (out[0].reason.as_str(), out[0].from, out[0].to),
            ("merge", Some(s2), s1)
        );
        assert_eq!(busy.quiet_rounds.get(&s1), None);
        assert_eq!(w.cells_of(s1).len(), 2);
    }

    #[test]
    fn dilate_follows_owner_lag_and_recovers() {
        let mut w = World::new(WorldConfig {
            cell_size: 100.0,
            tidi_after_ticks: 10,
            tidi_min: 0.1,
            ..WorldConfig::default()
        });
        let mut ctl = Controller::new(ControllerConfig::default());
        let sim = SimId::now_v7();
        ctl.register(sim, 4, 0);
        let c = CellId {
            x: 0,
            y: 0,
            level: 0,
        };
        w.grant_lease(c, sim);
        // never reported: no dilation
        w.tick = 100;
        w.renew_leases(sim);
        assert!(ctl.dilate(&mut w).is_empty());
        assert_eq!(w.time_scale(c), 1.0);
        // 10 ticks behind is within the allowance
        ctl.progress(sim, 90);
        assert!(ctl.dilate(&mut w).is_empty());
        // 40 ticks behind: 10/40 = 0.25
        w.tick = 130;
        w.renew_leases(sim);
        let ch = ctl.dilate(&mut w);
        assert_eq!(ch, vec![(c, 0.25)]);
        assert_eq!(w.time_scale(c), 0.25);
        assert_eq!(w.min_scale_near(Pos { x: 50.0, y: 50.0 }, 10.0), 0.25);
        assert_eq!(w.min_scale_near(Pos { x: 500.0, y: 50.0 }, 10.0), 1.0);
        // floor
        w.tick = 1000;
        w.renew_leases(sim);
        assert_eq!(ctl.dilate(&mut w), vec![(c, 0.1)]);
        // caught up: back to real time, reported once
        ctl.progress(sim, 1000);
        assert_eq!(ctl.dilate(&mut w), vec![(c, 1.0)]);
        assert!(ctl.dilate(&mut w).is_empty());
        assert_eq!(w.time_scale(c), 1.0);
    }
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
            .ensure_leased(
                &mut w,
                CellId {
                    x: 0,
                    y: 0,
                    level: 0,
                },
                Some(s1),
                "spawn",
            )
            .unwrap();
        assert_eq!(a.to, s1);
        // neighbour of s1's cell → s1 again (adjacency beats s2's lower load)
        assert_eq!(
            c.pick(
                &w,
                CellId {
                    x: 1,
                    y: 0,
                    level: 0
                },
                None
            ),
            Some(s1)
        );
        c.ensure_leased(
            &mut w,
            CellId {
                x: 1,
                y: 0,
                level: 0,
            },
            None,
            "x",
        );
        // s1 is full now → far cell goes to s2
        assert_eq!(
            c.pick(
                &w,
                CellId {
                    x: 2,
                    y: 0,
                    level: 0
                },
                None
            ),
            Some(s2)
        );
        c.ensure_leased(
            &mut w,
            CellId {
                x: 2,
                y: 0,
                level: 0,
            },
            None,
            "x",
        );
        c.ensure_leased(
            &mut w,
            CellId {
                x: 3,
                y: 0,
                level: 0,
            },
            None,
            "x",
        );
        assert_eq!(
            c.pick(
                &w,
                CellId {
                    x: 9,
                    y: 9,
                    level: 0
                },
                None
            ),
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
            ..ControllerConfig::default()
        });
        let (s1, s2) = (Uuid::now_v7(), Uuid::now_v7());
        c.register(s1, 4, 0);
        c.register(s2, 4, 0);
        w.grant_lease(
            CellId {
                x: 0,
                y: 0,
                level: 0,
            },
            s1,
        );
        w.grant_lease(
            CellId {
                x: 1,
                y: 0,
                level: 0,
            },
            s1,
        );
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
