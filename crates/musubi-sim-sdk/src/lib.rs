//! Simulator SDK (v0): drive a Musubi world from any Rust process — a headless game server, a
//! bot farm, a Rust/ECS simulation. The SDK owns the loop (register → heartbeat → drain events →
//! tick owned cells → push authoritative updates); you implement [`Simulator`]. Handover requests
//! are acked automatically unless you refuse them. Engine SDKs (Unreal/Unity/Godot) speak the same
//! HTTP API; this crate is the reference client.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use musubi_mesh::CellId;
use musubi_proto::realtime::WorldEntity;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SdkError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server {status}: {body}")]
    Server { status: u16, body: String },
    #[error("protocol: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, SdkError>;

#[derive(Clone)]
pub struct WorldClient {
    http: reqwest::Client,
    base: String,
    key: String,
    pub world: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct View {
    pub tick: u64,
    pub owner: Option<Uuid>,
    pub owned: Vec<WorldEntity>,
    pub ghosts: Vec<WorldEntity>,
}

/// An authoritative change produced by a tick.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Update {
    pub entity: String,
    pub pos: Option<(f64, f64)>,
    pub components: BTreeMap<String, Value>,
}

impl WorldClient {
    pub fn new(base_url: &str, server_key: &str, world: &str) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base_url.trim_end_matches('/').to_string(),
            key: server_key.to_string(),
            world: world.to_string(),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let url = format!("{}/v1/server/worlds/{}{}", self.base, self.world, path);
        let mut req = self
            .http
            .request(method, url)
            .header("x-musubi-server-key", &self.key);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await?;
        let status = res.status().as_u16();
        let text = res.text().await?;
        if !(200..300).contains(&status) {
            return Err(SdkError::Server { status, body: text });
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn create_world(&self, config: Value, controller: Value) -> Result<Value> {
        let url = format!("{}/v1/server/worlds", self.base);
        let res = self
            .http
            .post(url)
            .header("x-musubi-server-key", &self.key)
            .json(&json!({"name": self.world, "config": config, "controller": controller}))
            .send()
            .await?;
        let status = res.status().as_u16();
        let text = res.text().await?;
        if !(200..300).contains(&status) {
            return Err(SdkError::Server { status, body: text });
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn info(&self) -> Result<Value> {
        self.call(reqwest::Method::GET, "", None).await
    }

    pub async fn register(&self, sim: Uuid, capacity: u32) -> Result<()> {
        self.call(
            reqwest::Method::POST,
            "/sims",
            Some(json!({"sim": sim, "capacity": capacity})),
        )
        .await?;
        Ok(())
    }

    pub async fn unregister(&self, sim: Uuid) -> Result<()> {
        self.call(reqwest::Method::DELETE, "/sims", Some(json!({"sim": sim})))
            .await?;
        Ok(())
    }

    pub async fn lease(&self, sim: Uuid, cells: &[CellId]) -> Result<Vec<CellId>> {
        let v = self
            .call(
                reqwest::Method::POST,
                "/lease",
                Some(json!({"sim": sim, "cells": cells})),
            )
            .await?;
        Ok(serde_json::from_value(v["cells"].clone()).unwrap_or_default())
    }

    /// Heartbeat: renews every lease and marks the simulator alive. Returns the cells it owns.
    pub async fn renew(&self, sim: Uuid) -> Result<Vec<CellId>> {
        let v = self
            .call(reqwest::Method::POST, "/renew", Some(json!({"sim": sim})))
            .await?;
        Ok(serde_json::from_value(v["cells"].clone()).unwrap_or_default())
    }

    pub async fn spawn(
        &self,
        sim: Uuid,
        x: f64,
        y: f64,
        components: Value,
        persistent: bool,
    ) -> Result<WorldEntity> {
        let v = self
            .call(
                reqwest::Method::POST,
                "/spawn",
                Some(json!({"sim": sim, "x": x, "y": y, "components": components, "persistent": persistent})),
            )
            .await?;
        serde_json::from_value(v).map_err(|e| SdkError::Server {
            status: 200,
            body: e.to_string(),
        })
    }

    pub async fn despawn(&self, sim: Uuid, entity: &str) -> Result<()> {
        self.call(
            reqwest::Method::POST,
            "/despawn",
            Some(json!({"sim": sim, "entity": entity})),
        )
        .await?;
        Ok(())
    }

    pub async fn apply(&self, sim: Uuid, u: &Update) -> Result<()> {
        let mut body = json!({"sim": sim, "entity": u.entity, "components": u.components});
        if let Some((x, y)) = u.pos {
            body["x"] = json!(x);
            body["y"] = json!(y);
        }
        self.call(reqwest::Method::POST, "/apply", Some(body))
            .await?;
        Ok(())
    }

    pub async fn view(&self, cell: CellId) -> Result<View> {
        let v = self
            .call(
                reqwest::Method::GET,
                &format!("/view/{}/{}", cell.x, cell.y),
                None,
            )
            .await?;
        serde_json::from_value(v).map_err(|e| SdkError::Server {
            status: 200,
            body: e.to_string(),
        })
    }

    pub async fn events(&self, sim: Uuid) -> Result<Vec<Value>> {
        let v = self
            .call(reqwest::Method::GET, &format!("/events?sim={sim}"), None)
            .await?;
        Ok(serde_json::from_value(v["events"].clone()).unwrap_or_default())
    }

    /// Act on an entity this simulator does not own (a hit on a ghost across a cell or node
    /// border): the owner's simulator receives an `interaction` event with a per-entity `seq`.
    pub async fn interact(
        &self,
        sim: Uuid,
        entity: &str,
        kind: &str,
        payload: Value,
    ) -> Result<Value> {
        self.call(
            reqwest::Method::POST,
            "/interact",
            Some(json!({"sim": sim, "entity": entity, "kind": kind, "payload": payload})),
        )
        .await
    }

    pub async fn handover_ack(&self, sim: Uuid, entity: &str) -> Result<Vec<Value>> {
        let v = self
            .call(
                reqwest::Method::POST,
                "/handover/ack",
                Some(json!({"sim": sim, "entity": entity})),
            )
            .await?;
        Ok(serde_json::from_value(v["buffered_inputs"].clone()).unwrap_or_default())
    }
}

/// Your game logic. All hooks run on the SDK loop; keep them short.
pub trait Simulator: Send {
    fn on_cell_assigned(&mut self, _cell: CellId) {}
    fn on_cell_revoked(&mut self, _cell: CellId) {}
    /// Return `false` to refuse (the replication layer rolls the handover back after its timeout).
    fn on_handover_request(&mut self, _entity: &WorldEntity) -> bool {
        true
    }
    /// Inputs routed to us for an entity we own (also buffered ones delivered after a handover).
    fn on_input(&mut self, _entity: &str, _input: &Value) {}
    /// Simulate one cell for one tick; `view.owned` is authoritative, `view.ghosts` read-only.
    fn tick(&mut self, cell: CellId, view: &View) -> Vec<Update>;
}

pub struct SimConfig {
    pub capacity: u32,
    pub hz: u64,
    /// Heartbeat every N ticks.
    pub renew_every: u64,
}

impl Default for SimConfig {
    fn default() -> Self {
        Self {
            capacity: 4,
            hz: 30,
            renew_every: 10,
        }
    }
}

/// Run the loop until `stop` resolves. Errors from a single tick are logged, not fatal.
pub async fn run_sim<S: Simulator, F: std::future::Future<Output = ()>>(
    client: WorldClient,
    sim: Uuid,
    cfg: SimConfig,
    mut logic: S,
    stop: F,
) -> Result<()> {
    client.register(sim, cfg.capacity).await?;
    let mut cells: Vec<CellId> = client.renew(sim).await?;
    let mut iv = tokio::time::interval(Duration::from_millis(1000 / cfg.hz.max(1)));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut n: u64 = 0;
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = &mut stop => break,
            _ = iv.tick() => {}
        }
        n += 1;
        if n % cfg.renew_every == 0 {
            match client.renew(sim).await {
                Ok(c) => cells = c,
                Err(e) => tracing::warn!(error = %e, "renew failed"),
            }
        }
        let events = match client.events(sim).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "events failed");
                continue;
            }
        };
        for ev in events {
            match ev["type"].as_str().unwrap_or("") {
                "cell_assigned" => {
                    if let Ok(c) = serde_json::from_value::<CellId>(ev["cell"].clone()) {
                        if !cells.contains(&c) {
                            cells.push(c);
                        }
                        logic.on_cell_assigned(c);
                    }
                }
                "cell_revoked" => {
                    if let Ok(c) = serde_json::from_value::<CellId>(ev["cell"].clone()) {
                        cells.retain(|x| *x != c);
                        logic.on_cell_revoked(c);
                    }
                }
                "handover_request" => {
                    let Ok(e) = serde_json::from_value::<WorldEntity>(ev["state"].clone()) else {
                        continue;
                    };
                    if logic.on_handover_request(&e) {
                        let id = e.id.to_string();
                        match client.handover_ack(sim, &id).await {
                            Ok(inputs) => {
                                for i in inputs {
                                    logic.on_input(&id, &i["input"]);
                                }
                            }
                            Err(err) => {
                                tracing::warn!(error = %err, entity = %e.id, "handover ack failed")
                            }
                        }
                    }
                }
                "input" => {
                    if let Some(id) = ev["entity"].as_str() {
                        logic.on_input(id, &ev["input"]);
                    }
                }
                _ => {}
            }
        }
        let mut updates: HashMap<String, Update> = HashMap::new();
        for c in cells.clone() {
            let view = match client.view(c).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, ?c, "view failed");
                    continue;
                }
            };
            if view.owner != Some(sim) {
                cells.retain(|x| *x != c);
                logic.on_cell_revoked(c);
                continue;
            }
            for u in logic.tick(c, &view) {
                updates.insert(u.entity.clone(), u);
            }
        }
        for u in updates.values() {
            if let Err(e) = client.apply(sim, u).await {
                tracing::debug!(error = %e, entity = %u.entity, "apply rejected (handover in flight?)");
            }
        }
    }
    let _ = client.unregister(sim).await;
    Ok(())
}

/// Streaming variant of [`run_sim`]: one WebSocket (`stream?sim=&delta=1`) carries leases,
/// per-tick delta frames, events and commands, and the SDK keeps each leased cell's [`View`]
/// itself (first frame of a cell is full, later frames carry only changed `owned`/`ghosts`
/// and departed ids in `gone`). `logic.tick` runs once per received frame per owned cell, so
/// the cadence is the world's tick rate; `cfg.hz`/`cfg.renew_every` are not used (the stream
/// renews the lease on every frame). Disconnects reconnect after 300 ms with the local view
/// dropped — the server sends full frames again on a fresh stream.
pub async fn run_sim_stream<S: Simulator, F: std::future::Future<Output = ()>>(
    client: WorldClient,
    sim: Uuid,
    cfg: SimConfig,
    mut logic: S,
    stop: F,
) -> Result<()> {
    use futures::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::Message;

    client.register(sim, cfg.capacity).await?;
    tokio::pin!(stop);
    let ws_base = client.base.replacen("http", "ws", 1);
    let url = format!(
        "{ws_base}/v1/server/worlds/{}/stream?sim={sim}&delta=1",
        client.world
    );
    let mut views: HashMap<CellId, View> = HashMap::new();
    'conn: loop {
        let mut req = url
            .as_str()
            .into_client_request()
            .map_err(|e| SdkError::Protocol(e.to_string()))?;
        req.headers_mut().insert(
            "x-musubi-server-key",
            client
                .key
                .parse()
                .map_err(|_| SdkError::Protocol("invalid server key header".into()))?,
        );
        let ws = tokio::select! {
            _ = &mut stop => break 'conn,
            r = tokio_tungstenite::connect_async(req) => r,
        };
        let (mut tx, mut rx) = match ws {
            Ok((ws, _)) => ws.split(),
            Err(e) => {
                tracing::warn!(error = %e, "sim stream connect failed");
                tokio::select! {
                    _ = &mut stop => break 'conn,
                    _ = tokio::time::sleep(Duration::from_millis(300)) => continue 'conn,
                }
            }
        };
        for c in views.keys() {
            logic.on_cell_revoked(*c);
        }
        views.clear();
        let mut pending_acks: std::collections::VecDeque<String> = Default::default();
        loop {
            let msg = tokio::select! {
                _ = &mut stop => break 'conn,
                m = rx.next() => m,
            };
            let text = match msg {
                Some(Ok(Message::Text(t))) => t,
                Some(Ok(Message::Binary(b))) => match String::from_utf8(b) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => continue,
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v["type"] == "tick" {
                let mut seen: Vec<CellId> = Vec::new();
                for cv in v["cells"].as_array().cloned().unwrap_or_default() {
                    let Ok(c) = serde_json::from_value::<CellId>(cv["cell"].clone()) else {
                        continue;
                    };
                    seen.push(c);
                    let fresh = !views.contains_key(&c);
                    let view = views.entry(c).or_insert_with(|| View {
                        tick: 0,
                        owner: None,
                        owned: Vec::new(),
                        ghosts: Vec::new(),
                    });
                    apply_delta(view, &cv);
                    if fresh {
                        logic.on_cell_assigned(c);
                    }
                }
                let gone: Vec<CellId> = views
                    .keys()
                    .filter(|c| !seen.contains(c))
                    .copied()
                    .collect();
                for c in gone {
                    views.remove(&c);
                    logic.on_cell_revoked(c);
                }
                for ev in v["events"].as_array().cloned().unwrap_or_default() {
                    match ev["type"].as_str().unwrap_or("") {
                        "handover_request" => {
                            let Ok(e) = serde_json::from_value::<WorldEntity>(ev["state"].clone())
                            else {
                                continue;
                            };
                            if logic.on_handover_request(&e) {
                                let id = e.id.to_string();
                                let cmd = json!({"op": "handover_ack", "entity": id});
                                if tx.send(Message::Text(cmd.to_string())).await.is_err() {
                                    break;
                                }
                                pending_acks.push_back(id);
                            }
                        }
                        "input" => {
                            if let Some(id) = ev["entity"].as_str() {
                                logic.on_input(id, &ev["input"]);
                            }
                        }
                        _ => {}
                    }
                }
                let mut updates: HashMap<String, Update> = HashMap::new();
                for (c, view) in views.iter() {
                    if view.owner != Some(sim) {
                        continue;
                    }
                    for u in logic.tick(*c, view) {
                        updates.insert(u.entity.clone(), u);
                    }
                }
                for u in updates.values() {
                    let mut cmd =
                        json!({"op": "apply", "entity": u.entity, "components": u.components});
                    if let Some((x, y)) = u.pos {
                        cmd["x"] = json!(x);
                        cmd["y"] = json!(y);
                    }
                    if tx.send(Message::Text(cmd.to_string())).await.is_err() {
                        break;
                    }
                }
            } else if v["op"] == "handover_ack" {
                let id = pending_acks.pop_front();
                if let (Some(id), Some(inputs)) = (id, v["result"]["buffered_inputs"].as_array()) {
                    for i in inputs {
                        logic.on_input(&id, &i["input"]);
                    }
                }
            } else if v["op"] == "apply" && !v["error"].is_null() {
                tracing::debug!(error = %v["error"], "apply rejected (handover in flight?)");
            }
        }
        tokio::select! {
            _ = &mut stop => break 'conn,
            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    }
    let _ = client.unregister(sim).await;
    Ok(())
}

/// Folds one cell frame (full or delta) into a locally kept [`View`].
fn apply_delta(view: &mut View, cv: &Value) {
    if cv["full"].as_bool().unwrap_or(false) {
        view.owned.clear();
        view.ghosts.clear();
    }
    if let Some(t) = cv["tick"].as_u64() {
        view.tick = t;
    }
    view.owner = cv["owner"].as_str().and_then(|s| s.parse().ok());
    let owned: Vec<WorldEntity> = serde_json::from_value(cv["owned"].clone()).unwrap_or_default();
    let ghosts: Vec<WorldEntity> = serde_json::from_value(cv["ghosts"].clone()).unwrap_or_default();
    let gone: Vec<Uuid> = serde_json::from_value(cv["gone"].clone()).unwrap_or_default();
    for e in owned {
        view.ghosts.retain(|g| g.id != e.id);
        match view.owned.iter_mut().find(|o| o.id == e.id) {
            Some(o) => *o = e,
            None => view.owned.push(e),
        }
    }
    for e in ghosts {
        view.owned.retain(|o| o.id != e.id);
        match view.ghosts.iter_mut().find(|g| g.id == e.id) {
            Some(g) => *g = e,
            None => view.ghosts.push(e),
        }
    }
    if !gone.is_empty() {
        view.owned.retain(|o| !gone.contains(&o.id));
        view.ghosts.retain(|g| !gone.contains(&g.id));
    }
}
