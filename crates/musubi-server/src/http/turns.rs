//! Asynchronous turn-based match API; see `crate::turns`.

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};
use crate::turns::{CreateMatch, Finish, GetQuery, JoinMatch, ListQuery, MakeMove, TurnMatch};
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/turns", get(list_open).post(create))
        .route("/v1/turns/join", post(join))
        .route("/v1/turns/mine", get(mine))
        .route("/v1/turns/{id}", get(get_one))
        .route("/v1/turns/{id}/start", post(start))
        .route("/v1/turns/{id}/leave", post(leave))
        .route("/v1/turns/{id}/move", post(make_move))
        .route("/v1/turns/{id}/resign", post(resign))
        .route("/v1/turns/{id}/rematch", post(rematch))
        .route("/v1/server/turns", get(server_list))
        .route(
            "/v1/server/turns/{id}",
            get(server_get).delete(server_delete),
        )
        .route("/v1/server/turns/{id}/finish", post(server_finish))
        .route("/v1/server/turns/sweep", post(server_sweep))
}

async fn username(s: &AppState, me: &musubi_auth::Authenticated) -> Result<String, ApiError> {
    match &me.username {
        Some(u) => Ok(u.clone()),
        None => Ok(s.accounts.get(me.account_id).await?.username),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OpenQuery {
    game: Option<String>,
    limit: Option<i64>,
}

async fn list_open(
    Bearer(_me): Bearer,
    State(s): State<AppState>,
    Query(q): Query<OpenQuery>,
) -> Result<Json<Value>, ApiError> {
    let matches = s
        .turns
        .list_open(q.game.as_deref(), q.limit.unwrap_or(50))
        .await?;
    Ok(Json(json!({ "matches": matches })))
}

async fn create(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(req): Json<CreateMatch>,
) -> Result<Json<TurnMatch>, ApiError> {
    let name = username(&s, &me).await?;
    Ok(Json(s.turns.create(me.account_id, &name, req).await?))
}

async fn join(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(req): Json<JoinMatch>,
) -> Result<Json<TurnMatch>, ApiError> {
    let name = username(&s, &me).await?;
    Ok(Json(s.turns.join(me.account_id, &name, req).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MineQuery {
    state: Option<String>,
    limit: Option<i64>,
}

async fn mine(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Query(q): Query<MineQuery>,
) -> Result<Json<Value>, ApiError> {
    let matches = s
        .turns
        .mine(me.account_id, q.state.as_deref(), q.limit.unwrap_or(50))
        .await?;
    Ok(Json(json!({ "matches": matches })))
}

async fn get_one(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<GetQuery>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.get(id, Some(me.account_id), &q).await?))
}

async fn start(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.start(me.account_id, id).await?))
}

async fn leave(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.leave(me.account_id, id).await?))
}

async fn make_move(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<MakeMove>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.make_move(me.account_id, id, req).await?))
}

async fn resign(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.resign(me.account_id, id).await?))
}

async fn rematch(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<TurnMatch>, ApiError> {
    let name = username(&s, &me).await?;
    Ok(Json(s.turns.rematch(me.account_id, &name, id).await?))
}

async fn server_list(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "matches": s.turns.list(&q).await? })))
}

async fn server_get(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<GetQuery>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.get(id, None, &q).await?))
}

async fn server_finish(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(f): Json<Finish>,
) -> Result<Json<TurnMatch>, ApiError> {
    Ok(Json(s.turns.force_finish(id, f).await?))
}

async fn server_delete(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.turns.delete(id).await?;
    Ok(Json(json!({ "deleted": true })))
}

/// Run the deadline sweep now (tests, ops).
async fn server_sweep(
    ServerKey: ServerKey,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let (timed_out, cancelled) = s.turns.sweep().await?;
    Ok(Json(
        json!({ "timed_out": timed_out, "cancelled": cancelled }),
    ))
}
