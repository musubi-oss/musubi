use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_leaderboard::{
    CreateTournament, Record, RecordList, Tournament, TournamentFilter, WriteRecord,
};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/tournaments", get(list))
        .route("/v1/tournaments/{id}", get(get_one))
        .route("/v1/tournaments/{id}/join", post(join))
        .route("/v1/tournaments/{id}/records", get(records).post(write))
        .route("/v1/tournaments/{id}/around/{owner}", get(around))
        .route("/v1/server/tournaments", post(create))
        .route("/v1/server/tournaments/{id}", delete(delete_one))
        .route(
            "/v1/server/tournaments/{id}/records/{owner}",
            post(write_as),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    category_start: Option<i32>,
    #[serde(default)]
    category_end: Option<i32>,
    #[serde(default)]
    start_time: Option<i64>,
    #[serde(default)]
    end_time: Option<i64>,
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

async fn username(s: &AppState, id: AccountId) -> Result<Option<String>, ApiError> {
    Ok(s.accounts
        .get_users(&[id], &[])
        .await?
        .pop()
        .map(|u| u.username))
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let f = TournamentFilter {
        category_start: q.category_start,
        category_end: q.category_end,
        start_time: q.start_time,
        end_time: q.end_time,
    };
    let (tournaments, cursor) = s.tournaments.list(f, q.limit, q.cursor.as_deref()).await?;
    Ok(Json(
        json!({ "tournaments": tournaments, "cursor": cursor }),
    ))
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Tournament>, ApiError> {
    Ok(Json(s.tournaments.get(&id).await?))
}

async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let u = username(&s, me.account_id).await?;
    s.tournaments.join(&id, me.account_id, u.as_deref()).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn records(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    let owners = parse_ids(q.owner_ids.as_deref())?;
    Ok(Json(
        s.tournaments
            .records(
                &s.leaderboards,
                &id,
                &owners,
                q.limit,
                q.cursor.as_deref(),
                q.expiry,
            )
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
        s.tournaments
            .around(&s.leaderboards, &id, owner, q.limit, q.expiry)
            .await?,
    ))
}

async fn write(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let u = username(&s, me.account_id).await?;
    Ok(Json(
        s.tournaments
            .write(&s.leaderboards, &id, me.account_id, u.as_deref(), w, false)
            .await?,
    ))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<CreateTournament>,
) -> Result<Json<Tournament>, ApiError> {
    Ok(Json(s.tournaments.create(c).await?))
}

async fn delete_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.tournaments.delete(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn write_as(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, owner)): Path<(String, AccountId)>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let u = username(&s, owner).await?;
    Ok(Json(
        s.tournaments
            .write(&s.leaderboards, &id, owner, u.as_deref(), w, true)
            .await?,
    ))
}
