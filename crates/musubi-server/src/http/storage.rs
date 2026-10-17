use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_storage::{Ack, Caller, DeleteOp, Object, PatchOp, ReadRef, WriteOp};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/storage/read", post(read))
        .route("/v1/storage/write", post(write))
        .route("/v1/storage/delete", post(delete))
        .route("/v1/storage/patch", post(patch))
        .route("/v1/storage/{collection}", get(list))
        .route("/v1/storage/{collection}/count", get(count))
}

#[derive(Deserialize)]
struct ReadReq {
    objects: Vec<ReadRef>,
}

#[derive(Deserialize)]
struct WriteReq {
    objects: Vec<WriteOp>,
}

#[derive(Deserialize)]
struct DeleteReq {
    objects: Vec<DeleteOp>,
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    owner_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    cursor: Option<String>,
    /// JSON object; only objects whose value contains it are listed (`value @> where`).
    #[serde(default, rename = "where")]
    filter: Option<String>,
    /// Only keys starting with this prefix (literal match, `%`/`_` are escaped).
    #[serde(default)]
    prefix: Option<String>,
    /// Field of the value to order by (`score`, `stats.wins`), `-field` for descending.
    #[serde(default)]
    sort: Option<String>,
}

#[derive(Deserialize)]
struct PatchReq {
    objects: Vec<PatchOp>,
}

async fn patch(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<PatchReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let acks: Vec<Ack> = s
        .storage
        .patch(Caller::User(me.account_id), None, &r.objects)
        .await?;
    Ok(Json(json!({ "acks": acks })))
}

async fn count(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(collection): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let filter = q
        .filter
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .map_err(|e| musubi_core::Error::InvalidArgument(format!("where: {e}")))?;
    let n = s
        .storage
        .count(
            Caller::User(me.account_id),
            &collection,
            q.owner_id,
            musubi_storage::ListFilter {
                cursor: None,
                filter: filter.as_ref(),
                key_prefix: q.prefix.as_deref(),
                sort: None,
            },
        )
        .await?;
    Ok(Json(json!({ "count": n })))
}

fn default_limit() -> i64 {
    20
}

#[derive(Serialize)]
struct ListResp {
    objects: Vec<Object>,
    cursor: Option<String>,
}

async fn read(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<ReadReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let objects = s
        .storage
        .read(Caller::User(me.account_id), &r.objects)
        .await?;
    Ok(Json(json!({ "objects": objects })))
}

async fn write(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<WriteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let acks: Vec<Ack> = s
        .storage
        .write(Caller::User(me.account_id), None, &r.objects)
        .await?;
    Ok(Json(json!({ "acks": acks })))
}

async fn delete(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<DeleteReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.storage
        .delete(Caller::User(me.account_id), None, &r.objects)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(collection): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<Json<ListResp>, ApiError> {
    let filter = q
        .filter
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .map_err(|e| musubi_core::Error::InvalidArgument(format!("where: {e}")))?;
    let (objects, cursor) = s
        .storage
        .list_where(
            Caller::User(me.account_id),
            &collection,
            q.owner_id,
            q.limit,
            musubi_storage::ListFilter {
                cursor: q.cursor.as_deref(),
                filter: filter.as_ref(),
                key_prefix: q.prefix.as_deref(),
                sort: q.sort.as_deref(),
            },
        )
        .await?;
    Ok(Json(ListResp { objects, cursor }))
}
