use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/server/cluster/nodes", get(nodes))
}

async fn nodes(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    let nodes = match &s.cluster {
        Some(c) => c.nodes().await?,
        None => vec![],
    };
    Ok(Json(
        json!({ "self": s.cluster.as_ref().map(|c| c.node_id()), "nodes": nodes }),
    ))
}
