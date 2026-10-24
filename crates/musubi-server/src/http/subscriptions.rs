//! Plan subscriptions (competitive P0-12): store-independent VIP / season-pass subscriptions.
//! A plan has a period, optional trial and grace, a wallet price (or none), per-period rewards
//! and feature tags; a subscription is paid from the wallet or prepaid by the server, renews on
//! the leader sweep, falls into grace when the wallet is short, can be paused/cancelled, and
//! `entitled(feature)` answers access checks. Store subscriptions (Apple/Google/…) stay in
//! `economy.rs`; `GET /v1/subscriptions` lists both.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/server/subscription_plans",
            get(list_plans).post(put_plan_body),
        )
        .route(
            "/v1/server/subscription_plans/{id}",
            get(get_plan).put(put_plan).delete(delete_plan),
        )
        .route(
            "/v1/server/subscriptions",
            get(server_list).post(server_grant),
        )
        .route("/v1/server/subscriptions/{id}/cancel", post(server_cancel))
        .route("/v1/server/subscriptions/{id}/pause", post(server_pause))
        .route("/v1/server/subscriptions/{id}/resume", post(server_resume))
        .route("/v1/server/subscriptions/{id}/revoke", post(server_revoke))
        .route("/v1/server/subscriptions/sweep", post(server_sweep))
        .route("/v1/subscription_plans", get(player_plans))
        .route("/v1/subscriptions", get(mine))
        .route("/v1/subscriptions/subscribe", post(subscribe))
        .route("/v1/subscriptions/entitled", get(entitled_q))
        .route("/v1/subscriptions/{id}/cancel", post(my_cancel))
        .route("/v1/subscriptions/{id}/resume", post(my_resume))
}

// ---------------------------------------------------------------- plans

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Price {
    pub currency: String,
    pub amount: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub period_secs: i64,
    #[serde(default)]
    pub grace_secs: i64,
    #[serde(default)]
    pub trial_secs: i64,
    #[serde(default)]
    pub price: Option<Price>,
    #[serde(default)]
    pub reward_table: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// Upper bound for operator-supplied spans so `now + span * periods` cannot overflow.
const MAX_SPAN_SECS: i64 = 10 * 365 * 24 * 3600;

/// Player-supplied subscription metadata is stored verbatim; keep it small.
const MAX_METADATA_BYTES: usize = 4096;

/// A `pending` subscription older than this never got its first charge finished (crash between
/// the claim and the wallet debit); it no longer blocks a new subscribe.
const PENDING_STALE_SECS: i64 = 600;

#[derive(Deserialize)]
pub struct PlanBody {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub period_secs: i64,
    #[serde(default)]
    pub grace_secs: i64,
    #[serde(default)]
    pub trial_secs: i64,
    #[serde(default)]
    pub price: Option<Price>,
    #[serde(default)]
    pub reward_table: Option<String>,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct PlanBodyWithId {
    pub id: String,
    #[serde(flatten)]
    pub body: PlanBody,
}

type PlanRow = (
    String,
    String,
    String,
    i64,
    i64,
    i64,
    Option<Value>,
    Option<String>,
    Vec<String>,
    Value,
    bool,
);

const PLAN_SELECT: &str = "select id, name, description, period_secs, grace_secs, trial_secs, price, reward_table, features, metadata, enabled from musubi.subscription_plans";

fn plan_of(r: PlanRow) -> Plan {
    let (
        id,
        name,
        description,
        period_secs,
        grace_secs,
        trial_secs,
        price,
        reward_table,
        features,
        metadata,
        enabled,
    ) = r;
    Plan {
        id,
        name,
        description,
        period_secs,
        grace_secs,
        trial_secs,
        price: price.and_then(|p| serde_json::from_value(p).ok()),
        reward_table,
        features,
        metadata,
        enabled,
    }
}

async fn load_plan(s: &AppState, id: &str) -> Result<Plan> {
    let row: Option<PlanRow> = sqlx::query_as(&format!("{PLAN_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    row.map(plan_of)
        .ok_or_else(|| Error::NotFound(format!("subscription plan {id}")))
}

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(Error::InvalidArgument(
            "plan id: [A-Za-z0-9_.-]{1,64}".into(),
        ));
    }
    Ok(())
}

async fn save_plan(s: &AppState, id: &str, b: PlanBody) -> Result<Plan> {
    valid_id(id)?;
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 chars".into()));
    }
    if b.period_secs < 60 {
        return Err(Error::InvalidArgument("period_secs must be ≥ 60".into()));
    }
    if b.grace_secs < 0 || b.trial_secs < 0 {
        return Err(Error::InvalidArgument(
            "grace_secs / trial_secs must be ≥ 0".into(),
        ));
    }
    if b.period_secs > MAX_SPAN_SECS || b.grace_secs > MAX_SPAN_SECS || b.trial_secs > MAX_SPAN_SECS
    {
        return Err(Error::InvalidArgument(
            "period_secs / grace_secs / trial_secs must be ≤ 10 years".into(),
        ));
    }
    if b.features.len() > 32 || b.features.iter().any(|f| f.is_empty() || f.len() > 64) {
        return Err(Error::InvalidArgument(
            "features: ≤ 32 tags of ≤ 64 chars".into(),
        ));
    }
    if let Some(p) = &b.price {
        if p.amount <= 0 {
            return Err(Error::InvalidArgument(
                "price.amount must be positive".into(),
            ));
        }
        if !s
            .economy
            .currencies()
            .await?
            .iter()
            .any(|c| c.code == p.currency)
        {
            return Err(Error::NotFound(format!("currency {}", p.currency)));
        }
    }
    if let Some(t) = &b.reward_table {
        if !s
            .economy
            .reward_tables(true)
            .await?
            .iter()
            .any(|r| r.id == *t)
        {
            return Err(Error::NotFound(format!("reward table {t}")));
        }
    }
    let price = b
        .price
        .as_ref()
        .map(|p| serde_json::to_value(p).unwrap_or(Value::Null));
    sqlx::query(
        "insert into musubi.subscription_plans
            (id, name, description, period_secs, grace_secs, trial_secs, price, reward_table, features, metadata, enabled)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (id) do update set name = excluded.name, description = excluded.description,
            period_secs = excluded.period_secs, grace_secs = excluded.grace_secs,
            trial_secs = excluded.trial_secs, price = excluded.price,
            reward_table = excluded.reward_table, features = excluded.features,
            metadata = excluded.metadata, enabled = excluded.enabled, updated_at = now()",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(b.period_secs)
    .bind(b.grace_secs)
    .bind(b.trial_secs)
    .bind(price)
    .bind(&b.reward_table)
    .bind(&b.features)
    .bind(if b.metadata.is_null() { json!({}) } else { b.metadata })
    .bind(b.enabled)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    load_plan(s, id).await
}

async fn list_plans(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<PlanRow> = sqlx::query_as(&format!("{PLAN_SELECT} order by id"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(
        json!({ "plans": rows.into_iter().map(plan_of).collect::<Vec<_>>() }),
    ))
}

/// Player view: enabled plans only.
async fn player_plans(
    State(s): State<AppState>,
    Bearer(_): Bearer,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<PlanRow> = sqlx::query_as(&format!("{PLAN_SELECT} where enabled order by id"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(
        json!({ "plans": rows.into_iter().map(plan_of).collect::<Vec<_>>() }),
    ))
}

async fn get_plan(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Plan>, ApiError> {
    Ok(Json(load_plan(&s, &id).await?))
}

async fn put_plan(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<PlanBody>,
) -> std::result::Result<Json<Plan>, ApiError> {
    Ok(Json(save_plan(&s, &id, b).await?))
}

async fn put_plan_body(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<PlanBodyWithId>,
) -> std::result::Result<Json<Plan>, ApiError> {
    Ok(Json(save_plan(&s, &b.id, b.body).await?))
}

async fn delete_plan(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.subscription_plans where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

// ---------------------------------------------------------------- subscriptions

#[derive(Debug, Clone, Serialize)]
pub struct Subscription {
    pub id: Uuid,
    pub account_id: Uuid,
    pub plan_id: String,
    pub status: String,
    pub source: String,
    pub started_at: i64,
    pub current_period_end: i64,
    pub grace_until: Option<i64>,
    pub cancel_at_period_end: bool,
    pub periods: i32,
    pub periods_left: i32,
    pub metadata: Value,
    /// Access right now (trial / active / grace inside its window).
    pub active: bool,
}

type SubRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    i64,
    i64,
    Option<i64>,
    bool,
    i32,
    i32,
    Value,
);

const SUB_SELECT: &str = "select id, account_id, plan_id, status, source, started_at, current_period_end, grace_until, cancel_at_period_end, periods, periods_left, metadata from musubi.subscriptions";

fn now_s() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn sub_of(r: SubRow) -> Subscription {
    let (
        id,
        account_id,
        plan_id,
        status,
        source,
        started_at,
        current_period_end,
        grace_until,
        cancel_at_period_end,
        periods,
        periods_left,
        metadata,
    ) = r;
    let now = now_s();
    let active = match status.as_str() {
        "trial" | "active" => true,
        "grace" => grace_until.is_some_and(|g| g > now),
        _ => false,
    };
    Subscription {
        id,
        account_id,
        plan_id,
        status,
        source,
        started_at,
        current_period_end,
        grace_until,
        cancel_at_period_end,
        periods,
        periods_left,
        metadata,
        active,
    }
}

async fn load_sub(s: &AppState, id: Uuid) -> Result<Subscription> {
    let row: Option<SubRow> = sqlx::query_as(&format!("{SUB_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    row.map(sub_of)
        .ok_or_else(|| Error::NotFound("subscription".into()))
}

async fn subs_of(s: &AppState, account: Uuid) -> Result<Vec<Subscription>> {
    let rows: Vec<SubRow> = sqlx::query_as(&format!(
        "{SUB_SELECT} where account_id = $1 order by created_at desc"
    ))
    .bind(account)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(rows.into_iter().map(sub_of).collect())
}

/// Does the account hold an active plan subscription carrying `feature`?
pub async fn entitled(s: &AppState, account: AccountId, feature: &str) -> Result<bool> {
    sqlx::query_scalar(
        "select exists(
            select 1 from musubi.subscriptions x join musubi.subscription_plans p on p.id = x.plan_id
            where x.account_id = $1 and $2 = any(p.features)
              and (x.status in ('trial', 'active') or (x.status = 'grace' and x.grace_until > $3)))",
    )
    .bind(account.0)
    .bind(feature)
    .bind(now_s())
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)
}

async fn emit(s: &AppState, kind: &str, sub: &Subscription, extra: Value) {
    let mut payload = serde_json::to_value(sub).unwrap_or_default();
    if let (Some(o), Some(e)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    s.events
        .emit(
            &format!("subscription.{kind}"),
            Some(AccountId(sub.account_id)),
            payload,
        )
        .await;
}

async fn grant_period_rewards(s: &AppState, plan: &Plan, sub_id: Uuid, account: Uuid, period: i64) {
    if let Some(t) = &plan.reward_table {
        let reference = format!("subscription:{sub_id}:{period}");
        if let Err(e) = s
            .economy
            .grant_table(AccountId(account), t, &reference)
            .await
        {
            tracing::warn!(error = %e, table = %t, "subscription reward grant failed");
        }
    }
}

/// Charge one period from the wallet; `Ok(false)` = insufficient funds.
async fn charge(
    s: &AppState,
    plan: &Plan,
    sub_id: Uuid,
    account: Uuid,
    period: i64,
) -> Result<bool> {
    let Some(p) = &plan.price else {
        return Ok(true);
    };
    let key = format!("subscription:{sub_id}:{period}");
    match s
        .economy
        .adjust(
            AccountId(account),
            &p.currency,
            -p.amount,
            "subscription",
            &key,
            Some(&key),
            json!({ "plan_id": plan.id, "subscription_id": sub_id, "period": period }),
        )
        .await
    {
        Ok(_) => Ok(true),
        Err(Error::ResourceExhausted(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

#[derive(Deserialize)]
pub struct SubscribeBody {
    pub plan_id: String,
    #[serde(default)]
    pub metadata: Value,
}

/// `POST /v1/subscriptions/subscribe {plan_id}` — trial if the plan has one and the account never
/// had it, otherwise the first period is charged now.
async fn subscribe(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<SubscribeBody>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    let plan = load_plan(&s, &b.plan_id).await?;
    if !plan.enabled {
        return Err(Error::NotFound(format!("subscription plan {}", plan.id)).into());
    }
    let metadata = if b.metadata.is_null() {
        json!({})
    } else {
        b.metadata
    };
    if metadata.to_string().len() > MAX_METADATA_BYTES {
        return Err(
            Error::InvalidArgument(format!("metadata: ≤ {MAX_METADATA_BYTES} bytes")).into(),
        );
    }
    let id = Uuid::now_v7();
    let now = now_s();
    // The row is claimed first (status `pending`, no access) under a per-(account, plan) lock,
    // so two concurrent subscribes cannot both pass the "one live subscription" check and both
    // charge the wallet; the charge then activates it, and a failed charge removes the claim.
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('subscription'), hashtext($1))")
        .bind(format!("{}:{}", me.account_id.0, plan.id))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    sqlx::query(
        "delete from musubi.subscriptions where account_id = $1 and plan_id = $2
         and status = 'pending' and started_at < $3",
    )
    .bind(me.account_id.0)
    .bind(&plan.id)
    .bind(now - PENDING_STALE_SECS)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let (existing, ever): (bool, bool) = sqlx::query_as(
        "select exists(select 1 from musubi.subscriptions where account_id = $1 and plan_id = $2
                        and status in ('trial', 'active', 'grace', 'paused', 'pending')),
                exists(select 1 from musubi.subscriptions where account_id = $1 and plan_id = $2
                        and status <> 'pending')",
    )
    .bind(me.account_id.0)
    .bind(&plan.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(Error::internal)?;
    if existing {
        return Err(Error::Conflict("already subscribed".into()).into());
    }
    let trial = plan.trial_secs > 0 && !ever;
    let (status, end) = if trial {
        ("trial", now + plan.trial_secs)
    } else {
        ("pending", now + plan.period_secs)
    };
    sqlx::query(
        "insert into musubi.subscriptions (id, account_id, plan_id, status, source, started_at, current_period_end, periods, periods_left, metadata)
         values ($1, $2, $3, $4, 'wallet', $5, $6, 1, 0, $7)",
    )
    .bind(id)
    .bind(me.account_id.0)
    .bind(&plan.id)
    .bind(status)
    .bind(now)
    .bind(end)
    .bind(metadata)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    if !trial {
        let paid = match charge(&s, &plan, id, me.account_id.0, 1).await {
            Ok(paid) => paid,
            Err(e) => {
                drop_pending(&s, id).await;
                return Err(e.into());
            }
        };
        if !paid {
            drop_pending(&s, id).await;
            return Err(Error::ResourceExhausted(format!(
                "insufficient {} for plan {}",
                plan.price
                    .as_ref()
                    .map(|p| p.currency.as_str())
                    .unwrap_or(""),
                plan.id
            ))
            .into());
        }
        sqlx::query(
            "update musubi.subscriptions set status = 'active', updated_at = now()
             where id = $1 and status = 'pending'",
        )
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        grant_period_rewards(&s, &plan, id, me.account_id.0, 1).await;
    }
    let sub = load_sub(&s, id).await?;
    emit(&s, "started", &sub, json!({ "trial": status == "trial" })).await;
    Ok(Json(sub))
}

/// Remove a claim whose first charge did not go through.
async fn drop_pending(s: &AppState, id: Uuid) {
    if let Err(e) =
        sqlx::query("delete from musubi.subscriptions where id = $1 and status = 'pending'")
            .bind(id)
            .execute(&s.db)
            .await
    {
        tracing::warn!(error = %e, %id, "could not drop pending subscription");
    }
}

async fn mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "plans": subs_of(&s, me.account_id.0).await?,
        "store": s.economy.subscriptions_of(me.account_id).await?,
    })))
}

#[derive(Deserialize)]
pub struct EntitledQ {
    pub feature: String,
}

async fn entitled_q(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<EntitledQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "feature": q.feature, "entitled": entitled(&s, me.account_id, &q.feature).await?,
    })))
}

async fn set_cancel(s: &AppState, id: Uuid, owner: Option<Uuid>, by: &str) -> Result<Subscription> {
    let sub = load_sub(s, id).await?;
    if owner.is_some_and(|o| o != sub.account_id) {
        return Err(Error::NotFound("subscription".into()));
    }
    match sub.status.as_str() {
        "trial" | "active" | "grace" | "paused" => {}
        _ => return Err(Error::Conflict(format!("subscription is {}", sub.status))),
    }
    sqlx::query(
        "update musubi.subscriptions set cancel_at_period_end = true, updated_at = now() where id = $1",
    )
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let sub = load_sub(s, id).await?;
    emit(s, "cancel_scheduled", &sub, json!({ "by": by })).await;
    Ok(sub)
}

async fn unset_cancel(s: &AppState, id: Uuid, owner: Option<Uuid>) -> Result<Subscription> {
    let sub = load_sub(s, id).await?;
    if owner.is_some_and(|o| o != sub.account_id) {
        return Err(Error::NotFound("subscription".into()));
    }
    sqlx::query(
        "update musubi.subscriptions set cancel_at_period_end = false, updated_at = now()
         where id = $1 and status in ('trial', 'active', 'grace')",
    )
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    load_sub(s, id).await
}

/// `POST /v1/subscriptions/{id}/cancel` — keeps access until the period ends, no renewal.
async fn my_cancel(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    Ok(Json(
        set_cancel(&s, id, Some(me.account_id.0), "player").await?,
    ))
}

/// `POST /v1/subscriptions/{id}/resume` — undo a scheduled cancellation.
async fn my_resume(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    Ok(Json(unset_cancel(&s, id, Some(me.account_id.0)).await?))
}

#[derive(Deserialize)]
pub struct AccountQ {
    pub account_id: Uuid,
}

async fn server_list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<AccountQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "subscriptions": subs_of(&s, q.account_id).await? }),
    ))
}

#[derive(Deserialize)]
pub struct GrantBody {
    pub account_id: Uuid,
    pub plan_id: String,
    /// Prepaid periods (default 1); when they run out the subscription expires (or, for a plan
    /// with a price, keeps renewing from the wallet if `continue_from_wallet`).
    #[serde(default = "one")]
    pub periods: i32,
    #[serde(default)]
    pub continue_from_wallet: bool,
    #[serde(default)]
    pub metadata: Value,
}

fn one() -> i32 {
    1
}

/// `POST /v1/server/subscriptions {account_id, plan_id, periods}` — grant (or extend) a
/// subscription without payment: promotions, support, prepaid codes.
async fn server_grant(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<GrantBody>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    if !(1..=1000).contains(&b.periods) {
        return Err(Error::InvalidArgument("periods: 1..1000".into()).into());
    }
    let plan = load_plan(&s, &b.plan_id).await?;
    // a `pending` player claim counts as existing so the grant never creates a second row
    let existing: Option<(Uuid, String, i64)> = sqlx::query_as(
        "select id, status, current_period_end from musubi.subscriptions
         where account_id = $1 and plan_id = $2 and status in ('trial', 'active', 'grace', 'paused', 'pending')",
    )
    .bind(b.account_id)
    .bind(&plan.id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = now_s();
    let source = if b.continue_from_wallet {
        "wallet"
    } else {
        "server"
    };
    let id = if let Some((id, status, end)) = existing {
        // extend: prepaid periods stack on top of whatever is left
        let base = if status == "paused" {
            now
        } else {
            end.max(now)
        };
        sqlx::query(
            "update musubi.subscriptions set status = 'active', current_period_end = $2,
                periods_left = periods_left + $3, periods = periods + $3, grace_until = null,
                paused_remaining_secs = null, updated_at = now()
             where id = $1",
        )
        .bind(id)
        .bind(
            base + plan.period_secs * (b.periods as i64 - 1)
                + if status == "paused" {
                    plan.period_secs
                } else {
                    0
                },
        )
        .bind(if status == "paused" {
            b.periods - 1
        } else {
            b.periods
        })
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        id
    } else {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into musubi.subscriptions (id, account_id, plan_id, status, source, started_at, current_period_end, periods, periods_left, metadata)
             values ($1, $2, $3, 'active', $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(b.account_id)
        .bind(&plan.id)
        .bind(source)
        .bind(now)
        .bind(now + plan.period_secs)
        .bind(b.periods)
        .bind(b.periods - 1)
        .bind(if b.metadata.is_null() { json!({}) } else { b.metadata })
        .execute(&s.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => Error::NotFound("account".into()),
            e => Error::internal(e),
        })?;
        grant_period_rewards(&s, &plan, id, b.account_id, 1).await;
        id
    };
    let sub = load_sub(&s, id).await?;
    emit(&s, "granted", &sub, json!({ "periods": b.periods })).await;
    Ok(Json(sub))
}

async fn server_cancel(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    Ok(Json(set_cancel(&s, id, None, "server").await?))
}

/// `POST /v1/server/subscriptions/{id}/pause` — freeze the remaining time; no renewals.
async fn server_pause(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    let sub = load_sub(&s, id).await?;
    if !matches!(sub.status.as_str(), "trial" | "active") {
        return Err(Error::Conflict(format!("subscription is {}", sub.status)).into());
    }
    sqlx::query(
        "update musubi.subscriptions set status = 'paused', paused_remaining_secs = greatest(current_period_end - $2, 0),
            updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(now_s())
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let sub = load_sub(&s, id).await?;
    emit(&s, "paused", &sub, json!({})).await;
    Ok(Json(sub))
}

async fn server_resume(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    let sub = load_sub(&s, id).await?;
    if sub.status != "paused" {
        return Ok(Json(unset_cancel(&s, id, None).await?));
    }
    sqlx::query(
        "update musubi.subscriptions set status = 'active',
            current_period_end = $2 + coalesce(paused_remaining_secs, 0),
            paused_remaining_secs = null, updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(now_s())
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let sub = load_sub(&s, id).await?;
    emit(&s, "resumed", &sub, json!({})).await;
    Ok(Json(sub))
}

/// `POST /v1/server/subscriptions/{id}/revoke` — end access immediately (fraud, refund).
async fn server_revoke(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Subscription>, ApiError> {
    let sub = load_sub(&s, id).await?;
    if matches!(sub.status.as_str(), "expired" | "cancelled" | "revoked") {
        return Err(Error::Conflict(format!("subscription is {}", sub.status)).into());
    }
    sqlx::query(
        "update musubi.subscriptions set status = 'revoked', updated_at = now() where id = $1",
    )
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let sub = load_sub(&s, id).await?;
    emit(&s, "revoked", &sub, json!({})).await;
    Ok(Json(sub))
}

async fn server_sweep(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "changed": sweep(&s).await? })))
}

/// Renewals, grace and expiry for plan subscriptions whose period ended (leader, every minute).
pub async fn sweep(s: &AppState) -> Result<usize> {
    let now = now_s();
    let due: Vec<(Uuid, String)> = sqlx::query_as(
        "select id, status from musubi.subscriptions
         where (status in ('trial', 'active') and current_period_end <= $1)
            or (status = 'grace' and grace_until <= $1)
         order by current_period_end limit 500",
    )
    .bind(now)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut changed = 0;
    for (id, _) in due {
        let sub = load_sub(s, id).await?;
        let plan = match load_plan(s, &sub.plan_id).await {
            Ok(p) => p,
            Err(_) => continue,
        };
        let period = sub.periods + 1;
        let end_base = sub.current_period_end.max(now - plan.period_secs);
        let finish = |status: &'static str| async move {
            sqlx::query("update musubi.subscriptions set status = $2, grace_until = null, updated_at = now() where id = $1")
                .bind(id)
                .bind(status)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
            let sub = load_sub(s, id).await?;
            emit(s, status, &sub, json!({})).await;
            Ok::<(), Error>(())
        };
        if sub.cancel_at_period_end {
            finish("cancelled").await?;
            changed += 1;
            continue;
        }
        let plan_r = &plan;
        let account = sub.account_id;
        let renew = |paid: &'static str| async move {
            sqlx::query(
                "update musubi.subscriptions set status = 'active', current_period_end = $2,
                    periods = periods + 1, periods_left = greatest(periods_left - $3, 0),
                    grace_until = null, updated_at = now() where id = $1",
            )
            .bind(id)
            .bind(end_base + plan_r.period_secs)
            .bind(if paid == "prepaid" { 1 } else { 0 })
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
            grant_period_rewards(s, plan_r, id, account, period as i64).await;
            let sub = load_sub(s, id).await?;
            emit(s, "renewed", &sub, json!({ "paid": paid })).await;
            Ok::<(), Error>(())
        };
        if sub.periods_left > 0 {
            renew("prepaid").await?;
            changed += 1;
            continue;
        }
        let can_pay = sub.source == "wallet" || plan.price.is_none();
        if can_pay && charge(s, &plan, id, sub.account_id, period as i64).await? {
            renew(if plan.price.is_some() {
                "wallet"
            } else {
                "free"
            })
            .await?;
            changed += 1;
            continue;
        }
        if sub.status != "grace" && plan.grace_secs > 0 {
            sqlx::query(
                "update musubi.subscriptions set status = 'grace', grace_until = $2, updated_at = now() where id = $1",
            )
            .bind(id)
            .bind(sub.current_period_end.max(now) + plan.grace_secs)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
            let sub = load_sub(s, id).await?;
            emit(s, "grace", &sub, json!({ "reason": if can_pay { "insufficient_funds" } else { "prepaid_exhausted" } })).await;
        } else {
            finish("expired").await?;
        }
        changed += 1;
    }
    Ok(changed)
}
