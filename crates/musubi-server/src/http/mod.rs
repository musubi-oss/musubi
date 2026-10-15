mod account;
mod auth;
mod chat;
mod cluster;
pub mod economy;
mod error;
mod extract;
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
pub use extract::{Bearer, ServerKey};

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(crate::metrics::render))
        .route("/readyz", get(readyz))
        .route("/v1/node", get(node_info))
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
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz(State(s): State<AppState>) -> Response {
    match sqlx::query_scalar::<_, i32>("select 1")
        .fetch_one(&s.db)
        .await
    {
        Ok(_) => (StatusCode::OK, "ok").into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, format!("db: {e}")).into_response(),
    }
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
