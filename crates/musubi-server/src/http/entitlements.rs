//! Platform entitlements (plan 5.11): the catalog maps a platform purchase (Steam DLC appid, PSN
//! entitlement label, Xbox product id, …) to a reward table, and every account receives each
//! entitlement at most once. Steam ownership is checked server-side with the publisher Web API
//! (`ISteamUser/CheckAppOwnership`); console stores only expose entitlements to the title, so
//! those are fed by the game server through the trusted `/v1/server/entitlements/grant`.
use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::extract::Bearer;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/entitlements", get(mine))
        .route("/v1/entitlements/sync", post(sync))
        .route("/v1/server/entitlements", get(catalog).put(upsert))
        .route(
            "/v1/server/entitlements/{provider}/{external_id}",
            delete(remove),
        )
        .route("/v1/server/entitlements/grant", post(server_grant))
        .route("/v1/server/entitlements/revoke", post(server_revoke))
}

#[derive(Deserialize)]
struct Entitlement {
    provider: String,
    external_id: String,
    reward_table: String,
    #[serde(default)]
    label: String,
}

fn check_key(provider: &str, external_id: &str) -> Result<(), ApiError> {
    let ok = |s: &str, max: usize| {
        !s.is_empty() && s.len() <= max && !s.chars().any(|c| c.is_control() || c == '/')
    };
    if !ok(provider, 64) || !ok(external_id, 256) {
        return Err(Error::InvalidArgument("provider / external_id".into()).into());
    }
    Ok(())
}

async fn catalog(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "select provider, external_id, reward_table, label from musubi.entitlements order by provider, external_id",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "entitlements": rows.into_iter().map(|(p, e, t, l)| json!({
            "provider": p, "external_id": e, "reward_table": t, "label": l,
        })).collect::<Vec<_>>()
    })))
}

async fn upsert(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(e): Json<Entitlement>,
) -> Result<Json<Value>, ApiError> {
    check_key(&e.provider, &e.external_id)?;
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from musubi.reward_tables where id = $1)")
            .bind(&e.reward_table)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if !exists {
        return Err(Error::NotFound(format!("reward table {}", e.reward_table)).into());
    }
    sqlx::query(
        "insert into musubi.entitlements (provider, external_id, reward_table, label) values ($1, $2, $3, $4)
         on conflict (provider, external_id) do update set reward_table = excluded.reward_table, label = excluded.label",
    )
    .bind(&e.provider)
    .bind(&e.external_id)
    .bind(&e.reward_table)
    .bind(&e.label)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "ok": true })))
}

async fn remove(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((provider, external_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.entitlements where provider = $1 and external_id = $2")
        .bind(&provider)
        .bind(&external_id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("entitlement".into()).into());
    }
    Ok(Json(json!({ "ok": true })))
}

/// Grant once: the grant row is the idempotency key, the reward table pays. `None` = already held.
async fn grant(
    s: &AppState,
    account: AccountId,
    provider: &str,
    external_id: &str,
) -> Result<Option<Vec<musubi_economy::RewardEntry>>, ApiError> {
    let table: Option<String> = sqlx::query_scalar(
        "select reward_table from musubi.entitlements where provider = $1 and external_id = $2",
    )
    .bind(provider)
    .bind(external_id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(table) = table else {
        return Err(Error::NotFound(format!("entitlement {provider}:{external_id}")).into());
    };
    let n = sqlx::query(
        "insert into musubi.entitlement_grants (account_id, provider, external_id) values ($1, $2, $3)
         on conflict (account_id, provider, external_id) do update set granted_at = now(), revoked_at = null
         where musubi.entitlement_grants.revoked_at is not null",
    )
    .bind(account.0)
    .bind(provider)
    .bind(external_id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Ok(None);
    }
    let reference = format!("entitlement:{provider}:{external_id}");
    let rewards = match s.economy.grant_table(account, &table, &reference).await {
        Ok(r) => r,
        Err(e) => {
            // keep the ledger honest: no reward, no grant row
            let _ = sqlx::query(
                "delete from musubi.entitlement_grants where account_id = $1 and provider = $2 and external_id = $3",
            )
            .bind(account.0)
            .bind(provider)
            .bind(external_id)
            .execute(&s.db)
            .await;
            return Err(e.into());
        }
    };
    s.events
        .emit(
            "entitlement.granted",
            Some(account),
            json!({ "provider": provider, "external_id": external_id, "reward_table": table, "rewards": rewards }),
        )
        .await;
    Ok(Some(rewards))
}

/// Mark the entitlement as no longer held (refund, chargeback, license removed). Items already
/// delivered are not clawed back here — modules get `entitlement.revoked` to decide.
async fn revoke(
    s: &AppState,
    account: AccountId,
    provider: &str,
    external_id: &str,
    reason: &str,
) -> Result<bool, ApiError> {
    let n = sqlx::query(
        "update musubi.entitlement_grants set revoked_at = now()
         where account_id = $1 and provider = $2 and external_id = $3 and revoked_at is null",
    )
    .bind(account.0)
    .bind(provider)
    .bind(external_id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n > 0 {
        s.events
            .emit(
                "entitlement.revoked",
                Some(account),
                json!({ "provider": provider, "external_id": external_id, "reason": reason }),
            )
            .await;
    }
    Ok(n > 0)
}

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(
        String,
        String,
        time::OffsetDateTime,
        Option<time::OffsetDateTime>,
    )> = sqlx::query_as(
        "select provider, external_id, granted_at, revoked_at from musubi.entitlement_grants
             where account_id = $1 order by granted_at",
    )
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "entitlements": rows.into_iter().map(|(p, e, g, r)| json!({
            "provider": p, "external_id": e,
            "granted_at": g.format(&time::format_description::well_known::Rfc3339).ok(),
            "revoked_at": r.and_then(|r| r.format(&time::format_description::well_known::Rfc3339).ok()),
            "active": r.is_none(),
        })).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
struct SyncReq {
    provider: String,
}

/// Steam: every catalog entry is checked against the publisher Web API for the caller's
/// SteamID — newly owned ones are granted, previously granted ones no longer owned are revoked.
async fn sync(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<SyncReq>,
) -> Result<Json<Value>, ApiError> {
    if r.provider != musubi_auth::provider::STEAM {
        return Err(Error::InvalidArgument(
            "only steam entitlements are checked server-side; console entitlements are fed by the game server via POST /v1/server/entitlements/grant".into(),
        )
        .into());
    }
    let c = &s.cfg.iap.steam;
    if c.key.is_empty() {
        return Err(Error::InvalidArgument(
            "steam entitlements are not configured ([iap.steam] key)".into(),
        )
        .into());
    }
    let steam_id: Option<String> = sqlx::query_scalar(
        "select subject from musubi.identities where account_id = $1 and provider = 'steam' limit 1",
    )
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(steam_id) = steam_id else {
        return Err(Error::InvalidArgument("no steam identity linked".into()).into());
    };
    let catalog: Vec<String> = sqlx::query_scalar(
        "select external_id from musubi.entitlements where provider = 'steam' order by external_id",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(Error::internal)?;
    let url = format!(
        "{}/ISteamUser/CheckAppOwnership/v4/",
        c.url.trim_end_matches('/')
    );
    let (mut owned, mut granted, mut revoked) = (vec![], vec![], vec![]);
    for appid in catalog {
        let v: Value = http
            .get(&url)
            .query(&[
                ("key", c.key.as_str()),
                ("steamid", steam_id.as_str()),
                ("appid", appid.as_str()),
            ])
            .send()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {e}")))?
            .error_for_status()
            .map_err(|e| Error::Internal(format!("steam api: {e}")))?
            .json()
            .await
            .map_err(|e| Error::Internal(format!("steam api: {e}")))?;
        let owns = v["appownership"]["ownsapp"].as_bool().unwrap_or(false);
        if owns {
            owned.push(appid.clone());
            if grant(&s, me.account_id, "steam", &appid).await?.is_some() {
                granted.push(appid);
            }
        } else if revoke(&s, me.account_id, "steam", &appid, "not owned on steam").await? {
            revoked.push(appid);
        }
    }
    Ok(Json(
        json!({ "owned": owned, "granted": granted, "revoked": revoked }),
    ))
}

#[derive(Deserialize)]
struct ServerGrantReq {
    account_id: Uuid,
    provider: String,
    external_id: String,
    #[serde(default)]
    reason: String,
}

async fn server_grant(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<ServerGrantReq>,
) -> Result<Json<Value>, ApiError> {
    check_key(&r.provider, &r.external_id)?;
    let rewards = grant(&s, AccountId(r.account_id), &r.provider, &r.external_id).await?;
    Ok(Json(
        json!({ "granted": rewards.is_some(), "rewards": rewards }),
    ))
}

async fn server_revoke(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<ServerGrantReq>,
) -> Result<Json<Value>, ApiError> {
    check_key(&r.provider, &r.external_id)?;
    let reason = if r.reason.is_empty() {
        "revoked by server"
    } else {
        &r.reason
    };
    let revoked = revoke(
        &s,
        AccountId(r.account_id),
        &r.provider,
        &r.external_id,
        reason,
    )
    .await?;
    Ok(Json(json!({ "revoked": revoked })))
}
