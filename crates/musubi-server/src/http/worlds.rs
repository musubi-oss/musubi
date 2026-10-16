//! Server meshing: simulator-facing API (server key). Simulators lease cells, spawn/apply
//! authoritative state, read their view (owned + ghosts), drain events (handover requests,
//! routed inputs) and ack handovers.

// Handlers return a ready `Response` (redirect) on the error path; axum's Response is large.
#![allow(clippy::result_large_err)]

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use musubi_mesh::controller::ControllerConfig;
use musubi_mesh::{CellId, Pos, WorldConfig};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};
use crate::realtime::world::{InteractSpec, SpawnOutcome, SpawnSpec};

/// Simulator calls must hit the world's home node: redirect (307, same method+body) when the home
/// advertises an HTTP address, otherwise explain which node to talk to.
async fn ensure_home(s: &AppState, world: &str, uri: &OriginalUri) -> Result<(), Response> {
    let home = match s.worlds.home_of(world).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return Err(ApiError::from(Error::NotFound(format!("world {world}"))).into_response())
        }
        Err(e) => return Err(ApiError::from(e).into_response()),
    };
    redirect_to(s, world, home, uri).await
}

/// Simulator-scoped operations run on any node that keeps a shard of the world (the home or a
/// cell host); without one, fall back to the home redirect.
async fn ensure_shard(s: &AppState, world: &str, uri: &OriginalUri) -> Result<(), Response> {
    if s.worlds.has_shard(world) {
        return Ok(());
    }
    ensure_home(s, world, uri).await
}

/// A lease names cells; they must all be hosted by one node, which serves the request.
async fn ensure_cells_home(
    s: &AppState,
    world: &str,
    cells: &[CellId],
    uri: &OriginalUri,
) -> Result<(), Response> {
    let mut host: Option<uuid::Uuid> = None;
    for c in cells {
        let h = match s.worlds.cell_host_of(world, *c).await {
            Ok(Some(h)) => h,
            Ok(None) => {
                return Err(
                    ApiError::from(Error::NotFound(format!("world {world}"))).into_response()
                )
            }
            Err(e) => return Err(ApiError::from(e).into_response()),
        };
        if host.is_some_and(|x| x != h) {
            return Err(ApiError::from(Error::InvalidArgument(
                "cells are hosted on different nodes; lease them separately".into(),
            ))
            .into_response());
        }
        host = Some(h);
    }
    match host {
        Some(h) => redirect_to(s, world, h, uri).await,
        None => ensure_shard(s, world, uri).await,
    }
}

/// Cell-level twin of `ensure_home`: cells of one world may be hosted on different nodes.
async fn ensure_cell_home(
    s: &AppState,
    world: &str,
    cell: CellId,
    uri: &OriginalUri,
) -> Result<(), Response> {
    let host = match s.worlds.cell_host_of(world, cell).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return Err(ApiError::from(Error::NotFound(format!("world {world}"))).into_response())
        }
        Err(e) => return Err(ApiError::from(e).into_response()),
    };
    redirect_to(s, world, host, uri).await
}

async fn redirect_to(
    s: &AppState,
    world: &str,
    home: uuid::Uuid,
    uri: &OriginalUri,
) -> Result<(), Response> {
    if home == s.worlds.node_id() {
        return Ok(());
    }
    let advertise = match &s.cluster {
        Some(c) => c
            .nodes()
            .await
            .ok()
            .and_then(|ns| ns.into_iter().find(|n| n.id.0 == home))
            .map(|n| n.advertise)
            .unwrap_or_default(),
        None => String::new(),
    };
    let usable = advertise
        .trim_start_matches("http://")
        .parse::<std::net::SocketAddr>()
        .map(|a| a.port() != 0)
        .unwrap_or(!advertise.is_empty());
    if !usable {
        return Err(ApiError::from(Error::Conflict(format!(
            "world {world} lives on node {home}"
        )))
        .into_response());
    }
    let location = format!(
        "http://{}{}",
        advertise.trim_start_matches("http://"),
        uri.0
    );
    let mut res = StatusCode::TEMPORARY_REDIRECT.into_response();
    if let Ok(v) = HeaderValue::from_str(&location) {
        res.headers_mut().insert(header::LOCATION, v);
    }
    Err(res)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/worlds", get(list).post(create))
        .route("/v1/server/worlds/{w}", get(info).delete(delete))
        .route(
            "/v1/server/worlds/{w}/sims",
            post(register_sim).delete(unregister_sim),
        )
        .route("/v1/server/worlds/{w}/lease", post(lease))
        .route("/v1/server/worlds/{w}/renew", post(renew))
        .route("/v1/server/worlds/{w}/spawn", post(spawn))
        .route("/v1/server/worlds/{w}/despawn", post(despawn))
        .route("/v1/server/worlds/{w}/apply", post(apply))
        .route("/v1/server/worlds/{w}/view/{x}/{y}", get(view))
        .route("/v1/server/worlds/{w}/events", get(events))
        .route("/v1/server/worlds/{w}/stream", get(stream))
        .route("/v1/server/worlds/{w}/handover/ack", post(handover_ack))
        .route("/v1/server/worlds/{w}/interact", post(interact))
        .route("/v1/server/worlds/{w}/rewind", post(rewind))
        .route("/v1/server/worlds/{w}/colocate", post(colocate))
}

#[derive(Deserialize)]
struct CreateReq {
    name: String,
    #[serde(default)]
    config: WorldConfig,
    #[serde(default)]
    controller: ControllerConfig,
}

#[derive(Deserialize)]
struct RegisterSimReq {
    sim: Uuid,
    #[serde(default = "one")]
    capacity: u32,
}

fn one() -> u32 {
    1
}

#[derive(Deserialize)]
struct TickDoneReq {
    tick: u64,
}

#[derive(Deserialize)]
struct SimQuery {
    sim: Uuid,
    /// Last world tick the simulator has fully processed (drives time dilation).
    #[serde(default)]
    tick: Option<u64>,
}

#[derive(Deserialize)]
struct LeaseReq {
    sim: Uuid,
    cells: Vec<CellId>,
}

#[derive(Deserialize)]
struct SpawnReq {
    sim: Uuid,
    x: f64,
    y: f64,
    #[serde(default)]
    components: Value,
    #[serde(default)]
    persistent: bool,
    /// Wait in the cell's admission queue instead of failing when `soft_cap` is reached.
    #[serde(default)]
    queue: bool,
    /// Despawn after this many ticks without a write, interaction or handover (litter).
    #[serde(default)]
    decay_ticks: Option<u64>,
}

#[derive(Deserialize)]
struct EntityReq {
    sim: Uuid,
    entity: String,
}

#[derive(Deserialize)]
struct InteractReq {
    sim: Uuid,
    entity: String,
    kind: String,
    #[serde(default)]
    payload: Value,
    /// Lag compensation: the actor's tick and where it saw the target then.
    #[serde(default)]
    tick: Option<u64>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    tolerance: Option<f64>,
}

impl InteractReq {
    fn spec(self) -> (Uuid, String, InteractSpec) {
        let at = match (self.x, self.y) {
            (Some(x), Some(y)) => Some(Pos { x, y }),
            _ => None,
        };
        (
            self.sim,
            self.entity,
            InteractSpec {
                kind: self.kind,
                payload: self.payload,
                at_tick: self.tick,
                at,
                tolerance: self.tolerance,
            },
        )
    }
}

#[derive(Deserialize)]
struct CoLocateReq {
    sim: Uuid,
    /// The entity whose cell the partner joins (the caller must own that cell).
    entity: String,
    /// The partner to bring over.
    with: String,
    #[serde(default = "default_colocate_ttl")]
    ttl_ticks: u64,
}

fn default_colocate_ttl() -> u64 {
    300
}

#[derive(Deserialize)]
struct RewindReq {
    entity: String,
    tick: u64,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    tolerance: Option<f64>,
}
#[derive(Deserialize)]
struct ApplyReq {
    sim: Uuid,
    entity: String,
    x: Option<f64>,
    y: Option<f64>,
    #[serde(default)]
    components: Value,
}

async fn list(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "worlds": s.worlds.list() })))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateReq>,
) -> Result<Json<Value>, ApiError> {
    let info = s.worlds.create(&r.name, r.config, r.controller).await?;
    s.events
        .emit(
            "world.created",
            None,
            json!({ "name": r.name, "node_id": s.worlds.node_id() }),
        )
        .await;
    Ok(Json(serde_json::to_value(info).map_err(Error::internal)?))
}

async fn info(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
) -> Result<Json<Value>, Response> {
    ensure_home(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        Ok(Json(
            serde_json::to_value(s.worlds.info(&w)?).map_err(Error::internal)?,
        ))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn delete(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
) -> Result<Json<Value>, Response> {
    ensure_home(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        s.worlds.delete(&w).await?;
        s.events
            .emit("world.deleted", None, serde_json::json!({ "name": w }))
            .await;
        Ok(Json(json!({ "ok": true })))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn register_sim(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<RegisterSimReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        Ok(Json(
            json!({ "sims": s.worlds.register_sim(&w, r.sim, r.capacity)? }),
        ))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn unregister_sim(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SimQuery>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        s.worlds.unregister_sim(&w, r.sim)?;
        Ok(Json(json!({ "ok": true })))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn lease(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<LeaseReq>,
) -> Result<Json<Value>, Response> {
    ensure_cells_home(&s, &w, &r.cells, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        Ok(Json(
            json!({ "cells": s.worlds.lease(&w, r.sim, &r.cells)? }),
        ))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn renew(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SimQuery>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        Ok(Json(
            json!({ "cells": s.worlds.renew_at(&w, r.sim, r.tick)? }),
        ))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn spawn(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SpawnReq>,
) -> Result<Response, Response> {
    match s.worlds.cell_of(&w, Pos { x: r.x, y: r.y }) {
        Ok(c) => ensure_cell_home(&s, &w, c, &uri).await?,
        Err(_) => ensure_home(&s, &w, &uri).await?,
    }
    let r: Result<Response, ApiError> = async {
        let out = s.worlds.spawn_or_queue(
            &w,
            r.sim,
            SpawnSpec {
                pos: Pos { x: r.x, y: r.y },
                components: r.components,
                persistent: r.persistent,
                decay_ticks: r.decay_ticks,
                queue: r.queue,
            },
        )?;
        Ok(match out {
            SpawnOutcome::Spawned(e) => {
                Json(serde_json::to_value(e).map_err(Error::internal)?).into_response()
            }
            SpawnOutcome::Queued { .. } => {
                (StatusCode::ACCEPTED, Json(queued_json(out))).into_response()
            }
        })
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

fn queued_json(out: SpawnOutcome) -> Value {
    match out {
        SpawnOutcome::Queued {
            ticket,
            cell,
            position,
            eta_ticks,
        } => json!({"queued": true, "ticket": ticket, "cell": cell, "position": position,
                    "eta_ticks": eta_ticks}),
        SpawnOutcome::Spawned(_) => Value::Null,
    }
}

async fn despawn(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<EntityReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        s.worlds.despawn(&w, r.sim, &r.entity)?;
        Ok(Json(json!({ "ok": true })))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn apply(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<ApplyReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        let pos = match (r.x, r.y) {
            (Some(x), Some(y)) => Some(Pos { x, y }),
            (None, None) => None,
            _ => return Err(Error::InvalidArgument("x and y go together".into()).into()),
        };
        s.worlds.apply(&w, r.sim, &r.entity, pos, r.components)?;
        Ok(Json(json!({ "ok": true })))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn view(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path((w, x, y)): Path<(String, i32, i32)>,
) -> Result<Json<Value>, Response> {
    ensure_cell_home(&s, &w, CellId { x, y }, &uri).await?;
    let r: Result<Json<Value>, ApiError> =
        async { Ok(Json(s.worlds.view(&w, CellId { x, y })?)) }.await;
    r.map_err(IntoResponse::into_response)
}

async fn events(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Query(q): Query<SimQuery>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> =
        async { Ok(Json(json!({ "events": s.worlds.events(&w, q.sim)? }))) }.await;
    r.map_err(IntoResponse::into_response)
}

async fn handover_ack(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<EntityReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> =
        async { Ok(Json(s.worlds.handover_ack(&w, r.sim, &r.entity)?)) }.await;
    r.map_err(IntoResponse::into_response)
}

async fn interact(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<InteractReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let (sim, entity, spec) = r.spec();
    let r: Result<Json<Value>, ApiError> =
        async { Ok(Json(s.worlds.interact(&w, sim, &entity, spec, &s.fanout)?)) }.await;
    r.map_err(IntoResponse::into_response)
}

/// Co-locate: pin `with` into `entity`'s cell for `ttl_ticks` so one simulator runs both (docking,
/// grappling); the regular handover moves it and brings it back when the pin lapses.
async fn colocate(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<CoLocateReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> = async {
        Ok(Json(s.worlds.co_locate(
            &w,
            r.sim,
            &r.entity,
            &r.with,
            r.ttl_ticks,
        )?))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

/// Lag compensation: where an entity was at `tick`, and whether `(x, y)` was within `tolerance`
/// of it (`hit`); `pos` is null when history does not reach that far back.
async fn rewind(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<RewindReq>,
) -> Result<Json<Value>, Response> {
    ensure_shard(&s, &w, &uri).await?;
    let at = match (r.x, r.y) {
        (Some(x), Some(y)) => Some(Pos { x, y }),
        _ => None,
    };
    let r: Result<Json<Value>, ApiError> = async {
        Ok(Json(s.worlds.rewind(
            &w,
            &r.entity,
            r.tick,
            at,
            r.tolerance.unwrap_or(1.0),
        )?))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

/// `GET /v1/server/worlds/{w}/stream?sim=<uuid>` — the streaming simulator protocol. Instead of
/// polling `/view` and `/events` every tick, the simulator keeps one WebSocket per node that
/// hosts its cells: after every registry tick the node renews the simulator's lease (connected
/// means alive), and pushes one `tick` frame with the view of every leased cell and the pending
/// events. Commands go the other way as JSON objects with an `op` (`lease`, `spawn`, `despawn`,
/// `apply`, `interact`, `handover_ack`) and the same fields as the matching HTTP body (minus `sim`); the reply
/// echoes `op` and an optional `id`. A cell hosted by another node answers `lease`/`spawn` with
/// `redirect` (that node's URL) exactly like the HTTP endpoints.
async fn stream(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Query(q): Query<SimQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, Response> {
    ensure_shard(&s, &w, &uri).await?;
    Ok(ws.on_upgrade(move |socket| serve_stream(s, w, q.sim, uri, socket)))
}

async fn serve_stream(s: AppState, w: String, sim: Uuid, uri: OriginalUri, mut socket: WebSocket) {
    let mut ticks = s.worlds.subscribe_ticks();
    loop {
        tokio::select! {
            t = ticks.recv() => {
                let t = match t {
                    Ok(t) => t,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                let frame = match tick_frame(&s, &w, sim, t) {
                    Ok(f) => f,
                    Err(e) => json!({ "type": "error", "tick": t, "error": e.to_string() }),
                };
                if socket.send(Message::Text(frame.to_string().into())).await.is_err() {
                    break;
                }
            }
            m = socket.recv() => {
                match m {
                    Some(Ok(Message::Text(txt))) => {
                        let reply = stream_command(&s, &w, sim, &uri, txt.as_str()).await;
                        if socket.send(Message::Text(reply.to_string().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_))) => {}
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                }
            }
        }
    }
}

fn tick_frame(s: &AppState, w: &str, sim: Uuid, tick: u64) -> Result<Value, Error> {
    let cells = s.worlds.renew(w, sim)?;
    let mut views = Vec::with_capacity(cells.len());
    for c in &cells {
        let mut v = s.worlds.view(w, *c)?;
        if let Some(o) = v.as_object_mut() {
            o.insert("cell".into(), json!(c));
            let scale = s.worlds.time_scale(w, *c)?;
            if scale < 1.0 {
                o.insert("time_scale".into(), json!(scale));
            }
        }
        views.push(v);
    }
    let events = s.worlds.events(w, sim)?;
    Ok(json!({ "type": "tick", "tick": tick, "cells": views, "events": events }))
}

/// Where the HTTP handler would redirect, the stream answers with the target URL.
fn redirected(mut reply: Value, r: Result<(), Response>) -> Result<Value, Value> {
    match r {
        Ok(()) => Ok(reply),
        Err(resp) => {
            match resp
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
            {
                Some(loc) => reply["redirect"] = json!(loc),
                None => reply["error"] = json!(resp.status().to_string()),
            }
            Err(reply)
        }
    }
}

async fn stream_command(s: &AppState, w: &str, sim: Uuid, uri: &OriginalUri, txt: &str) -> Value {
    let mut cmd: Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(e) => return json!({ "error": format!("bad command: {e}") }),
    };
    let op = cmd["op"].as_str().unwrap_or("").to_string();
    let mut reply = json!({ "op": op });
    if let Some(id) = cmd.get("id").cloned() {
        reply["id"] = id;
    }
    cmd["sim"] = json!(sim);
    let r: Result<Value, Error> = async {
        match op.as_str() {
            "lease" => {
                let r: LeaseReq = serde_json::from_value(cmd).map_err(bad)?;
                let mut reply =
                    match redirected(reply, ensure_cells_home(s, w, &r.cells, uri).await) {
                        Ok(r) => r,
                        Err(r) => return Ok(r),
                    };
                reply["cells"] = json!(s.worlds.lease(w, sim, &r.cells)?);
                Ok(reply)
            }
            "spawn" => {
                let r: SpawnReq = serde_json::from_value(cmd).map_err(bad)?;
                let pos = Pos { x: r.x, y: r.y };
                let at_home = match s.worlds.cell_of(w, pos) {
                    Ok(c) => ensure_cell_home(s, w, c, uri).await,
                    Err(_) => ensure_home(s, w, uri).await,
                };
                let mut reply = match redirected(reply, at_home) {
                    Ok(r) => r,
                    Err(r) => return Ok(r),
                };
                match s.worlds.spawn_or_queue(
                    w,
                    sim,
                    SpawnSpec {
                        pos,
                        components: r.components,
                        persistent: r.persistent,
                        decay_ticks: r.decay_ticks,
                        queue: r.queue,
                    },
                )? {
                    SpawnOutcome::Spawned(e) => {
                        reply["entity"] = serde_json::to_value(e).map_err(Error::internal)?;
                    }
                    out => reply["queued"] = queued_json(out),
                }
                Ok(reply)
            }
            "tick_done" => {
                let r: TickDoneReq = serde_json::from_value(cmd).map_err(bad)?;
                reply["cells"] = json!(s.worlds.renew_at(w, sim, Some(r.tick))?);
                Ok(reply)
            }
            "despawn" => {
                let r: EntityReq = serde_json::from_value(cmd).map_err(bad)?;
                s.worlds.despawn(w, sim, &r.entity)?;
                reply["ok"] = json!(true);
                Ok(reply)
            }
            "apply" => {
                let r: ApplyReq = serde_json::from_value(cmd).map_err(bad)?;
                let pos = match (r.x, r.y) {
                    (Some(x), Some(y)) => Some(Pos { x, y }),
                    (None, None) => None,
                    _ => return Err(Error::InvalidArgument("x and y go together".into())),
                };
                s.worlds.apply(w, sim, &r.entity, pos, r.components)?;
                reply["ok"] = json!(true);
                Ok(reply)
            }
            "interact" => {
                let r: InteractReq = serde_json::from_value(cmd).map_err(bad)?;
                let (_, entity, spec) = r.spec();
                reply["result"] = s.worlds.interact(w, sim, &entity, spec, &s.fanout)?;
                Ok(reply)
            }
            "colocate" => {
                let r: CoLocateReq = serde_json::from_value(cmd).map_err(bad)?;
                reply["result"] = s
                    .worlds
                    .co_locate(w, sim, &r.entity, &r.with, r.ttl_ticks)?;
                Ok(reply)
            }
            "rewind" => {
                let r: RewindReq = serde_json::from_value(cmd).map_err(bad)?;
                let at = match (r.x, r.y) {
                    (Some(x), Some(y)) => Some(Pos { x, y }),
                    _ => None,
                };
                reply["result"] =
                    s.worlds
                        .rewind(w, &r.entity, r.tick, at, r.tolerance.unwrap_or(1.0))?;
                Ok(reply)
            }
            "handover_ack" => {
                let r: EntityReq = serde_json::from_value(cmd).map_err(bad)?;
                reply["result"] = s.worlds.handover_ack(w, sim, &r.entity)?;
                Ok(reply)
            }
            other => Err(Error::InvalidArgument(format!("unknown op `{other}`"))),
        }
    }
    .await;
    match r {
        Ok(v) => v,
        Err(e) => {
            let mut reply = json!({ "op": op, "error": e.to_string() });
            if let Some(id) = serde_json::from_str::<Value>(txt)
                .ok()
                .and_then(|v| v.get("id").cloned())
            {
                reply["id"] = id;
            }
            reply
        }
    }
}

fn bad(e: serde_json::Error) -> Error {
    Error::InvalidArgument(e.to_string())
}
