//! Quests: daily / weekly / one-off objectives driven by named game events, with claimable rewards.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Objective {
    pub event: String,
    pub target: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Quest {
    pub id: String,
    pub name: String,
    pub description: String,
    /// `daily` | `weekly` | `one_off`
    pub kind: String,
    pub objectives: Vec<Objective>,
    pub rewards: Option<String>,
    pub enabled: bool,
    /// Players may hide it for the current period (`POST /v1/quests/{id}/reroll`).
    pub rerollable: bool,
    /// Rerolls a player gets per period across all quests of this kind.
    pub rerolls_per_period: i32,
}

impl Default for Quest {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            kind: "daily".into(),
            objectives: Vec::new(),
            rewards: None,
            enabled: true,
            rerollable: false,
            rerolls_per_period: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QuestEventResult {
    pub updated: Vec<String>,
    pub completed: Vec<String>,
}

pub fn period_seconds(kind: &str) -> Option<i64> {
    match kind {
        "daily" => Some(86_400),
        "weekly" => Some(604_800),
        _ => None,
    }
}

pub fn period_start(kind: &str, now: OffsetDateTime) -> OffsetDateTime {
    match period_seconds(kind) {
        Some(p) => {
            OffsetDateTime::UNIX_EPOCH + Duration::seconds(now.unix_timestamp().div_euclid(p) * p)
        }
        None => OffsetDateTime::UNIX_EPOCH,
    }
}

fn is_complete(q: &Quest, progress: &Value) -> bool {
    q.objectives
        .iter()
        .all(|o| progress[&o.event].as_i64().unwrap_or(0) >= o.target)
}

type QuestRow = (
    String,
    String,
    String,
    String,
    Value,
    Option<String>,
    bool,
    bool,
    i32,
);
type ProgressRow = (Value, Option<OffsetDateTime>, Option<OffsetDateTime>);

fn quest(r: QuestRow) -> Quest {
    Quest {
        id: r.0,
        name: r.1,
        description: r.2,
        kind: r.3,
        objectives: serde_json::from_value(r.4).unwrap_or_default(),
        rewards: r.5,
        enabled: r.6,
        rerollable: r.7,
        rerolls_per_period: r.8,
    }
}

const COLS: &str =
    "id, name, description, kind, objectives, rewards, enabled, rerollable, rerolls_per_period";

impl EconomyService {
    pub async fn upsert_quest(&self, q: Quest) -> Result<Quest> {
        crate::check_code(&q.id)?;
        if !matches!(q.kind.as_str(), "daily" | "weekly" | "one_off") {
            return Err(Error::InvalidArgument("kind: daily|weekly|one_off".into()));
        }
        if q.objectives.is_empty()
            || q.objectives
                .iter()
                .any(|o| o.event.is_empty() || o.target <= 0)
        {
            return Err(Error::InvalidArgument(
                "objectives: non-empty, event + positive target".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.quests (id, name, description, kind, objectives, rewards, enabled, rerollable, rerolls_per_period)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             on conflict (id) do update set name = excluded.name, description = excluded.description, kind = excluded.kind,
             objectives = excluded.objectives, rewards = excluded.rewards, enabled = excluded.enabled,
             rerollable = excluded.rerollable, rerolls_per_period = excluded.rerolls_per_period",
        )
        .bind(&q.id)
        .bind(&q.name)
        .bind(&q.description)
        .bind(&q.kind)
        .bind(serde_json::to_value(&q.objectives)?)
        .bind(&q.rewards)
        .bind(q.enabled)
        .bind(q.rerollable)
        .bind(q.rerolls_per_period.max(0))
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(q)
    }

    pub async fn quests(&self) -> Result<Vec<Quest>> {
        let rows: Vec<QuestRow> =
            sqlx::query_as(&format!("select {COLS} from musubi.quests order by id"))
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows.into_iter().map(quest).collect())
    }

    /// Player view of enabled quests with progress for the current period.
    pub async fn quest_views(&self, account: AccountId, now: OffsetDateTime) -> Result<Vec<Value>> {
        let rows: Vec<QuestRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.quests where enabled order by id"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::with_capacity(rows.len());
        let rerolled: Vec<(String, OffsetDateTime)> = sqlx::query_as(
            "select quest_id, period_start from musubi.quest_rerolls where account_id = $1",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let quests: Vec<Quest> = rows.into_iter().map(quest).collect();
        // rerolls used this period, per kind
        let used = |kind: &str, ps: OffsetDateTime| -> i64 {
            rerolled
                .iter()
                .filter(|(qid, p)| {
                    *p == ps && quests.iter().any(|q| q.id == *qid && q.kind == kind)
                })
                .count() as i64
        };
        for q in quests.iter().cloned() {
            let ps = period_start(&q.kind, now);
            if rerolled.iter().any(|(qid, p)| *qid == q.id && *p == ps) {
                continue;
            }
            let p: Option<ProgressRow> = sqlx::query_as(
                "select progress, completed_at, claimed_at from musubi.quest_progress where account_id = $1 and quest_id = $2 and period_start = $3",
            )
            .bind(account.0)
            .bind(&q.id)
            .bind(ps)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
            let (progress, completed, claimed) = p.unwrap_or((json!({}), None, None));
            let resets_at = period_seconds(&q.kind).map(|s| ps + Duration::seconds(s));
            out.push(json!({
                "quest_id": q.id, "name": q.name, "description": q.description, "kind": q.kind,
                "objectives": q.objectives, "rewards": q.rewards, "progress": progress,
                "completed": completed.is_some(), "claimed": claimed.is_some(),
                "resets_at": resets_at.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
                "rerollable": q.rerollable,
                "rerolls_left": if q.rerollable { (q.rerolls_per_period as i64 - used(&q.kind, ps)).max(0) } else { 0 },
            }));
        }
        Ok(out)
    }

    /// Hide a rerollable quest for the rest of its period; counts against the kind's
    /// `rerolls_per_period`. Completed quests cannot be rerolled.
    pub async fn reroll_quest(
        &self,
        account: AccountId,
        quest_id: &str,
        now: OffsetDateTime,
    ) -> Result<Value> {
        let row: Option<QuestRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.quests where id = $1 and enabled"
        ))
        .bind(quest_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some(q) = row.map(quest) else {
            return Err(Error::NotFound(format!("quest {quest_id}")));
        };
        if !q.rerollable {
            return Err(Error::InvalidArgument("quest cannot be rerolled".into()));
        }
        let ps = period_start(&q.kind, now);
        let completed: Option<Option<OffsetDateTime>> = sqlx::query_scalar(
            "select completed_at from musubi.quest_progress where account_id = $1 and quest_id = $2 and period_start = $3",
        )
        .bind(account.0)
        .bind(quest_id)
        .bind(ps)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        if completed.flatten().is_some() {
            return Err(Error::Conflict("quest already completed".into()));
        }
        // Quota check and insert share one per-(account, kind, period) lock: two concurrent
        // rerolls must not both pass the count.
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        Self::lock_key(
            &mut tx,
            "quest.reroll",
            &format!("{}:{}:{}", account.0, q.kind, ps.unix_timestamp()),
        )
        .await?;
        let used: i64 = sqlx::query_scalar(
            "select count(*) from musubi.quest_rerolls r join musubi.quests q on q.id = r.quest_id
             where r.account_id = $1 and r.period_start = $2 and q.kind = $3 and r.quest_id <> $4",
        )
        .bind(account.0)
        .bind(ps)
        .bind(&q.kind)
        .bind(quest_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if used >= q.rerolls_per_period as i64 {
            return Err(Error::ResourceExhausted(format!(
                "no rerolls left this period ({} per period)",
                q.rerolls_per_period
            )));
        }
        sqlx::query(
            "insert into musubi.quest_rerolls (account_id, quest_id, period_start) values ($1, $2, $3) on conflict do nothing",
        )
        .bind(account.0)
        .bind(quest_id)
        .bind(ps)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(json!({
            "quest_id": quest_id,
            "rerolls_left": (q.rerolls_per_period as i64 - used - 1).max(0),
            "resets_at": period_seconds(&q.kind).map(|s| (ps + Duration::seconds(s)).unix_timestamp()),
        }))
    }

    /// Records a game event for the player: every enabled quest with a matching objective advances.
    pub async fn quest_event(
        &self,
        account: AccountId,
        event: &str,
        delta: i64,
        now: OffsetDateTime,
    ) -> Result<QuestEventResult> {
        if event.is_empty() || delta <= 0 {
            return Err(Error::InvalidArgument(
                "event and positive delta required".into(),
            ));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let rows: Vec<QuestRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.quests where enabled order by id"
        ))
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let mut res = QuestEventResult {
            updated: Vec::new(),
            completed: Vec::new(),
        };
        for q in rows.into_iter().map(quest) {
            let Some(obj) = q.objectives.iter().find(|o| o.event == event) else {
                continue;
            };
            let ps = period_start(&q.kind, now);
            sqlx::query("insert into musubi.quest_progress (account_id, quest_id, period_start) values ($1, $2, $3) on conflict do nothing")
                .bind(account.0)
                .bind(&q.id)
                .bind(ps)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            let (mut progress, completed, _): ProgressRow = sqlx::query_as(
                "select progress, completed_at, claimed_at from musubi.quest_progress where account_id = $1 and quest_id = $2 and period_start = $3 for update",
            )
            .bind(account.0)
            .bind(&q.id)
            .bind(ps)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if completed.is_some() {
                continue;
            }
            let cur = progress[event].as_i64().unwrap_or(0);
            progress[event] = json!(cur.saturating_add(delta).min(obj.target));
            let done = is_complete(&q, &progress);
            sqlx::query(
                "update musubi.quest_progress set progress = $4, completed_at = $5 where account_id = $1 and quest_id = $2 and period_start = $3",
            )
            .bind(account.0)
            .bind(&q.id)
            .bind(ps)
            .bind(&progress)
            .bind(done.then_some(now))
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            res.updated.push(q.id.clone());
            if done {
                res.completed.push(q.id);
            }
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(res)
    }

    pub async fn claim_quest(
        &self,
        account: AccountId,
        quest_id: &str,
        now: OffsetDateTime,
    ) -> Result<Vec<RewardEntry>> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<QuestRow> =
            sqlx::query_as(&format!("select {COLS} from musubi.quests where id = $1"))
                .bind(quest_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some(q) = row.map(quest) else {
            return Err(Error::NotFound(format!("quest {quest_id}")));
        };
        let ps = period_start(&q.kind, now);
        let p: Option<ProgressRow> = sqlx::query_as(
            "select progress, completed_at, claimed_at from musubi.quest_progress where account_id = $1 and quest_id = $2 and period_start = $3 for update",
        )
        .bind(account.0)
        .bind(quest_id)
        .bind(ps)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        match p {
            Some((_, Some(_), None)) => {}
            Some((_, _, Some(_))) => return Err(Error::Conflict("quest already claimed".into())),
            _ => return Err(Error::Conflict("quest not completed".into())),
        }
        let mut rewards = Vec::new();
        if let Some(t) = &q.rewards {
            let Some(table) = Self::load_table(&mut tx, t).await? else {
                return Err(Error::NotFound(format!("reward table {t}")));
            };
            Self::apply_rewards(&mut tx, account, &table.entries, "quest.claim", quest_id).await?;
            rewards = table.entries;
        }
        sqlx::query("update musubi.quest_progress set claimed_at = $4 where account_id = $1 and quest_id = $2 and period_start = $3")
            .bind(account.0)
            .bind(quest_id)
            .bind(ps)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(rewards)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_align_to_epoch_buckets() {
        let t = OffsetDateTime::UNIX_EPOCH + Duration::seconds(86_400 * 3 + 5000);
        assert_eq!(
            period_start("daily", t),
            OffsetDateTime::UNIX_EPOCH + Duration::seconds(86_400 * 3)
        );
        assert_eq!(period_start("weekly", t), OffsetDateTime::UNIX_EPOCH);
        assert_eq!(period_start("one_off", t), OffsetDateTime::UNIX_EPOCH);
    }
}
