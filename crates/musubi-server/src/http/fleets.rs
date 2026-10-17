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
        .route("/v1/server/fleets/{id}/builds", get(builds).post(add_build))
        .route(
            "/v1/server/fleets/{id}/builds/{version}",
            axum::routing::delete(retire_build),
        )
        .route(
            "/v1/server/fleets/{id}/builds/{version}/promote",
            post(promote_build),
        )
        .route("/v1/server/fleets/{id}/usage", get(usage))
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
        .route("/v1/gameserver/players", post(sdk_players))
        .route("/v1/gameserver/watch", get(sdk_watch))
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
    /// Prefer a live server with free player slots (backfill) over a fresh one.
    backfill: bool,
}

async fn allocate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<AllocateReq>,
) -> Result<Json<Allocation>, ApiError> {
    Ok(Json(
        s.fleets
            .allocate(parse(&id)?, r.payload, r.backfill)
            .await?,
    ))
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

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlayersReq {
    players: i32,
    capacity: Option<i32>,
}

/// `POST /v1/gameserver/players {players, capacity?}` — occupancy for backfill allocations.
async fn sdk_players(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(r): Json<PlayersReq>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(
        s.fleets
            .sdk_players(self_id(&headers)?, r.players, r.capacity)
            .await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WatchQuery {
    /// Last `version` the caller saw; the call returns as soon as the server changed past it.
    since: Option<i64>,
    /// Long-poll budget, 1..=60 s (default 30).
    timeout_secs: Option<u64>,
}

/// `GET /v1/gameserver/watch?since=&timeout_secs=` — WatchGameServer: returns the server record
/// when it changes (state, labels, allocation, players) or when the timeout passes.
async fn sdk_watch(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Query(q): Query<WatchQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = self_id(&headers)?;
    let budget = std::time::Duration::from_secs(q.timeout_secs.unwrap_or(30).clamp(1, 60));
    let gs = s.fleets.watch(id, q.since, budget).await?;
    let changed = q.since.is_none_or(|v| gs.version > v);
    Ok(Json(json!({ "changed": changed, "server": gs })))
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

async fn builds(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    s.fleets.get(id).await?;
    let builds: Vec<musubi_fleet::FleetBuild> = s.fleets.builds(id).await?;
    Ok(Json(json!({ "builds": builds })))
}

async fn add_build(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(c): Json<musubi_fleet::CreateBuild>,
) -> Result<Json<musubi_fleet::FleetBuild>, ApiError> {
    let id = parse(&id)?;
    let b = s.fleets.add_build(id, c).await?;
    s.events
        .emit(
            "fleet.build_added",
            None,
            json!({ "fleet_id": id, "version": b.version, "state": b.state }),
        )
        .await;
    Ok(Json(b))
}

async fn promote_build(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, version)): Path<(String, String)>,
) -> Result<Json<musubi_fleet::FleetBuild>, ApiError> {
    let id = parse(&id)?;
    let b = s.fleets.promote_build(id, &version).await?;
    s.events
        .emit(
            "fleet.build_promoted",
            None,
            json!({ "fleet_id": id, "version": b.version }),
        )
        .await;
    Ok(Json(b))
}

async fn retire_build(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, version)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    s.fleets.retire_build(id, &version).await?;
    Ok(Json(json!({ "retired": version })))
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct UsageQuery {
    /// RFC 3339; default = 30 days ago.
    from: Option<String>,
    /// RFC 3339; default = now.
    to: Option<String>,
}

fn parse_time(
    s: Option<&str>,
    default: time::OffsetDateTime,
) -> Result<time::OffsetDateTime, ApiError> {
    match s {
        None => Ok(default),
        Some(s) => time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .map_err(|_| {
                musubi_core::Error::InvalidArgument("from/to must be RFC 3339".into()).into()
            }),
    }
}

async fn usage(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<UsageQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let now = time::OffsetDateTime::now_utc();
    let to = parse_time(q.to.as_deref(), now)?;
    let from = parse_time(q.from.as_deref(), now - time::Duration::days(30))?;
    if from >= to {
        return Err(musubi_core::Error::InvalidArgument("from must be before to".into()).into());
    }
    let rows: Vec<musubi_fleet::UsageRow> = s.fleets.usage(id, from, to).await?;
    let total: f64 = rows.iter().map(|r| r.vcpu_hours).sum();
    Ok(Json(
        json!({ "from": from.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(), "to": to.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(), "rows": rows, "vcpu_hours": total }),
    ))
}
