//! VIP tiers (Beamable `VipContent` parity): `GET /v1/vip[/{program}]` for the player's standing
//! and perks; server `GET|PUT|DELETE /v1/server/vip/programs[/{id}]`,
//! `GET /v1/server/accounts/{id}/vip`, `POST /v1/server/accounts/{id}/vip/{program}/points`.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_economy::vip::VipProgram;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/vip", get(mine))
        .route("/v1/vip/{program}", get(mine_program))
        .route("/v1/server/vip/programs", get(admin_list))
        .route(
            "/v1/server/vip/programs/{id}",
            get(admin_get).put(admin_put).delete(admin_delete),
        )
        .route("/v1/server/accounts/{id}/vip", get(server_of_account))
        .route(
            "/v1/server/accounts/{id}/vip/{program}/points",
            post(server_points),
        )
}

fn account(id: &str) -> Result<AccountId, Error> {
    Uuid::parse_str(id)
        .map(AccountId)
        .map_err(|_| Error::InvalidArgument("account id".into()))
}

async fn emit_tier_ups(s: &AppState, statuses: &[musubi_economy::vip::VipStatus]) {
    for st in statuses.iter().filter(|st| !st.rewards.is_empty()) {
        s.events
            .emit(
                "vip.tier_up",
                Some(st.account_id),
                json!({ "program": st.program_id, "tier": st.tier, "tier_name": st.tier_name,
                    "points": st.points, "rewards": st.rewards }),
            )
            .await;
    }
}

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let statuses = s.economy.vip_statuses(me.account_id).await?;
    emit_tier_ups(&s, &statuses).await;
    Ok(Json(json!({ "programs": statuses })))
}

async fn mine_program(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(program): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = s.economy.vip_program(&program).await?;
    if !p.enabled {
        return Err(Error::NotFound(format!("vip program `{program}`")).into());
    }
    let st = s.economy.vip_status(me.account_id, &p).await?;
    emit_tier_ups(&s, std::slice::from_ref(&st)).await;
    Ok(Json(serde_json::to_value(st).unwrap_or_default()))
}

async fn admin_list(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "programs": s.economy.vip_programs(true).await? }),
    ))
}

async fn admin_get(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.vip_program(&id).await?).unwrap_or_default(),
    ))
}

async fn admin_put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Json(mut p): Json<VipProgram>,
) -> Result<Json<Value>, ApiError> {
    p.id = id;
    Ok(Json(
        serde_json::to_value(s.economy.upsert_vip_program(p).await?).unwrap_or_default(),
    ))
}

async fn admin_delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !s.economy.delete_vip_program(&id).await? {
        return Err(Error::NotFound(format!("vip program `{id}`")).into());
    }
    Ok(Json(json!({ "ok": true })))
}

async fn server_of_account(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let statuses = s.economy.vip_statuses(account(&id)?).await?;
    emit_tier_ups(&s, &statuses).await;
    Ok(Json(json!({ "programs": statuses })))
}

#[derive(Deserialize)]
struct PointsBody {
    delta: i64,
    #[serde(default)]
    reason: String,
}

async fn server_points(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, program)): Path<(String, String)>,
    Json(b): Json<PointsBody>,
) -> Result<Json<Value>, ApiError> {
    let acc = account(&id)?;
    let st = s.economy.vip_add_points(acc, &program, b.delta).await?;
    s.events
        .emit(
            "vip.points_granted",
            Some(acc),
            json!({ "program": program, "delta": b.delta, "reason": b.reason, "points": st.points, "tier": st.tier }),
        )
        .await;
    emit_tier_ups(&s, std::slice::from_ref(&st)).await;
    Ok(Json(serde_json::to_value(st).unwrap_or_default()))
}
