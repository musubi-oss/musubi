//! Item lifecycle routes: publish state, use with cooldown / effects / durability, repair,
//! revocation and player effects.

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_economy::items_lifecycle::LifecycleRules;
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/inventory/use", post(use_item))
        .route("/v1/effects", get(my_effects))
        .route("/v1/server/effects/{account_id}", get(effects_of))
        .route(
            "/v1/server/effects/{account_id}/{key}",
            axum::routing::delete(remove_effect),
        )
        .route("/v1/server/items/{id}/lifecycle", put(set_lifecycle))
        .route("/v1/server/items/{id}/status", post(set_status))
        .route("/v1/server/items/{id}/revoke", post(revoke))
        .route("/v1/server/inventory/repair", post(repair))
}

#[derive(Deserialize)]
struct UseReq {
    id: Uuid,
    #[serde(default = "one")]
    quantity: i64,
}

fn one() -> i64 {
    1
}

async fn use_item(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<UseReq>,
) -> Result<Json<Value>, ApiError> {
    let out = s
        .economy
        .use_item(me.account_id, r.id, r.quantity, OffsetDateTime::now_utc())
        .await?;
    s.events
        .emit(
            "item.used",
            Some(me.account_id),
            json!({ "item_id": out.item_id, "quantity": r.quantity, "broken": out.broken,
                    "effects": out.effects.len() }),
        )
        .await;
    if out.broken {
        s.events
            .emit(
                "item.broken",
                Some(me.account_id),
                json!({ "item_id": out.item_id, "entry_id": r.id }),
            )
            .await;
    }
    Ok(Json(serde_json::to_value(out).map_err(Error::internal)?))
}

async fn my_effects(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    let e = s
        .economy
        .effects(me.account_id, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(json!({ "effects": e })))
}

async fn effects_of(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(account_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let e = s
        .economy
        .effects(AccountId(account_id), OffsetDateTime::now_utc())
        .await?;
    Ok(Json(json!({ "effects": e })))
}

async fn remove_effect(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((account_id, key)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    let removed = s.economy.remove_effect(AccountId(account_id), &key).await?;
    Ok(Json(json!({ "removed": removed })))
}

async fn set_lifecycle(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<LifecycleRules>,
) -> Result<Json<Value>, ApiError> {
    let r = s.economy.set_item_lifecycle(&id, r).await?;
    Ok(Json(serde_json::to_value(r).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct StatusReq {
    status: String,
}

async fn set_status(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(r): Json<StatusReq>,
) -> Result<Json<Value>, ApiError> {
    s.economy.set_item_status(&id, &r.status).await?;
    s.events
        .emit(
            "item.status",
            None,
            json!({ "item_id": id, "status": r.status }),
        )
        .await;
    Ok(Json(json!({ "id": id, "status": r.status })))
}

async fn revoke(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (entries, accounts) = s.economy.revoke_item(&id).await?;
    for a in &accounts {
        s.events
            .emit("item.revoked", Some(*a), json!({ "item_id": id }))
            .await;
    }
    Ok(Json(
        json!({ "id": id, "status": "retired", "entries_removed": entries, "accounts": accounts }),
    ))
}

#[derive(Deserialize)]
struct RepairReq {
    account_id: Uuid,
    id: Uuid,
    amount: Option<i64>,
}

async fn repair(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<RepairReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.economy
            .repair(AccountId(r.account_id), r.id, r.amount)
            .await?,
    ))
}
