//! `GET /v1/parties` — the party directory over HTTP (same filter as the realtime `party_list`).

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, Bearer};
use crate::realtime::party::{ListFilter, PartyRegistry};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/parties", get(list))
}

#[derive(Deserialize)]
pub struct ListQ {
    pub label: Option<String>,
    pub label_prefix: Option<String>,
    pub open: Option<bool>,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    50
}

async fn list(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let parties = PartyRegistry::list(
        &s.db,
        &ListFilter {
            label: q.label,
            label_prefix: q.label_prefix,
            open: q.open,
            limit: q.limit,
        },
    )
    .await?;
    Ok(Json(json!({ "parties": parties })))
}
