use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_actor::MatchInfo;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/matches", get(list))
        .route("/v1/server/matches", post(create))
        .route("/v1/server/matches/{id}", get(get_one).delete(terminate))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    min_size: Option<u32>,
    #[serde(default)]
    max_size: Option<u32>,
    #[serde(default = "default_limit")]
    limit: usize,
    /// Matchmaker-style query over the label, e.g. `+label.mode:ranked +label.open:1`.
    #[serde(default)]
    query: Option<String>,
}

fn default_limit() -> usize {
    50
}

#[derive(Deserialize)]
struct CreateReq {
    module: String,
    #[serde(default)]
    params: serde_json::Value,
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = q
        .query
        .as_deref()
        .filter(|q| !q.trim().is_empty() && q.trim() != "*")
        .map(musubi_matchmaker::Query::parse)
        .transpose()?;
    let matches: Vec<MatchInfo> = s
        .matches
        .list_query(
            q.label.as_deref(),
            query.as_ref(),
            q.min_size,
            q.max_size,
            q.limit.clamp(1, 100),
        )
        .await;
    Ok(Json(json!({ "matches": matches })))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateReq>,
) -> Result<Json<MatchInfo>, ApiError> {
    let h = s.matches.create(s.hub.clone(), &r.module, r.params)?;
    Ok(Json(h.info().await))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<MatchInfo>, ApiError> {
    let h = s
        .matches
        .get(&id)
        .ok_or_else(|| musubi_core::Error::NotFound("match".into()))?;
    Ok(Json(h.info().await))
}

async fn terminate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let h = s
        .matches
        .get(&id)
        .ok_or_else(|| musubi_core::Error::NotFound("match".into()))?;
    h.terminate(0);
    Ok(Json(json!({ "ok": true })))
}
