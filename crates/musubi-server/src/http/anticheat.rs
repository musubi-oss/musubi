//! Anti-cheat HTTP: flags from game servers, vendor session checks and the moderator view.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/anticheat/flag", post(flag))
        .route("/v1/server/anticheat/verify", post(verify))
        .route("/v1/admin/anticheat", get(queue))
        .route("/v1/admin/accounts/{id}/anticheat", get(account))
}

#[derive(Deserialize)]
struct FlagReq {
    account_id: Uuid,
    kind: String,
    #[serde(default = "ten")]
    severity: i32,
    #[serde(default)]
    details: Value,
    #[serde(default)]
    match_id: Option<String>,
    #[serde(default)]
    session_id: Option<Uuid>,
}

fn ten() -> i32 {
    10
}

/// A game server (or trusted tool) reports a suspicious player: `kind` is free-form
/// (`speedhack`, `impossible_shot`, …), `severity` 1..=100 adds to the account's score.
async fn flag(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<FlagReq>,
) -> Result<Json<Value>, ApiError> {
    let out = s
        .anticheat
        .flag(
            AccountId(r.account_id),
            r.session_id,
            r.match_id.as_deref(),
            "server",
            &r.kind,
            r.severity,
            r.details,
        )
        .await?;
    Ok(Json(out))
}

#[derive(Deserialize)]
struct VerifyReq {
    provider: String,
    account_id: Uuid,
    ticket: String,
}

/// Relay a client's EAC / BattlEye session ticket to the vendor endpoint from
/// `[anticheat] eac_url|battleye_url`; a rejection is recorded as a `vendor_rejected` signal.
async fn verify(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<VerifyReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.anticheat
            .verify(&r.provider, AccountId(r.account_id), &r.ticket)
            .await?,
    ))
}

#[derive(Deserialize)]
struct QueueQuery {
    #[serde(default = "one")]
    min_score: i64,
    #[serde(default = "fifty")]
    limit: i64,
}

fn one() -> i64 {
    1
}

fn fifty() -> i64 {
    50
}

/// Accounts with signals in the window (`[anticheat] window_hours`), worst first.
async fn queue(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    let hours = s.cfg.anticheat.window_hours;
    let accounts = crate::anticheat::queue(&s.db, hours, q.min_score, q.limit).await?;
    Ok(Json(json!({ "window_hours": hours, "accounts": accounts })))
}

async fn account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    let hours = s.cfg.anticheat.window_hours;
    let score = crate::anticheat::score(&s.db, AccountId(id), hours).await?;
    let signals = crate::anticheat::signals_of(&s.db, AccountId(id), q.limit).await?;
    if signals.is_empty() {
        let known: Option<i32> = sqlx::query_scalar("select 1 from musubi.accounts where id = $1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
        if known.is_none() {
            return Err(Error::NotFound(format!("account {id}")).into());
        }
    }
    Ok(Json(
        json!({ "account_id": id, "score": score, "window_hours": hours, "kick_score": s.cfg.anticheat.kick_score,
        "signals": signals }),
    ))
}
