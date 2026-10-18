//! Reward tables (guaranteed / weighted / tiered) with audited rolls: published odds, pity timers
//! and a revealed server seed so players can verify `sha256(server_seed || client_seed || nonce)`.

use musubi_core::{AccountId, Error, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct RewardEntry {
    pub weight: u32,
    pub rarity: String,
    pub item_id: Option<String>,
    pub currency: Option<String>,
    pub amount: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Pity {
    /// After this many rolls without `rarity`, the next roll is forced to it (0 = off).
    pub after: u32,
    pub rarity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RewardTable {
    pub id: String,
    pub name: String,
    /// `guaranteed` (every entry), `weighted` (one entry by weight), `tiered` (one entry per rarity by weight)
    pub kind: String,
    pub entries: Vec<RewardEntry>,
    pub pity: Pity,
    pub enabled: bool,
}

impl Default for RewardTable {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            kind: "weighted".into(),
            entries: Vec::new(),
            pity: Pity::default(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedOdds {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub odds: Vec<Value>,
    pub pity: Pity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Roll {
    pub id: Uuid,
    pub table_id: String,
    pub server_seed: String,
    pub client_seed: String,
    pub nonce: i64,
    pub roll: String,
    pub rewards: Vec<RewardEntry>,
    pub pity_forced: bool,
    pub replayed: bool,
}

pub fn roll_hash(server_seed: &str, client_seed: &str, nonce: i64) -> String {
    let mut h = Sha256::new();
    h.update(server_seed.as_bytes());
    h.update(client_seed.as_bytes());
    h.update(nonce.to_be_bytes());
    hex::encode(h.finalize())
}

fn pick<'a>(entries: &[&'a RewardEntry], hash: &str, salt: usize) -> Option<&'a RewardEntry> {
    let total: u64 = entries.iter().map(|e| e.weight as u64).sum();
    if total == 0 {
        return None;
    }
    let slice = &hash[(salt * 16) % 48..][..16];
    let r = u64::from_str_radix(slice, 16).unwrap_or(0) % total;
    let mut acc = 0u64;
    for e in entries {
        acc += e.weight as u64;
        if r < acc {
            return Some(e);
        }
    }
    entries.last().copied()
}

impl RewardTable {
    fn validate(&self) -> Result<()> {
        if !matches!(self.kind.as_str(), "guaranteed" | "weighted" | "tiered") {
            return Err(Error::InvalidArgument(
                "kind: guaranteed|weighted|tiered".into(),
            ));
        }
        if self.entries.is_empty() {
            return Err(Error::InvalidArgument("entries required".into()));
        }
        for e in &self.entries {
            if e.item_id.is_none() && e.currency.is_none() {
                return Err(Error::InvalidArgument(
                    "entry needs item_id or currency".into(),
                ));
            }
            if e.amount <= 0 {
                return Err(Error::InvalidArgument(
                    "entry amount must be positive".into(),
                ));
            }
            if self.kind != "guaranteed" && e.weight == 0 {
                return Err(Error::InvalidArgument("weight must be positive".into()));
            }
        }
        if self.pity.after > 0 && !self.entries.iter().any(|e| e.rarity == self.pity.rarity) {
            return Err(Error::InvalidArgument("pity.rarity has no entries".into()));
        }
        Ok(())
    }

    pub fn odds(&self) -> PublishedOdds {
        let total: u64 = self
            .entries
            .iter()
            .map(|e| e.weight as u64)
            .sum::<u64>()
            .max(1);
        let odds = self
            .entries
            .iter()
            .map(|e| {
                let pct = if self.kind == "guaranteed" { 100.0 } else { e.weight as f64 * 100.0 / total as f64 };
                json!({ "rarity": e.rarity, "item_id": e.item_id, "currency": e.currency, "amount": e.amount, "percent": pct })
            })
            .collect();
        PublishedOdds {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
            odds,
            pity: self.pity.clone(),
        }
    }

    /// Deterministic outcome for a hash; `force_rarity` implements pity.
    pub fn outcome(&self, hash: &str, force_rarity: Option<&str>) -> Vec<RewardEntry> {
        match self.kind.as_str() {
            "guaranteed" => self.entries.clone(),
            "tiered" => {
                let mut rarities: Vec<&str> =
                    self.entries.iter().map(|e| e.rarity.as_str()).collect();
                rarities.sort_unstable();
                rarities.dedup();
                rarities
                    .iter()
                    .enumerate()
                    .filter_map(|(i, r)| {
                        let pool: Vec<&RewardEntry> =
                            self.entries.iter().filter(|e| e.rarity == *r).collect();
                        pick(&pool, hash, i).cloned()
                    })
                    .collect()
            }
            _ => {
                let pool: Vec<&RewardEntry> = match force_rarity {
                    Some(r) => self.entries.iter().filter(|e| e.rarity == r).collect(),
                    None => self.entries.iter().collect(),
                };
                pick(&pool, hash, 0).cloned().into_iter().collect()
            }
        }
    }
}

type TableRow = (String, String, String, Value, Value, bool);

fn table(r: TableRow) -> RewardTable {
    RewardTable {
        id: r.0,
        name: r.1,
        kind: r.2,
        entries: serde_json::from_value(r.3).unwrap_or_default(),
        pity: serde_json::from_value(r.4).unwrap_or_default(),
        enabled: r.5,
    }
}

const TABLE_COLS: &str = "id, name, kind, entries, pity, enabled";

impl EconomyService {
    pub async fn upsert_reward_table(&self, t: RewardTable) -> Result<RewardTable> {
        crate::check_code(&t.id)?;
        t.validate()?;
        sqlx::query(
            "insert into musubi.reward_tables (id, name, kind, entries, pity, enabled) values ($1, $2, $3, $4, $5, $6)
             on conflict (id) do update set name = excluded.name, kind = excluded.kind, entries = excluded.entries,
             pity = excluded.pity, enabled = excluded.enabled",
        )
        .bind(&t.id)
        .bind(&t.name)
        .bind(&t.kind)
        .bind(serde_json::to_value(&t.entries)?)
        .bind(serde_json::to_value(&t.pity)?)
        .bind(t.enabled)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(t)
    }

    pub async fn reward_tables(&self, include_disabled: bool) -> Result<Vec<RewardTable>> {
        let rows: Vec<TableRow> = sqlx::query_as(&format!(
            "select {TABLE_COLS} from musubi.reward_tables where enabled or $1 order by id"
        ))
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(table).collect())
    }

    pub async fn rolls(&self, account: AccountId, limit: i64) -> Result<Vec<Roll>> {
        let rows: Vec<(Uuid, String, String, String, i64, String, Value)> = sqlx::query_as(
            "select id, table_id, server_seed, client_seed, nonce, roll, result from musubi.reward_rolls
             where account_id = $1 order by created_at desc, id desc limit $2",
        )
        .bind(account.0)
        .bind(limit.clamp(1, crate::MAX_LEDGER_PAGE))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| Roll {
                id: r.0,
                table_id: r.1,
                server_seed: r.2,
                client_seed: r.3,
                nonce: r.4,
                roll: r.5,
                rewards: serde_json::from_value(r.6["rewards"].clone()).unwrap_or_default(),
                pity_forced: r.6["pity_forced"].as_bool().unwrap_or(false),
                replayed: true,
            })
            .collect())
    }

    /// Roll a table for a player: seeds + nonce → hash → outcome, rewards applied in the same
    /// transaction; idempotent per key.
    pub async fn roll(
        &self,
        account: AccountId,
        table_id: &str,
        client_seed: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Roll> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        if let Some(k) = idempotency_key {
            let prior: Option<(Uuid, String, String, i64, String, Value)> = sqlx::query_as(
                "select id, server_seed, client_seed, nonce, roll, result from musubi.reward_rolls where account_id = $1 and idempotency_key = $2",
            )
            .bind(account.0)
            .bind(k)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if let Some(p) = prior {
                return Ok(Roll {
                    id: p.0,
                    table_id: table_id.into(),
                    server_seed: p.1,
                    client_seed: p.2,
                    nonce: p.3,
                    roll: p.4,
                    rewards: serde_json::from_value(p.5["rewards"].clone()).unwrap_or_default(),
                    pity_forced: p.5["pity_forced"].as_bool().unwrap_or(false),
                    replayed: true,
                });
            }
        }
        let row: Option<TableRow> = sqlx::query_as(&format!(
            "select {TABLE_COLS} from musubi.reward_tables where id = $1"
        ))
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(t) = row.map(table) else {
            return Err(Error::NotFound(format!("reward table {table_id}")));
        };
        if !t.enabled {
            return Err(Error::PermissionDenied("reward table disabled".into()));
        }
        sqlx::query("insert into musubi.reward_pity (account_id, table_id) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(table_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let (counter, nonce): (i32, i64) = sqlx::query_as(
            "select counter, nonce from musubi.reward_pity where account_id = $1 and table_id = $2 for update",
        )
        .bind(account.0)
        .bind(table_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let nonce = nonce + 1;
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let server_seed = hex::encode(seed);
        let hash = roll_hash(&server_seed, client_seed, nonce);
        let pity_forced =
            t.kind == "weighted" && t.pity.after > 0 && counter as u32 >= t.pity.after;
        let rewards = t.outcome(&hash, pity_forced.then_some(t.pity.rarity.as_str()));
        if rewards.is_empty() {
            return Err(Error::Internal("empty outcome".into()));
        }
        let id = Uuid::now_v7();
        let rewards = Self::apply_rewards_for(
            &mut tx,
            account,
            Some(table_id),
            &rewards,
            "reward.roll",
            &id.to_string(),
        )
        .await?;
        let hit = t.pity.after > 0 && rewards.iter().any(|e| e.rarity == t.pity.rarity);
        sqlx::query("update musubi.reward_pity set counter = $3, nonce = $4 where account_id = $1 and table_id = $2")
            .bind(account.0)
            .bind(table_id)
            .bind(if hit { 0 } else { counter + 1 })
            .bind(nonce)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.reward_rolls (id, account_id, table_id, server_seed, client_seed, nonce, roll, result, idempotency_key)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(account.0)
        .bind(table_id)
        .bind(&server_seed)
        .bind(client_seed)
        .bind(nonce)
        .bind(&hash)
        .bind(json!({ "rewards": rewards, "pity_forced": pity_forced }))
        .bind(idempotency_key)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Roll {
            id,
            table_id: table_id.into(),
            server_seed,
            client_seed: client_seed.into(),
            nonce,
            roll: hash,
            rewards,
            pity_forced,
            replayed: false,
        })
    }

    /// Server-side grant of every entry of a table (login bonus, quest completion …).
    pub async fn grant_table(
        &self,
        account: AccountId,
        table_id: &str,
        reference: &str,
    ) -> Result<Vec<RewardEntry>> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<TableRow> = sqlx::query_as(&format!(
            "select {TABLE_COLS} from musubi.reward_tables where id = $1"
        ))
        .bind(table_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(t) = row.map(table) else {
            return Err(Error::NotFound(format!("reward table {table_id}")));
        };
        let applied = Self::apply_rewards_for(
            &mut tx,
            account,
            Some(table_id),
            &t.entries,
            "reward.grant",
            reference,
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(applied)
    }

    pub(crate) async fn load_table(
        tx: &mut Transaction<'_, Postgres>,
        id: &str,
    ) -> Result<Option<RewardTable>> {
        let row: Option<TableRow> = sqlx::query_as(&format!(
            "select {TABLE_COLS} from musubi.reward_tables where id = $1"
        ))
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(table))
    }

    /// Applies rewards with the active modifiers (currency scopes only); returns what was granted.
    pub(crate) async fn apply_rewards(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        rewards: &[RewardEntry],
        reason: &str,
        reference: &str,
    ) -> Result<Vec<RewardEntry>> {
        Self::apply_rewards_for(tx, account, None, rewards, reason, reference).await
    }

    /// Same with the table's own modifiers (`table:<id>`) taken into account.
    pub(crate) async fn apply_rewards_for(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        table_id: Option<&str>,
        rewards: &[RewardEntry],
        reason: &str,
        reference: &str,
    ) -> Result<Vec<RewardEntry>> {
        let rewards = Self::modified_rewards(tx, account, table_id, rewards).await?;
        for e in &rewards {
            if let Some(c) = &e.currency {
                Self::adjust_tx(tx, account, c, e.amount, reason, reference, None, json!({}))
                    .await?;
            }
            if let Some(i) = &e.item_id {
                Self::grant_item_tx(tx, account, i, e.amount, Value::Object(Default::default()))
                    .await?;
            }
        }
        Ok(rewards)
    }
}
