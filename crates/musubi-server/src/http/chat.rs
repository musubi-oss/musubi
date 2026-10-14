use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_chat::{Channel, History};
use serde::Deserialize;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/channel/{id}/messages", get(history))
}

#[derive(Deserialize)]
struct HistoryQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    forward: bool,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_limit() -> usize {
    50
}

async fn history(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<History>, ApiError> {
    let channel = Channel::parse(&id)?;
    s.chat.authorize(me.account_id, &channel).await?;
    Ok(Json(
        s.chat
            .history(&channel, q.limit, q.forward, q.cursor.as_deref())
            .await?,
    ))
}
