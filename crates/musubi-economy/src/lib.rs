//! Economy v0 (open-source Hiro direction): currencies and wallets with an append-only, idempotent
//! ledger (no negative balances), an item catalog with stacks / instances, and a store whose
//! purchases debit the wallet and grant the item in one transaction.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

pub mod iap;
pub mod progression;
pub use iap::{IapProduct, Receipt, ReceiptInput};
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
    /// currency code → price
    pub prices: Value,
    pub enabled: bool,
    pub max_per_account: Option<i32>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Purchase {
    pub id: Uuid,
    pub store_item_id: String,
    pub currency: String,
    pub amount: i64,
    pub balance: i64,
    pub granted: InventoryEntry,
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
    async fn adjust_tx(
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

    async fn grant_item_tx(
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
        let s = StoreItem {
            id: c.id,
            item_id: c.item_id,
            quantity: c.quantity.unwrap_or(1).max(1),
            prices: c.prices,
            enabled: c.enabled.unwrap_or(true),
            max_per_account: c.max_per_account,
        };
        sqlx::query(
            "insert into musubi.store_items (id, item_id, quantity, prices, enabled, max_per_account) values ($1, $2, $3, $4, $5, $6)
             on conflict (id) do update set item_id = excluded.item_id, quantity = excluded.quantity, prices = excluded.prices,
             enabled = excluded.enabled, max_per_account = excluded.max_per_account",
        )
        .bind(&s.id)
        .bind(&s.item_id)
        .bind(s.quantity)
        .bind(&s.prices)
        .bind(s.enabled)
        .bind(s.max_per_account)
        .execute(&self.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => Error::NotFound(format!("item {}", s.item_id)),
            e => Error::internal(e),
        })?;
        Ok(s)
    }

    pub async fn store(&self, include_disabled: bool) -> Result<Vec<StoreItem>> {
        let rows: Vec<(String, String, i64, Value, bool, Option<i32>)> = sqlx::query_as(
            "select id, item_id, quantity, prices, enabled, max_per_account from musubi.store_items where enabled or $1 order by id",
        )
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| StoreItem {
                id: r.0,
                item_id: r.1,
                quantity: r.2,
                prices: r.3,
                enabled: r.4,
                max_per_account: r.5,
            })
            .collect())
    }

    /// Debit the price in `currency` and grant the item, atomically; idempotent per key.
    pub async fn buy(
        &self,
        account: AccountId,
        store_item_id: &str,
        currency: &str,
        idempotency_key: Option<&str>,
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
                    replayed: true,
                });
            }
        }
        let s: Option<(String, i64, Value, bool, Option<i32>)> = sqlx::query_as(
            "select item_id, quantity, prices, enabled, max_per_account from musubi.store_items where id = $1",
        )
        .bind(store_item_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, qty, prices, enabled, max_per)) = s else {
            return Err(Error::NotFound(format!("store item {store_item_id}")));
        };
        if !enabled {
            return Err(Error::PermissionDenied("store item disabled".into()));
        }
        let price = prices
            .get(currency)
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                Error::InvalidArgument(format!("{store_item_id} is not sold for {currency}"))
            })?;
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
        sqlx::query(
            "insert into musubi.purchases (id, account_id, store_item_id, currency, amount, idempotency_key) values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(account.0)
        .bind(store_item_id)
        .bind(currency)
        .bind(price)
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Purchase {
            id,
            store_item_id: store_item_id.into(),
            currency: currency.into(),
            amount: price,
            balance,
            granted,
            replayed: false,
        })
    }
}
