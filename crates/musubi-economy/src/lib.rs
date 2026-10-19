//! Economy v0 (open-source Hiro direction): currencies and wallets with an append-only, idempotent
//! ledger (no negative balances), an item catalog with stacks / instances, and a store whose
//! purchases debit the wallet and grant the item in one transaction.

use musubi_core::{AccountId, Error, Result};
pub use musubi_storage::schema::{Migration, MigrationOp};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub mod achievements;
pub mod iap;
pub mod items_lifecycle;
pub mod mailbox;
pub mod market;
pub mod orderbook;
pub use achievements::{Achievement, AchievementUpdate};
pub use mailbox::{Audience, Mail, SendMail};
pub use market::{
    EscrowItem, Listing, ListingFilter, MarketRules, NewListing, NewTrade, OfferItem, Sale,
    Settled, Trade, TradeSide, WantItem,
};
pub use orderbook::{Book, Fill, NewOrder, Order, Placed, Side};
pub mod challenges;
pub mod group_inventory;
pub mod progression;
pub mod quest_pools;
pub mod quests;
pub mod seasons;
pub mod treasury;
pub use group_inventory::{GroupInventoryEntry, GroupItem};
pub use iap::{IapProduct, Receipt, ReceiptInput, SteamOrder};
pub use quests::Quest;
pub use seasons::Season;
pub use treasury::TreasuryEntry;
pub mod crafting;
pub mod graphs;
pub use graphs::{GraphNode, ProgressGraph};
pub mod modifiers;
pub use modifiers::RewardModifier;
pub mod rewards;
pub use crafting::{Crafted, Ingredient, Output, Recipe};
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
    /// Bumped on every change of the definition; the previous one stays in `item_versions`.
    #[serde(default = "one_i32")]
    pub version: i32,
    /// JSON schema the `instance` data of a grant must satisfy (`null` = anything).
    #[serde(default)]
    pub instance_schema: Value,
    /// How instance data granted under an older version is brought to the current one.
    #[serde(default)]
    pub migrations: Vec<Migration>,
    /// At most one entry of this item per account.
    #[serde(default)]
    pub unique: bool,
    /// When an entry becomes bound (no trade, market, deposit): never, on grant, on first equip.
    #[serde(default)]
    pub bind: Bind,
    #[serde(default = "yes")]
    pub tradeable: bool,
    /// Equipment slots the item fits (`[]` = not equippable).
    #[serde(default)]
    pub slots: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Bind {
    #[default]
    None,
    Pickup,
    Equip,
}

impl Bind {
    fn as_str(self) -> &'static str {
        match self {
            Bind::None => "none",
            Bind::Pickup => "pickup",
            Bind::Equip => "equip",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "pickup" => Bind::Pickup,
            "equip" => Bind::Equip,
            _ => Bind::None,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryLimit {
    pub category: String,
    pub max_entries: i32,
}

fn one_i32() -> i32 {
    1
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
    pub instance_schema: Value,
    /// Omitted = keep the migrations the item already has.
    pub migrations: Option<Vec<Migration>>,
    /// Stored with the new version when the definition changed.
    pub note: Option<String>,
    /// Omitted = keep what the item already has (false / none / true / [] for a new item).
    pub unique: Option<bool>,
    pub bind: Option<Bind>,
    pub tradeable: Option<bool>,
    pub slots: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemVersion {
    pub item_id: String,
    pub version: i32,
    pub definition: Value,
    pub note: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryEntry {
    pub id: Uuid,
    pub item_id: String,
    pub quantity: i64,
    pub instance: Value,
    /// Definition version the instance data conforms to (migrated lazily on read).
    #[serde(default = "one_i32")]
    pub item_version: i32,
    /// Bound entries cannot be traded, listed or deposited.
    #[serde(default)]
    pub bound: bool,
    /// Equipment slot the entry currently occupies.
    #[serde(default)]
    pub slot: Option<String>,
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
    /// Client platforms this listing is shown on (`steam`, `ios`, `android`, `psn`, `xbox`,
    /// `nintendo`, `epic`, `web` …); empty = every platform.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Shown and sold only to accounts in one of these named audiences; empty = everyone.
    #[serde(default)]
    pub audiences: Vec<String>,
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
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Shown and sold only to accounts in one of these named audiences; empty = everyone.
    #[serde(default)]
    pub audiences: Vec<String>,
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
    'platforms', platforms, 'audiences', audiences,
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

type EntryRow = (Uuid, String, i64, Value, i32, bool, Option<String>);
type ItemRulesRow = (bool, i32, i32, Option<Value>, Value, bool, String, String);
type InventoryListRow = (
    Uuid,
    String,
    i64,
    Value,
    i32,
    i32,
    Value,
    bool,
    Option<String>,
);

type ItemRow = (
    String,
    String,
    String,
    bool,
    i32,
    Value,
    i32,
    Option<Value>,
    Value,
    bool,
    String,
    bool,
    Vec<String>,
);
const ITEM_COLS: &str =
    "id, name, category, stackable, max_stack, properties, version, instance_schema, migrations, unique_per_account, bind, tradeable, slots";

fn item_of(r: ItemRow) -> Item {
    Item {
        id: r.0,
        name: r.1,
        category: r.2,
        stackable: r.3,
        max_stack: r.4,
        properties: r.5,
        version: r.6,
        instance_schema: r.7.unwrap_or(Value::Null),
        migrations: serde_json::from_value(r.8).unwrap_or_default(),
        unique: r.9,
        bind: Bind::parse(&r.10),
        tradeable: r.11,
        slots: r.12,
    }
}

fn same_definition(a: &Item, b: &Item) -> bool {
    a.name == b.name
        && a.category == b.category
        && a.stackable == b.stackable
        && a.max_stack == b.max_stack
        && a.properties == b.properties
        && a.instance_schema == b.instance_schema
        && a.migrations == b.migrations
        && a.unique == b.unique
        && a.bind == b.bind
        && a.tradeable == b.tradeable
        && a.slots == b.slots
}

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
    pub async fn adjust_tx(
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
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let prev = Self::item_tx(&mut tx, &c.id).await?;
        let mut item = Item {
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
            version: prev.as_ref().map_or(1, |p| p.version),
            instance_schema: if c.instance_schema.is_object() {
                c.instance_schema
            } else {
                Value::Null
            },
            migrations: c.migrations.unwrap_or_else(|| {
                prev.as_ref()
                    .map(|p| p.migrations.clone())
                    .unwrap_or_default()
            }),
            unique: c
                .unique
                .unwrap_or_else(|| prev.as_ref().is_some_and(|p| p.unique)),
            bind: c
                .bind
                .unwrap_or_else(|| prev.as_ref().map(|p| p.bind).unwrap_or_default()),
            tradeable: c
                .tradeable
                .unwrap_or_else(|| prev.as_ref().is_none_or(|p| p.tradeable)),
            slots: c
                .slots
                .unwrap_or_else(|| prev.as_ref().map(|p| p.slots.clone()).unwrap_or_default()),
        };
        for slot in &item.slots {
            check_code(slot)
                .map_err(|_| Error::InvalidArgument("slots: 1..=64 of [A-Za-z0-9_-]".into()))?;
        }
        if !item.instance_schema.is_null() {
            musubi_storage::schema::compile_schema(&item.instance_schema)
                .map_err(|e| Error::InvalidArgument(format!("instance_schema: {e}")))?;
        }
        let changed = match &prev {
            None => true,
            Some(p) => !same_definition(p, &item),
        };
        if changed {
            if let Some(p) = &prev {
                item.version = p.version + 1;
            }
        }
        musubi_storage::schema::check_migrations(&item.migrations, item.version)?;
        let migrations = serde_json::to_value(&item.migrations)?;
        sqlx::query(
            "insert into musubi.items (id, name, category, stackable, max_stack, properties, version, instance_schema, migrations, unique_per_account, bind, tradeable, slots)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             on conflict (id) do update set name = excluded.name, category = excluded.category, stackable = excluded.stackable,
             max_stack = excluded.max_stack, properties = excluded.properties, version = excluded.version,
             instance_schema = excluded.instance_schema, migrations = excluded.migrations,
             unique_per_account = excluded.unique_per_account, bind = excluded.bind, tradeable = excluded.tradeable, slots = excluded.slots",
        )
        .bind(&item.id)
        .bind(&item.name)
        .bind(&item.category)
        .bind(item.stackable)
        .bind(item.max_stack)
        .bind(&item.properties)
        .bind(item.version)
        .bind((!item.instance_schema.is_null()).then_some(&item.instance_schema))
        .bind(&migrations)
        .bind(item.unique)
        .bind(item.bind.as_str())
        .bind(item.tradeable)
        .bind(&item.slots)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if changed {
            sqlx::query(
                "insert into musubi.item_versions (item_id, version, definition, note) values ($1, $2, $3, $4)
                 on conflict (item_id, version) do update set definition = excluded.definition, note = excluded.note",
            )
            .bind(&item.id)
            .bind(item.version)
            .bind(serde_json::to_value(&item)?)
            .bind(c.note.unwrap_or_default())
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(item)
    }

    async fn item_tx(tx: &mut Transaction<'_, Postgres>, id: &str) -> Result<Option<Item>> {
        let row: Option<ItemRow> = sqlx::query_as(&format!(
            "select {ITEM_COLS} from musubi.items where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(item_of))
    }

    pub async fn item(&self, id: &str) -> Result<Item> {
        let row: Option<ItemRow> = sqlx::query_as(&format!(
            "select {ITEM_COLS} from musubi.items where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(item_of)
            .ok_or_else(|| Error::NotFound(format!("item {id}")))
    }

    pub async fn items(&self) -> Result<Vec<Item>> {
        let rows: Vec<ItemRow> =
            sqlx::query_as(&format!("select {ITEM_COLS} from musubi.items order by id"))
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows.into_iter().map(item_of).collect())
    }

    /// Every definition the item ever had, newest first.
    pub async fn item_versions(&self, id: &str) -> Result<Vec<ItemVersion>> {
        self.item(id).await?;
        let rows: Vec<(String, i32, Value, String, OffsetDateTime)> = sqlx::query_as(
            "select item_id, version, definition, note, created_at from musubi.item_versions
             where item_id = $1 order by version desc",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| ItemVersion {
                item_id: r.0,
                version: r.1,
                definition: r.2,
                note: r.3,
                created_at: r.4,
            })
            .collect())
    }

    /// Re-apply an older definition as a new version. The migration chain is kept (and may be
    /// extended in the same call) so instances granted meanwhile still reach the current version.
    pub async fn rollback_item(
        &self,
        id: &str,
        version: i32,
        migrations: Option<Vec<Migration>>,
        note: Option<String>,
    ) -> Result<Item> {
        let current = self.item(id).await?;
        let def: Option<(Value,)> = sqlx::query_as(
            "select definition from musubi.item_versions where item_id = $1 and version = $2",
        )
        .bind(id)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some((def,)) = def else {
            return Err(Error::NotFound(format!("item {id} version {version}")));
        };
        let old: Item = serde_json::from_value(def)?;
        self.upsert_item(CreateItem {
            id: id.into(),
            name: old.name,
            category: old.category,
            stackable: Some(old.stackable),
            max_stack: Some(old.max_stack),
            properties: old.properties,
            instance_schema: old.instance_schema,
            migrations: Some(migrations.unwrap_or(current.migrations)),
            note: Some(note.unwrap_or_else(|| format!("rollback to version {version}"))),
            unique: Some(old.unique),
            bind: Some(old.bind),
            tradeable: Some(old.tradeable),
            slots: Some(old.slots.clone()),
        })
        .await
    }

    /// Bring up to `batch` inventory entries of the item granted under an older definition to the
    /// current one, in the database. Returns `(migrated, remaining)`.
    pub async fn migrate_item_inventory(&self, id: &str, batch: i64) -> Result<(i64, i64)> {
        let item = self.item(id).await?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let rows: Vec<(Uuid, Value, i32)> = sqlx::query_as(
            "select id, instance, item_version from musubi.inventory
             where item_id = $1 and item_version < $2 order by created_at limit $3 for update skip locked",
        )
        .bind(id)
        .bind(item.version)
        .bind(batch.clamp(1, 5000))
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let mut migrated = 0i64;
        for (eid, mut instance, iv) in rows {
            let reached = musubi_storage::schema::migrate_steps(
                &item.migrations,
                item.version,
                &mut instance,
                iv,
            );
            if reached == iv {
                continue;
            }
            sqlx::query(
                "update musubi.inventory set instance = $2, item_version = $3, updated_at = now() where id = $1",
            )
            .bind(eid)
            .bind(&instance)
            .bind(reached)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            migrated += 1;
        }
        tx.commit().await.map_err(Error::internal)?;
        let remaining: i64 = sqlx::query_scalar(
            "select count(*) from musubi.inventory where item_id = $1 and item_version < $2",
        )
        .bind(id)
        .bind(item.version)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok((migrated, remaining))
    }

    /// Turn up to `batch` inventory entries of `from` into `to` (`quantity × ratio`), e.g. when two
    /// items are merged or one is retired. Plain stacks join an existing stack of `to`; instance
    /// data is carried over as is. Returns `(converted, remaining)`.
    pub async fn convert_item(
        &self,
        from: &str,
        to: &str,
        ratio: i64,
        batch: i64,
    ) -> Result<(i64, i64)> {
        if from == to {
            return Err(Error::InvalidArgument(
                "from and to are the same item".into(),
            ));
        }
        if ratio < 1 {
            return Err(Error::InvalidArgument("ratio must be ≥ 1".into()));
        }
        self.item(from).await?;
        let target = self.item(to).await?;
        if !target.stackable && ratio != 1 {
            return Err(Error::InvalidArgument(format!(
                "{to} is not stackable, ratio must be 1"
            )));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let rows: Vec<(Uuid, Uuid, i64, Value)> = sqlx::query_as(
            "select id, account_id, quantity, instance from musubi.inventory
             where item_id = $1 order by created_at limit $2 for update skip locked",
        )
        .bind(from)
        .bind(batch.clamp(1, 5000))
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let mut converted = 0i64;
        for (eid, account, q, instance) in rows {
            let nq = q
                .checked_mul(ratio)
                .ok_or_else(|| Error::InvalidArgument("overflow".into()))?;
            if !target.stackable && nq != 1 {
                return Err(Error::InvalidArgument(format!(
                    "{to} is not stackable but entry {eid} holds {q}"
                )));
            }
            let plain = target.stackable && instance.as_object().is_some_and(|m| m.is_empty());
            if plain {
                let existing: Option<(Uuid, i64)> = sqlx::query_as(
                    "select id, quantity from musubi.inventory where account_id = $1 and item_id = $2
                     and instance = '{}'::jsonb and id <> $3 order by created_at limit 1 for update",
                )
                .bind(account)
                .bind(to)
                .bind(eid)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
                if let Some((sid, sq)) = existing {
                    let total = sq
                        .checked_add(nq)
                        .ok_or_else(|| Error::InvalidArgument("overflow".into()))?;
                    if total <= target.max_stack as i64 {
                        sqlx::query("update musubi.inventory set quantity = $2, item_version = $3, updated_at = now() where id = $1")
                            .bind(sid)
                            .bind(total)
                            .bind(target.version)
                            .execute(&mut *tx)
                            .await
                            .map_err(Error::internal)?;
                        sqlx::query("delete from musubi.inventory where id = $1")
                            .bind(eid)
                            .execute(&mut *tx)
                            .await
                            .map_err(Error::internal)?;
                        converted += 1;
                        continue;
                    }
                }
            }
            if nq > target.max_stack as i64 {
                return Err(Error::ResourceExhausted(format!(
                    "stack of {to} capped at {}: entry {eid} would hold {nq}",
                    target.max_stack
                )));
            }
            sqlx::query(
                "update musubi.inventory set item_id = $2, quantity = $3, item_version = $4, updated_at = now() where id = $1",
            )
            .bind(eid)
            .bind(to)
            .bind(nq)
            .bind(target.version)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            converted += 1;
        }
        tx.commit().await.map_err(Error::internal)?;
        let remaining: i64 =
            sqlx::query_scalar("select count(*) from musubi.inventory where item_id = $1")
                .bind(from)
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok((converted, remaining))
    }

    pub async fn inventory(&self, account: AccountId) -> Result<Vec<InventoryEntry>> {
        let rows: Vec<InventoryListRow> = sqlx::query_as(
            "select i.id, i.item_id, i.quantity, i.instance, i.item_version, d.version, d.migrations, i.bound, i.slot
             from musubi.inventory i join musubi.items d on d.id = i.item_id
             where i.account_id = $1 order by i.item_id, i.created_at",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let (
                    id,
                    item_id,
                    quantity,
                    mut instance,
                    mut item_version,
                    current,
                    migrations,
                    bound,
                    slot,
                ) = r;
                if item_version < current {
                    let migrations: Vec<Migration> =
                        serde_json::from_value(migrations).unwrap_or_default();
                    item_version = musubi_storage::schema::migrate_steps(
                        &migrations,
                        current,
                        &mut instance,
                        item_version,
                    );
                }
                InventoryEntry {
                    id,
                    item_id,
                    quantity,
                    instance,
                    item_version,
                    bound,
                    slot,
                }
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

    pub async fn grant_item_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        item_id: &str,
        quantity: i64,
        instance: Value,
    ) -> Result<InventoryEntry> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let status: Option<String> =
            sqlx::query_scalar("select status from musubi.items where id = $1")
                .bind(item_id)
                .fetch_optional(&mut **tx)
                .await
                .map_err(Error::internal)?;
        if status.as_deref().is_some_and(|st| st != "published") {
            return Err(Error::Conflict(format!(
                "item {item_id} is {} and cannot be granted",
                status.unwrap_or_default()
            )));
        }
        let item: Option<ItemRulesRow> = sqlx::query_as(
            "select stackable, max_stack, version, instance_schema, migrations, unique_per_account, bind, category from musubi.items where id = $1",
        )
        .bind(item_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let Some((stackable, max_stack, version, schema, migrations, unique, bind, category)) =
            item
        else {
            return Err(Error::NotFound(format!("item {item_id}")));
        };
        let bind = Bind::parse(&bind);
        if unique {
            let owned: Option<(Uuid,)> = sqlx::query_as(
                "select id from musubi.inventory where account_id = $1 and item_id = $2 limit 1",
            )
            .bind(account.0)
            .bind(item_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
            if owned.is_some() {
                return Err(Error::AlreadyExists(format!(
                    "{item_id} is unique and already owned"
                )));
            }
        }
        let instance = if instance.is_object() {
            instance
        } else {
            Value::Object(Default::default())
        };
        if let Some(schema) = schema.filter(|s| s.is_object()) {
            musubi_storage::schema::validate(&schema, &instance)
                .map_err(|e| Error::InvalidArgument(format!("instance of {item_id}: {e}")))?;
        }
        // stackable items with default instance data merge into the existing stack
        if stackable && instance.as_object().is_some_and(|m| m.is_empty()) {
            let existing: Option<(Uuid, i64, i32)> = sqlx::query_as(
                "select id, quantity, item_version from musubi.inventory where account_id = $1 and item_id = $2 and instance = '{}'::jsonb
                 order by created_at limit 1 for update",
            )
            .bind(account.0)
            .bind(item_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
            // an older plain stack only merges when migrating it keeps it plain
            let still_plain = |iv: i32| {
                if iv >= version {
                    return true;
                }
                let migrations: Vec<Migration> =
                    serde_json::from_value(migrations.clone()).unwrap_or_default();
                let mut probe = Value::Object(Default::default());
                musubi_storage::schema::migrate_steps(&migrations, version, &mut probe, iv);
                probe.as_object().is_some_and(|m| m.is_empty())
            };
            if let Some((id, q, iv)) = existing.filter(|(_, _, iv)| still_plain(*iv)) {
                let _ = iv;
                let nq = q
                    .checked_add(quantity)
                    .ok_or_else(|| Error::InvalidArgument("overflow".into()))?;
                if nq > max_stack as i64 {
                    return Err(Error::ResourceExhausted(format!(
                        "stack of {item_id} capped at {max_stack}"
                    )));
                }
                sqlx::query(
                    "update musubi.inventory set quantity = $2, item_version = $3, updated_at = now() where id = $1",
                )
                .bind(id)
                .bind(nq)
                .bind(version)
                .execute(&mut **tx)
                .await
                .map_err(Error::internal)?;
                return Ok(InventoryEntry {
                    id,
                    item_id: item_id.into(),
                    quantity: nq,
                    instance,
                    item_version: version,
                    bound: bind == Bind::Pickup,
                    slot: None,
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
        if let Some((max,)) = sqlx::query_as::<_, (i32,)>(
            "select max_entries from musubi.inventory_limits where category = $1",
        )
        .bind(&category)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?
        {
            let (n,): (i64,) = sqlx::query_as(
                "select count(*) from musubi.inventory i join musubi.items d on d.id = i.item_id
                 where i.account_id = $1 and d.category = $2",
            )
            .bind(account.0)
            .bind(&category)
            .fetch_one(&mut **tx)
            .await
            .map_err(Error::internal)?;
            if n >= max as i64 {
                return Err(Error::ResourceExhausted(format!(
                    "inventory holds {n} entries of category {category}, limit {max}"
                )));
            }
        }
        let id = Uuid::now_v7();
        sqlx::query("insert into musubi.inventory (id, account_id, item_id, quantity, instance, item_version, bound) values ($1, $2, $3, $4, $5, $6, $7)")
            .bind(id)
            .bind(account.0)
            .bind(item_id)
            .bind(quantity)
            .bind(&instance)
            .bind(version)
            .bind(bind == Bind::Pickup)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        Ok(InventoryEntry {
            id,
            item_id: item_id.into(),
            quantity,
            instance,
            item_version: version,
            bound: bind == Bind::Pickup,
            slot: None,
        })
    }

    /// Put an entry into an equipment slot the item fits; whatever occupied the slot comes off.
    /// `bind = equip` items become bound here.
    pub async fn equip(
        &self,
        account: AccountId,
        entry_id: Uuid,
        slot: &str,
    ) -> Result<InventoryEntry> {
        check_code(slot)
            .map_err(|_| Error::InvalidArgument("slot: 1..=64 of [A-Za-z0-9_-]".into()))?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(String, Vec<String>, String, bool)> = sqlx::query_as(
            "select i.item_id, d.slots, d.bind, i.bound from musubi.inventory i join musubi.items d on d.id = i.item_id
             where i.id = $1 and i.account_id = $2 for update of i",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, slots, bind, bound)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        if !slots.iter().any(|s| s == slot) {
            return Err(Error::InvalidArgument(format!(
                "{item_id} does not fit slot {slot} (fits {slots:?})"
            )));
        }
        sqlx::query(
            "update musubi.inventory set slot = null, updated_at = now() where account_id = $1 and slot = $2 and id <> $3",
        )
        .bind(account.0)
        .bind(slot)
        .bind(entry_id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "update musubi.inventory set slot = $2, bound = $3, updated_at = now() where id = $1",
        )
        .bind(entry_id)
        .bind(slot)
        .bind(bound || Bind::parse(&bind) == Bind::Equip)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Self::entry(&self.db, account, entry_id).await
    }

    pub async fn unequip(&self, account: AccountId, entry_id: Uuid) -> Result<InventoryEntry> {
        let n = sqlx::query(
            "update musubi.inventory set slot = null, updated_at = now() where id = $1 and account_id = $2",
        )
        .bind(entry_id)
        .bind(account.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("inventory entry".into()));
        }
        Self::entry(&self.db, account, entry_id).await
    }

    /// Server-side change of an entry's instance data (durability, level, enchants…): `merge` is a
    /// shallow JSON merge (`null` removes a key); the result must still satisfy the item's schema.
    pub async fn update_instance(
        &self,
        account: AccountId,
        entry_id: Uuid,
        merge: Value,
    ) -> Result<InventoryEntry> {
        let Some(m) = merge.as_object() else {
            return Err(Error::InvalidArgument("merge: object".into()));
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(String, Value, Option<Value>)> = sqlx::query_as(
            "select i.item_id, i.instance, d.instance_schema from musubi.inventory i join musubi.items d on d.id = i.item_id
             where i.id = $1 and i.account_id = $2 for update of i",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, instance, schema)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        let mut obj = instance.as_object().cloned().unwrap_or_default();
        for (k, v) in m {
            if v.is_null() {
                obj.remove(k);
            } else {
                obj.insert(k.clone(), v.clone());
            }
        }
        let instance = Value::Object(obj);
        if let Some(schema) = schema.filter(|s| s.is_object()) {
            musubi_storage::schema::validate(&schema, &instance)
                .map_err(|e| Error::InvalidArgument(format!("instance of {item_id}: {e}")))?;
        }
        sqlx::query("update musubi.inventory set instance = $2, updated_at = now() where id = $1")
            .bind(entry_id)
            .bind(&instance)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Self::entry(&self.db, account, entry_id).await
    }

    async fn entry(db: &PgPool, account: AccountId, id: Uuid) -> Result<InventoryEntry> {
        let r: Option<EntryRow> = sqlx::query_as(
            "select id, item_id, quantity, instance, item_version, bound, slot from musubi.inventory where id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?;
        let Some(r) = r else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        Ok(InventoryEntry {
            id: r.0,
            item_id: r.1,
            quantity: r.2,
            instance: r.3,
            item_version: r.4,
            bound: r.5,
            slot: r.6,
        })
    }

    /// Cap the number of inventory entries (stacks / instances) of a category per account;
    /// `max_entries <= 0` removes the cap.
    pub async fn set_inventory_limit(&self, category: &str, max_entries: i32) -> Result<()> {
        if category.is_empty() || category.len() > 64 {
            return Err(Error::InvalidArgument("category: 1..=64 chars".into()));
        }
        if max_entries <= 0 {
            sqlx::query("delete from musubi.inventory_limits where category = $1")
                .bind(category)
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
            return Ok(());
        }
        sqlx::query(
            "insert into musubi.inventory_limits (category, max_entries) values ($1, $2)
             on conflict (category) do update set max_entries = excluded.max_entries",
        )
        .bind(category)
        .bind(max_entries)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn inventory_limits(&self) -> Result<Vec<InventoryLimit>> {
        let rows: Vec<(String, i32)> = sqlx::query_as(
            "select category, max_entries from musubi.inventory_limits order by category",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(category, max_entries)| InventoryLimit {
                category,
                max_entries,
            })
            .collect())
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
        let r = Self::consume_tx(&mut tx, account, entry_id, quantity).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(r)
    }

    /// `consume` inside a caller-owned transaction (server-side atomic transactions).
    pub async fn consume_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        entry_id: Uuid,
        quantity: i64,
    ) -> Result<Option<InventoryEntry>> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let row: Option<(String, i64, Value, i32, bool, Option<String>)> = sqlx::query_as(
            "select item_id, quantity, instance, item_version, bound, slot from musubi.inventory where id = $1 and account_id = $2 for update",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, have, instance, item_version, bound, slot)) = row else {
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
                .execute(&mut **tx)
                .await
                .map_err(Error::internal)?;
        } else {
            sqlx::query(
                "update musubi.inventory set quantity = $2, updated_at = now() where id = $1",
            )
            .bind(entry_id)
            .bind(left)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        }
        Ok((left > 0).then_some(InventoryEntry {
            id: entry_id,
            item_id,
            quantity: left,
            instance,
            item_version,
            bound,
            slot,
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
            platforms: c
                .platforms
                .iter()
                .map(|p| p.trim().to_ascii_lowercase())
                .filter(|p| !p.is_empty())
                .collect(),
            audiences: c
                .audiences
                .iter()
                .map(|a| a.trim().to_string())
                .filter(|a| !a.is_empty())
                .collect(),
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
                discount_until, rotation, rotation_slots, platforms, audiences)
             values ($1, $2, $3, $4, $5, $6, to_timestamp($7), to_timestamp($8), $9, $10, $11, $12,
                to_timestamp($13), $14, $15, $16, $17)
             on conflict (id) do update set item_id = excluded.item_id, quantity = excluded.quantity,
                prices = excluded.prices, enabled = excluded.enabled,
                max_per_account = excluded.max_per_account, available_from = excluded.available_from,
                available_until = excluded.available_until, bundle = excluded.bundle,
                min_level = excluded.min_level, level_track = excluded.level_track,
                discount_percent = excluded.discount_percent, discount_until = excluded.discount_until,
                rotation = excluded.rotation, rotation_slots = excluded.rotation_slots,
                platforms = excluded.platforms, audiences = excluded.audiences",
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
        .bind(&s.platforms)
        .bind(&s.audiences)
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
        self.store_for(viewer, include_disabled, None, &[]).await
    }

    /// `platform`: only listings open to that platform (or to all).
    /// Audience names a listing is restricted to (empty = everyone).
    pub async fn store_item_audiences(&self, id: &str) -> Result<Vec<String>> {
        let row: Option<(Vec<String>,)> =
            sqlx::query_as("select audiences from musubi.store_items where id = $1")
                .bind(id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(row.map(|r| r.0).unwrap_or_default())
    }

    pub async fn store_for(
        &self,
        viewer: Option<AccountId>,
        include_disabled: bool,
        platform: Option<&str>,
        audiences: &[String],
    ) -> Result<Vec<StoreItem>> {
        let rows: Vec<(Value,)> = sqlx::query_as(&format!(
            "select {STORE_COLS} from musubi.store_items
             where (enabled or $1) and ($2::text is null or platforms = '{{}}' or $2 = any(platforms))
             order by id"
        ))
        .bind(include_disabled)
        .bind(platform)
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
        items.retain(|i| {
            i.audiences.is_empty() || i.audiences.iter().any(|a| audiences.contains(a))
        });
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
        self.buy_with_price(
            account,
            store_item_id,
            currency,
            idempotency_key,
            coupon,
            None,
        )
        .await
    }

    /// `buy` with an explicit price (offers): replaces the listing price and its discount;
    /// coupons still apply on top.
    pub async fn buy_with_price(
        &self,
        account: AccountId,
        store_item_id: &str,
        currency: &str,
        idempotency_key: Option<&str>,
        coupon: Option<&str>,
        price_override: Option<i64>,
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
                let granted: Option<EntryRow> = sqlx::query_as(
                    "select i.id, i.item_id, i.quantity, i.instance, i.item_version, i.bound, i.slot from musubi.inventory i
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
                    item_version: g.4,
                    bound: g.5,
                    slot: g.6,
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
        let mut price = match price_override {
            Some(p) => p.max(0),
            None => list_price - list_price * listing.discount_now(now) as i64 / 100,
        };
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
