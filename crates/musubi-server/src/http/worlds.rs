//! Server meshing: simulator-facing API (server key). Simulators lease cells, spawn/apply
//! authoritative state, read their view (owned + ghosts), drain events (handover requests,
//! routed inputs) and ack handovers.

// Handlers return a ready `Response` (redirect) on the error path; axum's Response is large.
#![allow(clippy::result_large_err)]

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
        .route("/v1/server/worlds/{w}/handover/ack", post(handover_ack))
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
struct SimQuery {
    sim: Uuid,
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
}

#[derive(Deserialize)]
struct EntityReq {
    sim: Uuid,
    entity: String,
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
    ensure_home(&s, &w, &uri).await?;
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
    ensure_home(&s, &w, &uri).await?;
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
    ensure_home(&s, &w, &uri).await?;
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
    ensure_home(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> =
        async { Ok(Json(json!({ "cells": s.worlds.renew(&w, r.sim)? }))) }.await;
    r.map_err(IntoResponse::into_response)
}

async fn spawn(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SpawnReq>,
) -> Result<Json<Value>, Response> {
    match s.worlds.cell_of(&w, Pos { x: r.x, y: r.y }) {
        Ok(c) => ensure_cell_home(&s, &w, c, &uri).await?,
        Err(_) => ensure_home(&s, &w, &uri).await?,
    }
    let r: Result<Json<Value>, ApiError> = async {
        let e = s.worlds.spawn(
            &w,
            r.sim,
            Pos { x: r.x, y: r.y },
            r.components,
            r.persistent,
        )?;
        Ok(Json(serde_json::to_value(e).map_err(Error::internal)?))
    }
    .await;
    r.map_err(IntoResponse::into_response)
}

async fn despawn(
    State(s): State<AppState>,
    uri: OriginalUri,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<EntityReq>,
) -> Result<Json<Value>, Response> {
    ensure_home(&s, &w, &uri).await?;
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
    ensure_home(&s, &w, &uri).await?;
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
    ensure_home(&s, &w, &uri).await?;
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
    ensure_home(&s, &w, &uri).await?;
    let r: Result<Json<Value>, ApiError> =
        async { Ok(Json(s.worlds.handover_ack(&w, r.sim, &r.entity)?)) }.await;
    r.map_err(IntoResponse::into_response)
}
