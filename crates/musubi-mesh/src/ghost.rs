//! Server-to-server ghost replication: the host of a cell streams the entities near its border
//! to the hosts of the neighbouring cells as component-level deltas instead of full snapshots.
//!
//! The sender keeps, per cell, what the neighbours already hold (`GhostSendState`) and each tick
//! emits only entities that moved or whose components changed, with only the changed components;
//! a complete keyframe goes out every `ghost_keyframe_ticks` so a late or restarted neighbour
//! converges. Entities nearest the border go first; a per-tick byte budget defers the rest to the
//! next tick (keyframes are never cut). The receiver merges deltas into a `GhostCache` per source
//! cell and ignores deltas until it has seen a keyframe.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::{CellId, Component, Entity, EntityId, Pos, World};

/// One ghost in a delta: identity, placement and the components the receiver lacks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GhostUpsert {
    pub id: EntityId,
    pub pos: Pos,
    pub cell: CellId,
    pub rev: u64,
    pub changed_tick: u64,
    #[serde(default)]
    pub persistent: bool,
    #[serde(default)]
    pub attached_to: Option<EntityId>,
    /// Only the components changed since the revision the receiver holds (all of them when the
    /// receiver has never seen the entity or on a keyframe).
    pub components: BTreeMap<String, Component>,
}

/// What one cell host sends its neighbours for one tick.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GhostDelta {
    pub cell: CellId,
    pub tick: u64,
    /// Complete picture of the border band: the receiver drops every ghost not listed.
    pub keyframe: bool,
    pub upserts: Vec<GhostUpsert>,
    pub removed: Vec<EntityId>,
}

impl GhostDelta {
    /// Rough wire size, for budgets and metrics.
    pub fn estimated_bytes(&self) -> usize {
        self.upserts.iter().map(upsert_bytes).sum::<usize>() + self.removed.len() * 20
    }
}

fn upsert_bytes(u: &GhostUpsert) -> usize {
    80 + u
        .components
        .iter()
        .map(|(k, c)| k.len() + c.json().len() + 24)
        .sum::<usize>()
}

/// Sender side: what the neighbours of one cell hold, so the next delta carries only changes.
#[derive(Debug, Default, Clone)]
pub struct GhostSendState {
    sent: HashMap<EntityId, (u64, Pos)>,
    last_keyframe: Option<u64>,
    /// Entities that did not fit the budget on the last tick (sent first next time).
    pub deferred: usize,
    /// Estimated bytes of the last delta.
    pub last_bytes: usize,
}

impl GhostSendState {
    pub fn tracked(&self) -> usize {
        self.sent.len()
    }
    /// Revision of `id` the neighbours hold (what a handover may build on), if it was ever sent.
    pub fn sent_rev(&self, id: EntityId) -> Option<u64> {
        self.sent.get(&id).map(|(r, _)| *r)
    }
    /// Forget everything: the next delta is a keyframe (a neighbour asked for a resync).
    pub fn reset(&mut self) {
        self.sent.clear();
        self.last_keyframe = None;
    }
}

impl World {
    /// The delta the neighbours of `cell` need this tick, or `None` when nothing changed.
    /// Entities closest to the border come first; past `ghost_budget_bytes` the rest wait for
    /// the next tick (never on a keyframe, which must be complete).
    pub fn ghost_delta(&self, cell: CellId, st: &mut GhostSendState) -> Option<GhostDelta> {
        let border = self.border_entities(cell);
        self.delta_of(
            cell,
            st,
            border,
            self.cfg.ghost_budget_bytes,
            self.cfg.ghost_keyframe_ticks,
        )
    }

    /// Replication stream for the standby replicas of `cell`: every entity of the cell
    /// (persistent and ephemeral), component-level, never budget-cut, keyframe every
    /// `repl_keyframe_ticks`. A replica that applied the stream up to tick `t` can take the cell
    /// over at `t` without touching snapshots or the journal.
    pub fn repl_delta(&self, cell: CellId, st: &mut GhostSendState) -> Option<GhostDelta> {
        let mut ents = self.entities_of(cell);
        ents.sort_by_key(|e| e.id);
        self.delta_of(cell, st, ents, 0, self.cfg.repl_keyframe_ticks)
    }

    fn delta_of(
        &self,
        cell: CellId,
        st: &mut GhostSendState,
        border: Vec<&Entity>,
        budget: usize,
        keyframe_ticks: u64,
    ) -> Option<GhostDelta> {
        let tick = self.tick;
        let keyframe = match st.last_keyframe {
            None => true,
            Some(k) => keyframe_ticks > 0 && tick.saturating_sub(k) >= keyframe_ticks,
        };
        let removed: Vec<EntityId> = if keyframe {
            Vec::new()
        } else {
            st.sent
                .keys()
                .filter(|id| !border.iter().any(|e| e.id == **id))
                .copied()
                .collect()
        };
        let mut changed: Vec<&Entity> = border
            .iter()
            .copied()
            .filter(|e| {
                keyframe
                    || st
                        .sent
                        .get(&e.id)
                        .is_none_or(|(rev, pos)| *rev != e.rev || *pos != e.pos)
            })
            .collect();
        if budget > 0 {
            changed.sort_by(|a, b| {
                self.border_distance(cell, a.pos)
                    .abs()
                    .total_cmp(&self.border_distance(cell, b.pos).abs())
                    .then(a.id.cmp(&b.id))
            });
        }
        let mut bytes = 0usize;
        let mut upserts = Vec::new();
        let mut deferred = 0usize;
        for e in changed {
            let full = keyframe || !st.sent.contains_key(&e.id);
            let since = st.sent.get(&e.id).map(|(r, _)| *r).unwrap_or(0);
            let components: BTreeMap<String, Component> = e
                .components
                .iter()
                .filter(|(_, c)| full || c.changed_rev > since)
                .map(|(k, c)| {
                    (
                        k.clone(),
                        Component::new(c.value.clone(), c.version, c.changed_rev),
                    )
                })
                .collect();
            let u = GhostUpsert {
                id: e.id,
                pos: e.pos,
                cell: e.cell,
                rev: e.rev,
                changed_tick: e.changed_tick,
                persistent: e.persistent,
                attached_to: e.attached_to,
                components,
            };
            let sz = upsert_bytes(&u);
            if !keyframe && budget > 0 && bytes + sz > budget && !upserts.is_empty() {
                deferred += 1;
                continue;
            }
            bytes += sz;
            upserts.push(u);
        }
        st.deferred = deferred;
        for id in &removed {
            st.sent.remove(id);
        }
        if keyframe {
            st.sent.clear();
            st.last_keyframe = Some(tick);
        }
        for u in &upserts {
            st.sent.insert(u.id, (u.rev, u.pos));
        }
        if !keyframe && upserts.is_empty() && removed.is_empty() {
            st.last_bytes = 0;
            return None;
        }
        let d = GhostDelta {
            cell,
            tick,
            keyframe,
            upserts,
            removed,
        };
        st.last_bytes = d.estimated_bytes();
        Some(d)
    }
}

/// Receiver side: the ghosts one remote cell has streamed to this node.
#[derive(Debug, Default, Clone)]
pub struct GhostCache {
    /// Sender's tick of the last applied delta.
    pub remote_tick: u64,
    /// Local tick when the last delta arrived (staleness is judged against local ticks).
    pub received_tick: u64,
    synced: bool,
    entities: HashMap<EntityId, Entity>,
}

impl GhostCache {
    /// Merge a delta; `false` when it was ignored (no keyframe seen yet, or older than what we hold).
    pub fn apply(&mut self, d: GhostDelta, local_tick: u64) -> bool {
        if d.keyframe {
            self.entities.clear();
            self.synced = true;
        } else if !self.synced || d.tick < self.remote_tick {
            return false;
        }
        self.remote_tick = d.tick;
        self.received_tick = local_tick;
        for id in d.removed {
            self.entities.remove(&id);
        }
        for u in d.upserts {
            let e = self.entities.entry(u.id).or_insert_with(|| Entity {
                id: u.id,
                pos: u.pos,
                cell: u.cell,
                components: BTreeMap::new(),
                persistent: u.persistent,
                transit: None,
                attached_to: None,
                handover_attempts: 0,
                handover_retry_tick: 0,
                decay_ticks: None,
                touched_tick: 0,
                changed_tick: 0,
                rev: 0,
            });
            e.pos = u.pos;
            e.cell = u.cell;
            e.rev = u.rev;
            e.changed_tick = u.changed_tick;
            e.persistent = u.persistent;
            e.attached_to = u.attached_to;
            for (k, c) in u.components {
                e.components.insert(k, c);
            }
        }
        true
    }

    /// Remove and return a ghost: it is being promoted to an owned entity by a handover.
    /// Every entity held, leaving the cache empty (a replica taking its cell over).
    pub fn drain(&mut self) -> Vec<Entity> {
        let mut v: Vec<Entity> = self.entities.drain().map(|(_, e)| e).collect();
        v.sort_by_key(|e| e.id);
        v
    }
    pub fn take(&mut self, id: EntityId) -> Option<Entity> {
        self.entities.remove(&id)
    }
    pub fn is_synced(&self) -> bool {
        self.synced
    }
    /// No delta for more than `max_ticks` local ticks: the host stopped publishing or moved.
    pub fn stale(&self, local_tick: u64, max_ticks: u64) -> bool {
        max_ticks > 0 && local_tick.saturating_sub(self.received_tick) > max_ticks
    }
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.entities.values()
    }
    pub fn len(&self) -> usize {
        self.entities.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorldConfig;
    use serde_json::json;

    fn world(budget: usize, keyframe: u64) -> World {
        World::new(WorldConfig {
            cell_size: 100.0,
            ghost_margin: 20.0,
            ghost_budget_bytes: budget,
            ghost_keyframe_ticks: keyframe,
            ..WorldConfig::default()
        })
    }
    fn comps(pairs: &[(&str, serde_json::Value)]) -> BTreeMap<String, serde_json::Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }
    fn p(x: f64, y: f64) -> Pos {
        Pos { x, y, z: 0.0 }
    }

    #[test]
    fn repl_delta_mirrors_the_whole_cell_and_a_mirror_can_take_it_over() {
        let mut w = world(0, 30);
        let cell = w.cell_of(p(50.0, 50.0));
        let border = w.spawn(p(95.0, 50.0), comps(&[("hp", json!(100))]), true);
        let interior = w.spawn(p(50.0, 50.0), comps(&[("hp", json!(1))]), false);
        let _elsewhere = w.spawn(p(150.0, 50.0), comps(&[("hp", json!(9))]), false);
        let mut st = GhostSendState::default();
        let d = w.repl_delta(cell, &mut st).expect("first delta");
        assert!(d.keyframe);
        let mut ids: Vec<_> = d.upserts.iter().map(|u| u.id).collect();
        ids.sort();
        let mut want = vec![border, interior];
        want.sort();
        assert_eq!(ids, want, "every entity of the cell, interior included");
        let mut mirror = GhostCache::default();
        assert!(mirror.apply(d, 0));
        assert!(w.repl_delta(cell, &mut st).is_none(), "nothing changed");

        w.force_apply(interior, None, comps(&[("hp", json!(2))]))
            .unwrap();
        let d = w.repl_delta(cell, &mut st).expect("hp changed");
        assert!(!d.keyframe);
        assert_eq!(d.upserts.len(), 1);
        assert_eq!(d.upserts[0].id, interior);
        assert!(mirror.apply(d, 1));

        let taken = mirror.drain();
        assert!(mirror.is_empty());
        assert_eq!(taken.len(), 2);
        let i = taken.iter().find(|e| e.id == interior).unwrap();
        assert_eq!(i.components["hp"].value, json!(2));
        assert!(!i.persistent);
        let b = taken.iter().find(|e| e.id == border).unwrap();
        assert_eq!(b.components["hp"].value, json!(100));
        assert!(b.persistent);
    }

    #[test]
    fn delta_sends_only_changes_and_changed_components() {
        let mut w = world(0, 30);
        let cell = w.cell_of(p(50.0, 50.0));
        let a = w.spawn(
            p(95.0, 50.0),
            comps(&[("hp", json!(100)), ("name", json!("a"))]),
            false,
        );
        let _b = w.spawn(p(50.0, 50.0), comps(&[("hp", json!(1))]), false);
        let c = w.spawn(p(90.0, 10.0), comps(&[("hp", json!(5))]), false);
        let mut st = GhostSendState::default();
        let d = w.ghost_delta(cell, &mut st).expect("first delta");
        assert!(d.keyframe);
        let mut ids: Vec<_> = d.upserts.iter().map(|u| u.id).collect();
        ids.sort();
        let mut want = vec![a, c];
        want.sort();
        assert_eq!(ids, want, "interior entity must not be ghosted");
        assert_eq!(
            d.upserts
                .iter()
                .find(|u| u.id == a)
                .unwrap()
                .components
                .len(),
            2
        );
        assert!(w.ghost_delta(cell, &mut st).is_none(), "nothing changed");

        w.force_apply(a, None, comps(&[("hp", json!(90))])).unwrap();
        let d = w.ghost_delta(cell, &mut st).expect("hp changed");
        assert!(!d.keyframe);
        assert_eq!(d.upserts.len(), 1);
        let u = &d.upserts[0];
        assert_eq!(u.id, a);
        assert_eq!(u.components.keys().collect::<Vec<_>>(), vec!["hp"]);
        assert_eq!(u.components["hp"].value, json!(90));
        assert!(d.removed.is_empty());

        w.force_apply(c, Some(p(50.0, 50.0)), BTreeMap::new())
            .unwrap();
        let d = w.ghost_delta(cell, &mut st).expect("c left the band");
        assert_eq!(d.removed, vec![c]);
        assert!(d.upserts.is_empty());
        assert_eq!(st.tracked(), 1);

        w.force_apply(a, Some(p(96.0, 50.0)), BTreeMap::new())
            .unwrap();
        let d = w.ghost_delta(cell, &mut st).expect("a moved");
        assert_eq!(d.upserts.len(), 1);
        assert!(
            d.upserts[0].components.is_empty(),
            "a move carries no components"
        );
    }

    #[test]
    fn budget_defers_far_entities_and_keyframe_is_complete() {
        let mut w = world(1, 5);
        let cell = w.cell_of(p(50.0, 50.0));
        let near = w.spawn(p(99.0, 50.0), comps(&[("hp", json!(1))]), false);
        let far = w.spawn(p(85.0, 50.0), comps(&[("hp", json!(1))]), false);
        let mut st = GhostSendState::default();
        let d = w.ghost_delta(cell, &mut st).unwrap();
        assert!(
            d.keyframe && d.upserts.len() == 2,
            "keyframes are never cut"
        );
        w.force_apply(near, None, comps(&[("hp", json!(2))]))
            .unwrap();
        w.force_apply(far, None, comps(&[("hp", json!(2))]))
            .unwrap();
        w.tick = 1;
        let d = w.ghost_delta(cell, &mut st).unwrap();
        assert_eq!(
            d.upserts.iter().map(|u| u.id).collect::<Vec<_>>(),
            vec![near]
        );
        assert_eq!(st.deferred, 1);
        w.tick = 2;
        let d = w.ghost_delta(cell, &mut st).unwrap();
        assert_eq!(
            d.upserts.iter().map(|u| u.id).collect::<Vec<_>>(),
            vec![far]
        );
        assert_eq!(st.deferred, 0);
        w.tick = 5;
        let d = w.ghost_delta(cell, &mut st).unwrap();
        assert!(d.keyframe && d.upserts.len() == 2);
    }

    #[test]
    fn cache_waits_for_keyframe_and_merges() {
        let mut w = world(0, 30);
        let cell = w.cell_of(p(50.0, 50.0));
        let a = w.spawn(
            p(95.0, 50.0),
            comps(&[("hp", json!(100)), ("name", json!("a"))]),
            false,
        );
        let mut st = GhostSendState::default();
        let key = w.ghost_delta(cell, &mut st).unwrap();
        w.force_apply(a, None, comps(&[("hp", json!(7))])).unwrap();
        let upd = w.ghost_delta(cell, &mut st).unwrap();

        let mut cache = GhostCache::default();
        assert!(!cache.apply(upd.clone(), 10), "no keyframe yet");
        assert!(cache.is_empty());
        assert!(cache.apply(key, 11));
        assert!(cache.apply(upd, 12));
        let e = cache.entities().next().unwrap();
        assert_eq!(e.components["hp"].value, json!(7));
        assert_eq!(e.components["name"].value, json!("a"));
        assert_eq!(e.rev, w.get(a).unwrap().rev);
        assert!(!cache.stale(12 + 60, 60));
        assert!(cache.stale(12 + 61, 60));

        w.force_apply(a, Some(p(50.0, 50.0)), BTreeMap::new())
            .unwrap();
        let gone = w.ghost_delta(cell, &mut st).unwrap();
        assert!(cache.apply(gone, 13));
        assert!(cache.is_empty());
    }
}
