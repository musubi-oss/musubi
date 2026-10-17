//! Dedicated-server orchestration: control surface (server key) and the game-server SDK surface
//! (server key + `X-Musubi-Server-Id`, Agones-like: ready / health / allocate / shutdown / label / connect).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use musubi_fleet::{Allocation, CreateFleet, DemandSlot, Fleet, GameServer, ScalePolicy};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/fleets", get(list).post(create))
        .route("/v1/server/fleets/{id}", get(get_one).delete(delete))
        .route("/v1/server/fleets/{id}/scale", post(scale))
        .route("/v1/server/fleets/{id}/allocate", post(allocate))
        .route("/v1/server/fleets/{id}/servers", get(servers))
        .route("/v1/server/fleets/{id}/demand", get(demand))
        .route(
            "/v1/server/gameservers/{id}",
            get(server_get).delete(server_delete),
        )
        .route("/v1/gameserver", get(sdk_get))
        .route("/v1/gameserver/ready", post(sdk_ready))
        .route("/v1/gameserver/health", post(sdk_health))
        .route("/v1/gameserver/allocate", post(sdk_allocate))
        .route("/v1/gameserver/shutdown", post(sdk_shutdown))
        .route("/v1/gameserver/label", post(sdk_label))
        .route("/v1/gameserver/connect", post(sdk_connect))
}

fn parse(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad id".into()).into())
}

fn self_id(headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let v = headers
        .get("x-musubi-server-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Id".into()))?;
    parse(v)
}

async fn list(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "fleets": s.fleets.list().await?, "providers": s.fleets.providers() }),
    ))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<CreateFleet>,
) -> Result<Json<Fleet>, ApiError> {
    Ok(Json(s.fleets.create(c).await?))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Fleet>, ApiError> {
    Ok(Json(s.fleets.get(parse(&id)?).await?))
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.delete(parse(&id)?).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScaleReq {
    buffer: Option<i32>,
    max_servers: Option<i32>,
    min_servers: Option<i32>,
    policy: Option<ScalePolicy>,
}

async fn scale(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<ScaleReq>,
) -> Result<Json<Fleet>, ApiError> {
    Ok(Json(
        s.fleets
            .scale(
                parse(&id)?,
                r.buffer,
                r.max_servers,
                r.min_servers,
                r.policy,
            )
            .await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AllocateReq {
    payload: Value,
}

async fn allocate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<AllocateReq>,
) -> Result<Json<Allocation>, ApiError> {
    Ok(Json(s.fleets.allocate(parse(&id)?, r.payload).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServersQuery {
    state: Option<String>,
}

async fn servers(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<ServersQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "servers": s.fleets.servers(parse(&id)?, q.state.as_deref()).await? }),
    ))
}

/// Demand history (hour-of-week EWMA of allocations) and what the autoscaler wants right now.
async fn demand(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let f = s.fleets.get(id).await?;
    let now = time::OffsetDateTime::now_utc();
    let slots: Vec<DemandSlot> = s.fleets.demand(id).await?;
    let per_hour = s.fleets.forecast_per_hour(id, now).await?;
    let buffer = f.policy.buffer_at(now, f.buffer);
    Ok(Json(json!({
        "slot": musubi_fleet::demand_slot(now),
        "per_hour": per_hour,
        "buffer": buffer,
        "target_ready": musubi_fleet::target_ready(&f.policy, buffer, per_hour),
        "slots": slots,
    })))
}

async fn server_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.server(parse(&id)?).await?))
}

async fn server_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.sdk_shutdown(parse(&id)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn sdk_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.server(self_id(&headers)?).await?))
}

async fn sdk_ready(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.sdk_ready(self_id(&headers)?).await?))
}

async fn sdk_health(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    s.fleets.sdk_health(self_id(&headers)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn sdk_allocate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.sdk_allocate(self_id(&headers)?).await?))
}

async fn sdk_shutdown(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    s.fleets.sdk_shutdown(self_id(&headers)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn sdk_label(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(labels): Json<Value>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.sdk_labels(self_id(&headers)?, labels).await?))
}

#[derive(Deserialize)]
struct ConnectReq {
    token: String,
}

async fn sdk_connect(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(r): Json<ConnectReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.fleets.sdk_connect(self_id(&headers)?, &r.token).await?,
    ))
}
