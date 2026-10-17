//! Crafting: a recipe turns inputs (items, currency) into outputs (items, currency, reward
//! tables) in one transaction — nothing is taken unless everything is given — idempotently per
//! key and under per-account caps.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::EconomyService;

pub const MAX_BATCH: i64 = 1000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ingredient {
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    pub amount: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Output {
    #[serde(default)]
    pub item_id: Option<String>,
    #[serde(default)]
    pub currency: Option<String>,
    /// Roll/grant a reward table once per crafted unit.
    #[serde(default)]
    pub reward_table: Option<String>,
    #[serde(default = "one")]
    pub amount: i64,
    /// Instance data for granted items (non-stackable items get one row each).
    #[serde(default)]
    pub instance: Value,
}

fn one() -> i64 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Recipe {
    pub id: String,
    pub name: String,
    pub category: String,
    pub inputs: Vec<Ingredient>,
    pub outputs: Vec<Output>,
    /// Lifetime cap of crafted units per account (0 = unlimited).
    pub max_per_account: i64,
    /// Units per request (0 → 1).
    pub max_batch: i64,
    /// Item the crafter must own (not consumed), e.g. a blueprint.
    pub requires_item: Option<String>,
    pub enabled: bool,
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Crafted {
    pub recipe_id: String,
    pub count: i64,
    pub consumed: Vec<Value>,
    pub granted: Vec<Value>,
    #[serde(default)]
    pub replayed: bool,
}

type RecipeRow = (
    String,
    String,
    String,
    Value,
    Value,
    i64,
    i64,
    Option<String>,
    bool,
    Value,
);

fn recipe(r: RecipeRow) -> Recipe {
    Recipe {
        id: r.0,
        name: r.1,
        category: r.2,
        inputs: serde_json::from_value(r.3).unwrap_or_default(),
        outputs: serde_json::from_value(r.4).unwrap_or_default(),
        max_per_account: r.5,
        max_batch: r.6,
        requires_item: r.7,
        enabled: r.8,
        metadata: r.9,
    }
}

const RECIPE_COLS: &str =
    "id, name, category, inputs, outputs, max_per_account, max_batch, requires_item, enabled, metadata";

impl EconomyService {
    pub async fn upsert_recipe(&self, mut r: Recipe) -> Result<Recipe> {
        if r.id.is_empty() || r.id.len() > 64 {
            return Err(Error::InvalidArgument(
                "recipe id must be 1..=64 chars".into(),
            ));
        }
        if r.inputs.is_empty() && r.requires_item.is_none() {
            return Err(Error::InvalidArgument(
                "a recipe needs inputs or a required item".into(),
            ));
        }
        if r.outputs.is_empty() {
            return Err(Error::InvalidArgument("a recipe needs outputs".into()));
        }
        if r.max_batch <= 0 {
            r.max_batch = 1;
        }
        if r.max_batch > MAX_BATCH || r.max_per_account < 0 {
            return Err(Error::InvalidArgument(format!(
                "max_batch must be 1..={MAX_BATCH}, max_per_account ≥ 0"
            )));
        }
        if r.metadata.is_null() {
            r.metadata = json!({});
        }
        for i in &r.inputs {
            if i.amount <= 0 || i.item_id.is_some() == i.currency.is_some() {
                return Err(Error::InvalidArgument(
                    "each input is exactly one of item_id|currency with amount > 0".into(),
                ));
            }
        }
        for o in &r.outputs {
            let kinds = [
                o.item_id.is_some(),
                o.currency.is_some(),
                o.reward_table.is_some(),
            ]
            .iter()
            .filter(|b| **b)
            .count();
            if o.amount <= 0 || kinds != 1 {
                return Err(Error::InvalidArgument(
                    "each output is exactly one of item_id|currency|reward_table with amount > 0"
                        .into(),
                ));
            }
        }
        let items: Vec<&str> = r
            .inputs
            .iter()
            .filter_map(|i| i.item_id.as_deref())
            .chain(r.outputs.iter().filter_map(|o| o.item_id.as_deref()))
            .chain(r.requires_item.as_deref())
            .collect();
        for id in items {
            let known: Option<i32> = sqlx::query_scalar("select 1 from musubi.items where id = $1")
                .bind(id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
            if known.is_none() {
                return Err(Error::NotFound(format!("item {id}")));
            }
        }
        let currencies: Vec<&str> = r
            .inputs
            .iter()
            .filter_map(|i| i.currency.as_deref())
            .chain(r.outputs.iter().filter_map(|o| o.currency.as_deref()))
            .collect();
        for id in currencies {
            let known: Option<i32> =
                sqlx::query_scalar("select 1 from musubi.currencies where id = $1")
                    .bind(id)
                    .fetch_optional(&self.db)
                    .await
                    .map_err(Error::internal)?;
            if known.is_none() {
                return Err(Error::NotFound(format!("currency {id}")));
            }
        }
        for t in r.outputs.iter().filter_map(|o| o.reward_table.as_deref()) {
            let mut tx = self.db.begin().await.map_err(Error::internal)?;
            if Self::load_table(&mut tx, t).await?.is_none() {
                return Err(Error::NotFound(format!("reward table {t}")));
            }
        }
        sqlx::query(
            "insert into musubi.crafting_recipes (id, name, category, inputs, outputs, max_per_account, max_batch, requires_item, enabled, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             on conflict (id) do update set name = excluded.name, category = excluded.category,
               inputs = excluded.inputs, outputs = excluded.outputs, max_per_account = excluded.max_per_account,
               max_batch = excluded.max_batch, requires_item = excluded.requires_item, enabled = excluded.enabled,
               metadata = excluded.metadata, updated_at = now()",
        )
        .bind(&r.id)
        .bind(&r.name)
        .bind(&r.category)
        .bind(serde_json::to_value(&r.inputs)?)
        .bind(serde_json::to_value(&r.outputs)?)
        .bind(r.max_per_account)
        .bind(r.max_batch)
        .bind(&r.requires_item)
        .bind(r.enabled)
        .bind(&r.metadata)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r)
    }

    pub async fn recipes(&self, include_disabled: bool) -> Result<Vec<Recipe>> {
        let rows: Vec<RecipeRow> = sqlx::query_as(&format!(
            "select {RECIPE_COLS} from musubi.crafting_recipes where ($1 or enabled) order by category, id"
        ))
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(recipe).collect())
    }

    pub async fn delete_recipe(&self, id: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.crafting_recipes where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }

    /// Take `quantity` of `item_id` from the account's stacks (largest first); fails without
    /// touching anything when the account holds fewer.
    pub(crate) async fn consume_item_tx(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        item_id: &str,
        quantity: i64,
    ) -> Result<Vec<Value>> {
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "select id, quantity from musubi.inventory where account_id = $1 and item_id = $2
             order by quantity desc, id for update",
        )
        .bind(account.0)
        .bind(item_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let have: i64 = rows.iter().map(|r| r.1).sum();
        if have < quantity {
            return Err(Error::ResourceExhausted(format!(
                "not enough {item_id}: have {have}, need {quantity}"
            )));
        }
        let mut left = quantity;
        let mut taken = Vec::new();
        for (id, q) in rows {
            if left == 0 {
                break;
            }
            let take = q.min(left);
            if take == q {
                sqlx::query("delete from musubi.inventory where id = $1")
                    .bind(id)
                    .execute(&mut **tx)
                    .await
                    .map_err(Error::internal)?;
            } else {
                sqlx::query(
                    "update musubi.inventory set quantity = quantity - $2, updated_at = now() where id = $1",
                )
                .bind(id)
                .bind(take)
                .execute(&mut **tx)
                .await
                .map_err(Error::internal)?;
            }
            taken.push(json!({ "item_id": item_id, "entry_id": id, "quantity": take }));
            left -= take;
        }
        Ok(taken)
    }

    /// Craft `count` units of a recipe: inputs are consumed and outputs granted atomically; a
    /// repeated `idempotency_key` returns the stored outcome without crafting again.
    pub async fn craft(
        &self,
        account: AccountId,
        recipe_id: &str,
        count: i64,
        idempotency_key: Option<&str>,
    ) -> Result<Crafted> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        if let Some(k) = idempotency_key {
            let prior: Option<Value> = sqlx::query_scalar(
                "select result from musubi.crafting_log where account_id = $1 and idempotency_key = $2",
            )
            .bind(account.0)
            .bind(k)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if let Some(p) = prior {
                let mut c: Crafted = serde_json::from_value(p).map_err(Error::internal)?;
                c.replayed = true;
                return Ok(c);
            }
        }
        let row: Option<RecipeRow> = sqlx::query_as(&format!(
            "select {RECIPE_COLS} from musubi.crafting_recipes where id = $1"
        ))
        .bind(recipe_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(r) = row.map(recipe) else {
            return Err(Error::NotFound(format!("recipe {recipe_id}")));
        };
        if !r.enabled {
            return Err(Error::Unavailable(format!(
                "recipe {recipe_id} is disabled"
            )));
        }
        if count <= 0 || count > r.max_batch {
            return Err(Error::InvalidArgument(format!(
                "count must be 1..={}",
                r.max_batch
            )));
        }
        if let Some(req) = &r.requires_item {
            let owned: Option<i64> = sqlx::query_scalar(
                "select sum(quantity)::bigint from musubi.inventory where account_id = $1 and item_id = $2",
            )
            .bind(account.0)
            .bind(req)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if owned.unwrap_or(0) <= 0 {
                return Err(Error::PermissionDenied(format!(
                    "recipe {recipe_id} requires {req}"
                )));
            }
        }
        if r.max_per_account > 0 {
            sqlx::query("select 1 from musubi.crafting_recipes where id = $1 for update")
                .bind(recipe_id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            let done: Option<i64> = sqlx::query_scalar(
                "select sum(count)::bigint from musubi.crafting_log where account_id = $1 and recipe_id = $2",
            )
            .bind(account.0)
            .bind(recipe_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let done = done.unwrap_or(0);
            if done + count > r.max_per_account {
                return Err(Error::ResourceExhausted(format!(
                    "recipe {recipe_id}: {done}/{} crafted, {count} more exceeds the cap",
                    r.max_per_account
                )));
            }
        }
        let reference = format!("craft:{recipe_id}");
        let mut consumed = Vec::new();
        for i in &r.inputs {
            let amount = i.amount * count;
            if let Some(item) = &i.item_id {
                consumed.extend(Self::consume_item_tx(&mut tx, account, item, amount).await?);
            }
            if let Some(cur) = &i.currency {
                let (e, _) = Self::adjust_tx(
                    &mut tx,
                    account,
                    cur,
                    -amount,
                    "craft",
                    &reference,
                    None,
                    json!({ "recipe": recipe_id, "count": count }),
                )
                .await?;
                consumed
                    .push(json!({ "currency": cur, "amount": amount, "balance": e.balance_after }));
            }
        }
        let mut granted = Vec::new();
        for o in &r.outputs {
            let amount = o.amount * count;
            if let Some(item) = &o.item_id {
                let e =
                    Self::grant_item_tx(&mut tx, account, item, amount, o.instance.clone()).await?;
                granted.push(json!({ "item_id": item, "quantity": amount, "entry": e }));
            }
            if let Some(cur) = &o.currency {
                let (e, _) = Self::adjust_tx(
                    &mut tx,
                    account,
                    cur,
                    amount,
                    "craft",
                    &reference,
                    None,
                    json!({ "recipe": recipe_id, "count": count }),
                )
                .await?;
                granted
                    .push(json!({ "currency": cur, "amount": amount, "balance": e.balance_after }));
            }
            if let Some(t) = &o.reward_table {
                let Some(table) = Self::load_table(&mut tx, t).await? else {
                    return Err(Error::NotFound(format!("reward table {t}")));
                };
                for _ in 0..amount {
                    Self::apply_rewards(&mut tx, account, &table.entries, "craft", &reference)
                        .await?;
                }
                granted
                    .push(json!({ "reward_table": t, "times": amount, "entries": table.entries }));
            }
        }
        let out = Crafted {
            recipe_id: recipe_id.to_string(),
            count,
            consumed,
            granted,
            replayed: false,
        };
        sqlx::query(
            "insert into musubi.crafting_log (id, account_id, recipe_id, count, idempotency_key, result)
             values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(Uuid::now_v7())
        .bind(account.0)
        .bind(recipe_id)
        .bind(count)
        .bind(idempotency_key)
        .bind(serde_json::to_value(&out)?)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(out)
    }

    pub async fn crafting_log(&self, account: AccountId, limit: i64) -> Result<Vec<Value>> {
        let rows: Vec<(Uuid, String, i64, Value, time::OffsetDateTime)> = sqlx::query_as(
            "select id, recipe_id, count, result, created_at from musubi.crafting_log
             where account_id = $1 order by created_at desc limit $2",
        )
        .bind(account.0)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(id, recipe_id, count, result, at)| {
                json!({ "id": id, "recipe_id": recipe_id, "count": count, "result": result,
                    "created_at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
            })
            .collect())
    }
}
