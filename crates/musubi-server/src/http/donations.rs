//! Donations: ask friends / group mates / anyone for help (Hiro parity).
//!
//! Client: `GET /v1/donations/defs`, `GET /v1/donations?all=1`, `POST /v1/donations {def_id}`,
//! `GET /v1/donations/accounts/{id}` (what a friend is asking for), `POST /v1/donations/{id}/give`,
//! `POST /v1/donations/{id}/claim`. Server: `GET|PUT|DELETE /v1/server/donations/defs[/{id}]`,
//! `GET /v1/server/accounts/{id}/donations`, `POST /v1/server/donations/sweep`.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_economy::donations::DonationDef;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/donations/defs", get(defs))
        .route("/v1/donations", get(mine).post(request))
        .route("/v1/donations/accounts/{id}", get(of_account))
        .route("/v1/donations/{id}/give", post(give))
        .route("/v1/donations/{id}/claim", post(claim))
        .route("/v1/server/donations/defs", get(admin_defs))
        .route(
            "/v1/server/donations/defs/{id}",
            get(admin_def).put(admin_put_def).delete(admin_delete_def),
        )
        .route("/v1/server/accounts/{id}/donations", get(server_of_account))
        .route("/v1/server/donations/sweep", post(sweep))
}

fn parse(id: &str, what: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument(format!("{what} id")))
}

async fn defs(State(s): State<AppState>, Bearer(_me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "definitions": s.economy.donation_defs(false).await? }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MineQ {
    all: Option<String>,
}

async fn mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<MineQ>,
) -> Result<Json<Value>, ApiError> {
    let all = q
        .all
        .as_deref()
        .is_some_and(|v| v.is_empty() || v == "1" || v == "true");
    Ok(Json(
        json!({ "donations": s.economy.donations_of(me.account_id, !all).await? }),
    ))
}

#[derive(Deserialize)]
struct RequestBody {
    def_id: String,
}

async fn request(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<RequestBody>,
) -> Result<Json<Value>, ApiError> {
    s.limits.account(me.account_id)?;
    let d = s.economy.request_donation(me.account_id, &b.def_id).await?;
    s.events
        .emit(
            "donation.requested",
            Some(me.account_id),
            json!({ "donation_id": d.id, "def_id": d.def_id, "target_count": d.target_count,
                "max_count": d.max_count, "audience": d.audience, "expires_at": d.expires_at }),
        )
        .await;
    Ok(Json(serde_json::to_value(d).unwrap_or_default()))
}

async fn of_account(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let account = AccountId(parse(&id, "account")?);
    let all = s.economy.donations_of(account, true).await?;
    let open: Vec<_> = all.into_iter().filter(|d| d.status == "active").collect();
    Ok(Json(json!({ "donations": open })))
}

async fn give(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.limits.account(me.account_id)?;
    let id = parse(&id, "donation")?;
    let (d, rewards) = s.economy.give_donation(me.account_id, id).await?;
    s.events
        .emit(
            "donation.given",
            Some(d.account_id),
            json!({ "donation_id": d.id, "def_id": d.def_id, "contributor": me.account_id,
                "count": d.count, "max_count": d.max_count, "status": d.status }),
        )
        .await;
    Ok(Json(json!({ "donation": d, "rewards": rewards })))
}

async fn claim(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.limits.account(me.account_id)?;
    let id = parse(&id, "donation")?;
    let (d, rewards, claimed) = s.economy.claim_donation(me.account_id, id).await?;
    s.events
        .emit(
            "donation.claimed",
            Some(me.account_id),
            json!({ "donation_id": d.id, "def_id": d.def_id, "claimed": claimed, "rewards": rewards }),
        )
        .await;
    Ok(Json(
        json!({ "donation": d, "claimed": claimed, "rewards": rewards }),
    ))
}

async fn admin_defs(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "definitions": s.economy.donation_defs(true).await? }),
    ))
}

async fn admin_def(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.donation_def(&id).await?).unwrap_or_default(),
    ))
}

async fn admin_put_def(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Json(mut d): Json<DonationDef>,
) -> Result<Json<Value>, ApiError> {
    d.id = id;
    Ok(Json(
        serde_json::to_value(s.economy.upsert_donation_def(d).await?).unwrap_or_default(),
    ))
}

async fn admin_delete_def(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !s.economy.delete_donation_def(&id).await? {
        return Err(Error::NotFound(format!("donation definition `{id}`")).into());
    }
    Ok(Json(json!({ "ok": true })))
}

async fn server_of_account(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let account = AccountId(parse(&id, "account")?);
    Ok(Json(
        json!({ "donations": s.economy.donations_of(account, false).await? }),
    ))
}

async fn sweep(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "removed": s.economy.sweep_donations().await? }),
    ))
}
