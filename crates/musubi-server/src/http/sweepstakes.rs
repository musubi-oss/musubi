//! Sweepstakes / raffles (PlayFab Sweepstakes parity).
//!
//! Client: `GET /v1/sweepstakes`, `GET /v1/sweepstakes/{id}` (+ my entries, winners once drawn),
//! `POST /v1/sweepstakes/{id}/enter {entries}`. Server: `GET /v1/server/sweepstakes`,
//! `PUT|GET|DELETE /v1/server/sweepstakes/{id}`, `GET …/entries`, `POST …/draw {seed, force}`,
//! `POST …/cancel` (refunds), `POST /v1/server/sweepstakes/sweep` (draw everything due).

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use musubi_economy::sweepstakes::Sweepstake;
use serde::Deserialize;
use serde_json::{json, Value};

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/sweepstakes", get(list))
        .route("/v1/sweepstakes/{id}", get(get_one))
        .route("/v1/sweepstakes/{id}/enter", post(enter))
        .route("/v1/server/sweepstakes", get(admin_list))
        .route("/v1/server/sweepstakes/sweep", post(sweep))
        .route(
            "/v1/server/sweepstakes/{id}",
            get(admin_get).put(admin_put).delete(admin_delete),
        )
        .route("/v1/server/sweepstakes/{id}/entries", get(admin_entries))
        .route("/v1/server/sweepstakes/{id}/draw", post(draw))
        .route("/v1/server/sweepstakes/{id}/cancel", post(cancel))
}

fn v<T: serde::Serialize>(t: T) -> Value {
    serde_json::to_value(t).unwrap_or_default()
}

async fn list(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let mut out = Vec::new();
    for d in s.economy.sweepstakes(false).await? {
        let mine = s.economy.sweepstake_entry_of(&d.id, me.account_id).await?;
        let mut j = v(&d);
        j["my_entries"] = json!(mine.map(|e| e.entries).unwrap_or(0));
        out.push(j);
    }
    Ok(Json(json!({ "sweepstakes": out })))
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let d = s.economy.sweepstake(&id).await?;
    if !d.enabled {
        return Err(Error::NotFound(format!("sweepstake {id}")).into());
    }
    let mine = s.economy.sweepstake_entry_of(&id, me.account_id).await?;
    let mut j = v(&d);
    j["my_entries"] = json!(mine.map(|e| e.entries).unwrap_or(0));
    if d.status == "drawn" {
        let winners = s.economy.sweepstake_winners(&id).await?;
        j["won"] = json!(winners.iter().any(|w| w.account_id == me.account_id));
        j["winners"] = v(winners);
    }
    Ok(Json(j))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EnterReq {
    entries: Option<i32>,
}

const MAX_ENTRIES_PER_REQUEST: i32 = 10_000;

async fn enter(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    body: Option<Json<EnterReq>>,
) -> Result<Json<Value>, ApiError> {
    let count = body.and_then(|b| b.entries).unwrap_or(1);
    // the economy adds `count` to the account's running total in i32; keep the request far from
    // the overflow edge (and from buying an absurd batch in one call)
    if !(1..=MAX_ENTRIES_PER_REQUEST).contains(&count) {
        return Err(
            Error::InvalidArgument(format!("entries: 1..{MAX_ENTRIES_PER_REQUEST}")).into(),
        );
    }
    let e = s
        .economy
        .enter_sweepstake(me.account_id, &id, count)
        .await?;
    s.events
        .emit(
            "sweepstake.entered",
            Some(me.account_id),
            json!({ "id": id, "entries": count, "total": e.entries, "paid": e.paid }),
        )
        .await;
    Ok(Json(v(e)))
}

async fn admin_list(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "sweepstakes": s.economy.sweepstakes(true).await? }),
    ))
}

async fn admin_get(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let d = s.economy.sweepstake(&id).await?;
    let mut j = v(&d);
    if d.status == "drawn" {
        j["winners"] = v(s.economy.sweepstake_winners(&id).await?);
    }
    Ok(Json(j))
}

async fn admin_put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Json(mut d): Json<Sweepstake>,
) -> Result<Json<Value>, ApiError> {
    d.id = id;
    Ok(Json(v(s.economy.upsert_sweepstake(d).await?)))
}

async fn admin_delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.economy.delete_sweepstake(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn admin_entries(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let entries = s.economy.sweepstake_entries(&id).await?;
    let total: i64 = entries.iter().map(|e| e.entries as i64).sum();
    Ok(Json(
        json!({ "id": id, "entrants": entries.len(), "entries": total, "players": entries }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DrawReq {
    seed: Option<String>,
    force: bool,
}

async fn draw(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    body: Option<Json<DrawReq>>,
) -> Result<Json<Value>, ApiError> {
    let r = body.map(|b| b.0).unwrap_or_default();
    let d = s.economy.draw_sweepstake(&id, r.seed, r.force).await?;
    for w in &d.winners {
        s.events
            .emit(
                "sweepstake.won",
                Some(w.account_id),
                json!({ "id": id, "rank": w.rank, "rewards": w.rewards }),
            )
            .await;
    }
    s.events
        .emit(
            "sweepstake.drawn",
            None,
            json!({ "id": id, "winners": d.winners.len(), "entrants": d.sweepstake.entrants,
                    "entries": d.sweepstake.entries, "seed": d.sweepstake.seed, "forced": r.force }),
        )
        .await;
    Ok(Json(v(d)))
}

async fn cancel(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (d, refunded) = s.economy.cancel_sweepstake(&id).await?;
    s.events
        .emit(
            "sweepstake.cancelled",
            None,
            json!({ "id": id, "refunds": refunded, "entrants": d.entrants }),
        )
        .await;
    Ok(Json(json!({ "sweepstake": d, "refunds": refunded })))
}

async fn sweep(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let drawn = s.economy.draw_due_sweepstakes().await?;
    for id in &drawn {
        s.events
            .emit(
                "sweepstake.drawn",
                None,
                json!({ "id": id, "forced": false, "by": "sweep" }),
            )
            .await;
    }
    Ok(Json(json!({ "drawn": drawn })))
}
