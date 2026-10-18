//! Achievements with progress targets; reaching the target unlocks and auto-grants the reward table.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Achievement {
    pub id: String,
    pub name: String,
    pub description: String,
    pub target: i32,
    pub rewards: Option<String>,
    pub hidden: bool,
    pub enabled: bool,
    /// Platform achievement / trophy ids keyed by provider (`steam`, `psn`, `xbox`, ...). Steam is
    /// unlocked server-to-server when `[iap.steam]` is configured; the rest ride on the
    /// `achievement.unlocked` event for the client SDK.
    pub platform_ids: std::collections::HashMap<String, String>,
}

impl Default for Achievement {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            target: 1,
            rewards: None,
            hidden: false,
            enabled: true,
            platform_ids: Default::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AchievementUpdate {
    pub achievement_id: String,
    pub progress: i32,
    pub target: i32,
    /// True only on the call that reached the target.
    pub unlocked: bool,
    pub rewards: Vec<RewardEntry>,
    pub platform_ids: std::collections::HashMap<String, String>,
}

type AchRow = (
    String,
    String,
    String,
    i32,
    Option<String>,
    bool,
    bool,
    Value,
);
type ProgressRow = (
    String,
    String,
    String,
    i32,
    bool,
    Option<i32>,
    Option<OffsetDateTime>,
    Value,
);

fn ach(r: AchRow) -> Achievement {
    Achievement {
        id: r.0,
        name: r.1,
        description: r.2,
        target: r.3,
        rewards: r.4,
        hidden: r.5,
        enabled: r.6,
        platform_ids: serde_json::from_value(r.7).unwrap_or_default(),
    }
}

const COLS: &str = "id, name, description, target, rewards, hidden, enabled, platform_ids";

impl EconomyService {
    pub async fn upsert_achievement(&self, a: Achievement) -> Result<Achievement> {
        crate::check_code(&a.id)?;
        if a.target <= 0 {
            return Err(Error::InvalidArgument("target must be positive".into()));
        }
        sqlx::query(
            "insert into musubi.achievements (id, name, description, target, rewards, hidden, enabled, platform_ids)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (id) do update set name = excluded.name, description = excluded.description, target = excluded.target,
             rewards = excluded.rewards, hidden = excluded.hidden, enabled = excluded.enabled, platform_ids = excluded.platform_ids",
        )
        .bind(&a.id)
        .bind(&a.name)
        .bind(&a.description)
        .bind(a.target)
        .bind(&a.rewards)
        .bind(a.hidden)
        .bind(a.enabled)
        .bind(serde_json::to_value(&a.platform_ids).map_err(Error::internal)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(a)
    }

    pub async fn achievements(&self) -> Result<Vec<Achievement>> {
        let rows: Vec<AchRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.achievements order by id"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(ach).collect())
    }

    /// Player view: enabled achievements; hidden ones only once there is progress.
    pub async fn achievement_progress(&self, account: AccountId) -> Result<Vec<Value>> {
        let rows: Vec<ProgressRow> = sqlx::query_as(
            "select a.id, a.name, a.description, a.target, a.hidden, p.progress, p.unlocked_at, a.platform_ids from musubi.achievements a
             left join musubi.achievement_progress p on p.achievement_id = a.id and p.account_id = $1
             where a.enabled and (not a.hidden or coalesce(p.progress, 0) > 0) order by a.id",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                json!({
                    "achievement_id": r.0, "name": r.1, "description": r.2, "target": r.3, "hidden": r.4,
                    "progress": r.5.unwrap_or(0), "unlocked": r.6.is_some(),
                    "unlocked_at": r.6.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
                    "platform_ids": r.7,
                })
            })
            .collect())
    }

    pub async fn add_achievement_progress(
        &self,
        account: AccountId,
        achievement_id: &str,
        delta: i32,
        now: OffsetDateTime,
    ) -> Result<AchievementUpdate> {
        if delta <= 0 {
            return Err(Error::InvalidArgument("delta must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<AchRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.achievements where id = $1"
        ))
        .bind(achievement_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(a) = row.map(ach) else {
            return Err(Error::NotFound(format!("achievement {achievement_id}")));
        };
        if !a.enabled {
            return Err(Error::PermissionDenied("achievement disabled".into()));
        }
        sqlx::query("insert into musubi.achievement_progress (account_id, achievement_id) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(achievement_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let (progress, unlocked_at): (i32, Option<OffsetDateTime>) = sqlx::query_as(
            "select progress, unlocked_at from musubi.achievement_progress where account_id = $1 and achievement_id = $2 for update",
        )
        .bind(account.0)
        .bind(achievement_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let next = progress.saturating_add(delta).min(a.target);
        let unlocked = unlocked_at.is_none() && next >= a.target;
        let mut rewards = Vec::new();
        if unlocked {
            if let Some(t) = &a.rewards {
                let Some(table) = Self::load_table(&mut tx, t).await? else {
                    return Err(Error::NotFound(format!("reward table {t}")));
                };
                Self::apply_rewards(
                    &mut tx,
                    account,
                    &table.entries,
                    "achievement.unlock",
                    achievement_id,
                )
                .await?;
                rewards = table.entries;
            }
        }
        sqlx::query(
            "update musubi.achievement_progress set progress = $3, unlocked_at = coalesce(unlocked_at, $4)
             where account_id = $1 and achievement_id = $2",
        )
        .bind(account.0)
        .bind(achievement_id)
        .bind(next)
        .bind(unlocked.then_some(now))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(AchievementUpdate {
            platform_ids: a.platform_ids.clone(),
            achievement_id: achievement_id.into(),
            progress: next,
            target: a.target,
            unlocked,
            rewards,
        })
    }
}
