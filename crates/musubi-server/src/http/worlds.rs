//! Server meshing: simulator-facing API (server key). Simulators lease cells, spawn/apply
//! authoritative state, read their view (owned + ghosts), drain events (handover requests,
//! routed inputs) and ack handovers.

use axum::extract::{Path, Query, State};
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
    Ok(Json(
        serde_json::to_value(s.worlds.create(&r.name, r.config, r.controller)?)
            .map_err(Error::internal)?,
    ))
}

async fn info(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.worlds.info(&w)?).map_err(Error::internal)?,
    ))
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.worlds.delete(&w)?;
    Ok(Json(json!({ "ok": true })))
}

async fn register_sim(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<RegisterSimReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "sims": s.worlds.register_sim(&w, r.sim, r.capacity)? }),
    ))
}

async fn unregister_sim(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SimQuery>,
) -> Result<Json<Value>, ApiError> {
    s.worlds.unregister_sim(&w, r.sim)?;
    Ok(Json(json!({ "ok": true })))
}

async fn lease(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<LeaseReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "cells": s.worlds.lease(&w, r.sim, &r.cells)? }),
    ))
}

async fn renew(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SimQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "cells": s.worlds.renew(&w, r.sim)? })))
}

async fn spawn(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<SpawnReq>,
) -> Result<Json<Value>, ApiError> {
    let e = s.worlds.spawn(
        &w,
        r.sim,
        Pos { x: r.x, y: r.y },
        r.components,
        r.persistent,
    )?;
    Ok(Json(serde_json::to_value(e).map_err(Error::internal)?))
}

async fn despawn(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<EntityReq>,
) -> Result<Json<Value>, ApiError> {
    s.worlds.despawn(&w, r.sim, &r.entity)?;
    Ok(Json(json!({ "ok": true })))
}

async fn apply(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<ApplyReq>,
) -> Result<Json<Value>, ApiError> {
    let pos = match (r.x, r.y) {
        (Some(x), Some(y)) => Some(Pos { x, y }),
        (None, None) => None,
        _ => return Err(Error::InvalidArgument("x and y go together".into()).into()),
    };
    s.worlds.apply(&w, r.sim, &r.entity, pos, r.components)?;
    Ok(Json(json!({ "ok": true })))
}

async fn view(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((w, x, y)): Path<(String, i32, i32)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.worlds.view(&w, CellId { x, y })?))
}

async fn events(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Query(q): Query<SimQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "events": s.worlds.events(&w, q.sim)? })))
}

async fn handover_ack(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(w): Path<String>,
    Json(r): Json<EntityReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.worlds.handover_ack(&w, r.sim, &r.entity)?))
}
