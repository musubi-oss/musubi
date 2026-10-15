use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_social::NotificationList;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/notifications", get(list).delete(delete))
        .route("/v1/notifications/read", post(mark_read))
        .route("/v1/server/notifications", post(server_send))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    /// Only unread notifications.
    #[serde(default)]
    unread: bool,
}

#[derive(Deserialize)]
struct ReadReq {
    #[serde(default)]
    ids: Vec<Uuid>,
    /// Mark everything unread as read (ignores `ids`).
    #[serde(default)]
    all: bool,
}

fn default_limit() -> usize {
    50
}

#[derive(Deserialize)]
struct Ids {
    ids: Vec<Uuid>,
}

async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<NotificationList>, ApiError> {
    Ok(Json(
        s.notify
            .service
            .list(me.account_id, q.limit, q.cursor.as_deref(), q.unread)
            .await?,
    ))
}

async fn delete(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let n = s.notify.service.delete(me.account_id, &b.ids).await?;
    Ok(Json(json!({ "deleted": n })))
}

async fn mark_read(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<ReadReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = (!b.all).then_some(b.ids.as_slice());
    let n = s.notify.service.mark_read(me.account_id, ids).await?;
    Ok(Json(json!({ "read": n })))
}

#[derive(Deserialize)]
struct ServerSendReq {
    user_id: Uuid,
    subject: String,
    #[serde(default)]
    content: serde_json::Value,
    #[serde(default)]
    code: i32,
    #[serde(default)]
    sender_id: Option<Uuid>,
    #[serde(default = "yes")]
    persistent: bool,
}

fn yes() -> bool {
    true
}

/// Game servers and tools send a notification to one account (live delivery + push when offline).
async fn server_send(
    State(s): State<AppState>,
    _k: super::ServerKey,
    Json(r): Json<ServerSendReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if r.subject.trim().is_empty() || r.subject.len() > 256 {
        return Err(
            musubi_core::Error::InvalidArgument("subject must be 1..=256 chars".into()).into(),
        );
    }
    s.notify
        .send(vec![musubi_social::notifications::NewNotification {
            user_id: musubi_core::AccountId(r.user_id),
            sender_id: r.sender_id.map(musubi_core::AccountId),
            subject: r.subject,
            content: r.content,
            code: r.code,
            persistent: r.persistent,
        }])
        .await?;
    Ok(Json(json!({ "ok": true })))
}
