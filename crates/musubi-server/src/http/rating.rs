//! Skill rating (plan 5.6): queues of Elo / Glicko-2 / OpenSkill, match results, seasons, decay.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_leaderboard::{MatchResult, Rating, RatingQueue, ResultApplied, UpsertRatingQueue};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/rating/{queue}", get(mine))
        .route("/v1/rating/{queue}/top", get(top))
        .route("/v1/rating/{queue}/{account}", get(of))
        // server-to-server
        .route(
            "/v1/server/rating/queues",
            get(list_queues).put(upsert_queue),
        )
        .route(
            "/v1/server/rating/queues/{id}",
            get(get_queue).delete(delete_queue),
        )
        .route("/v1/server/rating/queues/{id}/results", post(results))
        .route("/v1/server/rating/queues/{id}/season", post(season))
        .route("/v1/server/rating/queues/{id}/decay", post(decay))
}

#[derive(Deserialize)]
struct TopQuery {
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

async fn mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(queue): Path<String>,
) -> Result<Json<Rating>, ApiError> {
    Ok(Json(s.ratings.get(&queue, me.account_id).await?))
}

async fn of(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path((queue, account)): Path<(String, AccountId)>,
) -> Result<Json<Rating>, ApiError> {
    Ok(Json(s.ratings.get(&queue, account).await?))
}

async fn top(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(queue): Path<String>,
    Query(q): Query<TopQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "ratings": s.ratings.top(&queue, q.limit).await? }),
    ))
}

async fn list_queues(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "queues": s.ratings.queues().await? })))
}

async fn upsert_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<UpsertRatingQueue>,
) -> Result<Json<RatingQueue>, ApiError> {
    Ok(Json(s.ratings.upsert_queue(c).await?))
}

async fn get_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<RatingQueue>, ApiError> {
    Ok(Json(s.ratings.queue(&id).await?))
}

async fn delete_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.ratings.delete_queue(&id).await?;
    Ok(Json(json!({ "deleted": id })))
}

async fn results(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<MatchResult>,
) -> Result<Json<ResultApplied>, ApiError> {
    Ok(Json(s.ratings.apply(&id, r).await?))
}

#[derive(Deserialize, Default)]
struct SeasonBody {
    #[serde(default)]
    carry: Option<f64>,
}

async fn season(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    body: Option<Json<SeasonBody>>,
) -> Result<Json<RatingQueue>, ApiError> {
    let carry = body.and_then(|b| b.0.carry);
    Ok(Json(s.ratings.new_season(&id, carry).await?))
}

async fn decay(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "decayed": s.ratings.decay(&id).await? })))
}
