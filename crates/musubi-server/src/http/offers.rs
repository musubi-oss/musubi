//! Offers & placements (competitive P0-57). A placement is a named slot in the game UI
//! ("shop_banner", "after_defeat"); offers are store items shown there with their own price,
//! targeting (audience, time window, precursor purchase), priority, per-player purchase and
//! activation limits, cooldown and duration. The first request that finds an offer eligible
//! creates an activation (`offer.activated`); listing marks it seen; buying goes through the
//! normal store purchase with the offer price (`offer.purchased`); players may dismiss.
//! Operators can force-enable an offer for one account and read seen / purchased stats.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/server/placements",
            get(list_placements).post(post_placement),
        )
        .route(
            "/v1/server/placements/{id}",
            get(get_placement)
                .put(put_placement)
                .delete(delete_placement),
        )
        .route(
            "/v1/server/placements/{id}/offers",
            get(placement_offers_for),
        )
        .route("/v1/server/offers", get(list_offers).post(post_offer))
        .route(
            "/v1/server/offers/{id}",
            get(get_offer).put(put_offer).delete(delete_offer),
        )
        .route("/v1/server/offers/{id}/stats", get(offer_stats))
        .route(
            "/v1/server/offers/{id}/force/{account_id}",
            put(force_offer).delete(unforce_offer),
        )
        .route("/v1/placements/{id}/offers", get(my_offers))
        .route("/v1/placements/{id}/offers/{offer_id}/buy", post(buy_offer))
        .route(
            "/v1/placements/{id}/offers/{offer_id}/dismiss",
            post(dismiss_offer),
        )
}

fn valid_id(id: &str, what: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(Error::InvalidArgument(format!(
            "{what}: [A-Za-z0-9_.-]{{1,64}}"
        )));
    }
    Ok(())
}

fn now_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

// ---------------------------------------------------------------- placements

#[derive(Debug, Clone, Serialize)]
pub struct Placement {
    pub id: String,
    pub name: String,
    pub description: String,
    pub max_offers: i32,
    pub metadata: Value,
}

type PlacementRow = (String, String, String, i32, Value);
const PLACEMENT_SELECT: &str =
    "select id, name, description, max_offers, metadata from musubi.placements";

fn placement_of((id, name, description, max_offers, metadata): PlacementRow) -> Placement {
    Placement {
        id,
        name,
        description,
        max_offers,
        metadata,
    }
}

async fn load_placement(s: &AppState, id: &str) -> Result<Placement> {
    let row: Option<PlacementRow> = sqlx::query_as(&format!("{PLACEMENT_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    row.map(placement_of)
        .ok_or_else(|| Error::NotFound(format!("placement {id}")))
}

#[derive(Deserialize)]
struct PlacementBody {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "one")]
    max_offers: i32,
    #[serde(default)]
    metadata: Value,
}

#[derive(Deserialize)]
struct PlacementBodyWithId {
    id: String,
    #[serde(flatten)]
    body: PlacementBody,
}

fn one() -> i32 {
    1
}

async fn save_placement(s: &AppState, id: &str, b: PlacementBody) -> Result<Placement> {
    valid_id(id, "placement id")?;
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 chars".into()));
    }
    if !(1..=20).contains(&b.max_offers) {
        return Err(Error::InvalidArgument("max_offers: 1..20".into()));
    }
    sqlx::query(
        "insert into musubi.placements (id, name, description, max_offers, metadata)
         values ($1, $2, $3, $4, $5)
         on conflict (id) do update set name = excluded.name, description = excluded.description,
            max_offers = excluded.max_offers, metadata = excluded.metadata, updated_at = now()",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(b.max_offers)
    .bind(if b.metadata.is_null() {
        json!({})
    } else {
        b.metadata
    })
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    load_placement(s, id).await
}

async fn list_placements(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<PlacementRow> = sqlx::query_as(&format!("{PLACEMENT_SELECT} order by id"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(
        json!({ "placements": rows.into_iter().map(placement_of).collect::<Vec<_>>() }),
    ))
}

async fn get_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Placement>, ApiError> {
    Ok(Json(load_placement(&s, &id).await?))
}

async fn put_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<PlacementBody>,
) -> std::result::Result<Json<Placement>, ApiError> {
    Ok(Json(save_placement(&s, &id, b).await?))
}

async fn post_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<PlacementBodyWithId>,
) -> std::result::Result<Json<Placement>, ApiError> {
    Ok(Json(save_placement(&s, &b.id, b.body).await?))
}

async fn delete_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.placements where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

// ---------------------------------------------------------------- offers

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Offer {
    pub id: String,
    pub placement_id: String,
    pub name: String,
    pub description: String,
    pub store_item_id: String,
    /// `{currency: amount}` replacing the listing price (listing discount does not apply).
    pub prices: Option<Value>,
    pub priority: i32,
    pub enabled: bool,
    pub audience: Option<String>,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
    /// Lifetime purchases per account.
    pub max_purchases: Option<i32>,
    /// Lifetime activations (times shown) per account.
    pub max_activations: Option<i32>,
    /// Pause after an activation ends (purchase, dismiss or expiry) before it can show again.
    pub cooldown_secs: i64,
    /// How long one activation stays available; `None` = until `ends_at`.
    pub duration_secs: Option<i64>,
    /// Offer that must have been purchased first (chains: starter → upgrade → whale).
    pub precursor: Option<String>,
    pub metadata: Value,
}

const OFFER_SELECT: &str = "select id, placement_id, name, description, store_item_id, prices, priority, enabled, audience, starts_at, ends_at, max_purchases, max_activations, cooldown_secs, duration_secs, precursor, metadata from musubi.offers";

async fn load_offer(s: &AppState, id: &str) -> Result<Offer> {
    let row: Option<Offer> = sqlx::query_as(&format!("{OFFER_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    row.ok_or_else(|| Error::NotFound(format!("offer {id}")))
}

async fn offers_of_placement(s: &AppState, placement: &str) -> Result<Vec<Offer>> {
    let rows: Vec<Offer> = sqlx::query_as(&format!(
        "{OFFER_SELECT} where placement_id = $1 order by priority desc, id"
    ))
    .bind(placement)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(rows)
}

#[derive(Deserialize)]
struct OfferBody {
    placement_id: String,
    name: String,
    #[serde(default)]
    description: String,
    store_item_id: String,
    #[serde(default)]
    prices: Option<Value>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    audience: Option<String>,
    #[serde(default)]
    starts_at: Option<i64>,
    #[serde(default)]
    ends_at: Option<i64>,
    #[serde(default)]
    max_purchases: Option<i32>,
    #[serde(default)]
    max_activations: Option<i32>,
    #[serde(default)]
    cooldown_secs: i64,
    #[serde(default)]
    duration_secs: Option<i64>,
    #[serde(default)]
    precursor: Option<String>,
    #[serde(default)]
    metadata: Value,
}

#[derive(Deserialize)]
struct OfferBodyWithId {
    id: String,
    #[serde(flatten)]
    body: OfferBody,
}

fn yes() -> bool {
    true
}

async fn save_offer(s: &AppState, id: &str, b: OfferBody) -> Result<Offer> {
    valid_id(id, "offer id")?;
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 chars".into()));
    }
    load_placement(s, &b.placement_id).await?;
    let item: Option<(String,)> = sqlx::query_as("select id from musubi.store_items where id = $1")
        .bind(&b.store_item_id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    if item.is_none() {
        return Err(Error::NotFound(format!("store item {}", b.store_item_id)));
    }
    if let Some(p) = &b.prices {
        let ok = p.as_object().is_some_and(|m| {
            !m.is_empty() && m.values().all(|v| v.as_i64().is_some_and(|a| a >= 0))
        });
        if !ok {
            return Err(Error::InvalidArgument(
                "prices: non-empty {currency: amount ≥ 0}".into(),
            ));
        }
    }
    if b.max_purchases.is_some_and(|m| m < 1) || b.max_activations.is_some_and(|m| m < 1) {
        return Err(Error::InvalidArgument(
            "max_purchases / max_activations must be ≥ 1".into(),
        ));
    }
    if b.cooldown_secs < 0 || b.duration_secs.is_some_and(|d| d <= 0) {
        return Err(Error::InvalidArgument(
            "cooldown_secs ≥ 0, duration_secs > 0".into(),
        ));
    }
    if matches!((b.starts_at, b.ends_at), (Some(a), Some(e)) if e <= a) {
        return Err(Error::InvalidArgument(
            "ends_at must be after starts_at".into(),
        ));
    }
    if let Some(p) = &b.precursor {
        if p == id {
            return Err(Error::InvalidArgument(
                "an offer cannot be its own precursor".into(),
            ));
        }
        load_offer(s, p).await?;
    }
    sqlx::query(
        "insert into musubi.offers (id, placement_id, name, description, store_item_id, prices, priority, enabled, audience, starts_at, ends_at, max_purchases, max_activations, cooldown_secs, duration_secs, precursor, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
         on conflict (id) do update set placement_id = excluded.placement_id, name = excluded.name,
            description = excluded.description, store_item_id = excluded.store_item_id, prices = excluded.prices,
            priority = excluded.priority, enabled = excluded.enabled, audience = excluded.audience,
            starts_at = excluded.starts_at, ends_at = excluded.ends_at, max_purchases = excluded.max_purchases,
            max_activations = excluded.max_activations, cooldown_secs = excluded.cooldown_secs,
            duration_secs = excluded.duration_secs, precursor = excluded.precursor,
            metadata = excluded.metadata, updated_at = now()",
    )
    .bind(id)
    .bind(&b.placement_id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(&b.store_item_id)
    .bind(&b.prices)
    .bind(b.priority)
    .bind(b.enabled)
    .bind(&b.audience)
    .bind(b.starts_at)
    .bind(b.ends_at)
    .bind(b.max_purchases)
    .bind(b.max_activations)
    .bind(b.cooldown_secs)
    .bind(b.duration_secs)
    .bind(&b.precursor)
    .bind(if b.metadata.is_null() { json!({}) } else { b.metadata })
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    load_offer(s, id).await
}

#[derive(Deserialize)]
struct OffersQ {
    placement_id: Option<String>,
}

async fn list_offers(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<OffersQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<Offer> = sqlx::query_as(&format!(
        "{OFFER_SELECT} where ($1::text is null or placement_id = $1) order by placement_id, priority desc, id"
    ))
    .bind(q.placement_id.as_deref())
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "offers": rows })))
}

async fn get_offer(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Offer>, ApiError> {
    Ok(Json(load_offer(&s, &id).await?))
}

async fn put_offer(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<OfferBody>,
) -> std::result::Result<Json<Offer>, ApiError> {
    Ok(Json(save_offer(&s, &id, b).await?))
}

async fn post_offer(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<OfferBodyWithId>,
) -> std::result::Result<Json<Offer>, ApiError> {
    Ok(Json(save_offer(&s, &b.id, b.body).await?))
}

async fn delete_offer(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.offers where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

#[derive(Deserialize)]
struct ForceBody {
    #[serde(default = "yes")]
    enabled: bool,
}

/// `PUT /v1/server/offers/{id}/force/{account_id} {enabled}` — show (or never show) this offer
/// to one account regardless of `enabled`, audience and limits.
async fn force_offer(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, account_id)): Path<(String, Uuid)>,
    body: Option<Json<ForceBody>>,
) -> std::result::Result<Json<Value>, ApiError> {
    load_offer(&s, &id).await?;
    let enabled = body.map(|Json(b)| b.enabled).unwrap_or(true);
    sqlx::query(
        "insert into musubi.offer_overrides (offer_id, account_id, force_enabled) values ($1, $2, $3)
         on conflict (offer_id, account_id) do update set force_enabled = excluded.force_enabled",
    )
    .bind(&id)
    .bind(account_id)
    .bind(enabled)
    .execute(&s.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => Error::NotFound("account".into()),
        e => Error::internal(e),
    })?;
    Ok(Json(
        json!({ "offer_id": id, "account_id": account_id, "force_enabled": enabled }),
    ))
}

async fn unforce_offer(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, account_id)): Path<(String, Uuid)>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n =
        sqlx::query("delete from musubi.offer_overrides where offer_id = $1 and account_id = $2")
            .bind(&id)
            .bind(account_id)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

/// `GET /v1/server/offers/{id}/stats` — funnel: activations, seen, purchased, dismissed,
/// expired, conversion.
async fn offer_stats(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    load_offer(&s, &id).await?;
    let now = now_s();
    let (activations, accounts, seen, purchased, dismissed, expired, active): (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "select count(*), count(distinct account_id),
                count(*) filter (where seen_at is not null),
                count(*) filter (where purchased_at is not null),
                count(*) filter (where dismissed_at is not null),
                count(*) filter (where purchased_at is null and dismissed_at is null and expires_at is not null and expires_at <= $2),
                count(*) filter (where purchased_at is null and dismissed_at is null and (expires_at is null or expires_at > $2))
         from musubi.offer_activations where offer_id = $1",
    )
    .bind(&id)
    .bind(now)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let revenue: Vec<(String, i64)> = sqlx::query_as(
        "select p.currency, coalesce(sum(p.amount), 0)::bigint from musubi.offer_activations a
         join musubi.purchases p on p.id = a.purchase_id where a.offer_id = $1 group by p.currency",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "offer_id": id, "activations": activations, "accounts": accounts, "seen": seen,
        "purchased": purchased, "dismissed": dismissed, "expired": expired, "active": active,
        "conversion": if seen > 0 { purchased as f64 / seen as f64 } else { 0.0 },
        "revenue": revenue.into_iter().map(|(c, a)| json!({ "currency": c, "amount": a })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------- evaluation

#[derive(Debug, Clone)]
struct Activation {
    id: Uuid,
    offer_id: String,
    activated_at: i64,
    expires_at: Option<i64>,
    seen_at: Option<i64>,
    purchased_at: Option<i64>,
    dismissed_at: Option<i64>,
}

impl Activation {
    fn live(&self, now: i64) -> bool {
        self.purchased_at.is_none()
            && self.dismissed_at.is_none()
            && self.expires_at.is_none_or(|e| e > now)
    }
    fn ended_at(&self) -> i64 {
        self.purchased_at
            .or(self.dismissed_at)
            .or(self.expires_at)
            .unwrap_or(self.activated_at)
    }
}

async fn history(s: &AppState, account: Uuid, offers: &[String]) -> Result<Vec<Activation>> {
    type Row = (
        Uuid,
        String,
        i64,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select id, offer_id, activated_at, expires_at, seen_at, purchased_at, dismissed_at
         from musubi.offer_activations where account_id = $1 and offer_id = any($2) order by activated_at",
    )
    .bind(account)
    .bind(offers)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(
            |(id, offer_id, activated_at, expires_at, seen_at, purchased_at, dismissed_at)| {
                Activation {
                    id,
                    offer_id,
                    activated_at,
                    expires_at,
                    seen_at,
                    purchased_at,
                    dismissed_at,
                }
            },
        )
        .collect())
}

async fn live_activation(s: &AppState, account: Uuid, offer: &str) -> Result<Option<Activation>> {
    let now = now_s();
    Ok(history(s, account, &[offer.to_string()])
        .await?
        .into_iter()
        .rev()
        .find(|a| a.live(now)))
}

/// Offers an account sees in a placement right now; creates activations for newly eligible
/// ones and (when `mark_seen`) stamps them seen.
async fn evaluate(
    s: &AppState,
    placement: &Placement,
    account: AccountId,
    mark_seen: bool,
) -> Result<Vec<Value>> {
    let offers = offers_of_placement(s, &placement.id).await?;
    if offers.is_empty() {
        return Ok(Vec::new());
    }
    let now = now_s();
    let ids: Vec<String> = offers.iter().map(|o| o.id.clone()).collect();
    let mut precursors: Vec<String> = offers.iter().filter_map(|o| o.precursor.clone()).collect();
    precursors.retain(|p| !ids.contains(p));
    let mut all_ids = ids.clone();
    all_ids.extend(precursors);
    let hist = history(s, account.0, &all_ids).await?;
    let overrides: Vec<(String, bool)> = sqlx::query_as(
        "select offer_id, force_enabled from musubi.offer_overrides where account_id = $1 and offer_id = any($2)",
    )
    .bind(account.0)
    .bind(&ids)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let overrides: HashMap<String, bool> = overrides.into_iter().collect();
    let audiences = s.audiences.of(&s.db, &s.economy, account).await?;
    let purchased: HashSet<&str> = hist
        .iter()
        .filter(|a| a.purchased_at.is_some())
        .map(|a| a.offer_id.as_str())
        .collect();
    let listings: HashMap<String, Value> = s
        .economy
        .store(Some(account), true)
        .await?
        .into_iter()
        .map(|i| (i.id.clone(), i.prices.clone()))
        .collect();

    let mut shown: Vec<(Offer, Activation, bool)> = Vec::new();
    for o in offers {
        let forced = overrides.get(&o.id).copied();
        if forced == Some(false) {
            continue;
        }
        let mine: Vec<&Activation> = hist.iter().filter(|a| a.offer_id == o.id).collect();
        if let Some(live) = mine.iter().rev().find(|a| a.live(now)) {
            shown.push((o.clone(), (*live).clone(), false));
            continue;
        }
        if forced != Some(true) {
            if !o.enabled
                || o.starts_at.is_some_and(|t| t > now)
                || o.ends_at.is_some_and(|t| t <= now)
                || o.audience.as_ref().is_some_and(|a| !audiences.contains(a))
            {
                continue;
            }
            let bought = mine.iter().filter(|a| a.purchased_at.is_some()).count() as i32;
            if o.max_purchases.is_some_and(|m| bought >= m)
                || o.max_activations.is_some_and(|m| mine.len() as i32 >= m)
            {
                continue;
            }
            if let Some(last) = mine.last() {
                if last.ended_at() + o.cooldown_secs > now {
                    continue;
                }
            }
            if o.precursor
                .as_ref()
                .is_some_and(|p| !purchased.contains(p.as_str()))
            {
                continue;
            }
        }
        if shown.len() >= placement.max_offers as usize {
            break;
        }
        let expires_at = match (o.duration_secs, o.ends_at) {
            (Some(d), Some(e)) => Some((now + d).min(e)),
            (Some(d), None) => Some(now + d),
            (None, e) => e,
        };
        let act = Activation {
            id: Uuid::now_v7(),
            offer_id: o.id.clone(),
            activated_at: now,
            expires_at,
            seen_at: None,
            purchased_at: None,
            dismissed_at: None,
        };
        sqlx::query(
            "insert into musubi.offer_activations (id, offer_id, placement_id, account_id, activated_at, expires_at)
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(act.id)
        .bind(&o.id)
        .bind(&placement.id)
        .bind(account.0)
        .bind(now)
        .bind(expires_at)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        s.events
            .emit(
                "offer.activated",
                Some(account),
                json!({ "offer_id": o.id, "placement_id": placement.id, "activation_id": act.id, "expires_at": expires_at, "forced": forced == Some(true) }),
            )
            .await;
        shown.push((o, act, true));
    }
    shown.sort_by(|a, b| {
        b.0.priority
            .cmp(&a.0.priority)
            .then(a.1.activated_at.cmp(&b.1.activated_at))
    });
    shown.truncate(placement.max_offers as usize);
    if mark_seen {
        let unseen: Vec<Uuid> = shown
            .iter()
            .filter(|(_, a, _)| a.seen_at.is_none())
            .map(|(_, a, _)| a.id)
            .collect();
        if !unseen.is_empty() {
            sqlx::query("update musubi.offer_activations set seen_at = $2 where id = any($1) and seen_at is null")
                .bind(&unseen)
                .bind(now)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
            for (o, a, _) in shown.iter().filter(|(_, a, _)| unseen.contains(&a.id)) {
                s.events
                    .emit("offer.seen", Some(account), json!({ "offer_id": o.id, "placement_id": placement.id, "activation_id": a.id }))
                    .await;
            }
        }
    }
    Ok(shown
        .into_iter()
        .map(|(o, a, fresh)| {
            json!({
                "offer_id": o.id, "placement_id": o.placement_id, "name": o.name, "description": o.description,
                "store_item_id": o.store_item_id,
                "prices": o.prices.clone().or_else(|| listings.get(&o.store_item_id).cloned()).unwrap_or(json!({})),
                "priority": o.priority, "activation_id": a.id, "activated_at": a.activated_at,
                "expires_at": a.expires_at, "seen": a.seen_at.is_some() || mark_seen, "new": fresh,
                "metadata": o.metadata,
            })
        })
        .collect())
}

/// `GET /v1/placements/{id}/offers` — what the caller sees in this placement (marks seen).
async fn my_offers(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = load_placement(&s, &id).await?;
    Ok(Json(
        json!({ "placement_id": p.id, "max_offers": p.max_offers, "offers": evaluate(&s, &p, me.account_id, true).await? }),
    ))
}

#[derive(Deserialize)]
struct AccountQ {
    account_id: Uuid,
}

/// `GET /v1/server/placements/{id}/offers?account_id=` — preview for one account (does not
/// mark seen; does activate newly eligible offers, like the client would).
async fn placement_offers_for(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<AccountQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = load_placement(&s, &id).await?;
    Ok(Json(
        json!({ "placement_id": p.id, "offers": evaluate(&s, &p, AccountId(q.account_id), false).await? }),
    ))
}

#[derive(Deserialize)]
struct BuyBody {
    currency: String,
    #[serde(default)]
    idempotency_key: Option<String>,
    #[serde(default)]
    coupon: Option<String>,
}

/// `POST /v1/placements/{id}/offers/{offer_id}/buy {currency, idempotency_key?}` — purchase the
/// offer's store item at the offer price; needs a live activation.
async fn buy_offer(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((placement_id, offer_id)): Path<(String, String)>,
    Json(b): Json<BuyBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    let offer = load_offer(&s, &offer_id).await?;
    if offer.placement_id != placement_id {
        return Err(Error::NotFound("offer".into()).into());
    }
    let Some(act) = live_activation(&s, me.account_id.0, &offer.id).await? else {
        return Err(Error::NotFound("offer is not active for this account".into()).into());
    };
    let price = match &offer.prices {
        Some(p) => Some(p.get(&b.currency).and_then(Value::as_i64).ok_or_else(|| {
            Error::InvalidArgument(format!("offer has no price in {}", b.currency))
        })?),
        None => None,
    };
    let purchase = s
        .economy
        .buy_with_price(
            me.account_id,
            &offer.store_item_id,
            &b.currency,
            b.idempotency_key.as_deref(),
            b.coupon.as_deref(),
            price,
        )
        .await?;
    let now = now_s();
    sqlx::query(
        "update musubi.offer_activations set purchased_at = $2, purchase_id = $3 where id = $1 and purchased_at is null",
    )
    .bind(act.id)
    .bind(now)
    .bind(purchase.id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "offer.purchased",
            Some(me.account_id),
            json!({ "offer_id": offer.id, "placement_id": placement_id, "activation_id": act.id, "purchase_id": purchase.id, "currency": purchase.currency, "amount": purchase.amount }),
        )
        .await;
    Ok(Json(
        json!({ "offer_id": offer.id, "activation_id": act.id, "purchase": purchase }),
    ))
}

/// `POST /v1/placements/{id}/offers/{offer_id}/dismiss` — hide it; cooldown starts now.
async fn dismiss_offer(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((placement_id, offer_id)): Path<(String, String)>,
) -> std::result::Result<Json<Value>, ApiError> {
    let offer = load_offer(&s, &offer_id).await?;
    if offer.placement_id != placement_id {
        return Err(Error::NotFound("offer".into()).into());
    }
    let Some(act) = live_activation(&s, me.account_id.0, &offer.id).await? else {
        return Err(Error::NotFound("offer is not active for this account".into()).into());
    };
    sqlx::query("update musubi.offer_activations set dismissed_at = $2 where id = $1 and dismissed_at is null")
        .bind(act.id)
        .bind(now_s())
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.events
        .emit(
            "offer.dismissed",
            Some(me.account_id),
            json!({ "offer_id": offer.id, "placement_id": placement_id, "activation_id": act.id }),
        )
        .await;
    Ok(Json(json!({ "dismissed": true })))
}
