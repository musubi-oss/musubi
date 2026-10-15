//! Economy: wallet / inventory / store for players (bearer) and catalog + grants for the server key.

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_economy::{CreateItem, CreateStoreItem, RewardTable};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::extract::Bearer;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/wallet", get(wallet))
        .route("/v1/wallet/ledger", get(ledger))
        .route("/v1/inventory", get(inventory))
        .route("/v1/inventory/consume", post(consume))
        .route("/v1/store", get(store))
        .route("/v1/store/buy", post(buy))
        .route("/v1/rewards/tables", get(reward_tables))
        .route("/v1/rewards/{table}/roll", post(reward_roll))
        .route("/v1/rewards/rolls", get(reward_rolls))
        .route(
            "/v1/server/rewards/tables",
            get(reward_tables_all).post(upsert_reward_table),
        )
        .route("/v1/server/rewards/grant", post(reward_grant))
        .route(
            "/v1/server/currencies",
            get(currencies).post(upsert_currency),
        )
        .route("/v1/server/wallet/adjust", post(adjust))
        .route("/v1/server/items", get(items).post(upsert_item))
        .route("/v1/server/inventory/grant", post(grant_item))
        .route("/v1/server/store", get(store_all).post(upsert_store_item))
}

fn account(id: &str) -> Result<AccountId, ApiError> {
    Uuid::parse_str(id)
        .map(AccountId)
        .map_err(|_| Error::InvalidArgument("bad account_id".into()).into())
}

async fn wallet(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "balances": s.economy.wallet(me.account_id).await? }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LedgerQuery {
    currency: Option<String>,
    limit: Option<i64>,
}

async fn ledger(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<LedgerQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "entries": s.economy.ledger(me.account_id, q.currency.as_deref(), q.limit.unwrap_or(50)).await? }),
    ))
}

async fn inventory(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "items": s.economy.inventory(me.account_id).await? }),
    ))
}

#[derive(Deserialize)]
struct ConsumeReq {
    id: Uuid,
    #[serde(default = "one")]
    quantity: i64,
}

fn one() -> i64 {
    1
}

async fn consume(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<ConsumeReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "remaining": s.economy.consume(me.account_id, r.id, r.quantity).await? }),
    ))
}

async fn store(State(s): State<AppState>, Bearer(_me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "items": s.economy.store(false).await? })))
}

#[derive(Deserialize)]
struct BuyReq {
    store_item_id: String,
    currency: String,
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn buy(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<BuyReq>,
) -> Result<Json<Value>, ApiError> {
    let p = s
        .economy
        .buy(
            me.account_id,
            &r.store_item_id,
            &r.currency,
            r.idempotency_key.as_deref(),
        )
        .await?;
    Ok(Json(serde_json::to_value(p).map_err(Error::internal)?))
}

// ----- server key -----

#[derive(Deserialize)]
struct CurrencyReq {
    code: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    max_balance: Option<i64>,
}

async fn currencies(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "currencies": s.economy.currencies().await? })))
}

async fn upsert_currency(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CurrencyReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(
            s.economy
                .upsert_currency(&r.code, &r.name, r.max_balance)
                .await?,
        )
        .map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct AdjustReq {
    account_id: String,
    currency: String,
    delta: i64,
    #[serde(default)]
    reason: String,
    #[serde(default, rename = "ref")]
    reference: String,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    metadata: Value,
}

async fn adjust(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<AdjustReq>,
) -> Result<Json<Value>, ApiError> {
    let (entry, replayed) = s
        .economy
        .adjust(
            account(&r.account_id)?,
            &r.currency,
            r.delta,
            &r.reason,
            &r.reference,
            r.idempotency_key.as_deref(),
            if r.metadata.is_object() {
                r.metadata
            } else {
                json!({})
            },
        )
        .await?;
    Ok(Json(json!({ "entry": entry, "replayed": replayed })))
}

async fn items(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "items": s.economy.items().await? })))
}

async fn upsert_item(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateItem>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_item(r).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct GrantItemReq {
    account_id: String,
    item_id: String,
    #[serde(default = "one")]
    quantity: i64,
    #[serde(default)]
    instance: Value,
}

async fn grant_item(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<GrantItemReq>,
) -> Result<Json<Value>, ApiError> {
    let e = s
        .economy
        .grant_item(account(&r.account_id)?, &r.item_id, r.quantity, r.instance)
        .await?;
    Ok(Json(serde_json::to_value(e).map_err(Error::internal)?))
}

async fn store_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "items": s.economy.store(true).await? })))
}

async fn upsert_store_item(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateStoreItem>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_store_item(r).await?).map_err(Error::internal)?,
    ))
}

// ----- rewards -----

async fn reward_tables(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
) -> Result<Json<Value>, ApiError> {
    let t = s.economy.reward_tables(false).await?;
    let odds: Vec<_> = t.iter().map(|t| t.odds()).collect();
    Ok(Json(json!({ "tables": odds })))
}

#[derive(Deserialize)]
struct RollReq {
    #[serde(default)]
    client_seed: String,
    #[serde(default)]
    idempotency_key: Option<String>,
}

async fn reward_roll(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(table): axum::extract::Path<String>,
    Json(r): Json<RollReq>,
) -> Result<Json<Value>, ApiError> {
    if r.client_seed.len() > 128 {
        return Err(Error::InvalidArgument("client_seed too long".into()).into());
    }
    let roll = s
        .economy
        .roll(
            me.account_id,
            &table,
            &r.client_seed,
            r.idempotency_key.as_deref(),
        )
        .await?;
    Ok(Json(serde_json::to_value(roll).map_err(Error::internal)?))
}

async fn reward_rolls(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<LimitQ>,
) -> Result<Json<Value>, ApiError> {
    let rolls = s
        .economy
        .rolls(me.account_id, q.limit.unwrap_or(20))
        .await?;
    Ok(Json(json!({ "rolls": rolls })))
}

#[derive(Deserialize)]
struct LimitQ {
    limit: Option<i64>,
}

async fn reward_tables_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "tables": s.economy.reward_tables(true).await? }),
    ))
}

async fn upsert_reward_table(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(t): Json<RewardTable>,
) -> Result<Json<Value>, ApiError> {
    let t = s.economy.upsert_reward_table(t).await?;
    Ok(Json(serde_json::to_value(t).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct RewardGrantReq {
    account_id: String,
    table_id: String,
    #[serde(default)]
    reference: String,
}

async fn reward_grant(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<RewardGrantReq>,
) -> Result<Json<Value>, ApiError> {
    let rewards = s
        .economy
        .grant_table(account(&r.account_id)?, &r.table_id, &r.reference)
        .await?;
    Ok(Json(json!({ "rewards": rewards })))
}
