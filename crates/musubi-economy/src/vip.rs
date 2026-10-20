//! VIP tiers: a program turns lifetime spending (negative wallet-ledger deltas in the configured
//! currencies, optionally only for some reasons) and directly granted points into a tier with
//! perks the game reads (`discount_pct`, multipliers — free-form JSON) and a reward table granted
//! once when a tier is first reached. Evaluation is lazy and idempotent: every read recomputes
//! points from the ledger and grants whatever tier-up rewards are still owed.

use std::collections::BTreeMap;

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct VipTier {
    pub name: String,
    /// Points needed to reach the tier (ascending across tiers).
    pub threshold: i64,
    /// Free-form perks for the game: `{"discount_pct": 10, "xp_multiplier": 1.2, ...}`.
    pub perks: Value,
    /// Reward table granted once when the tier is first reached.
    pub tier_up_reward: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VipProgram {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Currency → points per unit spent (ledger deltas < 0).
    pub spend_currencies: BTreeMap<String, i64>,
    /// Only spending with these ledger reasons counts (empty = all spending).
    pub spend_reasons: Vec<String>,
    pub tiers: Vec<VipTier>,
    pub enabled: bool,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

impl Default for VipProgram {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            spend_currencies: BTreeMap::new(),
            spend_reasons: Vec::new(),
            tiers: Vec::new(),
            enabled: true,
            metadata: Value::Object(Default::default()),
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct VipStatus {
    pub program_id: String,
    pub program_name: String,
    pub account_id: AccountId,
    pub points: i64,
    pub spend_points: i64,
    pub manual_points: i64,
    /// Index into `tiers` (-1 = below the first tier).
    pub tier: i32,
    pub tier_name: Option<String>,
    pub perks: Value,
    pub next_tier: Option<Value>,
    /// Tier-up rewards granted by this evaluation.
    pub rewards: Vec<RewardEntry>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn program_from(id: String, doc: Value, enabled: bool, at: OffsetDateTime) -> VipProgram {
    let mut p: VipProgram = serde_json::from_value(doc).unwrap_or_default();
    p.id = id;
    p.enabled = enabled;
    p.updated_at = Some(at);
    p
}

impl EconomyService {
    pub async fn upsert_vip_program(&self, mut p: VipProgram) -> Result<VipProgram> {
        if !valid_id(&p.id) {
            return Err(Error::InvalidArgument(
                "vip program id: 1..=64 of [a-z0-9_-]".into(),
            ));
        }
        if p.name.len() > 128 || p.description.len() > 2048 {
            return Err(Error::InvalidArgument(
                "name ≤ 128, description ≤ 2048".into(),
            ));
        }
        if p.tiers.is_empty() || p.tiers.len() > 32 {
            return Err(Error::InvalidArgument("tiers: 1..=32 entries".into()));
        }
        let mut last = -1i64;
        for (i, t) in p.tiers.iter_mut().enumerate() {
            if t.name.is_empty() || t.name.len() > 64 {
                return Err(Error::InvalidArgument(format!(
                    "tiers[{i}].name: 1..=64 chars"
                )));
            }
            if t.threshold < 0 || t.threshold <= last {
                return Err(Error::InvalidArgument(
                    "tiers: thresholds must be ≥ 0 and strictly ascending".into(),
                ));
            }
            last = t.threshold;
            if t.perks.is_null() {
                t.perks = Value::Object(Default::default());
            }
            if !t.perks.is_object() {
                return Err(Error::InvalidArgument(format!(
                    "tiers[{i}].perks must be an object"
                )));
            }
            if let Some(table) = t.tier_up_reward.as_deref().filter(|t| !t.is_empty()) {
                let known: Option<(String,)> =
                    sqlx::query_as("select id from musubi.reward_tables where id = $1")
                        .bind(table)
                        .fetch_optional(&self.db)
                        .await
                        .map_err(Error::internal)?;
                if known.is_none() {
                    return Err(Error::InvalidArgument(format!(
                        "tiers[{i}].tier_up_reward: unknown reward table `{table}`"
                    )));
                }
            } else {
                t.tier_up_reward = None;
            }
        }
        for (cur, ppu) in &p.spend_currencies {
            if *ppu <= 0 {
                return Err(Error::InvalidArgument(format!(
                    "spend_currencies.{cur} must be positive points per unit"
                )));
            }
            let known: Option<(String,)> =
                sqlx::query_as("select code from musubi.currencies where code = $1")
                    .bind(cur)
                    .fetch_optional(&self.db)
                    .await
                    .map_err(Error::internal)?;
            if known.is_none() {
                return Err(Error::InvalidArgument(format!(
                    "spend_currencies: unknown currency `{cur}`"
                )));
            }
        }
        if p.spend_reasons.len() > 32
            || p.spend_reasons.iter().any(|r| r.is_empty() || r.len() > 64)
        {
            return Err(Error::InvalidArgument(
                "spend_reasons: ≤ 32 of 1..=64 chars".into(),
            ));
        }
        if p.metadata.is_null() {
            p.metadata = Value::Object(Default::default());
        }
        if !p.metadata.is_object() {
            return Err(Error::InvalidArgument("metadata must be an object".into()));
        }
        let mut doc = serde_json::to_value(&p).unwrap_or_default();
        if let Some(o) = doc.as_object_mut() {
            o.remove("id");
            o.remove("enabled");
            o.remove("updated_at");
        }
        let row: (String, Value, bool, OffsetDateTime) = sqlx::query_as(
            "insert into musubi.vip_programs (id, doc, enabled) values ($1, $2, $3)
             on conflict (id) do update set doc = excluded.doc, enabled = excluded.enabled, updated_at = now()
             returning id, doc, enabled, updated_at",
        )
        .bind(&p.id)
        .bind(&doc)
        .bind(p.enabled)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(program_from(row.0, row.1, row.2, row.3))
    }

    pub async fn vip_programs(&self, include_disabled: bool) -> Result<Vec<VipProgram>> {
        let rows: Vec<(String, Value, bool, OffsetDateTime)> = sqlx::query_as(
            "select id, doc, enabled, updated_at from musubi.vip_programs where $1 or enabled order by id",
        )
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| program_from(r.0, r.1, r.2, r.3))
            .collect())
    }

    pub async fn vip_program(&self, id: &str) -> Result<VipProgram> {
        let row: Option<(String, Value, bool, OffsetDateTime)> = sqlx::query_as(
            "select id, doc, enabled, updated_at from musubi.vip_programs where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(|r| program_from(r.0, r.1, r.2, r.3))
            .ok_or_else(|| Error::NotFound(format!("vip program `{id}`")))
    }

    pub async fn delete_vip_program(&self, id: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.vip_programs where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }

    async fn spend_points(&self, account: AccountId, p: &VipProgram) -> Result<i64> {
        if p.spend_currencies.is_empty() {
            return Ok(0);
        }
        let currencies: Vec<String> = p.spend_currencies.keys().cloned().collect();
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "select currency, coalesce(sum(-delta), 0)::bigint from musubi.wallet_ledger
             where account_id = $1 and delta < 0 and currency = any($2)
               and (cardinality($3::text[]) = 0 or reason = any($3))
             group by currency",
        )
        .bind(account.0)
        .bind(&currencies)
        .bind(&p.spend_reasons)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .iter()
            .map(|(cur, spent)| spent.saturating_mul(*p.spend_currencies.get(cur).unwrap_or(&0)))
            .fold(0i64, |a, b| a.saturating_add(b)))
    }

    /// Current standing in one program; grants tier-up rewards still owed.
    pub async fn vip_status(&self, account: AccountId, p: &VipProgram) -> Result<VipStatus> {
        let spend = self.spend_points(account, p).await?;
        let st: Option<(i64, i32)> = sqlx::query_as(
            "select manual_points, granted_tier from musubi.vip_status where program_id = $1 and account_id = $2",
        )
        .bind(&p.id)
        .bind(account.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let (manual, granted) = st.unwrap_or((0, -1));
        let points = spend.saturating_add(manual);
        let tier = p
            .tiers
            .iter()
            .rposition(|t| t.threshold <= points)
            .map(|i| i as i32)
            .unwrap_or(-1);
        let mut rewards = Vec::new();
        if tier > granted {
            sqlx::query(
                "insert into musubi.vip_status (program_id, account_id, manual_points, granted_tier)
                 values ($1, $2, 0, $3)
                 on conflict (program_id, account_id) do update
                     set granted_tier = greatest(musubi.vip_status.granted_tier, excluded.granted_tier), updated_at = now()",
            )
            .bind(&p.id)
            .bind(account.0)
            .bind(tier)
            .execute(&self.db)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
                    Error::NotFound(format!("account {}", account.0))
                }
                e => Error::internal(e),
            })?;
            for i in (granted + 1)..=tier {
                if let Some(table) = p.tiers[i as usize].tier_up_reward.as_deref() {
                    rewards.extend(
                        self.grant_table(account, table, &format!("vip:{}:{i}", p.id))
                            .await?,
                    );
                }
            }
        }
        let current = (tier >= 0).then(|| &p.tiers[tier as usize]);
        let next = p.tiers.get((tier + 1) as usize).map(|t| {
            json!({ "name": t.name, "threshold": t.threshold, "remaining": t.threshold - points })
        });
        Ok(VipStatus {
            program_id: p.id.clone(),
            program_name: p.name.clone(),
            account_id: account,
            points,
            spend_points: spend,
            manual_points: manual,
            tier,
            tier_name: current.map(|t| t.name.clone()),
            perks: current
                .map(|t| t.perks.clone())
                .unwrap_or(Value::Object(Default::default())),
            next_tier: next,
            rewards,
        })
    }

    /// Standing in every enabled program.
    pub async fn vip_statuses(&self, account: AccountId) -> Result<Vec<VipStatus>> {
        let mut out = Vec::new();
        for p in self.vip_programs(false).await? {
            out.push(self.vip_status(account, &p).await?);
        }
        Ok(out)
    }

    /// Adds (or removes) directly granted points, then re-evaluates.
    pub async fn vip_add_points(
        &self,
        account: AccountId,
        program: &str,
        delta: i64,
    ) -> Result<VipStatus> {
        let p = self.vip_program(program).await?;
        if delta == 0 {
            return Err(Error::InvalidArgument("delta must be non-zero".into()));
        }
        sqlx::query(
            "insert into musubi.vip_status (program_id, account_id, manual_points) values ($1, $2, $3)
             on conflict (program_id, account_id) do update
                 set manual_points = greatest(0, musubi.vip_status.manual_points + excluded.manual_points), updated_at = now()",
        )
        .bind(&p.id)
        .bind(account.0)
        .bind(delta.max(0))
        .execute(&self.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
                Error::NotFound(format!("account {}", account.0))
            }
            e => Error::internal(e),
        })?;
        if delta < 0 {
            sqlx::query(
                "update musubi.vip_status set manual_points = greatest(0, manual_points + $3), updated_at = now()
                 where program_id = $1 and account_id = $2",
            )
            .bind(&p.id)
            .bind(account.0)
            .bind(delta)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        }
        self.vip_status(account, &p).await
    }
}
