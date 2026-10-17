//! `GET /v1/regions`: what a client should ping before queueing, and where it probably is.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::extract::Bearer;
use crate::http::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/regions", get(regions))
}

/// Regions with their probe endpoints (`echo` UDP, `http` URL). The client measures each and
/// submits `ping_<region>` numeric properties with its matchmaker ticket (or a `region` string);
/// `suggested` is the country-map fallback from the CDN country header when it cannot probe.
async fn regions(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let fleets = s.fleets.list().await?;
    let list = crate::regions::list(&s.cfg.regions, fleets.into_iter().map(|f| f.region));
    let (country, suggested) = crate::regions::suggest(&s.cfg.regions, &headers);
    Ok(Json(
        json!({ "regions": list, "country": country, "suggested": suggested,
        "ping_property_prefix": "ping_" }),
    ))
}
