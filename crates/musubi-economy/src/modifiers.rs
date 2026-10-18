//! Reward modifiers: multipliers with a time window, global or per account, on XP
//! (`xp`, `xp:<track>`), currency rewards (`currency`, `currency:<code>`) or a reward table
//! (`table:<id>`). Several active modifiers multiply together.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RewardModifier {
    pub id: String,
    /// `None` applies to every player.
    pub account_id: Option<Uuid>,
    pub scope: String,
    pub factor: f64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub starts_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    pub enabled: bool,
    pub note: String,
}

impl Default for RewardModifier {
    fn default() -> Self {
        Self {
            id: String::new(),
            account_id: None,
            scope: String::new(),
            factor: 1.0,
            starts_at: None,
            ends_at: None,
            enabled: true,
            note: String::new(),
        }
    }
}

type Row = (
    String,
    Option<Uuid>,
    String,
    f64,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    bool,
    String,
);
const COLS: &str = "id, account_id, scope, factor, starts_at, ends_at, enabled, note";

fn modifier(r: Row) -> RewardModifier {
    RewardModifier {
        id: r.0,
        account_id: r.1,
        scope: r.2,
        factor: r.3,
        starts_at: r.4,
        ends_at: r.5,
        enabled: r.6,
        note: r.7,
    }
}

fn valid_scope(s: &str) -> bool {
    match s.split_once(':') {
        None => matches!(s, "xp" | "currency"),
        Some((kind, id)) => matches!(kind, "xp" | "currency" | "table") && !id.is_empty(),
    }
}

/// Multiply a reward amount, never below 1 for a positive amount and a positive factor.
pub fn scale(amount: i64, factor: f64) -> i64 {
    if factor == 1.0 || amount <= 0 {
        return amount;
    }
    (((amount as f64) * factor).round() as i64).max(1)
}

impl EconomyService {
    pub async fn upsert_reward_modifier(&self, m: RewardModifier) -> Result<RewardModifier> {
        crate::check_code(&m.id)?;
        if !valid_scope(&m.scope) {
            return Err(Error::InvalidArgument(
                "scope must be xp, xp:<track>, currency, currency:<code> or table:<id>".into(),
            ));
        }
        if !(m.factor > 0.0 && m.factor <= 1000.0) || !m.factor.is_finite() {
            return Err(Error::InvalidArgument("factor must be in (0, 1000]".into()));
        }
        if let (Some(a), Some(b)) = (m.starts_at, m.ends_at) {
            if b <= a {
                return Err(Error::InvalidArgument(
                    "ends_at must be after starts_at".into(),
                ));
            }
        }
        sqlx::query(
            "insert into musubi.reward_modifiers (id, account_id, scope, factor, starts_at, ends_at, enabled, note)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (id) do update set account_id = excluded.account_id, scope = excluded.scope,
               factor = excluded.factor, starts_at = excluded.starts_at, ends_at = excluded.ends_at,
               enabled = excluded.enabled, note = excluded.note",
        )
        .bind(&m.id)
        .bind(m.account_id)
        .bind(&m.scope)
        .bind(m.factor)
        .bind(m.starts_at)
        .bind(m.ends_at)
        .bind(m.enabled)
        .bind(&m.note)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(m)
    }

    pub async fn reward_modifiers(&self) -> Result<Vec<RewardModifier>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.reward_modifiers order by id"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(modifier).collect())
    }

    pub async fn delete_reward_modifier(&self, id: &str) -> Result<bool> {
        let r = sqlx::query("delete from musubi.reward_modifiers where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    /// Modifiers currently active for a player: global ones plus their own.
    pub async fn active_reward_modifiers(&self, account: AccountId) -> Result<Vec<RewardModifier>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.reward_modifiers
             where enabled and (account_id is null or account_id = $1)
               and (starts_at is null or starts_at <= now()) and (ends_at is null or ends_at > now())
             order by id"
        ))
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(modifier).collect())
    }

    /// Product of the active factors for any of `scopes`.
    pub(crate) async fn active_factor(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        scopes: &[String],
    ) -> Result<f64> {
        let (f,): (f64,) = sqlx::query_as(
            "select coalesce(exp(sum(ln(factor))), 1.0)::float8 from musubi.reward_modifiers
             where enabled and (account_id is null or account_id = $1) and scope = any($2)
               and (starts_at is null or starts_at <= now()) and (ends_at is null or ends_at > now())",
        )
        .bind(account.0)
        .bind(scopes)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(f)
    }

    pub(crate) async fn xp_factor(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        track_id: &str,
    ) -> Result<f64> {
        Self::active_factor(tx, account, &["xp".to_string(), format!("xp:{track_id}")]).await
    }

    /// Applies the table and currency modifiers to a reward list (items are never multiplied).
    pub(crate) async fn modified_rewards(
        tx: &mut Transaction<'_, Postgres>,
        account: AccountId,
        table_id: Option<&str>,
        rewards: &[RewardEntry],
    ) -> Result<Vec<RewardEntry>> {
        let mut scopes = vec!["currency".to_string()];
        if let Some(t) = table_id {
            scopes.push(format!("table:{t}"));
        }
        for e in rewards {
            if let Some(c) = &e.currency {
                scopes.push(format!("currency:{c}"));
            }
        }
        scopes.sort();
        scopes.dedup();
        let mut out = Vec::with_capacity(rewards.len());
        for e in rewards {
            let mut e = e.clone();
            if let Some(c) = &e.currency {
                let mut s = vec!["currency".to_string(), format!("currency:{c}")];
                if let Some(t) = table_id {
                    s.push(format!("table:{t}"));
                }
                let f = Self::active_factor(tx, account, &s).await?;
                e.amount = scale(e.amount, f);
            }
            out.push(e);
        }
        Ok(out)
    }
}
