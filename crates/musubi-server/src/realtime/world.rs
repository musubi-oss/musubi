//! Worlds: one `musubi_mesh::World` per named world on this node, ticked at 30 Hz. Simulators
//! drive it through the server-key HTTP API (lease, spawn, apply, view, events, handover ack);
//! clients subscribe by interest over the socket and receive `WorldDelta`s. A world lives on one
//! home node (directory: `musubi.worlds`); clients on other nodes reach it over the bus
//! (`WorldCmd`), simulators are redirected to the home node's HTTP address.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::{Error, Result, SessionId};
use musubi_mesh::controller::{Assignment, Controller, ControllerConfig, SimInfo};
use musubi_mesh::{CellId, Delta, Entity, HandoverOutcome, Pos, World, WorldConfig};
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
}

pub struct WorldRegistry {
    node_id: Uuid,
    db: Option<PgPool>,
    worlds: DashMap<String, Arc<Mutex<WorldState>>>,
    /// world → home node (cache of `musubi.worlds`)
    homes: DashMap<String, Uuid>,
    /// Raft directory (`world:<name>` → home) when the cluster runs the QUIC bus.
    cluster: std::sync::OnceLock<Arc<musubi_cluster::Cluster>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorldCmd {
    Subscribe { x: f64, y: f64, radius: f64 },
    Unsubscribe,
    Input { entity: String, input: Value },
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
    }
}

impl WorldRegistry {
    pub fn new(node_id: Uuid, db: Option<PgPool>) -> Self {
        Self {
            node_id,
            db,
            worlds: DashMap::new(),
            homes: DashMap::new(),
            cluster: std::sync::OnceLock::new(),
        }
    }

    pub fn set_cluster(&self, cluster: Arc<musubi_cluster::Cluster>) {
        let _ = self.cluster.set(cluster);
    }

    fn directory_key(name: &str) -> String {
        format!("world:{name}")
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

    async fn write_snapshot(
        db: &PgPool,
        name: &str,
        entities: Vec<musubi_mesh::Entity>,
        tick: u64,
    ) -> Result<()> {
        sqlx::query(
            "insert into musubi.world_snapshots (name, entities, tick, updated_at) values ($1, $2, $3, now())
             on conflict (name) do update set entities = excluded.entities, tick = excluded.tick, updated_at = now()",
        )
        .bind(name)
        .bind(serde_json::to_value(&entities)?)
        .bind(tick as i64)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Write the world's persistent entities to the snapshot store now (the ticker also does it
    /// every `SNAPSHOT_EVERY` ticks).
    pub async fn snapshot_now(&self, name: &str) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let (entities, tick) = {
            let w = self.get(name)?;
            let st = w.lock();
            (st.world.snapshot_persistent(), st.world.tick)
        };
        Self::write_snapshot(db, name, entities, tick).await
    }

    /// Adopt worlds whose home node is dead (or gone from the node table): claim the directory row
    /// atomically, rebuild the world from its config and the last snapshot. Returns adopted names.
    pub async fn recover(&self, dead_nodes: &[Uuid]) -> Result<Vec<String>> {
        let Some(db) = &self.db else {
            return Ok(vec![]);
        };
        let rows: Vec<(String, Value, Value)> = sqlx::query_as(
            "update musubi.worlds w set node_id = $2
             where w.node_id <> $2
               and (w.node_id = any($1) or not exists (select 1 from musubi.nodes n where n.id = w.node_id))
             returning name, config, controller",
        )
        .bind(dead_nodes)
        .bind(self.node_id)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (name, cfg, ctl) in rows {
            let cfg: WorldConfig = serde_json::from_value(cfg)?;
            let ctl: ControllerConfig = serde_json::from_value(ctl)?;
            let snap: Option<(Value, i64)> =
                sqlx::query_as("select entities, tick from musubi.world_snapshots where name = $1")
                    .bind(&name)
                    .fetch_optional(db)
                    .await
                    .map_err(Error::internal)?;
            let mut world = World::new(cfg);
            if let Some((ents, tick)) = snap {
                let ents: Vec<musubi_mesh::Entity> = serde_json::from_value(ents)?;
                world.restore(ents);
                world.tick = tick as u64;
            }
            self.worlds.insert(
                name.clone(),
                Arc::new(Mutex::new(WorldState {
                    world,
                    controller: Controller::new(ctl),
                    subs: HashMap::new(),
                    sim_events: HashMap::new(),
                })),
            );
            self.homes.insert(name.clone(), self.node_id);
            self.publish_home(&name, Some(self.node_id)).await;
            out.push(name);
        }
        Ok(out)
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    /// Home node of a world: local registry first, then the directory. `None` = unknown world.
    pub async fn home_of(&self, name: &str) -> Result<Option<Uuid>> {
        if self.worlds.contains_key(name) {
            return Ok(Some(self.node_id));
        }
        if let Some(h) = self.homes.get(name) {
            return Ok(Some(*h));
        }
        if let Some(c) = self.cluster.get() {
            if let Some(v) = c.directory_get(&Self::directory_key(name)).await {
                if let Some(h) = v["node_id"].as_str().and_then(|x| Uuid::parse_str(x).ok()) {
                    self.homes.insert(name.to_string(), h);
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
        }
        Ok(row)
    }

    pub fn is_home(&self, name: &str) -> bool {
        self.worlds.contains_key(name)
    }

    /// Run a client command on the home node; errors go back to the session as messages.
    pub fn exec(&self, env: WorldCmdEnv) -> Option<ServerMessage> {
        let r = match env.cmd {
            WorldCmd::Subscribe { x, y, radius } => {
                self.subscribe(&env.world, env.session_id, Pos { x, y }, radius)
            }
            WorldCmd::Unsubscribe => self.unsubscribe(&env.world, env.session_id),
            WorldCmd::Input { entity, input } => {
                self.input(&env.world, env.session_id, &entity, input)
            }
        };
        r.err().map(|e| ServerMessage::error(&e))
    }

    fn get(&self, name: &str) -> Result<Arc<Mutex<WorldState>>> {
        self.worlds
            .get(name)
            .map(|w| w.clone())
            .ok_or_else(|| Error::NotFound(format!("world {name}")))
    }

    pub async fn create(
        &self,
        name: &str,
        cfg: WorldConfig,
        ctl: ControllerConfig,
    ) -> Result<WorldInfo> {
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
            sqlx::query("insert into musubi.worlds (name, node_id, config, controller) values ($1, $2, $3, $4)")
                .bind(name)
                .bind(self.node_id)
                .bind(serde_json::to_value(&cfg)?)
                .bind(serde_json::to_value(&ctl)?)
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
            })),
        );
        self.publish_home(name, Some(self.node_id)).await;
        self.info(name)
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
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        st.world.renew_leases(sim);
        let tick = st.world.tick;
        st.controller.seen(sim, tick);
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
        let st = &mut *st;
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
        let st = &mut *st;
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

    pub fn subscribe(&self, name: &str, sid: SessionId, pos: Pos, radius: f64) -> Result<()> {
        if !(0.0..=100_000.0).contains(&radius) {
            return Err(Error::InvalidArgument("radius 0..=100000".into()));
        }
        let w = self.get(name)?;
        let mut st = w.lock();
        let st = &mut *st;
        let client = *st.subs.entry(sid).or_insert_with(Uuid::now_v7);
        st.world.subscribe(client, pos, radius);
        for c in st.world.cells_in_radius(pos, radius) {
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
            let mut st = w.lock();
            let st = &mut *st;
            let mut outcomes = st.world.advance();
            outcomes.extend(st.world.detect_handovers());
            if st.world.tick % SNAPSHOT_EVERY == 0 {
                if let Some(db) = self.db.clone() {
                    let (entities, tick, n) =
                        (st.world.snapshot_persistent(), st.world.tick, name.clone());
                    tokio::spawn(async move {
                        if let Err(e) = Self::write_snapshot(&db, &n, entities, tick).await {
                            tracing::warn!(world = %n, error = %e, "world snapshot failed");
                        }
                    });
                }
            }
            if st.world.tick % CONTROL_EVERY == 0 {
                let moves = st.controller.reconcile(&mut st.world);
                for a in &moves {
                    announce(st, a);
                }
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
                    HandoverOutcome::Cancelled { entity, reason } => {
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
