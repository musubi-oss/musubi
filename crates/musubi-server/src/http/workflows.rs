//! Durable workflows: `POST /v1/server/workflows`, get/list, signal, cancel; `POST …/run`
//! forces a pass over due steps on this node (ops / tests).

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/workflows", get(list).post(start))
        .route("/v1/server/workflows/run", post(run))
        .route("/v1/server/workflows/{id}", get(get_one))
        .route("/v1/server/workflows/{id}/signal", post(signal))
        .route("/v1/server/workflows/{id}/cancel", post(cancel))
}

async fn start(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<crate::workflows::StartReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::workflows::start(&s, r).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    name: Option<String>,
    status: Option<String>,
    account_id: Option<Uuid>,
    limit: Option<i64>,
}

async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::workflows::list(
            &s,
            q.name.as_deref(),
            q.status.as_deref(),
            q.account_id,
            q.limit.unwrap_or(50),
        )
        .await?,
    ))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::workflows::get(&s, id).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SignalReq {
    payload: Value,
}

async fn signal(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Json(r): Json<SignalReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::workflows::signal(&s, id, r.payload).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CancelReq {
    reason: String,
}

async fn cancel(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    body: Option<Json<CancelReq>>,
) -> Result<Json<Value>, ApiError> {
    let reason = body.map(|b| b.0.reason).unwrap_or_default();
    Ok(Json(crate::workflows::cancel(&s, id, &reason).await?))
}

async fn run(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    let n = crate::workflows::run_due(&s, 256).await?;
    Ok(Json(json!({ "ran": n })))
}
