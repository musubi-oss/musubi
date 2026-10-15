//! Worlds: one `musubi_mesh::World` per named world on this node, ticked at 30 Hz. Simulators
//! drive it through the server-key HTTP API (lease, spawn, apply, view, events, handover ack);
//! clients subscribe by interest over the socket and receive `WorldDelta`s. v0 is node-local; the
//! shard controller / cross-node replication layer comes next.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::{Error, Result, SessionId};
use musubi_mesh::{CellId, Delta, Entity, HandoverOutcome, Pos, World, WorldConfig};
use musubi_proto::realtime::{WorldComponent, WorldEntity};
use musubi_proto::ServerMessage;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::fanout::Fanout;

pub const TICK_HZ: u64 = 30;
pub const MAX_SIM_EVENTS: usize = 4096;

struct WorldState {
    world: World,
    /// session → interest client id
    subs: HashMap<SessionId, Uuid>,
    /// simulator → pending events (handover requests, inputs, …)
    sim_events: HashMap<Uuid, Vec<Value>>,
}

#[derive(Default)]
pub struct WorldRegistry {
    worlds: DashMap<String, Arc<Mutex<WorldState>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldInfo {
    pub name: String,
    pub tick: u64,
    pub entities: usize,
    pub subscribers: usize,
    pub cells: Vec<(CellId, Uuid)>,
    pub orphaned: Vec<CellId>,
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
    }
}

impl WorldRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn get(&self, name: &str) -> Result<Arc<Mutex<WorldState>>> {
        self.worlds
            .get(name)
            .map(|w| w.clone())
            .ok_or_else(|| Error::NotFound(format!("world {name}")))
    }

    pub fn create(&self, name: &str, cfg: WorldConfig) -> Result<WorldInfo> {
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidArgument("world name 1..=64 chars".into()));
        }
        if cfg.cell_size <= 0.0 || cfg.ghost_margin < 0.0 || cfg.hysteresis < 0.0 {
            return Err(Error::InvalidArgument("bad world config".into()));
        }
        if self.worlds.contains_key(name) {
            return Err(Error::Conflict("world exists".into()));
        }
        self.worlds.insert(
            name.to_string(),
            Arc::new(Mutex::new(WorldState {
                world: World::new(cfg),
                subs: HashMap::new(),
                sim_events: HashMap::new(),
            })),
        );
        self.info(name)
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        self.worlds
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| Error::NotFound(format!("world {name}")))
    }

    pub fn info(&self, name: &str) -> Result<WorldInfo> {
        let w = self.get(name)?;
        let st = w.lock();
        let mut cells: Vec<(CellId, Uuid)> = Vec::new();
        for c in st.world.orphaned_cells() {
            let _ = c;
        }
        // leases are exposed through cells_of per sim; collect by scanning known sims
        let mut sims: Vec<Uuid> = st.sim_events.keys().copied().collect();
        sims.sort();
        for s in sims {
            for c in st.world.cells_of(s) {
                cells.push((c, s));
            }
        }
        Ok(WorldInfo {
            name: name.to_string(),
            tick: st.world.tick,
            entities: st.world.len(),
            subscribers: st.subs.len(),
            cells,
            orphaned: st.world.orphaned_cells(),
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

    pub fn renew(&self, name: &str, sim: Uuid) -> Result<Vec<CellId>> {
        let w = self.get(name)?;
        let mut st = w.lock();
        st.world.renew_leases(sim);
        Ok(st.world.cells_of(sim))
    }

    pub fn spawn(
        &self,
        name: &str,
        sim: Uuid,
        pos: Pos,
        components: Value,
        persistent: bool,
    ) -> Result<WorldEntity> {
        let comps = components
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let w = self.get(name)?;
        let mut st = w.lock();
        let cell = st.world.cell_of(pos);
        match st.world.owner(cell) {
            Some(o) if o == sim => {}
            Some(o) => {
                return Err(Error::PermissionDenied(format!(
                    "cell {cell:?} is owned by {o}"
                )))
            }
            None => return Err(Error::Conflict(format!("cell {cell:?} has no live lease"))),
        }
        let id = st.world.spawn(pos, comps, persistent);
        Ok(to_wire(st.world.get(id).unwrap()))
    }

    pub fn despawn(&self, name: &str, sim: Uuid, entity: &str) -> Result<()> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let cell = st
            .world
            .get(id)
            .ok_or_else(|| Error::NotFound("entity".into()))?
            .cell;
        if st.world.owner(cell) != Some(sim) {
            return Err(Error::PermissionDenied("not the owner".into()));
        }
        st.world.despawn(id);
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
        let comps = components
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        let w = self.get(name)?;
        let mut st = w.lock();
        st.world.apply(sim, id, pos, comps).map_err(mesh_err)
    }

    pub fn view(&self, name: &str, cell: CellId) -> Result<Value> {
        let w = self.get(name)?;
        let st = w.lock();
        let v = st.world.view(cell);
        Ok(json!({
            "tick": st.world.tick,
            "owner": st.world.owner(cell),
            "owned": v.owned.iter().map(to_wire).collect::<Vec<_>>(),
            "ghosts": v.ghosts.iter().map(to_wire).collect::<Vec<_>>(),
        }))
    }

    /// Drain pending events for a simulator (handover requests/results, routed inputs).
    pub fn events(&self, name: &str, sim: Uuid) -> Result<Vec<Value>> {
        let w = self.get(name)?;
        let mut st = w.lock();
        Ok(st.sim_events.entry(sim).or_default().drain(..).collect())
    }

    pub fn handover_ack(&self, name: &str, sim: Uuid, entity: &str) -> Result<Value> {
        let id = parse_entity(entity)?;
        let w = self.get(name)?;
        let mut st = w.lock();
        let from = st
            .world
            .get(id)
            .and_then(|e| e.transit.as_ref().map(|t| t.from));
        let (outcome, inputs) = st.world.handover_ack(sim, id).map_err(mesh_err)?;
        if let Some(from) = from {
            push_event(
                &mut st,
                from,
                json!({"type": "handover_completed", "entity": id, "to": sim}),
            );
        }
        Ok(json!({ "outcome": outcome, "buffered_inputs": inputs }))
    }

    // ----- client side -----

    pub fn subscribe(&self, name: &str, sid: SessionId, pos: Pos, radius: f64) -> Result<()> {
        if !(0.0..=100_000.0).contains(&radius) {
            return Err(Error::InvalidArgument("radius 0..=100000".into()));
        }
        let w = self.get(name)?;
        let mut st = w.lock();
        let client = *st.subs.entry(sid).or_insert_with(Uuid::now_v7);
        st.world.subscribe(client, pos, radius);
        Ok(())
    }

    pub fn unsubscribe(&self, name: &str, sid: SessionId) -> Result<()> {
        let w = self.get(name)?;
        let mut st = w.lock();
        if let Some(c) = st.subs.remove(&sid) {
            st.world.unsubscribe(c);
        }
        Ok(())
    }

    pub fn unsubscribe_all(&self, sid: SessionId) {
        for w in self.worlds.iter() {
            let mut st = w.lock();
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
        let ev = json!({"type": "input", "entity": id, "session": sid, "input": input});
        match st.world.route_input(id, ev.clone()).map_err(mesh_err)? {
            Some(sim) => push_event(&mut st, sim, ev),
            None => {}
        }
        Ok(())
    }

    // ----- ticking -----

    /// One tick of every world: clock, handover detection/expiry, client deltas.
    pub fn tick(&self, fanout: &Fanout) {
        for w in self.worlds.iter() {
            let name = w.key().clone();
            let mut st = w.lock();
            let mut outcomes = st.world.advance();
            outcomes.extend(st.world.detect_handovers());
            for o in outcomes {
                match o {
                    HandoverOutcome::Started { entity, from, to } => {
                        let full = st.world.get(entity).map(to_wire);
                        push_event(
                            &mut st,
                            to,
                            json!({"type": "handover_request", "entity": entity, "from": from, "state": full}),
                        );
                        push_event(
                            &mut st,
                            from,
                            json!({"type": "handover_started", "entity": entity, "to": to}),
                        );
                    }
                    HandoverOutcome::Completed { .. } => {}
                    HandoverOutcome::Cancelled { entity, reason } => {
                        let sims: Vec<Uuid> = st.sim_events.keys().copied().collect();
                        for s in sims {
                            push_event(
                                &mut st,
                                s,
                                json!({"type": "handover_cancelled", "entity": entity, "reason": reason}),
                            );
                        }
                    }
                }
            }
            let tick = st.world.tick;
            let subs: Vec<(SessionId, Uuid)> = st.subs.iter().map(|(s, c)| (*s, *c)).collect();
            for (sid, client) in subs {
                if let Some(d) = st.world.delta_for(client) {
                    if d != Delta::default() {
                        fanout.session(sid, delta_msg(&name, tick, d));
                    }
                }
            }
        }
    }

    pub fn spawn_ticker(self: Arc<Self>, fanout: Fanout) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(1000 / TICK_HZ));
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                iv.tick().await;
                self.tick(&fanout);
            }
        })
    }
}

fn push_event(st: &mut WorldState, sim: Uuid, ev: Value) {
    let q = st.sim_events.entry(sim).or_default();
    if q.len() < MAX_SIM_EVENTS {
        q.push(ev);
    }
}

fn delta_msg(world: &str, tick: u64, d: Delta) -> ServerMessage {
    ServerMessage::WorldDelta {
        world: world.to_string(),
        tick,
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
