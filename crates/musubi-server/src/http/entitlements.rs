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
        .route(
            "/v1/server/accounts/{id}/entitlements",
            get(account_entitlements),
        )
        .route("/v1/server/entitlements/grant", post(server_grant))
        .route("/v1/server/entitlements/revoke", post(server_revoke))
        .route("/v1/server/entitlements/sweep", post(server_sweep))
}

#[derive(Deserialize)]
struct Entitlement {
    provider: String,
    external_id: String,
    reward_table: String,
    #[serde(default)]
    label: String,
    /// `full` (default) | `trial` | `free_weekend` | `demo` | `subscription` — the license type
    /// grants carry unless the grant says otherwise.
    #[serde(default)]
    license: Option<String>,
    /// Default lifetime of a non-`full` grant (seconds); the grant may override.
    #[serde(default)]
    trial_secs: Option<i64>,
}

const LICENSES: &[&str] = &["full", "trial", "free_weekend", "demo", "subscription"];

fn check_license(l: &str) -> Result<(), ApiError> {
    if !LICENSES.contains(&l) {
        return Err(Error::InvalidArgument(
            "license must be full|trial|free_weekend|demo|subscription".into(),
        )
        .into());
    }
    Ok(())
}

fn check_secs(v: Option<i64>, what: &str) -> Result<(), ApiError> {
    if v.is_some_and(|d| !(60..=366 * 86_400).contains(&d)) {
        return Err(Error::InvalidArgument(format!("{what} must be 60..=31622400 seconds")).into());
    }
    Ok(())
}

/// Grant options beyond the catalog defaults.
#[derive(Default, Clone)]
pub struct GrantOpts {
    pub license: Option<String>,
    pub expires_at: Option<time::OffsetDateTime>,
    pub source: Option<String>,
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
    let license = e.license.clone().unwrap_or_else(|| "full".into());
    check_license(&license)?;
    check_secs(e.trial_secs, "trial_secs")?;
    if license == "full" && e.trial_secs.is_some() {
        return Err(Error::InvalidArgument("trial_secs needs a non-full license".into()).into());
    }
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
        "insert into musubi.entitlements (provider, external_id, reward_table, label, license, trial_secs)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (provider, external_id) do update set reward_table = excluded.reward_table,
             label = excluded.label, license = excluded.license, trial_secs = excluded.trial_secs",
    )
    .bind(&e.provider)
    .bind(&e.external_id)
    .bind(&e.reward_table)
    .bind(&e.label)
    .bind(&license)
    .bind(e.trial_secs)
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
    grant_with(s, account, provider, external_id, GrantOpts::default()).await
}

/// Grant with a license type / expiry. A live limited grant (trial, free weekend…) is upgraded
/// in place when a `full` (or later-expiring) grant arrives — no second reward delivery; an
/// expired or revoked grant is re-activated (rewards are delivered again only if the reward
/// table is repeatable, as before).
async fn grant_with(
    s: &AppState,
    account: AccountId,
    provider: &str,
    external_id: &str,
    opts: GrantOpts,
) -> Result<Option<Vec<musubi_economy::RewardEntry>>, ApiError> {
    let cat: Option<(String, String, Option<i64>)> = sqlx::query_as(
        "select reward_table, license, trial_secs from musubi.entitlements where provider = $1 and external_id = $2",
    )
    .bind(provider)
    .bind(external_id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((table, cat_license, trial_secs)) = cat else {
        return Err(Error::NotFound(format!("entitlement {provider}:{external_id}")).into());
    };
    let license = opts.license.clone().unwrap_or(cat_license);
    check_license(&license)?;
    let expires_at = match (opts.expires_at, license.as_str(), trial_secs) {
        (Some(t), _, _) => Some(t),
        (None, "full", _) => None,
        (None, _, Some(d)) => Some(time::OffsetDateTime::now_utc() + time::Duration::seconds(d)),
        (None, _, None) => None,
    };
    if expires_at.is_some_and(|t| t <= time::OffsetDateTime::now_utc()) {
        return Err(Error::InvalidArgument("expires_at must be in the future".into()).into());
    }
    // upgrade / extend a live limited grant in place
    let upgraded: Option<(String,)> = sqlx::query_as(
        "update musubi.entitlement_grants g set license = $4, expires_at = $5, source = coalesce($6, g.source)
         where g.account_id = $1 and g.provider = $2 and g.external_id = $3 and g.revoked_at is null
           and (g.expires_at is null or g.expires_at > now())
           and (g.license <> 'full')
           and ($4 = 'full'
                or ($5::timestamptz is null and g.expires_at is not null)
                or ($5::timestamptz is not null and g.expires_at is not null
                    and $5 > g.expires_at + interval '60 seconds'))
         returning g.license",
    )
    .bind(account.0)
    .bind(provider)
    .bind(external_id)
    .bind(&license)
    .bind(expires_at)
    .bind(&opts.source)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if upgraded.is_some() {
        s.events
            .emit(
                "entitlement.upgraded",
                Some(account),
                json!({ "provider": provider, "external_id": external_id, "license": license,
                    "expires_at": expires_at.map(|t| t.unix_timestamp()) }),
            )
            .await;
        return Ok(Some(Vec::new()));
    }
    let n = sqlx::query(
        "insert into musubi.entitlement_grants (account_id, provider, external_id, license, expires_at, source)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (account_id, provider, external_id) do update set granted_at = now(), revoked_at = null,
             license = excluded.license, expires_at = excluded.expires_at, source = excluded.source
         where musubi.entitlement_grants.revoked_at is not null
            or (musubi.entitlement_grants.expires_at is not null and musubi.entitlement_grants.expires_at <= now())",
    )
    .bind(account.0)
    .bind(provider)
    .bind(external_id)
    .bind(&license)
    .bind(expires_at)
    .bind(&opts.source)
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
            json!({ "provider": provider, "external_id": external_id, "reward_table": table, "rewards": rewards,
                "license": license, "expires_at": expires_at.map(|t| t.unix_timestamp()) }),
        )
        .await;
    Ok(Some(rewards))
}

/// Expire due limited grants (`revoked_at = expires_at`) and emit `entitlement.expired`.
pub async fn sweep(s: &AppState) -> Result<usize, ApiError> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "update musubi.entitlement_grants set revoked_at = expires_at
         where revoked_at is null and expires_at is not null and expires_at <= now()
         returning account_id, provider, external_id, license",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    for (acc, p, e, l) in &rows {
        s.events
            .emit(
                "entitlement.expired",
                Some(AccountId(*acc)),
                json!({ "provider": p, "external_id": e, "license": l }),
            )
            .await;
    }
    Ok(rows.len())
}

async fn server_sweep(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "expired": sweep(&s).await? })))
}

/// Background expiry (cluster-wide: only the lease holder runs it).
pub fn spawn_sweeper(state: AppState, held: impl Fn() -> bool + Send + 'static) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tick.tick().await;
            if held() {
                if let Err(e) = sweep(&state).await {
                    tracing::warn!(error = ?e.0, "entitlement sweep failed");
                }
            }
        }
    });
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

type GrantRow = (
    String,
    String,
    time::OffsetDateTime,
    Option<time::OffsetDateTime>,
    String,
    Option<time::OffsetDateTime>,
    Option<String>,
);

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    grants_of(&s, me.account_id).await
}

/// Operator view of one player's grants (console / support), same shape as `GET /v1/entitlements`.
async fn account_entitlements(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = Uuid::parse_str(&id)
        .map(AccountId)
        .map_err(|_| ApiError::from(Error::InvalidArgument("bad account id".into())))?;
    grants_of(&s, id).await
}

async fn grants_of(s: &AppState, account_id: AccountId) -> Result<Json<Value>, ApiError> {
    let rows: Vec<GrantRow> = sqlx::query_as(
        "select provider, external_id, granted_at, revoked_at, license, expires_at, source
             from musubi.entitlement_grants where account_id = $1 order by granted_at",
    )
    .bind(account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = time::OffsetDateTime::now_utc();
    Ok(Json(json!({
        "entitlements": rows.into_iter().map(|(p, e, g, r, license, exp, source)| {
            let expired = exp.is_some_and(|t| t <= now);
            json!({
                "provider": p, "external_id": e,
                "granted_at": g.format(&time::format_description::well_known::Rfc3339).ok(),
                "revoked_at": r.and_then(|r| r.format(&time::format_description::well_known::Rfc3339).ok()),
                "license": license, "source": source,
                "expires_at": exp.map(|t| t.unix_timestamp()),
                "remaining_secs": exp.map(|t| (t - now).whole_seconds().max(0)),
                "expired": expired,
                "active": r.is_none() && !expired,
            })
        }).collect::<Vec<_>>()
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
    // reqwest errors carry the request URL, and the URL carries the publisher key: strip it
    // before the error reaches the log
    let steam_err = |e: reqwest::Error| Error::Internal(format!("steam api: {}", e.without_url()));
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
            .map_err(steam_err)?
            .error_for_status()
            .map_err(steam_err)?
            .json()
            .await
            .map_err(steam_err)?;
        let owns = v["appownership"]["ownsapp"].as_bool();
        match owns {
            Some(true) => {
                owned.push(appid.clone());
                if grant(&s, me.account_id, "steam", &appid).await?.is_some() {
                    granted.push(appid);
                }
            }
            Some(false) => {
                if revoke(&s, me.account_id, "steam", &appid, "not owned on steam").await? {
                    revoked.push(appid);
                }
            }
            // an answer without the ownership flag is not a "not owned": keep the grant
            None => {
                tracing::warn!(appid, "steam CheckAppOwnership answered without ownsapp");
            }
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
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    duration_secs: Option<i64>,
    #[serde(default)]
    source: Option<String>,
}

async fn server_grant(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<ServerGrantReq>,
) -> Result<Json<Value>, ApiError> {
    check_key(&r.provider, &r.external_id)?;
    if let Some(l) = &r.license {
        check_license(l)?;
    }
    check_secs(r.duration_secs, "duration_secs")?;
    if r.expires_at.is_some() && r.duration_secs.is_some() {
        return Err(
            Error::InvalidArgument("expires_at and duration_secs are exclusive".into()).into(),
        );
    }
    if r.source.as_ref().is_some_and(|x| x.len() > 64) {
        return Err(Error::InvalidArgument("source exceeds 64 chars".into()).into());
    }
    let expires_at = match (r.expires_at, r.duration_secs) {
        (Some(t), _) => Some(
            time::OffsetDateTime::from_unix_timestamp(t)
                .map_err(|_| Error::InvalidArgument("expires_at".into()))?,
        ),
        (None, Some(d)) => Some(time::OffsetDateTime::now_utc() + time::Duration::seconds(d)),
        _ => None,
    };
    let opts = GrantOpts {
        license: r.license.clone(),
        expires_at,
        source: r.source.clone(),
    };
    let rewards = grant_with(
        &s,
        AccountId(r.account_id),
        &r.provider,
        &r.external_id,
        opts,
    )
    .await?;
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
