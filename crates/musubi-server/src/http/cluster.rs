use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/cluster/nodes", get(nodes))
        .route("/v1/server/presence/{account_id}", get(presence))
        .route("/v1/server/cluster/raft", get(raft))
        .route("/v1/server/cluster/directory/{key}", get(directory))
}

/// Raft view from this node: its id, who it thinks leads, the voter set.
async fn raft(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(c) = &s.cluster else {
        return Ok(Json(json!({ "enabled": false })));
    };
    Ok(Json(json!({
        "enabled": c.raft_id() != 0,
        "raft_id": c.raft_id(),
        "leader": c.raft_leader(),
        "is_leader": c.is_raft_leader(),
        "voters": c.raft_voters(),
    })))
}

async fn directory(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    axum::extract::Path(key): axum::extract::Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let value = match &s.cluster {
        Some(c) => c.directory_get_fresh(&key).await,
        None => None,
    };
    Ok(Json(json!({ "key": key, "value": value })))
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

/// Cluster-wide online check for game servers: is the account connected to any node?
async fn presence(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(account_id): Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account = musubi_core::AccountId(account_id);
    let local = s.fanout.hub_presences(account);
    Ok(Json(serde_json::json!({
        "account_id": account,
        "online": s.fanout.is_online_anywhere(account),
        "local_sessions": local,
    })))
}
