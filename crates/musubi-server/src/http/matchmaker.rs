//! Server-key API for matchmaker queues: inspect and change queue rules at runtime and read what
//! is waiting. Queues from `[[matchmaker.queues]]` are loaded at boot; changes made here live on
//! this node (the matchmaker leader forms the matches, so apply them where the leader runs or
//! put them in the config of every node).

use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use musubi_matchmaker::QueueConfig;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/server/matchmaker/queues",
            get(list_queues).put(put_queue),
        )
        .route("/v1/server/matchmaker/queues/{name}", delete(delete_queue))
        .route("/v1/server/matchmaker/stats", get(stats))
}

async fn list_queues(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "queues": s.matchmaker.queues() })))
}

async fn put_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(q): Json<QueueConfig>,
) -> Result<Json<Value>, ApiError> {
    s.matchmaker.set_queue(q.clone())?;
    Ok(Json(json!({ "queue": q })))
}

async fn delete_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !s.matchmaker.remove_queue(&name) {
        return Err(musubi_core::Error::NotFound(format!("queue {name} does not exist")).into());
    }
    Ok(Json(json!({ "removed": name })))
}

async fn stats(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "waiting": s.matchmaker.waiting(),
        "open_matches": s.matchmaker.open_count(),
        "queues": s.matchmaker.queues(),
    })))
}
