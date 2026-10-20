//! GeoIP API: database status / refresh / lookup for operators, the account's last known
//! location for the client and the server, accounts-per-country cohorts.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/geoip", get(status))
        .route("/v1/server/geoip/update", post(update))
        .route("/v1/server/geoip/reload", post(reload))
        .route("/v1/server/geoip/lookup/{ip}", get(lookup))
        .route("/v1/account/geo", get(my_geo))
        .route("/v1/server/accounts/{id}/geo", get(account_geo))
        .route("/v1/server/analytics/geo", get(cohorts))
}

async fn status(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.geoip.status()))
}

/// Download the configured edition now (needs `geoip.license_key`).
async fn update(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    if !s.geoip.enabled() {
        return Err(Error::InvalidArgument("geoip is not enabled".into()).into());
    }
    Ok(Json(s.geoip.update().await?))
}

/// Re-read the `.mmdb` at `path` after an operator copied a new one there.
async fn reload(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    if !s.geoip.enabled() {
        return Err(Error::InvalidArgument("geoip is not enabled".into()).into());
    }
    s.geoip.load()?;
    Ok(Json(s.geoip.status()))
}

async fn lookup(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(ip): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if ip.parse::<std::net::IpAddr>().is_err() {
        return Err(Error::InvalidArgument(format!("{ip:?} is not an IP address")).into());
    }
    let loc = s.geoip.lookup(&ip);
    Ok(Json(
        json!({ "ip": ip, "found": loc.is_some(), "location": loc }),
    ))
}

async fn my_geo(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::geoip::GeoIp::account_geo(&s.db, me.account_id)
            .await?
            .unwrap_or_else(|| json!({ "account_id": me.account_id.0, "known": false })),
    ))
}

async fn account_geo(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    crate::geoip::GeoIp::account_geo(&s.db, AccountId(id))
        .await?
        .map(Json)
        .ok_or_else(|| Error::NotFound("no location known for this account".into()).into())
}

#[derive(Deserialize)]
struct CohortQ {
    since_days: Option<i64>,
    limit: Option<i64>,
}

/// Accounts per last-known country.
async fn cohorts(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<CohortQ>,
) -> Result<Json<Value>, ApiError> {
    if q.since_days.is_some_and(|d| d <= 0) || q.limit.is_some_and(|l| !(1..=500).contains(&l)) {
        return Err(Error::InvalidArgument("since_days > 0, limit 1..=500".into()).into());
    }
    Ok(Json(
        crate::geoip::GeoIp::cohorts(&s.db, q.since_days, q.limit.unwrap_or(50)).await?,
    ))
}
