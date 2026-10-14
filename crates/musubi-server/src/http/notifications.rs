use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_social::NotificationList;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/notifications", get(list).delete(delete))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
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
            .list(me.account_id, q.limit, q.cursor.as_deref())
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
