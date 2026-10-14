use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_storage::{Ack, Caller, DeleteOp, Object, ReadRef, WriteOp};
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
        .route("/v1/storage/{collection}", get(list))
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
    let (objects, cursor) = s
        .storage
        .list(
            Caller::User(me.account_id),
            &collection,
            q.owner_id,
            q.limit,
            q.cursor.as_deref(),
        )
        .await?;
    Ok(Json(ListResp { objects, cursor }))
}
