mod account;
pub mod admin;
mod auth;
mod chat;
mod cluster;
pub mod economy;
pub(crate) mod error;
pub(crate) mod extract;
mod fleets;
mod friends;
mod groups;
mod integrations;
mod leaderboards;
mod matches;
mod notifications;
mod rpc;
mod storage;
mod tournaments;
mod worlds;
mod ws;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::app::AppState;

pub use error::ApiError;
pub use extract::{Bearer, ClientIp, ServerKey};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(crate::metrics::render))
        .route("/openapi.json", get(openapi))
        .route("/readyz", get(readyz))
        .route("/v1/node", get(node_info))
        .merge(admin::routes())
        .merge(crate::events::routes())
        .merge(auth::routes())
        .merge(account::routes())
        .merge(friends::routes())
        .merge(groups::routes())
        .merge(notifications::routes())
        .merge(chat::routes())
        .merge(leaderboards::routes())
        .merge(tournaments::routes())
        .merge(rpc::routes())
        .merge(cluster::routes())
        .merge(matches::routes())
        .merge(fleets::routes())
        .merge(worlds::routes())
        .merge(integrations::routes())
        .merge(economy::routes())
        .merge(storage::routes())
        .merge(ws::routes())
        .layer(TraceLayer::new_for_http())
        .layer(axum::middleware::from_fn(crate::metrics::track_http))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::audit::record,
        ))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Readiness for load balancers / Kubernetes: 200 only when the database answers, every embedded
/// migration is applied, the Raft group (when enabled) has a leader, and the node is not draining.
async fn readyz(State(s): State<AppState>) -> Response {
    let draining = s.draining.load(std::sync::atomic::Ordering::Relaxed);
    let db_timeout = std::time::Duration::from_secs(2);
    let db = tokio::time::timeout(
        db_timeout,
        sqlx::query_scalar::<_, i32>("select 1").fetch_one(&s.db),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    let migrations = if db {
        tokio::time::timeout(
            db_timeout,
            sqlx::query_scalar::<_, i64>("select count(*) from _sqlx_migrations where success")
                .fetch_one(&s.db),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|n| n as usize >= crate::db::MIGRATOR.migrations.len())
        .unwrap_or(false)
    } else {
        false
    };
    // `None` = Raft not enabled on this node (no bus); only a leaderless group is a failure.
    let raft = match &s.cluster {
        Some(c) if c.raft_id() != 0 => Some(c.raft_leader().is_some()),
        _ => None,
    };
    let ready = !draining && db && migrations && raft.unwrap_or(true);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if ready { "ok" } else { "unavailable" },
            "checks": { "db": db, "migrations": migrations, "raft": raft, "draining": draining },
        })),
    )
        .into_response()
}

async fn node_info(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "name": s.cfg.node.name,
        "region": s.cfg.node.region,
        "roles": s.cfg.node.roles,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": s.started_at.elapsed().as_secs(),
        "realtime_sessions": s.hub.session_count(),
        "relayed_matches": s.relayed.match_count(),
        "authoritative_matches": s.matches.count(),
        "parties": s.parties.party_count(),
        "matchmaker_tickets": s.matchmaker.ticket_count(),
    }))
}

async fn openapi() -> Json<serde_json::Value> {
    Json(crate::openapi::document(env!("CARGO_PKG_VERSION")))
}
