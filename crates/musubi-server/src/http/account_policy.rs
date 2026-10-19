//! Account policy routes: validation / edit-cadence rules per profile field, dormant-account
//! policy and sweep, and the support lookup card.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_account::policy::AccountPolicy;
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/accounts/policy", get(get_policy).put(put_policy))
        .route("/v1/admin/accounts/lookup", get(lookup))
        .route("/v1/admin/accounts/dormant/sweep", post(sweep))
        .route("/v1/account/policy", get(my_policy))
}

async fn get_policy(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.accounts.policy().await?).map_err(Error::internal)?,
    ))
}

async fn put_policy(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(p): Json<AccountPolicy>,
) -> Result<Json<Value>, ApiError> {
    let p = s.accounts.set_policy(p).await?;
    s.events
        .emit("account.policy_updated", None, json!({}))
        .await;
    Ok(Json(serde_json::to_value(p).map_err(Error::internal)?))
}

/// Rules the client should enforce in its profile UI plus the caller's per-field editability.
async fn my_policy(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let p = s.accounts.policy().await?;
    let status = s
        .accounts
        .edit_status(me.account_id, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(json!({ "fields": p.fields, "edit_status": status })))
}

#[derive(Deserialize, Default)]
struct LookupQ {
    id: Option<Uuid>,
    username: Option<String>,
    /// `provider:subject`
    identity: Option<String>,
    email: Option<String>,
}

/// Exact lookup by id / username / linked identity / e-mail → support card.
async fn lookup(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<LookupQ>,
) -> Result<Json<Value>, ApiError> {
    let id: Option<Uuid> = if let Some(id) = q.id {
        Some(id)
    } else if let Some(u) = q
        .username
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        sqlx::query_scalar("select id from musubi.accounts where lower(username) = lower($1)")
            .bind(u)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?
    } else if let Some((provider, subject)) = q
        .identity
        .as_deref()
        .and_then(|i| i.rsplit_once(':'))
        .or_else(|| q.email.as_deref().map(|e| ("email", e)))
    {
        sqlx::query_scalar(
            "select account_id from musubi.identities where provider = $1 and lower(subject) = lower($2)",
        )
        .bind(provider)
        .bind(subject.trim())
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?
    } else {
        return Err(
            Error::InvalidArgument("id, username, identity or email required".into()).into(),
        );
    };
    let Some(id) = id else {
        return Err(Error::NotFound("account".into()).into());
    };
    let acc = AccountId(id);
    let account = s.accounts.get(acc).await?;
    let status = s.accounts.status(acc).await?;
    let (last_seen, sessions): (Option<OffsetDateTime>, i64) = sqlx::query_as(
        "select max(last_seen_at), count(*) filter (where expires_at > now()) from musubi.sessions where account_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let devices: i64 =
        sqlx::query_scalar("select count(*) from musubi.account_devices where account_id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .unwrap_or(0);
    let ips: i64 =
        sqlx::query_scalar("select count(*) from musubi.account_ips where account_id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .unwrap_or(0);
    let dormancy: Option<(
        Option<OffsetDateTime>,
        Option<OffsetDateTime>,
        Option<String>,
    )> = sqlx::query_as(
        "select noticed_at, actioned_at, action from musubi.account_dormancy where account_id = $1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let edits = s
        .accounts
        .edit_status(acc, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(json!({
        "account": account,
        "status": status,
        "last_seen_at": last_seen.map(|t| t.unix_timestamp()),
        "active_sessions": sessions,
        "devices": devices,
        "ips": ips,
        "dormancy": dormancy.map(|(n, a, act)| json!({
            "noticed_at": n.map(|t| t.unix_timestamp()),
            "actioned_at": a.map(|t| t.unix_timestamp()),
            "action": act,
        })),
        "edit_status": edits,
    })))
}

/// One dormant-account pass: notices and actions with their events (actioned accounts are
/// kicked off every node); returns the sweep report.
pub async fn run_sweep(s: &AppState) -> Result<musubi_account::policy::DormantSweep> {
    let p = s.accounts.policy().await?;
    let r = s
        .accounts
        .dormant_sweep(&p.dormant, OffsetDateTime::now_utc())
        .await?;
    for id in &r.noticed {
        s.events
            .emit(
                "account.dormant_notice",
                Some(*id),
                json!({ "days": p.dormant.days, "action_in_days": p.dormant.notice_days, "action": p.dormant.action }),
            )
            .await;
    }
    for id in &r.actioned {
        crate::http::admin::kick_everywhere(s, *id).await;
        s.events
            .emit(
                "account.dormant_actioned",
                Some(*id),
                json!({ "action": p.dormant.action, "days": p.dormant.days }),
            )
            .await;
    }
    Ok(r)
}

/// Leader loop entry point: how many accounts were noticed or actioned.
pub async fn dormant_pass(s: &AppState) -> Result<usize> {
    let r = run_sweep(s).await?;
    Ok(r.noticed.len() + r.actioned.len())
}

async fn sweep(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let r = run_sweep(&s).await?;
    Ok(Json(serde_json::to_value(r).map_err(Error::internal)?))
}
