use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_leaderboard::{CreateLeaderboard, Leaderboard, Record, RecordList, WriteRecord};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/leaderboards", get(list))
        .route("/v1/leaderboards/{id}", get(get_board))
        .route(
            "/v1/leaderboards/{id}/records",
            get(records).post(write).delete(delete_own),
        )
        .route("/v1/leaderboards/{id}/around/{owner}", get(around))
        // server-to-server
        .route("/v1/server/leaderboards", post(create))
        .route("/v1/server/leaderboards/{id}", delete(delete_board))
        .route(
            "/v1/server/leaderboards/{id}/records/{owner}",
            post(write_as).delete(delete_as),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RecordsQuery {
    #[serde(default)]
    owner_ids: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    expiry: Option<i64>,
}

fn default_limit() -> usize {
    50
}

fn parse_ids(s: Option<&str>) -> Result<Vec<AccountId>, ApiError> {
    s.unwrap_or("")
        .split(',')
        .filter(|x| !x.is_empty())
        .map(|x| {
            x.parse::<AccountId>()
                .map_err(|_| musubi_core::Error::InvalidArgument(format!("bad id {x}")).into())
        })
        .collect()
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (leaderboards, cursor) = s.leaderboards.list(q.limit, q.cursor.as_deref()).await?;
    Ok(Json(
        json!({ "leaderboards": leaderboards, "cursor": cursor }),
    ))
}

async fn get_board(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Leaderboard>, ApiError> {
    Ok(Json(s.leaderboards.get(&id).await?))
}

async fn records(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    let owners = parse_ids(q.owner_ids.as_deref())?;
    Ok(Json(
        s.leaderboards
            .records(&id, &owners, q.limit, q.cursor.as_deref(), q.expiry)
            .await?,
    ))
}

async fn around(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path((id, owner)): Path<(String, AccountId)>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    Ok(Json(
        s.leaderboards.around(&id, owner, q.limit, q.expiry).await?,
    ))
}

async fn write(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let username = s
        .accounts
        .get_users(&[me.account_id], &[])
        .await?
        .pop()
        .map(|u| u.username);
    Ok(Json(
        s.leaderboards
            .write(&id, me.account_id, username.as_deref(), w, false)
            .await?,
    ))
}

async fn delete_own(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.leaderboards
        .delete_record(&id, me.account_id, false)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<CreateLeaderboard>,
) -> Result<Json<Leaderboard>, ApiError> {
    Ok(Json(s.leaderboards.create(c).await?))
}

async fn delete_board(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.leaderboards.delete(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn write_as(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, owner)): Path<(String, AccountId)>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let username = s
        .accounts
        .get_users(&[owner], &[])
        .await?
        .pop()
        .map(|u| u.username);
    Ok(Json(
        s.leaderboards
            .write(&id, owner, username.as_deref(), w, true)
            .await?,
    ))
}

async fn delete_as(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, owner)): Path<(String, AccountId)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.leaderboards.delete_record(&id, owner, true).await?;
    Ok(Json(json!({ "ok": true })))
}
