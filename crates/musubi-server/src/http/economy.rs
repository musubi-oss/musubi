//! Economy: wallet / inventory / store for players (bearer) and catalog + grants for the server key.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_economy::{
    Achievement, CreateItem, CreateStoreItem, EnergyDef, IapProduct, ListingFilter, MarketRules,
    NewListing, NewOrder, NewTrade, Quest, ReceiptInput, RewardTable, Season, SendMail, StreakDef,
    Track,
};
use musubi_integrations::StoreTransaction;
use musubi_social::notifications::NewNotification;
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
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
        .route("/v1/progression", get(progression))
        .route("/v1/energy", get(energy))
        .route("/v1/energy/{id}/spend", post(energy_spend))
        .route("/v1/streaks", get(streaks))
        .route("/v1/streaks/{id}/claim", post(streak_claim))
        .route(
            "/v1/server/progression/tracks",
            get(tracks).post(upsert_track),
        )
        .route("/v1/server/progression/xp", post(add_xp))
        .route("/v1/server/energy", get(energies).post(upsert_energy))
        .route("/v1/server/energy/grant", post(energy_grant))
        .route("/v1/server/streaks", get(streak_defs).post(upsert_streak))
        .route("/v1/iap/apple", post(iap_apple))
        .route("/v1/iap/google", post(iap_google))
        .route("/v1/iap/receipts", get(iap_receipts))
        .route("/v1/iap/apple/notifications", post(iap_apple_notifications))
        .route(
            "/v1/iap/google/notifications",
            post(iap_google_notifications),
        )
        .route(
            "/v1/server/iap/products",
            get(iap_products).post(upsert_iap_product),
        )
        .route("/v1/server/iap/receipts", get(iap_receipts_of))
        .route("/v1/quests", get(quests_mine))
        .route("/v1/quests/{id}/claim", post(quest_claim))
        .route("/v1/seasons/current", get(season_current))
        .route("/v1/seasons/{id}/claim", post(season_claim))
        .route("/v1/server/quests", get(quests_all).post(upsert_quest))
        .route("/v1/server/quests/event", post(quest_event))
        .route("/v1/server/seasons", get(seasons_all).post(upsert_season))
        .route("/v1/server/seasons/{id}/premium", post(season_premium))
        .route("/v1/iap/steam/init", post(iap_steam_init))
        .route("/v1/iap/steam/finalize", post(iap_steam_finalize))
        .route("/v1/server/iap/steam/reconcile", post(iap_steam_reconcile))
        .route(
            "/v1/market/listings",
            get(market_listings).post(market_list),
        )
        .route("/v1/market/listings/mine", get(market_my_listings))
        .route("/v1/market/listings/{id}", get(market_listing))
        .route("/v1/market/listings/{id}/buy", post(market_buy))
        .route("/v1/market/listings/{id}/bid", post(market_bid))
        .route("/v1/market/listings/{id}/cancel", post(market_cancel))
        .route("/v1/market/sales", get(market_sales))
        .route("/v1/market/orders", get(order_mine).post(order_place))
        .route("/v1/market/orders/{id}", get(order_get))
        .route("/v1/market/orders/{id}/cancel", post(order_cancel))
        .route("/v1/market/book/{item}", get(order_book))
        .route("/v1/market/fills", get(order_fills))
        .route("/v1/server/market/settle", post(market_settle))
        .route("/v1/trades", get(trades).post(trade_propose))
        .route("/v1/trades/{id}", get(trade_get))
        .route("/v1/trades/{id}/accept", post(trade_accept))
        .route("/v1/trades/{id}/decline", post(trade_close))
        .route("/v1/trades/{id}/cancel", post(trade_close))
        .route("/v1/mail", get(mail_list))
        .route("/v1/mail/{id}", axum::routing::delete(mail_delete))
        .route("/v1/mail/{id}/read", post(mail_read))
        .route("/v1/mail/{id}/claim", post(mail_claim))
        .route("/v1/server/mail", post(mail_send))
        .route("/v1/achievements", get(achievements_mine))
        .route(
            "/v1/server/achievements",
            get(achievements_all).post(upsert_achievement),
        )
        .route(
            "/v1/server/achievements/progress",
            post(achievement_progress),
        )
        .route(
            "/v1/server/currencies",
            get(currencies).post(upsert_currency),
        )
        .route("/v1/server/wallet/adjust", post(adjust))
        .route("/v1/server/items", get(items).post(upsert_item))
        .route("/v1/server/inventory/grant", post(grant_item))
        .route("/v1/server/store", get(store_all).post(upsert_store_item))
        .route("/v1/server/store/coupons", get(coupons).put(upsert_coupon))
        .route(
            "/v1/server/store/coupons/{code}",
            axum::routing::delete(delete_coupon),
        )
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

async fn store(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "items": s.economy.store(Some(me.account_id), false).await? }),
    ))
}

#[derive(Deserialize, serde::Serialize)]
struct BuyReq {
    store_item_id: String,
    currency: String,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    coupon: Option<String>,
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
            r.coupon.as_deref(),
        )
        .await?;
    s.events
        .emit(
            "store.purchase",
            Some(me.account_id),
            json!({ "request": &r }),
        )
        .await;
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
    Ok(Json(json!({ "items": s.economy.store(None, true).await? })))
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

async fn coupons(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "coupons": s.economy.coupons().await? })))
}

async fn upsert_coupon(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<musubi_economy::Coupon>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_coupon(c).await?).map_err(Error::internal)?,
    ))
}

async fn delete_coupon(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !s.economy.delete_coupon(&code).await? {
        return Err(Error::NotFound("coupon".into()).into());
    }
    Ok(Json(json!({ "ok": true })))
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

// ----- progression -----

async fn progression(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "tracks": s.economy.progress(me.account_id).await? }),
    ))
}

async fn energy(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "energy": s.economy.energy(me.account_id, OffsetDateTime::now_utc()).await? }),
    ))
}

#[derive(Deserialize)]
struct AmountReq {
    #[serde(default = "one_i32")]
    amount: i32,
}

fn one_i32() -> i32 {
    1
}

async fn energy_spend(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(r): Json<AmountReq>,
) -> Result<Json<Value>, ApiError> {
    if r.amount <= 0 {
        return Err(Error::InvalidArgument("amount must be positive".into()).into());
    }
    let st = s
        .economy
        .adjust_energy(me.account_id, &id, -r.amount, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(serde_json::to_value(st).map_err(Error::internal)?))
}

async fn streaks(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "streaks": s.economy.streak_states(me.account_id, OffsetDateTime::now_utc()).await? }),
    ))
}

async fn streak_claim(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let c = s
        .economy
        .claim_streak(me.account_id, &id, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(serde_json::to_value(c).map_err(Error::internal)?))
}

async fn tracks(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "tracks": s.economy.tracks().await? })))
}

async fn upsert_track(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(t): Json<Track>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_track(t).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct XpReq {
    account_id: String,
    track_id: String,
    delta: i64,
    #[serde(default)]
    reason: String,
}

async fn add_xp(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<XpReq>,
) -> Result<Json<Value>, ApiError> {
    let res = s
        .economy
        .add_xp(account(&r.account_id)?, &r.track_id, r.delta, &r.reason)
        .await?;
    Ok(Json(serde_json::to_value(res).map_err(Error::internal)?))
}

async fn energies(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "energies": s.economy.energies().await? })))
}

async fn upsert_energy(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(d): Json<EnergyDef>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_energy(d).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct EnergyGrantReq {
    account_id: String,
    energy_id: String,
    amount: i32,
}

async fn energy_grant(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<EnergyGrantReq>,
) -> Result<Json<Value>, ApiError> {
    if r.amount <= 0 {
        return Err(Error::InvalidArgument("amount must be positive".into()).into());
    }
    let st = s
        .economy
        .adjust_energy(
            account(&r.account_id)?,
            &r.energy_id,
            r.amount,
            OffsetDateTime::now_utc(),
        )
        .await?;
    Ok(Json(serde_json::to_value(st).map_err(Error::internal)?))
}

async fn streak_defs(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "streaks": s.economy.streaks().await? })))
}

async fn upsert_streak(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(d): Json<StreakDef>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_streak(d).await?).map_err(Error::internal)?,
    ))
}

// ----- in-app purchases -----

fn receipt_input(t: StoreTransaction) -> ReceiptInput {
    ReceiptInput {
        provider: t.provider,
        transaction_id: t.transaction_id,
        product_id: t.product_id,
        environment: t.environment,
        purchased_at_ms: t.purchased_at_ms,
        quantity: t.quantity,
        raw: t.raw,
    }
}

#[derive(Deserialize)]
struct AppleReq {
    transaction_id: String,
}

async fn iap_apple(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<AppleReq>,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.apple_iap else {
        return Err(Error::NotFound("apple iap is not configured".into()).into());
    };
    let t = c.transaction(&r.transaction_id).await?;
    let rc = s.economy.redeem(me.account_id, receipt_input(t)).await?;
    s.events
        .emit(
            "iap.redeemed",
            Some(me.account_id),
            json!({ "provider": "apple" }),
        )
        .await;
    Ok(Json(serde_json::to_value(rc).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct GoogleReq {
    product_id: String,
    purchase_token: String,
}

async fn iap_google(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<GoogleReq>,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.google_iap else {
        return Err(Error::NotFound("google iap is not configured".into()).into());
    };
    let t = c.product_purchase(&r.product_id, &r.purchase_token).await?;
    let rc = s.economy.redeem(me.account_id, receipt_input(t)).await?;
    s.events
        .emit(
            "iap.redeemed",
            Some(me.account_id),
            json!({ "provider": "google" }),
        )
        .await;
    Ok(Json(serde_json::to_value(rc).map_err(Error::internal)?))
}

async fn iap_receipts(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<LimitQ>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "receipts": s.economy.receipts(me.account_id, q.limit.unwrap_or(20)).await? }),
    ))
}

async fn iap_products(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "products": s.economy.iap_products().await? })))
}

async fn upsert_iap_product(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(p): Json<IapProduct>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_iap_product(p).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct AccountQ {
    account_id: String,
    limit: Option<i64>,
}

async fn iap_receipts_of(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<AccountQ>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "receipts": s.economy.receipts(account(&q.account_id)?, q.limit.unwrap_or(20)).await? }),
    ))
}

// ----- mailbox -----

/// Notification code for a new mailbox message.
pub const NOTIFY_MAIL: i32 = 100;

#[derive(Deserialize)]
struct MailQ {
    limit: Option<i64>,
    #[serde(default)]
    unread: bool,
}

async fn mail_list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<MailQ>,
) -> Result<Json<Value>, ApiError> {
    let m = s
        .economy
        .mail(
            me.account_id,
            q.unread,
            q.limit.unwrap_or(50),
            OffsetDateTime::now_utc(),
        )
        .await?;
    Ok(Json(json!({ "mail": m })))
}

fn mail_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad mail id".into()).into())
}

async fn mail_read(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let m = s
        .economy
        .read_mail(me.account_id, mail_id(&id)?, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(serde_json::to_value(m).map_err(Error::internal)?))
}

async fn mail_claim(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (m, rewards) = s
        .economy
        .claim_mail(me.account_id, mail_id(&id)?, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(json!({ "mail": m, "rewards": rewards })))
}

async fn mail_delete(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.economy
        .delete_mail(me.account_id, mail_id(&id)?, OffsetDateTime::now_utc())
        .await?;
    Ok(Json(json!({})))
}

async fn mail_send(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(m): Json<SendMail>,
) -> Result<Json<Value>, ApiError> {
    let recipients = m.account_ids.clone();
    let title = m.title.clone();
    let all = m.all;
    let n = s.economy.send_mail(m, OffsetDateTime::now_utc()).await?;
    if !all {
        let batch = recipients
            .into_iter()
            .map(|id| NewNotification {
                user_id: AccountId(id),
                sender_id: None,
                subject: title.clone(),
                content: json!({ "kind": "mail" }),
                code: NOTIFY_MAIL,
                persistent: false,
            })
            .collect();
        s.notify.send(batch).await?;
    }
    Ok(Json(json!({ "sent": n })))
}

// ----- achievements -----

async fn achievements_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "achievements": s.economy.achievement_progress(me.account_id).await? }),
    ))
}

async fn achievements_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "achievements": s.economy.achievements().await? }),
    ))
}

async fn upsert_achievement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(a): Json<Achievement>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_achievement(a).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct AchProgressReq {
    account_id: String,
    achievement_id: String,
    #[serde(default = "one_i32")]
    delta: i32,
}

async fn achievement_progress(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<AchProgressReq>,
) -> Result<Json<Value>, ApiError> {
    let u = s
        .economy
        .add_achievement_progress(
            account(&r.account_id)?,
            &r.achievement_id,
            r.delta,
            OffsetDateTime::now_utc(),
        )
        .await?;
    Ok(Json(serde_json::to_value(u).map_err(Error::internal)?))
}

// ----- quests -----

async fn quests_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "quests": s.economy.quest_views(me.account_id, OffsetDateTime::now_utc()).await? }),
    ))
}

async fn quest_claim(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<Value>, ApiError> {
    let rewards = s
        .economy
        .claim_quest(me.account_id, &id, OffsetDateTime::now_utc())
        .await?;
    s.events
        .emit(
            "quest.claimed",
            Some(me.account_id),
            json!({ "quest_id": id, "rewards": rewards }),
        )
        .await;
    Ok(Json(json!({ "rewards": rewards })))
}

async fn quests_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "quests": s.economy.quests().await? })))
}

async fn upsert_quest(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(q): Json<Quest>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_quest(q).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct QuestEventReq {
    account_id: String,
    event: String,
    #[serde(default = "one")]
    delta: i64,
}

async fn quest_event(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<QuestEventReq>,
) -> Result<Json<Value>, ApiError> {
    let res = s
        .economy
        .quest_event(
            account(&r.account_id)?,
            &r.event,
            r.delta,
            OffsetDateTime::now_utc(),
        )
        .await?;
    Ok(Json(serde_json::to_value(res).map_err(Error::internal)?))
}

// ----- seasons / battle pass -----

async fn season_current(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "current": s.economy.current_season(me.account_id, OffsetDateTime::now_utc()).await? }),
    ))
}

#[derive(Deserialize)]
struct SeasonClaimReq {
    level: i32,
    #[serde(default = "free")]
    tier: String,
}

fn free() -> String {
    "free".into()
}

async fn season_claim(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(r): Json<SeasonClaimReq>,
) -> Result<Json<Value>, ApiError> {
    let rewards = s
        .economy
        .claim_season(
            me.account_id,
            &id,
            r.level,
            &r.tier,
            OffsetDateTime::now_utc(),
        )
        .await?;
    s.events
        .emit(
            "season.claimed",
            Some(me.account_id),
            json!({ "season_id": id, "rewards": rewards }),
        )
        .await;
    Ok(Json(json!({ "rewards": rewards })))
}

async fn seasons_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "seasons": s.economy.seasons().await? })))
}

async fn upsert_season(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(q): Json<Season>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_season(q).await?).map_err(Error::internal)?,
    ))
}

#[derive(Deserialize)]
struct AccountReq {
    account_id: String,
}

async fn season_premium(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(r): Json<AccountReq>,
) -> Result<Json<Value>, ApiError> {
    s.economy
        .grant_premium(account(&r.account_id)?, &id)
        .await?;
    Ok(Json(json!({})))
}

// ----- store notifications (refunds / revocations) -----

#[derive(Deserialize)]
struct AppleNotification {
    #[serde(rename = "signedPayload")]
    signed_payload: String,
}

/// App Store Server Notifications v2: REFUND / REVOKE take the granted items back.
async fn iap_apple_notifications(
    State(s): State<AppState>,
    Json(n): Json<AppleNotification>,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.apple_iap else {
        return Err(Error::NotFound("apple iap is not configured".into()).into());
    };
    let p = c.verify_payload(&n.signed_payload)?;
    let kind = p["notificationType"].as_str().unwrap_or("").to_string();
    let mut revoked = None;
    if matches!(kind.as_str(), "REFUND" | "REVOKE") {
        if let Some(jws) = p["data"]["signedTransactionInfo"].as_str() {
            let t = c.verify_payload(jws)?;
            if let Some(id) = t["transactionId"].as_str() {
                revoked = s
                    .economy
                    .revoke_receipt("apple", id, &format!("apple.{}", kind.to_lowercase()))
                    .await?;
            }
        }
    }
    Ok(Json(
        json!({ "notification_type": kind, "revoked": revoked }),
    ))
}

#[derive(Deserialize)]
struct TokenQ {
    token: Option<String>,
}

#[derive(Deserialize)]
struct PubSubPush {
    message: PubSubMessage,
}

#[derive(Deserialize)]
struct PubSubMessage {
    data: String,
}

/// Google Play real-time developer notifications (Pub/Sub push): voided purchases and canceled
/// one-time products take the granted items back.
async fn iap_google_notifications(
    State(s): State<AppState>,
    Query(q): Query<TokenQ>,
    Json(m): Json<PubSubPush>,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.google_iap else {
        return Err(Error::NotFound("google iap is not configured".into()).into());
    };
    let expected = c.notifications_token();
    if expected.is_empty() || q.token.as_deref() != Some(expected) {
        return Err(Error::PermissionDenied("bad notifications token".into()).into());
    }
    use base64::Engine;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(m.message.data.trim())
        .map_err(|_| Error::InvalidArgument("message.data must be base64".into()))?;
    let v: Value = serde_json::from_slice(&raw)
        .map_err(|_| Error::InvalidArgument("message.data must be JSON".into()))?;
    let (token, reason) = if let Some(t) = v["voidedPurchaseNotification"]["purchaseToken"].as_str()
    {
        (Some(t), "google.voided")
    } else if v["oneTimeProductNotification"]["notificationType"].as_i64() == Some(2) {
        (
            v["oneTimeProductNotification"]["purchaseToken"].as_str(),
            "google.canceled",
        )
    } else {
        (None, "")
    };
    let mut revoked = None;
    if let Some(t) = token {
        revoked = s.economy.revoke_receipt("google", t, reason).await?;
    }
    Ok(Json(json!({ "revoked": revoked })))
}

// ----- steam microtransactions -----

#[derive(Deserialize)]
struct SteamInitReq {
    steam_id: String,
    store_item_id: String,
    #[serde(default = "one")]
    quantity: i64,
    #[serde(default = "en")]
    language: String,
    #[serde(default = "usd")]
    currency: String,
}

fn en() -> String {
    "en".into()
}

fn usd() -> String {
    "USD".into()
}

/// Starts a Steam order for a store listing mapped to a Steam item id; the client then shows the
/// Steam overlay and calls `finalize` once the user authorized it.
async fn iap_steam_init(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<SteamInitReq>,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.steam_iap else {
        return Err(Error::NotFound("steam iap is not configured".into()).into());
    };
    if r.quantity <= 0 || r.quantity > 100 || r.steam_id.is_empty() {
        return Err(Error::InvalidArgument("steam_id and quantity 1..=100".into()).into());
    }
    let Some(product) = s.economy.steam_product(&r.store_item_id).await? else {
        return Err(Error::NotFound("store item is not sold on steam".into()).into());
    };
    let order_id = s
        .economy
        .create_steam_order(
            me.account_id,
            &product,
            r.quantity,
            &r.steam_id,
            &r.currency,
        )
        .await?;
    let description = if product.description.is_empty() {
        product.store_item_id.clone()
    } else {
        product.description.clone()
    };
    let transid = c
        .init_txn(&musubi_integrations::SteamInitTxn {
            order_id,
            steam_id: r.steam_id,
            item_id: product.product_id.clone(),
            quantity: r.quantity,
            amount_cents: product.amount_cents,
            description,
            language: r.language,
            currency: r.currency,
        })
        .await?;
    Ok(Json(
        json!({ "order_id": order_id.to_string(), "transaction_id": transid }),
    ))
}

#[derive(Deserialize)]
struct SteamFinalizeReq {
    order_id: String,
}

/// Captures the authorized order and grants the mapped store item.
async fn iap_steam_finalize(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<SteamFinalizeReq>,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.steam_iap else {
        return Err(Error::NotFound("steam iap is not configured".into()).into());
    };
    let order_id: i64 = r
        .order_id
        .parse()
        .map_err(|_| Error::InvalidArgument("order_id".into()))?;
    let Some(order) = s.economy.steam_order(order_id).await? else {
        return Err(Error::NotFound("steam order".into()).into());
    };
    if order.account_id != me.account_id {
        return Err(Error::PermissionDenied("not your order".into()).into());
    }
    if order.finalized {
        return Err(Error::Conflict("order already finalized".into()).into());
    }
    let p = c.finalize_txn(order_id).await?;
    let transid = p["transid"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| format!("order-{order_id}"));
    let mut raw = p.clone();
    raw["orderid"] = json!(order_id.to_string());
    raw["quantity"] = json!(order.quantity);
    let environment = if c.sandbox() { "Sandbox" } else { "Production" };
    let rc = s
        .economy
        .redeem(
            me.account_id,
            ReceiptInput {
                provider: "steam".into(),
                transaction_id: transid,
                product_id: order.product_id.clone(),
                environment: environment.into(),
                purchased_at_ms: OffsetDateTime::now_utc().unix_timestamp() * 1000,
                quantity: order.quantity,
                raw,
            },
        )
        .await?;
    s.economy.finalize_steam_order(order_id).await?;
    s.events
        .emit(
            "iap.redeemed",
            Some(me.account_id),
            json!({ "provider": "steam" }),
        )
        .await;
    Ok(Json(serde_json::to_value(rc).map_err(Error::internal)?))
}

/// Steam has no refund push: poll QueryTxn for every live Steam receipt and revoke refunded ones.
async fn iap_steam_reconcile(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let Some(c) = &s.steam_iap else {
        return Err(Error::NotFound("steam iap is not configured".into()).into());
    };
    let mut revoked = Vec::new();
    let orders = s.economy.steam_receipt_orders().await?;
    for (transid, order_id) in &orders {
        let status = match c.query_txn(*order_id).await {
            Ok(p) => p["status"].as_str().unwrap_or("").to_string(),
            Err(e) => {
                tracing::warn!(order_id, error = %e, "steam QueryTxn failed");
                continue;
            }
        };
        if matches!(
            status.as_str(),
            "Refunded"
                | "PartialRefund"
                | "Chargedback"
                | "RefundedSuspectedFraud"
                | "RefundedFriendlyFraud"
        ) {
            if let Some(r) = s
                .economy
                .revoke_receipt(
                    "steam",
                    transid,
                    &format!("steam.{}", status.to_lowercase()),
                )
                .await?
            {
                revoked.push(r.transaction_id);
            }
        }
    }
    Ok(Json(json!({ "checked": orders.len(), "revoked": revoked })))
}

// ----- player market -----

pub const NOTIFY_MARKET: i32 = 110;
pub const NOTIFY_TRADE: i32 = 111;

pub fn market_rules(cfg: &musubi_core::config::Config) -> MarketRules {
    MarketRules {
        fee_bps: cfg.market.fee_bps,
        max_listing_secs: cfg.market.max_listing_secs,
        default_listing_secs: cfg.market.default_listing_secs,
        trade_ttl_secs: cfg.market.trade_ttl_secs,
        anti_snipe_secs: cfg.market.anti_snipe_secs,
        trade_hold_secs: cfg.market.trade_hold_secs,
    }
}

async fn notify_one(s: &AppState, to: AccountId, code: i32, subject: &str, content: Value) {
    let n = NewNotification {
        user_id: to,
        sender_id: None,
        subject: subject.into(),
        content,
        code,
        persistent: true,
    };
    if let Err(e) = s.notify.send(vec![n]).await {
        tracing::warn!(error = %e, "notification failed");
    }
}

async fn market_listings(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Query(f): Query<ListingFilter>,
) -> Result<Json<Value>, ApiError> {
    let l = s.economy.listings(&f).await?;
    Ok(Json(
        json!({ "listings": l, "cursor": l.last().map(|x| x.id) }),
    ))
}

async fn market_list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(n): Json<NewListing>,
) -> Result<Json<Value>, ApiError> {
    let l = s
        .economy
        .list_item(me.account_id, n, &market_rules(&s.cfg))
        .await?;
    Ok(Json(serde_json::to_value(l).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct MarketQ {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    pending: Option<bool>,
}

async fn market_my_listings(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<MarketQ>,
) -> Result<Json<Value>, ApiError> {
    let l = s
        .economy
        .my_listings(me.account_id, q.limit.unwrap_or(50))
        .await?;
    Ok(Json(json!({ "listings": l })))
}

async fn market_listing(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let l = s.economy.listing(id).await?;
    Ok(Json(serde_json::to_value(l).map_err(Error::internal)?))
}

async fn market_buy(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let sale = s
        .economy
        .buyout(me.account_id, id, &market_rules(&s.cfg))
        .await?;
    notify_one(
        &s,
        sale.seller_id,
        NOTIFY_MARKET,
        "Your listing sold",
        json!({ "kind": "market.sold", "listing_id": id, "sale_id": sale.id, "price": sale.price, "fee": sale.fee }),
    )
    .await;
    s.events
        .emit(
            "market.sale",
            Some(me.account_id),
            serde_json::to_value(&sale).unwrap_or_default(),
        )
        .await;
    Ok(Json(serde_json::to_value(sale).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct BidReq {
    amount: i64,
}

async fn market_bid(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
    Json(r): Json<BidReq>,
) -> Result<Json<Value>, ApiError> {
    let before = s.economy.listing(id).await?;
    let l = s
        .economy
        .bid(me.account_id, id, r.amount, &market_rules(&s.cfg))
        .await?;
    if let Some(prev) = before.bidder_id.filter(|p| *p != me.account_id) {
        notify_one(
            &s,
            prev,
            NOTIFY_MARKET,
            "You were outbid",
            json!({ "kind": "market.outbid", "listing_id": id, "bid": l.bid }),
        )
        .await;
    }
    Ok(Json(serde_json::to_value(l).map_err(Error::internal)?))
}

async fn market_cancel(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let l = s.economy.cancel_listing(me.account_id, id).await?;
    Ok(Json(serde_json::to_value(l).map_err(Error::internal)?))
}

async fn market_sales(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<MarketQ>,
) -> Result<Json<Value>, ApiError> {
    let l = s
        .economy
        .sales(me.account_id, q.limit.unwrap_or(50))
        .await?;
    Ok(Json(json!({ "sales": l })))
}

/// Close due listings and trades now (the leader node also does this every few seconds).
async fn market_settle(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let (listings, trades) = settle_market(&s).await?;
    Ok(Json(json!({ "listings": listings, "trades": trades })))
}

pub async fn settle_market(s: &AppState) -> Result<(usize, usize), Error> {
    let now = OffsetDateTime::now_utc();
    let settled = s
        .economy
        .settle_listings(&market_rules(&s.cfg), now, 500)
        .await?;
    for st in &settled {
        match &st.sale {
            Some(sale) => {
                notify_one(
                    s,
                    sale.seller_id,
                    NOTIFY_MARKET,
                    "Your auction sold",
                    json!({ "kind": "market.sold", "listing_id": st.listing.id, "sale_id": sale.id, "price": sale.price, "fee": sale.fee }),
                )
                .await;
                notify_one(
                    s,
                    sale.buyer_id,
                    NOTIFY_MARKET,
                    "You won an auction",
                    json!({ "kind": "market.won", "listing_id": st.listing.id, "sale_id": sale.id, "price": sale.price }),
                )
                .await;
            }
            None => {
                notify_one(
                    s,
                    st.listing.seller_id,
                    NOTIFY_MARKET,
                    "Your listing expired",
                    json!({ "kind": "market.expired", "listing_id": st.listing.id }),
                )
                .await;
            }
        }
    }
    let orders = s.economy.expire_orders(now, 500).await?;
    for o in &orders {
        notify_one(
            s,
            o.account_id,
            NOTIFY_MARKET,
            "Your order expired",
            json!({ "kind": "market.order_expired", "order_id": o.id, "remaining": o.remaining }),
        )
        .await;
    }
    let expired = s.economy.expire_trades(now, 500).await?;
    for t in &expired {
        notify_one(
            s,
            t.from_id,
            NOTIFY_TRADE,
            "Trade offer expired",
            json!({ "kind": "trade.expired", "trade_id": t.id }),
        )
        .await;
    }
    let held = s.economy.settle_trades(now, 500).await?;
    for t in &held {
        for who in [t.from_id, t.to_id] {
            notify_one(
                s,
                who,
                NOTIFY_TRADE,
                "Trade settled",
                json!({ "kind": "trade.accepted", "trade_id": t.id }),
            )
            .await;
        }
        s.events
            .emit(
                "trade.accepted",
                Some(t.to_id),
                serde_json::to_value(t).unwrap_or_default(),
            )
            .await;
    }
    Ok((settled.len() + orders.len(), expired.len() + held.len()))
}

// ----- order book -----

async fn order_place(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(req): Json<NewOrder>,
) -> Result<Json<Value>, ApiError> {
    let placed = s
        .economy
        .place_order(me.account_id, &req, &market_rules(&s.cfg))
        .await?;
    for f in &placed.fills {
        let (maker, subject, kind) = if f.buyer_id == me.account_id {
            (f.seller_id, "Your sell order filled", "market.order_sold")
        } else {
            (f.buyer_id, "Your buy order filled", "market.order_bought")
        };
        notify_one(
            &s,
            maker,
            NOTIFY_MARKET,
            subject,
            json!({ "kind": kind, "fill_id": f.id, "item_id": f.item_id, "quantity": f.quantity, "price": f.price, "fee": f.fee }),
        )
        .await;
        s.events
            .emit(
                "market.fill",
                Some(me.account_id),
                serde_json::to_value(f).unwrap_or_default(),
            )
            .await;
    }
    Ok(Json(serde_json::to_value(placed).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct OrdersQuery {
    status: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    100
}

async fn order_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<OrdersQuery>,
) -> Result<Json<Value>, ApiError> {
    let v = s
        .economy
        .my_orders(me.account_id, q.status.as_deref(), q.limit)
        .await?;
    Ok(Json(json!({ "orders": v })))
}

async fn order_get(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let o = s.economy.order(id).await?;
    Ok(Json(serde_json::to_value(o).map_err(Error::internal)?))
}

async fn order_cancel(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let o = s.economy.cancel_order(me.account_id, id).await?;
    Ok(Json(serde_json::to_value(o).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct BookQuery {
    currency: String,
    #[serde(default = "default_depth")]
    depth: i64,
}

fn default_depth() -> i64 {
    20
}

async fn order_book(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    axum::extract::Path(item): axum::extract::Path<String>,
    Query(q): Query<BookQuery>,
) -> Result<Json<Value>, ApiError> {
    let b = s.economy.book(&item, &q.currency, q.depth).await?;
    Ok(Json(serde_json::to_value(b).map_err(Error::internal)?))
}

async fn order_fills(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<OrdersQuery>,
) -> Result<Json<Value>, ApiError> {
    let v = s.economy.my_fills(me.account_id, q.limit).await?;
    Ok(Json(json!({ "fills": v })))
}

// ----- direct trades -----

async fn trades(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<MarketQ>,
) -> Result<Json<Value>, ApiError> {
    let t = s
        .economy
        .trades(
            me.account_id,
            q.pending.unwrap_or(true),
            q.limit.unwrap_or(50),
        )
        .await?;
    Ok(Json(json!({ "trades": t })))
}

async fn trade_propose(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(n): Json<NewTrade>,
) -> Result<Json<Value>, ApiError> {
    let t = s
        .economy
        .propose_trade(me.account_id, n, &market_rules(&s.cfg))
        .await?;
    notify_one(
        &s,
        t.to_id,
        NOTIFY_TRADE,
        "Trade offer",
        json!({ "kind": "trade.offer", "trade_id": t.id, "from": t.from_id }),
    )
    .await;
    Ok(Json(serde_json::to_value(t).map_err(Error::internal)?))
}

async fn trade_get(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let t = s.economy.trade(me.account_id, id).await?;
    Ok(Json(serde_json::to_value(t).map_err(Error::internal)?))
}

async fn trade_accept(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let t = s
        .economy
        .accept_trade(me.account_id, id, &market_rules(&s.cfg))
        .await?;
    let kind = if t.status == "hold" {
        "trade.held"
    } else {
        "trade.accepted"
    };
    notify_one(
        &s,
        t.from_id,
        NOTIFY_TRADE,
        if t.status == "hold" {
            "Trade accepted, settling after the hold"
        } else {
            "Trade accepted"
        },
        json!({ "kind": kind, "trade_id": t.id, "settle_at": t.settle_at.map(|d| d.unix_timestamp()) }),
    )
    .await;
    s.events
        .emit(
            kind,
            Some(me.account_id),
            serde_json::to_value(&t).unwrap_or_default(),
        )
        .await;
    Ok(Json(serde_json::to_value(t).map_err(Error::internal)?))
}

async fn trade_close(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let t = s.economy.close_trade(me.account_id, id).await?;
    let other = if t.from_id == me.account_id {
        t.to_id
    } else {
        t.from_id
    };
    notify_one(
        &s,
        other,
        NOTIFY_TRADE,
        "Trade closed",
        json!({ "kind": format!("trade.{}", t.status), "trade_id": t.id }),
    )
    .await;
    Ok(Json(serde_json::to_value(t).map_err(Error::internal)?))
}
