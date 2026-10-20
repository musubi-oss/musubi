use axum::extract::{Path, State};
use axum::response::IntoResponse;
use axum::routing::{get, post};
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
        .route("/v1/server/cluster/mesh_net", get(mesh_net))
        .route("/v1/server/cluster/mesh_net/config", get(mesh_net_config))
        .route(
            "/v1/server/cluster/mesh_net/refresh",
            post(mesh_net_refresh),
        )
        .route("/v1/server/cluster/mesh_net/rotate", post(mesh_net_rotate))
}

/// WireGuard mesh view of this node: key, overlay address, endpoint, peer set, apply state.
async fn mesh_net(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(match s.cluster.as_ref().and_then(|c| c.mesh_net()) {
        Some(m) => m.status(),
        None => json!({ "enabled": false }),
    }))
}

#[derive(serde::Deserialize)]
struct ConfigQ {
    /// `quick` (default, `wg-quick` file with Address) or `setconf` (`wg setconf` input).
    format: Option<String>,
}

/// Rendered WireGuard configuration of this node for operators applying it themselves
/// (`apply = none`, or hosts without `wg` in the server's PATH). Carries the private key.
async fn mesh_net_config(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    axum::extract::Query(q): axum::extract::Query<ConfigQ>,
) -> Result<axum::response::Response, ApiError> {
    let m = s
        .cluster
        .as_ref()
        .and_then(|c| c.mesh_net())
        .ok_or_else(|| musubi_core::Error::NotFound("mesh_net is not enabled".into()))?;
    let quick = match q.format.as_deref() {
        None | Some("quick") => true,
        Some("setconf") => false,
        Some(other) => {
            return Err(musubi_core::Error::InvalidArgument(format!(
                "format {other:?}: want quick | setconf"
            ))
            .into())
        }
    };
    Ok((
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        m.render(quick),
    )
        .into_response())
}

/// Re-derive the peer set from the registry now (instead of waiting for the next tick).
async fn mesh_net_refresh(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s
        .cluster
        .as_ref()
        .filter(|c| c.mesh_net().is_some())
        .ok_or_else(|| musubi_core::Error::NotFound("mesh_net is not enabled".into()))?;
    c.refresh_mesh().await?;
    Ok(Json(c.mesh_net().map(|m| m.status()).unwrap_or_default()))
}

/// New key pair for this node; peers learn it from the registry within one refresh.
async fn mesh_net_rotate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s
        .cluster
        .as_ref()
        .filter(|c| c.mesh_net().is_some())
        .ok_or_else(|| musubi_core::Error::NotFound("mesh_net is not enabled".into()))?;
    let public_key = c.rotate_mesh_key().await?;
    Ok(Json(json!({ "rotated": true, "public_key": public_key })))
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
