//! Item lifecycle: publish state (draft → published → retired), use cooldowns, timed status
//! effects applied on use, durability on instances, and operator revocation.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::{EconomyService, InventoryEntry};

type EffectRow = (
    String,
    Value,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
    i32,
    Option<String>,
);

pub const STATUSES: [&str; 3] = ["draft", "published", "retired"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ItemEffect {
    pub key: String,
    pub value: Value,
    /// 0 = until removed.
    pub duration_secs: i64,
}

impl Default for ItemEffect {
    fn default() -> Self {
        Self {
            key: String::new(),
            value: Value::Bool(true),
            duration_secs: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct Durability {
    pub max: i64,
    pub per_use: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct UseResult {
    pub item_id: String,
    pub remaining: Option<InventoryEntry>,
    pub effects: Vec<Value>,
    pub cooldown_ready_at: Option<i64>,
    pub durability_left: Option<i64>,
    pub broken: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LifecycleRules {
    pub use_cooldown_secs: i64,
    pub effects: Vec<ItemEffect>,
    pub durability: Option<Durability>,
}

impl LifecycleRules {
    pub fn validate(&self) -> Result<()> {
        if self.use_cooldown_secs < 0 {
            return Err(Error::InvalidArgument("use_cooldown_secs: >= 0".into()));
        }
        for e in &self.effects {
            if e.key.is_empty() || e.key.len() > 64 || e.duration_secs < 0 {
                return Err(Error::InvalidArgument(
                    "effects: key 1..=64 chars, duration_secs >= 0".into(),
                ));
            }
        }
        if let Some(d) = self.durability {
            if d.max <= 0 || d.per_use <= 0 || d.per_use > d.max {
                return Err(Error::InvalidArgument(
                    "durability: 0 < per_use <= max".into(),
                ));
            }
        }
        Ok(())
    }
}

impl EconomyService {
    /// Cooldown / effects / durability of an item (separate from `upsert_item` so content bundles
    /// that predate them keep working).
    pub async fn set_item_lifecycle(&self, id: &str, r: LifecycleRules) -> Result<LifecycleRules> {
        r.validate()?;
        let n = sqlx::query(
            "update musubi.items set use_cooldown_secs = $2, effects = $3, durability = $4 where id = $1",
        )
        .bind(id)
        .bind(r.use_cooldown_secs)
        .bind(serde_json::to_value(&r.effects)?)
        .bind(r.durability.map(|d| json!(d)))
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("item {id}")));
        }
        Ok(r)
    }

    pub async fn set_item_status(&self, id: &str, status: &str) -> Result<()> {
        if !STATUSES.contains(&status) {
            return Err(Error::InvalidArgument(
                "status: draft|published|retired".into(),
            ));
        }
        let n = sqlx::query("update musubi.items set status = $2 where id = $1")
            .bind(id)
            .bind(status)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("item {id}")));
        }
        Ok(())
    }

    /// Retire the item and pull every instance from every inventory; returns (entries, accounts).
    pub async fn revoke_item(&self, id: &str) -> Result<(u64, Vec<AccountId>)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let n = sqlx::query("update musubi.items set status = 'retired' where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("item {id}")));
        }
        let rows: Vec<(Uuid,)> =
            sqlx::query_as("delete from musubi.inventory where item_id = $1 returning account_id")
                .bind(id)
                .fetch_all(&mut *tx)
                .await
                .map_err(Error::internal)?;
        sqlx::query("delete from musubi.player_effects where item_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let mut accounts: Vec<AccountId> = rows.into_iter().map(|(a,)| AccountId(a)).collect();
        accounts.sort_by_key(|a| a.0);
        accounts.dedup();
        Ok((n.max(accounts.len() as u64), accounts))
    }

    /// Use an inventory entry: cooldown gate, effects applied, durability spent (durable items
    /// break instead of being consumed; consumables are consumed).
    pub async fn use_item(
        &self,
        account: AccountId,
        entry_id: Uuid,
        quantity: i64,
        now: OffsetDateTime,
    ) -> Result<UseResult> {
        if quantity <= 0 {
            return Err(Error::InvalidArgument("quantity must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(String, i64, Value, i32, bool, Option<String>)> = sqlx::query_as(
            "select item_id, quantity, instance, item_version, bound, slot from musubi.inventory where id = $1 and account_id = $2 for update",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, have, mut instance, item_version, bound, slot)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        let item: Option<(String, i64, Value, Option<Value>)> = sqlx::query_as(
            "select status, use_cooldown_secs, effects, durability from musubi.items where id = $1",
        )
        .bind(&item_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((status, cooldown, effects, durability)) = item else {
            return Err(Error::NotFound(format!("item {item_id}")));
        };
        if status == "draft" {
            return Err(Error::Conflict(format!("item {item_id} is not published")));
        }
        let ready: Option<OffsetDateTime> = sqlx::query_scalar(
            "select ready_at from musubi.item_cooldowns where account_id = $1 and item_id = $2 for update",
        )
        .bind(account.0)
        .bind(&item_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if let Some(r) = ready.filter(|r| *r > now) {
            return Err(Error::Conflict(format!(
                "item {item_id} on cooldown until {} (ready_at)",
                r.unix_timestamp()
            )));
        }
        let effects: Vec<ItemEffect> = serde_json::from_value(effects).unwrap_or_default();
        let durability: Option<Durability> =
            durability.and_then(|d| serde_json::from_value(d).ok());
        let mut out = UseResult {
            item_id: item_id.clone(),
            remaining: None,
            effects: Vec::new(),
            cooldown_ready_at: None,
            durability_left: None,
            broken: false,
        };
        match durability {
            Some(d) => {
                let cur = instance["durability"].as_i64().unwrap_or(d.max);
                let left = cur.saturating_sub(d.per_use.saturating_mul(quantity));
                if left <= 0 {
                    out.broken = true;
                    out.durability_left = Some(0);
                    sqlx::query("delete from musubi.inventory where id = $1")
                        .bind(entry_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(Error::internal)?;
                } else {
                    instance["durability"] = json!(left);
                    out.durability_left = Some(left);
                    sqlx::query(
                        "update musubi.inventory set instance = $2, updated_at = now() where id = $1",
                    )
                    .bind(entry_id)
                    .bind(&instance)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                    out.remaining = Some(InventoryEntry {
                        id: entry_id,
                        item_id: item_id.clone(),
                        quantity: have,
                        instance,
                        item_version,
                        bound,
                        slot,
                    });
                }
            }
            None => {
                out.remaining = Self::consume_tx(&mut tx, account, entry_id, quantity).await?;
            }
        }
        for e in &effects {
            let expires = (e.duration_secs > 0).then(|| now + Duration::seconds(e.duration_secs));
            sqlx::query(
                "insert into musubi.player_effects (account_id, key, value, item_id, applied_at, expires_at)
                 values ($1, $2, $3, $4, $5, $6)
                 on conflict (account_id, key) do update set value = excluded.value, item_id = excluded.item_id,
                     applied_at = excluded.applied_at, expires_at = excluded.expires_at",
            )
            .bind(account.0)
            .bind(&e.key)
            .bind(&e.value)
            .bind(&item_id)
            .bind(now)
            .bind(expires)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            out.effects.push(json!({
                "key": e.key, "value": e.value,
                "expires_at": expires.map(|t| t.unix_timestamp()),
            }));
        }
        if cooldown > 0 {
            let ready = now + Duration::seconds(cooldown);
            sqlx::query(
                "insert into musubi.item_cooldowns (account_id, item_id, ready_at) values ($1, $2, $3)
                 on conflict (account_id, item_id) do update set ready_at = excluded.ready_at",
            )
            .bind(account.0)
            .bind(&item_id)
            .bind(ready)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            out.cooldown_ready_at = Some(ready.unix_timestamp());
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(out)
    }

    /// Active effects of a player (expired ones are pruned on read).
    pub async fn effects(&self, account: AccountId, now: OffsetDateTime) -> Result<Vec<Value>> {
        sqlx::query("delete from musubi.player_effects where account_id = $1 and expires_at is not null and expires_at <= $2")
            .bind(account.0)
            .bind(now)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        let rows: Vec<EffectRow> =
            sqlx::query_as(
                "select key, value, item_id, applied_at, expires_at, stacks, source from musubi.player_effects where account_id = $1 order by key",
            )
            .bind(account.0)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(key, value, item_id, applied, expires, stacks, source)| {
                json!({
                    "key": key, "value": value, "item_id": item_id, "stacks": stacks, "source": source,
                    "applied_at": applied.unix_timestamp(),
                    "expires_at": expires.map(|t| t.unix_timestamp()),
                    "seconds_left": expires.map(|t| (t - now).whole_seconds().max(0)),
                })
            })
            .collect())
    }

    pub async fn remove_effect(&self, account: AccountId, key: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.player_effects where account_id = $1 and key = $2")
            .bind(account.0)
            .bind(key)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }

    /// Restore durability (to `max`, or by `amount`).
    pub async fn repair(
        &self,
        account: AccountId,
        entry_id: Uuid,
        amount: Option<i64>,
    ) -> Result<Value> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(String, Value)> = sqlx::query_as(
            "select item_id, instance from musubi.inventory where id = $1 and account_id = $2 for update",
        )
        .bind(entry_id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((item_id, mut instance)) = row else {
            return Err(Error::NotFound("inventory entry".into()));
        };
        let d: Option<Option<Value>> =
            sqlx::query_scalar("select durability from musubi.items where id = $1")
                .bind(&item_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some(d) = d
            .flatten()
            .and_then(|v| serde_json::from_value::<Durability>(v).ok())
        else {
            return Err(Error::InvalidArgument(format!(
                "item {item_id} has no durability"
            )));
        };
        let cur = instance["durability"].as_i64().unwrap_or(d.max);
        let new = match amount {
            Some(a) if a > 0 => cur.saturating_add(a).min(d.max),
            Some(_) => return Err(Error::InvalidArgument("amount must be positive".into())),
            None => d.max,
        };
        instance["durability"] = json!(new);
        sqlx::query("update musubi.inventory set instance = $2, updated_at = now() where id = $1")
            .bind(entry_id)
            .bind(&instance)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({ "id": entry_id, "item_id": item_id, "durability": new, "max": d.max }))
    }
}
