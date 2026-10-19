//! Group economies (P1-54): contribution pools (community drives with reward tiers), member
//! donation requests fulfilled wallet-to-wallet, and a team store where the treasury buys items for
//! members. Built on the group treasury (`crate::http::groups`).

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, GroupId, Result};
use musubi_social::groups::perm;
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::groups::groups;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/groups/{id}/pools", get(list_pools).post(create_pool))
        .route("/v1/groups/{id}/pools/{pid}", get(get_pool))
        .route("/v1/groups/{id}/pools/{pid}/contribute", post(contribute))
        .route("/v1/groups/{id}/pools/{pid}/close", post(close_pool))
        .route(
            "/v1/groups/{id}/donations",
            get(list_donations).post(request_donation),
        )
        .route("/v1/groups/{id}/donations/{did}/give", post(give))
        .route(
            "/v1/groups/{id}/donations/{did}/cancel",
            post(cancel_donation),
        )
        .route("/v1/groups/{id}/store", get(store))
        .route("/v1/groups/{id}/store/{item}", put(store_put))
        .route("/v1/groups/{id}/store/{item}/buy", post(store_buy))
}

#[derive(sqlx::FromRow)]
struct Pool {
    id: Uuid,
    group_id: Uuid,
    name: String,
    currency: String,
    goal: i64,
    raised: i64,
    tiers: Value,
    contributor_reward_table: Option<String>,
    status: String,
    ends_at: Option<OffsetDateTime>,
    created_by: Uuid,
    created_at: OffsetDateTime,
    completed_at: Option<OffsetDateTime>,
}

const POOL_COLS: &str = "id, group_id, name, currency, goal, raised, tiers, contributor_reward_table, status, ends_at, created_by, created_at, completed_at";

fn pool_json(p: &Pool) -> Value {
    json!({
        "id": p.id, "group_id": p.group_id, "name": p.name, "currency": p.currency, "goal": p.goal,
        "raised": p.raised, "progress": if p.goal > 0 { (p.raised as f64 / p.goal as f64).min(1.0) } else { 1.0 },
        "tiers": p.tiers, "contributor_reward_table": p.contributor_reward_table, "status": p.status,
        "ends_at": p.ends_at.map(|t| t.unix_timestamp()), "created_by": p.created_by,
        "created_at": p.created_at.unix_timestamp(), "completed_at": p.completed_at.map(|t| t.unix_timestamp()),
    })
}

async fn load_pool(s: &AppState, group: GroupId, pid: Uuid) -> Result<Pool> {
    sqlx::query_as(&format!(
        "select {POOL_COLS} from musubi.group_pools where id = $1 and group_id = $2"
    ))
    .bind(pid)
    .bind(group.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("pool {pid}")))
}

#[derive(Deserialize)]
struct CreatePool {
    name: String,
    currency: String,
    goal: i64,
    /// `[{at: <raised amount>, reward_table: <id>}]` — every contributor is rewarded once per tier.
    #[serde(default)]
    tiers: Vec<Tier>,
    /// Granted to each contributor on their first contribution.
    #[serde(default)]
    contributor_reward_table: Option<String>,
    #[serde(default)]
    ends_at: Option<i64>,
}

#[derive(Deserialize, serde::Serialize, Clone)]
struct Tier {
    at: i64,
    reward_table: String,
}

/// `POST /v1/groups/{id}/pools` (treasury-withdraw permission = officers).
async fn create_pool(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
    Json(b): Json<CreatePool>,
) -> Result<Json<Value>, ApiError> {
    groups(&s)
        .check_perm(me.account_id, id, perm::TREASURY_WITHDRAW)
        .await?;
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128".into()).into());
    }
    if b.goal <= 0 {
        return Err(Error::InvalidArgument("goal must be positive".into()).into());
    }
    if b.tiers.len() > 16
        || b.tiers
            .iter()
            .any(|t| t.at <= 0 || t.reward_table.is_empty())
        || b.tiers.windows(2).any(|w| w[0].at >= w[1].at)
    {
        return Err(Error::InvalidArgument(
            "tiers: ascending positive `at` with reward_table, at most 16".into(),
        )
        .into());
    }
    let open: i64 = sqlx::query_scalar(
        "select count(*) from musubi.group_pools where group_id = $1 and status = 'open'",
    )
    .bind(id.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if open >= 8 {
        return Err(Error::ResourceExhausted("at most 8 open pools per group".into()).into());
    }
    let pid = Uuid::now_v7();
    let p: Pool = sqlx::query_as(&format!(
        "insert into musubi.group_pools (id, group_id, name, currency, goal, tiers, contributor_reward_table, ends_at, created_by)
         values ($1, $2, $3, $4, $5, $6, $7, to_timestamp($8), $9) returning {POOL_COLS}"
    ))
    .bind(pid)
    .bind(id.0)
    .bind(b.name.trim())
    .bind(&b.currency)
    .bind(b.goal)
    .bind(serde_json::to_value(&b.tiers).unwrap_or(json!([])))
    .bind(&b.contributor_reward_table)
    .bind(b.ends_at.map(|t| t as f64))
    .bind(me.account_id.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "group.pool_created",
            Some(me.account_id),
            json!({ "group_id": id, "pool_id": pid, "goal": b.goal, "currency": b.currency }),
        )
        .await;
    Ok(Json(pool_json(&p)))
}

async fn list_pools(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let rows: Vec<Pool> = sqlx::query_as(&format!(
        "select {POOL_COLS} from musubi.group_pools where group_id = $1 order by (status = 'open') desc, created_at desc limit 100"
    ))
    .bind(id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "pools": rows.iter().map(pool_json).collect::<Vec<_>>() }),
    ))
}

async fn get_pool(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, pid)): Path<(GroupId, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let p = load_pool(&s, id, pid).await?;
    let top: Vec<(Uuid, i64, i64)> = sqlx::query_as(
        "select account_id, sum(amount)::bigint, count(*) from musubi.group_pool_contributions
         where pool_id = $1 group by account_id order by 2 desc limit 50",
    )
    .bind(pid)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut v = pool_json(&p);
    v["contributors"] = json!(top
        .iter()
        .map(|(a, sum, n)| json!({ "account_id": a, "amount": sum, "contributions": n }))
        .collect::<Vec<_>>());
    Ok(Json(v))
}

#[derive(Deserialize)]
struct AmountBody {
    amount: i64,
}

/// `POST /v1/groups/{id}/pools/{pid}/contribute {amount}` — money moves wallet → treasury, the
/// pool's `raised` grows, newly crossed tiers reward every contributor so far, the goal completes.
async fn contribute(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, pid)): Path<(GroupId, Uuid)>,
    Json(b): Json<AmountBody>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    if b.amount <= 0 {
        return Err(Error::InvalidArgument("amount must be positive".into()).into());
    }
    let p = load_pool(&s, id, pid).await?;
    let now = OffsetDateTime::now_utc();
    if p.status != "open" || p.ends_at.is_some_and(|t| t <= now) {
        return Err(Error::Conflict("pool is not open".into()).into());
    }
    let amount = b.amount.min(p.goal - p.raised).max(1);
    s.economy
        .treasury_deposit(
            id.0,
            me.account_id,
            &p.currency,
            amount,
            &format!("pool:{pid}"),
        )
        .await?;
    let first: bool = sqlx::query_scalar(
        "select not exists(select 1 from musubi.group_pool_contributions where pool_id = $1 and account_id = $2)",
    )
    .bind(pid)
    .bind(me.account_id.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.group_pool_contributions (id, pool_id, account_id, amount) values ($1, $2, $3, $4)",
    )
    .bind(Uuid::now_v7())
    .bind(pid)
    .bind(me.account_id.0)
    .bind(amount)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let p: Pool = sqlx::query_as(&format!(
        "update musubi.group_pools set raised = raised + $2,
            status = case when raised + $2 >= goal then 'completed' else status end,
            completed_at = case when raised + $2 >= goal then coalesce(completed_at, now()) else completed_at end
         where id = $1 returning {POOL_COLS}"
    ))
    .bind(pid)
    .bind(amount)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut rewards = Vec::new();
    if first {
        if let Some(t) = &p.contributor_reward_table {
            if s.economy
                .grant_table(
                    me.account_id,
                    t,
                    &format!("pool:{pid}:contrib:{}", me.account_id.0),
                )
                .await
                .is_ok()
            {
                rewards.push(json!({ "kind": "contributor", "reward_table": t }));
            }
        }
    }
    let tiers: Vec<Tier> = serde_json::from_value(p.tiers.clone()).unwrap_or_default();
    let contributors: Vec<(Uuid,)> = sqlx::query_as(
        "select distinct account_id from musubi.group_pool_contributions where pool_id = $1",
    )
    .bind(pid)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    for (idx, tier) in tiers.iter().enumerate() {
        if p.raised < tier.at {
            continue;
        }
        for (acc,) in &contributors {
            let fresh = sqlx::query(
                "insert into musubi.group_pool_tier_grants (pool_id, tier, account_id) values ($1, $2, $3) on conflict do nothing",
            )
            .bind(pid)
            .bind(idx as i32)
            .bind(acc)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
            if fresh == 1 {
                let reference = format!("pool:{pid}:tier:{idx}:{acc}");
                if s.economy
                    .grant_table(AccountId(*acc), &tier.reward_table, &reference)
                    .await
                    .is_ok()
                    && *acc == me.account_id.0
                {
                    rewards.push(
                        json!({ "kind": "tier", "tier": idx, "reward_table": tier.reward_table }),
                    );
                }
            }
        }
    }
    s.events
        .emit(
            "group.pool_contributed",
            Some(me.account_id),
            json!({ "group_id": id, "pool_id": pid, "amount": amount, "raised": p.raised }),
        )
        .await;
    if p.status == "completed" && p.completed_at.is_some_and(|t| t >= now) {
        s.events
            .emit(
                "group.pool_completed",
                None,
                json!({ "group_id": id, "pool_id": pid, "raised": p.raised, "contributors": contributors.len() }),
            )
            .await;
    }
    let mut v = pool_json(&p);
    v["contributed"] = json!(amount);
    v["rewards"] = json!(rewards);
    Ok(Json(v))
}

async fn close_pool(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, pid)): Path<(GroupId, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    groups(&s)
        .check_perm(me.account_id, id, perm::TREASURY_WITHDRAW)
        .await?;
    let p: Option<Pool> = sqlx::query_as(&format!(
        "update musubi.group_pools set status = 'closed' where id = $1 and group_id = $2 and status = 'open' returning {POOL_COLS}"
    ))
    .bind(pid)
    .bind(id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let p = p.ok_or_else(|| Error::Conflict("pool is not open".into()))?;
    Ok(Json(pool_json(&p)))
}

// ----- donations -----

type DonationRow = (
    Uuid,
    Uuid,
    String,
    i64,
    String,
    String,
    Option<Uuid>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

fn donation_json((id, acc, cur, amount, note, status, by, at, closed): &DonationRow) -> Value {
    json!({ "id": id, "account_id": acc, "currency": cur, "amount": amount, "note": note, "status": status,
        "fulfilled_by": by, "created_at": at.unix_timestamp(), "closed_at": closed.map(|t| t.unix_timestamp()) })
}

const DON_COLS: &str =
    "id, account_id, currency, amount, note, status, fulfilled_by, created_at, closed_at";

#[derive(Deserialize)]
struct DonationReq {
    currency: String,
    amount: i64,
    #[serde(default)]
    note: String,
}

/// `POST /v1/groups/{id}/donations {currency, amount, note}` — ask guildmates for help (one open
/// request per member and currency).
async fn request_donation(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
    Json(b): Json<DonationReq>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    if b.amount <= 0 || b.note.len() > 280 {
        return Err(Error::InvalidArgument("amount must be positive, note ≤ 280".into()).into());
    }
    let open: bool = sqlx::query_scalar(
        "select exists(select 1 from musubi.group_donation_requests where group_id = $1 and account_id = $2 and currency = $3 and status = 'open')",
    )
    .bind(id.0)
    .bind(me.account_id.0)
    .bind(&b.currency)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if open {
        return Err(
            Error::Conflict("you already have an open request for this currency".into()).into(),
        );
    }
    let did = Uuid::now_v7();
    let row: DonationRow = sqlx::query_as(&format!(
        "insert into musubi.group_donation_requests (id, group_id, account_id, currency, amount, note)
         values ($1, $2, $3, $4, $5, $6) returning {DON_COLS}"
    ))
    .bind(did)
    .bind(id.0)
    .bind(me.account_id.0)
    .bind(&b.currency)
    .bind(b.amount)
    .bind(&b.note)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "group.donation_requested",
            Some(me.account_id),
            json!({ "group_id": id, "request_id": did, "currency": b.currency, "amount": b.amount }),
        )
        .await;
    Ok(Json(donation_json(&row)))
}

async fn list_donations(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let rows: Vec<DonationRow> = sqlx::query_as(&format!(
        "select {DON_COLS} from musubi.group_donation_requests where group_id = $1
         order by (status = 'open') desc, created_at desc limit 100"
    ))
    .bind(id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "requests": rows.iter().map(donation_json).collect::<Vec<_>>() }),
    ))
}

/// `POST /v1/groups/{id}/donations/{did}/give` — the caller pays the full amount from their own
/// wallet straight to the requester; the request closes.
async fn give(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, did)): Path<(GroupId, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let row: Option<DonationRow> = sqlx::query_as(&format!(
        "select {DON_COLS} from musubi.group_donation_requests where id = $1 and group_id = $2 and status = 'open'"
    ))
    .bind(did)
    .bind(id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let row = row.ok_or_else(|| Error::NotFound(format!("open donation request {did}")))?;
    let (to, currency, amount) = (AccountId(row.1), row.2.clone(), row.3);
    if to == me.account_id {
        return Err(Error::InvalidArgument("cannot fulfil your own request".into()).into());
    }
    // wallet → treasury → wallet keeps both legs on the audited ledgers
    let reason = format!("donation:{did}");
    s.economy
        .treasury_deposit(id.0, me.account_id, &currency, amount, &reason)
        .await?;
    s.economy
        .treasury_withdraw(id.0, to, &currency, amount, &reason)
        .await?;
    let row: DonationRow = sqlx::query_as(&format!(
        "update musubi.group_donation_requests set status = 'fulfilled', fulfilled_by = $2, closed_at = now()
         where id = $1 returning {DON_COLS}"
    ))
    .bind(did)
    .bind(me.account_id.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "group.donation_given",
            Some(me.account_id),
            json!({ "group_id": id, "request_id": did, "to": to, "currency": currency, "amount": amount }),
        )
        .await;
    Ok(Json(donation_json(&row)))
}

async fn cancel_donation(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, did)): Path<(GroupId, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let officer = groups(&s)
        .check_perm(me.account_id, id, perm::TREASURY_WITHDRAW)
        .await
        .is_ok();
    let row: Option<DonationRow> = sqlx::query_as(&format!(
        "update musubi.group_donation_requests set status = 'cancelled', closed_at = now()
         where id = $1 and group_id = $2 and status = 'open' and (account_id = $3 or $4) returning {DON_COLS}"
    ))
    .bind(did)
    .bind(id.0)
    .bind(me.account_id.0)
    .bind(officer)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let row = row.ok_or_else(|| Error::NotFound(format!("open donation request {did}")))?;
    Ok(Json(donation_json(&row)))
}

// ----- team store -----

type StoreRow = (String, String, i64, i64, bool);

fn store_json((item, cur, price, qty, enabled): &StoreRow) -> Value {
    json!({ "item_id": item, "currency": cur, "price": price, "quantity": qty, "enabled": enabled })
}

async fn store(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<GroupId>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let rows: Vec<StoreRow> = sqlx::query_as(
        "select item_id, currency, price, quantity, enabled from musubi.group_store where group_id = $1 order by item_id",
    )
    .bind(id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "items": rows.iter().map(store_json).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
struct StorePut {
    currency: String,
    price: i64,
    #[serde(default = "one")]
    quantity: i64,
    #[serde(default = "yes")]
    enabled: bool,
}

fn one() -> i64 {
    1
}
fn yes() -> bool {
    true
}

/// `PUT /v1/groups/{id}/store/{item} {currency, price, quantity, enabled}` — officers list an
/// inventory item the treasury will buy for members at this price.
async fn store_put(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, item)): Path<(GroupId, String)>,
    Json(b): Json<StorePut>,
) -> Result<Json<Value>, ApiError> {
    groups(&s)
        .check_perm(me.account_id, id, perm::TREASURY_WITHDRAW)
        .await?;
    if b.price < 0 || b.quantity <= 0 {
        return Err(Error::InvalidArgument("price ≥ 0, quantity > 0".into()).into());
    }
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from musubi.items where id = $1)")
            .bind(&item)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if !exists {
        return Err(Error::NotFound(format!("item {item}")).into());
    }
    sqlx::query(
        "insert into musubi.group_store (group_id, item_id, currency, price, quantity, enabled) values ($1, $2, $3, $4, $5, $6)
         on conflict (group_id, item_id) do update set currency = excluded.currency, price = excluded.price,
            quantity = excluded.quantity, enabled = excluded.enabled, updated_at = now()",
    )
    .bind(id.0)
    .bind(&item)
    .bind(&b.currency)
    .bind(b.price)
    .bind(b.quantity)
    .bind(b.enabled)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(store_json(&(
        item, b.currency, b.price, b.quantity, b.enabled,
    ))))
}

#[derive(Deserialize, Default)]
struct StoreBuy {
    /// Officers may buy for another member; members buy for themselves.
    #[serde(default)]
    account_id: Option<AccountId>,
}

/// `POST /v1/groups/{id}/store/{item}/buy {account_id?}` — the treasury pays, the member receives.
async fn store_buy(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, item)): Path<(GroupId, String)>,
    body: Option<Json<StoreBuy>>,
) -> Result<Json<Value>, ApiError> {
    groups(&s).require_member(me.account_id, id).await?;
    let to = body
        .and_then(|Json(b)| b.account_id)
        .unwrap_or(me.account_id);
    if to != me.account_id {
        groups(&s)
            .check_perm(me.account_id, id, perm::TREASURY_WITHDRAW)
            .await?;
        groups(&s).require_member(to, id).await?;
    }
    let row: Option<StoreRow> = sqlx::query_as(
        "select item_id, currency, price, quantity, enabled from musubi.group_store where group_id = $1 and item_id = $2",
    )
    .bind(id.0)
    .bind(&item)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((_, currency, price, quantity, enabled)) = row else {
        return Err(Error::NotFound(format!("team store item {item}")).into());
    };
    if !enabled {
        return Err(Error::Conflict("item is not for sale right now".into()).into());
    }
    let reason = format!("team_store:{item}:{}", to.0);
    if price > 0 {
        // treasury → buyer, then buyer pays nothing else: the item is granted directly
        s.economy
            .treasury_withdraw(id.0, to, &currency, price, &reason)
            .await?;
        if let Err(e) = s
            .economy
            .adjust(
                to,
                &currency,
                -price,
                "team_store",
                &reason,
                None,
                json!({ "group_id": id, "item_id": item }),
            )
            .await
        {
            // roll the money back to the treasury if the buyer-side debit failed
            let _ = s
                .economy
                .treasury_deposit(id.0, to, &currency, price, &format!("{reason}:rollback"))
                .await;
            return Err(e.into());
        }
    }
    let entry = s
        .economy
        .grant_item(
            to,
            &item,
            quantity,
            json!({ "from": "team_store", "group_id": id }),
        )
        .await?;
    s.events
        .emit(
            "group.store_purchase",
            Some(me.account_id),
            json!({ "group_id": id, "item_id": item, "to": to, "currency": currency, "price": price }),
        )
        .await;
    Ok(Json(
        json!({ "item": entry, "paid": price, "currency": currency, "to": to }),
    ))
}
