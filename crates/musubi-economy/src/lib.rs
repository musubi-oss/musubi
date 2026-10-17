//! Economy v0 (open-source Hiro direction): currencies and wallets with an append-only, idempotent
//! ledger (no negative balances), an item catalog with stacks / instances, and a store whose
//! purchases debit the wallet and grant the item in one transaction.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub mod achievements;
pub mod iap;
pub mod mailbox;
pub mod market;
pub mod orderbook;
pub use achievements::Achievement;
pub use mailbox::{Mail, SendMail};
pub use market::{
    EscrowItem, Listing, ListingFilter, MarketRules, NewListing, NewTrade, OfferItem, Sale,
    Settled, Trade, TradeSide, WantItem,
};
pub use orderbook::{Book, Fill, NewOrder, Order, Placed, Side};
pub mod progression;
pub mod quests;
pub mod seasons;
pub mod treasury;
pub use iap::{IapProduct, Receipt, ReceiptInput, SteamOrder};
pub use quests::Quest;
pub use seasons::Season;
pub use treasury::TreasuryEntry;
pub mod rewards;
pub use progression::{EnergyDef, Level, StreakDef, Track};
pub use rewards::{Pity, RewardEntry, RewardTable, Roll};

pub const MAX_LEDGER_PAGE: i64 = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Currency {
    pub code: String,
    pub name: String,
    pub max_balance: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    pub currency: String,
    pub balance: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: Uuid,
    pub currency: String,
    pub delta: i64,
    pub balance_after: i64,
    pub reason: String,
    #[serde(rename = "ref")]
    pub reference: String,
    pub idempotency_key: Option<String>,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub category: String,
    pub stackable: bool,
    pub max_stack: i32,
    pub properties: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateItem {
    pub id: String,
    pub name: String,
    pub category: String,
    pub stackable: Option<bool>,
    pub max_stack: Option<i32>,
    pub properties: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub id: Uuid,
    pub item_id: String,
    pub quantity: i64,
    pub instance: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreItem {
    pub id: String,
    pub item_id: String,
    pub quantity: i64,
    /// currency code → list price
    pub prices: Value,
    pub enabled: bool,
    pub max_per_account: Option<i32>,
    /// Unix seconds; outside `[available_from, available_until)` the listing is hidden and unbuyable.
    #[serde(default)]
    pub available_from: Option<i64>,
    #[serde(default)]
    pub available_until: Option<i64>,
    /// Extra items granted with the purchase (the main `item_id` is always granted).
    #[serde(default)]
    pub bundle: Vec<BundleEntry>,
    /// Requires this level on `level_track` (or on any track when unset).
    #[serde(default)]
    pub min_level: Option<i32>,
    #[serde(default)]
    pub level_track: Option<String>,
    /// 0..=100, applied while `discount_until` has not passed (or forever when unset).
    #[serde(default)]
    pub discount_percent: i32,
    #[serde(default)]
    pub discount_until: Option<i64>,
    /// Daily rotation group: of all enabled listings sharing a `rotation`, only `rotation_slots`
    /// are on sale each UTC day, chosen deterministically from the date.
    #[serde(default)]
    pub rotation: Option<String>,
    #[serde(default)]
    pub rotation_slots: i32,
    /// Filled for a viewer: prices after the current discount.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub effective_prices: Value,
    /// Filled for a viewer: why they cannot buy it right now (`level`), listing still shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct BundleEntry {
    pub item_id: String,
    #[serde(default = "one")]
    pub quantity: i64,
}

fn one() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateStoreItem {
    pub id: String,
    pub item_id: String,
    pub quantity: Option<i64>,
    pub prices: Value,
    pub enabled: Option<bool>,
    pub max_per_account: Option<i32>,
    pub available_from: Option<i64>,
    pub available_until: Option<i64>,
    pub bundle: Vec<BundleEntry>,
    pub min_level: Option<i32>,
    pub level_track: Option<String>,
    pub discount_percent: Option<i32>,
    pub discount_until: Option<i64>,
    pub rotation: Option<String>,
    pub rotation_slots: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Coupon {
    pub code: String,
    /// Restrict to one listing; `None` = any.
    pub store_item_id: Option<String>,
    pub percent: i32,
    /// `None` = unlimited.
    pub uses_left: Option<i32>,
    /// Default true: one redemption per account.
    pub per_account_once: bool,
    pub expires_at: Option<i64>,
}

impl Default for Coupon {
    fn default() -> Self {
        Self {
            code: String::new(),
            store_item_id: None,
            percent: 0,
            uses_left: None,
            per_account_once: true,
            expires_at: None,
        }
    }
}

const STORE_COLS: &str =
    "json_build_object('id', id, 'item_id', item_id, 'quantity', quantity, 'prices', prices,
    'enabled', enabled, 'max_per_account', max_per_account,
    'available_from', extract(epoch from available_from)::bigint,
    'available_until', extract(epoch from available_until)::bigint,
    'bundle', bundle, 'min_level', min_level, 'level_track', level_track,
    'discount_percent', discount_percent,
    'discount_until', extract(epoch from discount_until)::bigint,
    'rotation', rotation, 'rotation_slots', rotation_slots)";

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

impl StoreItem {
    fn from_row(v: Value) -> Result<Self> {
        serde_json::from_value(v).map_err(Error::internal)
    }

    pub fn in_window(&self, now: i64) -> bool {
        self.available_from.is_none_or(|f| f <= now) && self.available_until.is_none_or(|u| now < u)
    }

    pub fn discount_now(&self, now: i64) -> i32 {
        if self.discount_percent > 0 && self.discount_until.is_none_or(|u| now < u) {
            self.discount_percent.clamp(0, 100)
        } else {
            0
        }
    }

    fn fill_effective(&mut self, now: i64) {
        let d = self.discount_now(now) as i64;
        self.effective_prices = match self.prices.as_object() {
            Some(m) => Value::Object(
                m.iter()
                    .map(|(k, v)| {
                        let p = v.as_i64().unwrap_or(0);
                        (k.clone(), Value::from(p - p * d / 100))
                    })
                    .collect(),
            ),
            None => Value::Null,
        };
    }
}

/// FNV-1a: stable across builds, so every node agrees on today's rotation.
fn fnv1a(parts: &[&str]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for b in p.bytes().chain(std::iter::once(0u8)) {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

fn level_ok(
    levels: &std::collections::HashMap<String, i32>,
    track: Option<&str>,
    min: i32,
) -> bool {
    match track {
        Some(t) => levels.get(t).is_some_and(|l| *l >= min),
        None => levels.values().any(|l| *l >= min),
    }
}

/// Which listings of each rotation group are on sale on `day` (UTC day number).
fn rotation_filter(items: Vec<StoreItem>, day: i64) -> Vec<StoreItem> {
    let mut groups: std::collections::HashMap<String, Vec<(u64, String)>> = Default::default();
    for it in &items {
        if let (Some(r), true) = (&it.rotation, it.rotation_slots > 0) {
            groups
                .entry(r.clone())
                .or_default()
                .push((fnv1a(&[r, &day.to_string(), &it.id]), it.id.clone()));
        }
    }
    let mut on_sale = std::collections::HashSet::new();
    for (r, mut v) in groups {
        v.sort();
        let slots = items
            .iter()
            .filter(|i| i.rotation.as_deref() == Some(&r))
            .map(|i| i.rotation_slots)
            .max()
            .unwrap_or(0) as usize;
        on_sale.extend(v.into_iter().take(slots).map(|(_, id)| id));
    }
    items
        .into_iter()
        .filter(|i| i.rotation.is_none() || i.rotation_slots <= 0 || on_sale.contains(&i.id))
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Purchase {
    pub id: Uuid,
    pub store_item_id: String,
    pub currency: String,
    pub amount: i64,
    pub balance: i64,
    pub granted: InventoryEntry,
    /// Bundle entries granted alongside `granted`.
    #[serde(default)]
    pub bundle: Vec<InventoryEntry>,
    pub replayed: bool,
}

#[derive(Clone)]
pub struct EconomyService {
    db: PgPool,
}

type LedgerRow = (
    Uuid,
    String,
    i64,
    i64,
    String,
    String,
    Option<String>,
    Value,
    OffsetDateTime,
);

fn ledger(r: LedgerRow) -> LedgerEntry {
    LedgerEntry {
        id: r.0,
        currency: r.1,
        delta: r.2,
        balance_after: r.3,
        reason: r.4,
        reference: r.5,
        idempotency_key: r.6,
        metadata: r.7,
        created_at: r.8,
    }
}

const LEDGER_COLS: &str =
    "id, currency, delta, balance_after, reason, ref, idempotency_key, metadata, created_at";

pub(crate) fn check_code(code: &str) -> Result<()> {
    if code.is_empty()
        || code.len() > 64
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::InvalidArgument(
            "code: 1..=64 of [A-Za-z0-9_-]".into(),
        ));
    }
    Ok(())
}

impl EconomyService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    // ----- currencies -----

    pub async fn upsert_currency(
        &self,
        code: &str,
        name: &str,
        max_balance: Option<i64>,
    ) -> Result<Currency> {
        check_code(code)?;
        sqlx::query(
            "insert into musubi.currencies (code, name, max_balance) values ($1, $2, $3)
             on conflict (code) do update set name = excluded.name, max_balance = excluded.max_balance",
        )
        .bind(code)
        .bind(name)
        .bind(max_balance)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(Currency {
            code: code.into(),
            name: name.into(),
            max_balance,
        })
    }

    pub async fn currencies(&self) -> Result<Vec<Currency>> {
        let rows: Vec<(String, String, Option<i64>)> =
            sqlx::query_as("select code, name, max_balance from musubi.currencies order by code")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| Currency {
                code: r.0,
                name: r.1,
                max_balance: r.2,
            })
            .collect())
    }

    // ----- wallet -----

    pub async fn wallet(&self, account: AccountId) -> Result<Vec<Balance>> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "select currency, balance from musubi.wallets where account_id = $1 order by currency",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| Balance {
                currency: r.0,
                balance: r.1,
            })
            .collect())
    }

    pub async fn ledger(
        &self,
        account: AccountId,
        currency: Option<&str>,
        limit: i64,
    ) -> Result<Vec<LedgerEntry>> {
        let rows: Vec<LedgerRow> = sqlx::query_as(&format!(
            "select {LEDGER_COLS} from musubi.wallet_ledger where account_id = $1 and ($2::text is null or currency = $2)
             order by created_at desc, id desc limit $3"
        ))
        .bind(account.0)
        .bind(currency)
        .bind(limit.clamp(1, MAX_LEDGER_PAGE))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(ledger).collect())
    }

    /// Apply `delta` (positive = grant, negative = spend) atomically. With an idempotency key a
    /// repeat returns the original entry instead of applying twice.
    #[allow(clippy::too_many_arguments)]
    pub async fn adjust(
        &self,
        account: AccountId,
        currency: &str,
        delta: i64,
        reason: &str,
        reference: &str,
        idempotency_key: Option<&str>,
        metadata: Value,
    ) -> Result<(LedgerEntry, bool)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let r = Self::adjust_tx(
            &mut tx,
            account,
            currency,
            delta,
            reason,
            reference,
            idempotency_key,
            metadata,
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn adjust_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        currency: &str,
        delta: i64,
        reason: &str,
        reference: &str,
        idempotency_key: Option<&str>,
        metadata: Value,
    ) -> Result<(LedgerEntry, bool)> {
        if delta == 0 {
            return Err(Error::InvalidArgument("delta must be non-zero".into()));
        }
        if let Some(k) = idempotency_key {
            let prior: Option<LedgerRow> = sqlx::query_as(&format!(
                "select {LEDGER_COLS} from musubi.wallet_ledger where account_id = $1 and idempotency_key = $2"
            ))
            .bind(account.0)
            .bind(k)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
            if let Some(p) = prior {
                return Ok((ledger(p), true));
            }
        }
        let cur: Option<(String, Option<i64>)> =
            sqlx::query_as("select code, max_balance from musubi.currencies where code = $1")
                .bind(currency)
                .fetch_optional(&mut **tx)
                .await
                .map_err(Error::internal)?;
        let Some((_, max)) = cur else {
            return Err(Error::NotFound(format!("currency {currency}")));
        };
        sqlx::query("insert into musubi.wallets (account_id, currency) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(currency)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        let (balance,): (i64,) = sqlx::query_as(
            "select balance from musubi.wallets where account_id = $1 and currency = $2 for update",
        )
        .bind(account.0)
        .bind(currency)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let after = balance
            .checked_add(delta)
            .ok_or_else(|| Error::InvalidArgument("overflow".into()))?;
        if after < 0 {
            return Err(Error::ResourceExhausted(format!(
                "insufficient {currency}: have {balance}, need {}",
                -delta
            )));
        }
        if let Some(m) = max {
            if after > m {
                return Err(Error::ResourceExhausted(format!(
                    "{currency} capped at {m}"
                )));
            }
        }
        sqlx::query("update musubi.wallets set balance = $3, updated_at = now() where account_id = $1 and currency = $2")
            .bind(account.0)
            .bind(currency)
            .bind(after)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        let row: LedgerRow = sqlx::query_as(&format!(
            "insert into musubi.wallet_ledger (id, account_id, currency, delta, balance_after, reason, ref, idempotency_key, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning {LEDGER_COLS}"
        ))
        .bind(Uuid::now_v7())
        .bind(account.0)
        .bind(currency)
        .bind(delta)
        .bind(after)
        .bind(reason)
        .bind(reference)
        .bind(idempotency_key)
        .bind(metadata)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok((ledger(row), false))
    }

    // ----- items & inventory -----

    pub async fn upsert_item(&self, c: CreateItem) -> Result<Item> {
        check_code(&c.id)?;
        let item = Item {
            id: c.id,
            name: c.name,
            category: c.category,
            stackable: c.stackable.unwrap_or(true),
            max_stack: c.max_stack.unwrap_or(i32::MAX).max(1),
            properties: if c.properties.is_object() {
                c.properties
            } else {
                Value::Object(Default::default())
            },
        };
        sqlx::query(
            "insert into musubi.items (id, name, category, stackable, max_stack, properties) values ($1, $2, $3, $4, $5, $6)
             on conflict (id) do update set name = excluded.name, category = excluded.category, stackable = excluded.stackable,
             max_stack = excluded.max_stack, properties = excluded.properties",
        )
        .bind(&item.id)
        .bind(&item.name)
        .bind(&item.category)
        .bind(item.stackable)
        .bind(item.max_stack)
        .bind(&item.properties)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(item)
    }

    pub async fn items(&self) -> Result<Vec<Item>> {
        let rows: Vec<(String, String, String, bool, i32, Value)> =
            sqlx::query_as("select id, name, category, stackable, max_stack, properties from musubi.items order by id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| Item {
                id: r.0,
                name: r.1,
                category: r.2,
                stackable: r.3,
                max_stack: r.4,
                properties: r.5,
            })
            .collect())
    }

    pub async fn inventory(&self, account: AccountId) -> Result<Vec<InventoryEntry>> {
        let rows: Vec<(Uuid, String, i64, Value)> = sqlx::query_as(
            "select id, item_id, quantity, instance from musubi.inventory where account_id = $1 order by item_id, created_at",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| InventoryEntry {
                id: r.0,
                item_id: r.1,
                quantity: r.2,
                instance: r.3,
            })
            .collect())
    }

    pub async fn grant_item(
        &self,
        account: AccountId,
        item_id: &str,
        quantity: i64,
        instance: Value,
    ) -> Result<InventoryEntry> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let e = Self::grant_item_tx(&mut tx, account, item_id, quantity, instance).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(e)
    }

    pub(crate) async fn grant_item_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        item_id: &str,
        quantity: i64,
        instance: Value,
    ) -> Result<InventoryEntry> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let item: Option<(bool, i32)> =
            sqlx::query_as("select stackable, max_stack from musubi.items where id = $1")
                .bind(item_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(Error::internal)?;
        let Some((stackable, max_stack)) = item else {
            return Err(Error::NotFound(format!("item {item_id}")));
        };
        let instance = if instance.is_object() {
            instance
        } else {
            Value::Object(Default::default())
        };
        // stackable items with default instance data merge into the existing stack
        if stackable && instance.as_object().is_some_and(|m| m.is_empty()) {
            let existing: Option<(Uuid, i64)> = sqlx::query_as(
                "select id, quantity from musubi.inventory where account_id = $1 and item_id = $2 and instance = '{}'::jsonb
                 order by created_at limit 1 for update",
            )
            .bind(account.0)
            .bind(item_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
            if let Some((id, q)) = existing {
                let nq = q
                    .checked_add(quantity)
                    .ok_or_else(|| Error::InvalidArgument("overflow".into()))?;
                if nq > max_stack as i64 {
                    return Err(Error::ResourceExhausted(format!(
                        "stack of {item_id} capped at {max_stack}"
                    )));
                }
                sqlx::query(
                    "update musubi.inventory set quantity = $2, updated_at = now() where id = $1",
                )
                .bind(id)
                .bind(nq)
                .execute(&mut **tx)
                .await
                .map_err(Error::internal)?;
                return Ok(InventoryEntry {
                    id,
                    item_id: item_id.into(),
                    quantity: nq,
                    instance,
                });
            }
        }
        if !stackable && quantity != 1 {
            return Err(Error::InvalidArgument(format!(
                "{item_id} is not stackable"
            )));
        }
        if quantity > max_stack as i64 {
            return Err(Error::ResourceExhausted(format!(
                "stack of {item_id} capped at {max_stack}"
            )));
        }
        let id = Uuid::now_v7();
        sqlx::query("insert into musubi.inventory (id, account_id, item_id, quantity, instance) values ($1, $2, $3, $4, $5)")
            .bind(id)
            .bind(account.0)
            .bind(item_id)
            .bind(quantity)
            .bind(&instance)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        Ok(InventoryEntry {
            id,
            item_id: item_id.into(),
            quantity,
            instance,
        })
    }

    /// Remove `quantity` from one inventory entry (deleting it when it reaches zero).
    pub async fn consume(
        &self,
        account: AccountId,
        entry_id: Uuid,
        quantity: i64,
    ) -> Result<Option<InventoryEntry>> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(String, i64, Value)> = sqlx::query_as(
            "select item_id, quantity, instance from musubi.inventory where id = $1 and account_id = $2 for update",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, have, instance)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        if have < quantity {
            return Err(Error::ResourceExhausted(format!(
                "have {have} of {item_id}, consuming {quantity}"
            )));
        }
        let left = have - quantity;
        if left == 0 {
            sqlx::query("delete from musubi.inventory where id = $1")
                .bind(entry_id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        } else {
            sqlx::query(
                "update musubi.inventory set quantity = $2, updated_at = now() where id = $1",
            )
            .bind(entry_id)
            .bind(left)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok((left > 0).then_some(InventoryEntry {
            id: entry_id,
            item_id,
            quantity: left,
            instance,
        }))
    }

    // ----- store -----

    pub async fn upsert_store_item(&self, c: CreateStoreItem) -> Result<StoreItem> {
        check_code(&c.id)?;
        // An empty price map is allowed: the listing is then only obtainable via IAP / rewards.
        if !c.prices.is_object() {
            return Err(Error::InvalidArgument("prices: {currency: amount}".into()));
        }
        for (k, v) in c.prices.as_object().unwrap() {
            if !v.as_i64().is_some_and(|p| p >= 0) {
                return Err(Error::InvalidArgument(format!(
                    "price for {k} must be a non-negative integer"
                )));
            }
        }
        let discount = c.discount_percent.unwrap_or(0);
        if !(0..=100).contains(&discount) {
            return Err(Error::InvalidArgument(
                "discount_percent must be 0..=100".into(),
            ));
        }
        if c.bundle.len() > 32
            || c.bundle
                .iter()
                .any(|b| b.quantity < 1 || b.item_id.is_empty())
        {
            return Err(Error::InvalidArgument(
                "bundle: up to 32 entries of {item_id, quantity >= 1}".into(),
            ));
        }
        if let (Some(f), Some(u)) = (c.available_from, c.available_until) {
            if u <= f {
                return Err(Error::InvalidArgument(
                    "available_until must be after available_from".into(),
                ));
            }
        }
        let s = StoreItem {
            id: c.id,
            item_id: c.item_id,
            quantity: c.quantity.unwrap_or(1).max(1),
            prices: c.prices,
            enabled: c.enabled.unwrap_or(true),
            max_per_account: c.max_per_account,
            available_from: c.available_from,
            available_until: c.available_until,
            bundle: c.bundle,
            min_level: c.min_level,
            level_track: c.level_track,
            discount_percent: discount,
            discount_until: c.discount_until,
            rotation: c.rotation.filter(|r| !r.is_empty()),
            rotation_slots: c.rotation_slots.unwrap_or(0).max(0),
            effective_prices: Value::Null,
            locked: None,
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        for b in &s.bundle {
            let known: Option<(String,)> =
                sqlx::query_as("select id from musubi.items where id = $1")
                    .bind(&b.item_id)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
            if known.is_none() {
                return Err(Error::NotFound(format!("bundle item {}", b.item_id)));
            }
        }
        sqlx::query(
            "insert into musubi.store_items (id, item_id, quantity, prices, enabled, max_per_account,
                available_from, available_until, bundle, min_level, level_track, discount_percent,
                discount_until, rotation, rotation_slots)
             values ($1, $2, $3, $4, $5, $6, to_timestamp($7), to_timestamp($8), $9, $10, $11, $12,
                to_timestamp($13), $14, $15)
             on conflict (id) do update set item_id = excluded.item_id, quantity = excluded.quantity,
                prices = excluded.prices, enabled = excluded.enabled,
                max_per_account = excluded.max_per_account, available_from = excluded.available_from,
                available_until = excluded.available_until, bundle = excluded.bundle,
                min_level = excluded.min_level, level_track = excluded.level_track,
                discount_percent = excluded.discount_percent, discount_until = excluded.discount_until,
                rotation = excluded.rotation, rotation_slots = excluded.rotation_slots",
        )
        .bind(&s.id)
        .bind(&s.item_id)
        .bind(s.quantity)
        .bind(&s.prices)
        .bind(s.enabled)
        .bind(s.max_per_account)
        .bind(s.available_from.map(|v| v as f64))
        .bind(s.available_until.map(|v| v as f64))
        .bind(serde_json::to_value(&s.bundle).map_err(Error::internal)?)
        .bind(s.min_level)
        .bind(&s.level_track)
        .bind(s.discount_percent)
        .bind(s.discount_until.map(|v| v as f64))
        .bind(&s.rotation)
        .bind(s.rotation_slots)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => Error::NotFound(format!("item {}", s.item_id)),
            e => Error::internal(e),
        })?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(s)
    }

    /// Listings on sale now (window + today's rotation). With a `viewer`, discounted prices are
    /// filled in and level-gated listings are marked `locked` instead of hidden.
    /// `include_disabled` (server view) skips every filter and shows the raw catalogue.
    pub async fn store(
        &self,
        viewer: Option<AccountId>,
        include_disabled: bool,
    ) -> Result<Vec<StoreItem>> {
        let rows: Vec<(Value,)> = sqlx::query_as(&format!(
            "select {STORE_COLS} from musubi.store_items where enabled or $1 order by id"
        ))
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut items = rows
            .into_iter()
            .map(|(v,)| StoreItem::from_row(v))
            .collect::<Result<Vec<_>>>()?;
        if include_disabled {
            return Ok(items);
        }
        let now = now_unix();
        items.retain(|i| i.in_window(now));
        items = rotation_filter(items, now.div_euclid(86_400));
        let levels = match viewer {
            Some(a) if items.iter().any(|i| i.min_level.is_some()) => Some(self.levels(a).await?),
            _ => None,
        };
        for it in &mut items {
            it.fill_effective(now);
            if let (Some(min), Some(levels)) = (it.min_level, &levels) {
                if !level_ok(levels, it.level_track.as_deref(), min) {
                    it.locked = Some("level".into());
                }
            }
        }
        Ok(items)
    }

    /// track → level for the account.
    async fn levels(&self, account: AccountId) -> Result<std::collections::HashMap<String, i32>> {
        Ok(self
            .progress(account)
            .await?
            .into_iter()
            .map(|p| (p.track_id, p.level))
            .collect())
    }

    pub async fn upsert_coupon(&self, c: Coupon) -> Result<Coupon> {
        check_code(&c.code)?;
        if !(1..=100).contains(&c.percent) {
            return Err(Error::InvalidArgument("percent must be 1..=100".into()));
        }
        if c.uses_left.is_some_and(|u| u < 0) {
            return Err(Error::InvalidArgument("uses_left must be >= 0".into()));
        }
        sqlx::query(
            "insert into musubi.store_coupons (code, store_item_id, percent, uses_left, per_account_once, expires_at)
             values ($1, $2, $3, $4, $5, to_timestamp($6))
             on conflict (code) do update set store_item_id = excluded.store_item_id, percent = excluded.percent,
                uses_left = excluded.uses_left, per_account_once = excluded.per_account_once,
                expires_at = excluded.expires_at",
        )
        .bind(&c.code)
        .bind(&c.store_item_id)
        .bind(c.percent)
        .bind(c.uses_left)
        .bind(c.per_account_once)
        .bind(c.expires_at.map(|v| v as f64))
        .execute(&self.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
                Error::NotFound(format!("store item {}", c.store_item_id.clone().unwrap_or_default()))
            }
            e => Error::internal(e),
        })?;
        Ok(c)
    }

    pub async fn coupons(&self) -> Result<Vec<Coupon>> {
        type Row = (String, Option<String>, i32, Option<i32>, bool, Option<i64>);
        let rows: Vec<Row> = sqlx::query_as(
            "select code, store_item_id, percent, uses_left, per_account_once,
                    extract(epoch from expires_at)::bigint from musubi.store_coupons order by code",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| Coupon {
                code: r.0,
                store_item_id: r.1,
                percent: r.2,
                uses_left: r.3,
                per_account_once: r.4,
                expires_at: r.5,
            })
            .collect())
    }

    pub async fn delete_coupon(&self, code: &str) -> Result<bool> {
        let r = sqlx::query("delete from musubi.store_coupons where code = $1")
            .bind(code)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    /// Debit the price in `currency` and grant the item, atomically; idempotent per key.
    /// Debit the price in `currency` (after the listing's discount and an optional coupon) and
    /// grant the item plus its bundle, atomically; idempotent per key.
    pub async fn buy(
        &self,
        account: AccountId,
        store_item_id: &str,
        currency: &str,
        idempotency_key: Option<&str>,
        coupon: Option<&str>,
    ) -> Result<Purchase> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        if let Some(k) = idempotency_key {
            let prior: Option<(Uuid, String, i64)> = sqlx::query_as(
                "select id, currency, amount from musubi.purchases where account_id = $1 and idempotency_key = $2",
            )
            .bind(account.0)
            .bind(k)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if let Some((id, cur, amount)) = prior {
                let bal: Option<(i64,)> = sqlx::query_as(
                    "select balance from musubi.wallets where account_id = $1 and currency = $2",
                )
                .bind(account.0)
                .bind(&cur)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
                let granted: Option<(Uuid, String, i64, Value)> = sqlx::query_as(
                    "select i.id, i.item_id, i.quantity, i.instance from musubi.inventory i
                     join musubi.store_items s on s.item_id = i.item_id where i.account_id = $1 and s.id = $2 order by i.updated_at desc limit 1",
                )
                .bind(account.0)
                .bind(store_item_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
                let granted = granted.map(|g| InventoryEntry {
                    id: g.0,
                    item_id: g.1,
                    quantity: g.2,
                    instance: g.3,
                });
                return Ok(Purchase {
                    id,
                    store_item_id: store_item_id.into(),
                    currency: cur,
                    amount,
                    balance: bal.map(|b| b.0).unwrap_or(0),
                    granted: granted
                        .ok_or_else(|| Error::Internal("purchase without inventory".into()))?,
                    bundle: vec![],
                    replayed: true,
                });
            }
        }
        let s: Option<(Value,)> = sqlx::query_as(&format!(
            "select {STORE_COLS} from musubi.store_items where id = $1"
        ))
        .bind(store_item_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((row,)) = s else {
            return Err(Error::NotFound(format!("store item {store_item_id}")));
        };
        let listing = StoreItem::from_row(row)?;
        let (item_id, qty, max_per) = (
            listing.item_id.clone(),
            listing.quantity,
            listing.max_per_account,
        );
        if !listing.enabled {
            return Err(Error::PermissionDenied("store item disabled".into()));
        }
        let now = now_unix();
        if !listing.in_window(now) {
            return Err(Error::PermissionDenied(
                "store item not available now".into(),
            ));
        }
        if listing.rotation.is_some() && listing.rotation_slots > 0 {
            let on_sale = self.store(None, false).await?;
            if !on_sale.iter().any(|i| i.id == listing.id) {
                return Err(Error::PermissionDenied(
                    "store item not in today's rotation".into(),
                ));
            }
        }
        if let Some(min) = listing.min_level {
            let levels = self.levels(account).await?;
            if !level_ok(&levels, listing.level_track.as_deref(), min) {
                return Err(Error::PermissionDenied(format!("requires level {min}")));
            }
        }
        let list_price = listing
            .prices
            .get(currency)
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                Error::InvalidArgument(format!("{store_item_id} is not sold for {currency}"))
            })?;
        let mut price = list_price - list_price * listing.discount_now(now) as i64 / 100;
        if let Some(code) = coupon {
            type CouponRow = (Option<String>, i32, Option<i32>, bool, Option<i64>);
            let c: Option<CouponRow> = sqlx::query_as(
                "select store_item_id, percent, uses_left, per_account_once,
                        extract(epoch from expires_at)::bigint
                 from musubi.store_coupons where code = $1 for update",
            )
            .bind(code)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some((only, percent, uses_left, once, expires)) = c else {
                return Err(Error::NotFound("coupon".into()));
            };
            if only.as_deref().is_some_and(|o| o != store_item_id) {
                return Err(Error::InvalidArgument(
                    "coupon does not apply to this item".into(),
                ));
            }
            if expires.is_some_and(|e| e <= now) {
                return Err(Error::PermissionDenied("coupon expired".into()));
            }
            if uses_left.is_some_and(|u| u <= 0) {
                return Err(Error::ResourceExhausted("coupon used up".into()));
            }
            if once {
                let (used,): (i64,) = sqlx::query_as(
                    "select count(*) from musubi.store_coupon_uses where code = $1 and account_id = $2",
                )
                .bind(code)
                .bind(account.0)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
                if used > 0 {
                    return Err(Error::PermissionDenied("coupon already used".into()));
                }
            }
            price -= price * percent as i64 / 100;
            sqlx::query("update musubi.store_coupons set uses_left = uses_left - 1 where code = $1 and uses_left is not null")
                .bind(code)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        }
        if let Some(m) = max_per {
            let (n,): (i64,) = sqlx::query_as(
                "select count(*) from musubi.purchases where account_id = $1 and store_item_id = $2",
            )
            .bind(account.0)
            .bind(store_item_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if n >= m as i64 {
                return Err(Error::ResourceExhausted(format!(
                    "{store_item_id}: limit {m} per account"
                )));
            }
        }
        let id = Uuid::now_v7();
        let balance = if price > 0 {
            let (entry, _) = Self::adjust_tx(
                &mut tx,
                account,
                currency,
                -price,
                "store.buy",
                store_item_id,
                idempotency_key.map(|k| format!("buy:{k}")).as_deref(),
                serde_json::json!({ "purchase_id": id }),
            )
            .await?;
            entry.balance_after
        } else {
            0
        };
        let granted = Self::grant_item_tx(
            &mut tx,
            account,
            &item_id,
            qty,
            Value::Object(Default::default()),
        )
        .await?;
        let mut bundle = Vec::with_capacity(listing.bundle.len());
        for b in &listing.bundle {
            bundle.push(
                Self::grant_item_tx(
                    &mut tx,
                    account,
                    &b.item_id,
                    b.quantity,
                    Value::Object(Default::default()),
                )
                .await?,
            );
        }
        sqlx::query(
            "insert into musubi.purchases (id, account_id, store_item_id, currency, amount, idempotency_key, coupon)
             values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(account.0)
        .bind(store_item_id)
        .bind(currency)
        .bind(price)
        .bind(idempotency_key)
        .bind(coupon)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if let Some(code) = coupon {
            sqlx::query(
                "insert into musubi.store_coupon_uses (code, account_id, purchase_id) values ($1, $2, $3)",
            )
            .bind(code)
            .bind(account.0)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(Purchase {
            id,
            store_item_id: store_item_id.into(),
            currency: currency.into(),
            amount: price,
            balance,
            granted,
            bundle,
            replayed: false,
        })
    }
}
