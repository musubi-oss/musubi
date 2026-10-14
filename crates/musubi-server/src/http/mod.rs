mod account;
mod auth;
mod error;
mod extract;
mod friends;
mod groups;
mod notifications;
mod storage;
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
pub use extract::Bearer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/node", get(node_info))
        .merge(auth::routes())
        .merge(account::routes())
        .merge(friends::routes())
        .merge(groups::routes())
        .merge(notifications::routes())
        .merge(storage::routes())
        .merge(ws::routes())
        .layer(TraceLayer::new_for_http())
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
    }))
}
