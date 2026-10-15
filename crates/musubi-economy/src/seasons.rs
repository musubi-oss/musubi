//! Seasons / battle pass: a time window bound to an XP track, with free and premium reward tiers per level.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::progression::{level_for, Level};
use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct SeasonLevel {
    pub level: i32,
    pub free: Option<String>,
    pub premium: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Season {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ends_at: OffsetDateTime,
    /// Progression track that carries season XP.
    pub track_id: String,
    #[serde(default)]
    pub levels: Vec<SeasonLevel>,
}

type SeasonRow = (
    String,
    String,
    OffsetDateTime,
    OffsetDateTime,
    String,
    Value,
);

fn season(r: SeasonRow) -> Season {
    Season {
        id: r.0,
        name: r.1,
        starts_at: r.2,
        ends_at: r.3,
        track_id: r.4,
        levels: serde_json::from_value(r.5).unwrap_or_default(),
    }
}

const COLS: &str = "id, name, starts_at, ends_at, track_id, levels";

impl EconomyService {
    pub async fn upsert_season(&self, s: Season) -> Result<Season> {
        crate::check_code(&s.id)?;
        if s.ends_at <= s.starts_at {
            return Err(Error::InvalidArgument(
                "ends_at must be after starts_at".into(),
            ));
        }
        if s.levels.is_empty()
            || s.levels
                .iter()
                .any(|l| l.level <= 0 || (l.free.is_none() && l.premium.is_none()))
        {
            return Err(Error::InvalidArgument(
                "levels: positive level with a free and/or premium table".into(),
            ));
        }
        let track: Option<(String,)> =
            sqlx::query_as("select id from musubi.progression_tracks where id = $1")
                .bind(&s.track_id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        if track.is_none() {
            return Err(Error::NotFound(format!("track {}", s.track_id)));
        }
        sqlx::query(
            "insert into musubi.seasons (id, name, starts_at, ends_at, track_id, levels) values ($1, $2, $3, $4, $5, $6)
             on conflict (id) do update set name = excluded.name, starts_at = excluded.starts_at, ends_at = excluded.ends_at,
             track_id = excluded.track_id, levels = excluded.levels",
        )
        .bind(&s.id)
        .bind(&s.name)
        .bind(s.starts_at)
        .bind(s.ends_at)
        .bind(&s.track_id)
        .bind(serde_json::to_value(&s.levels)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(s)
    }

    pub async fn seasons(&self) -> Result<Vec<Season>> {
        let rows: Vec<SeasonRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.seasons order by starts_at desc"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(season).collect())
    }

    /// The active season (if any) with the player's level, premium flag and claimed tiers.
    pub async fn current_season(
        &self,
        account: AccountId,
        now: OffsetDateTime,
    ) -> Result<Option<Value>> {
        let row: Option<SeasonRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.seasons where starts_at <= $1 and ends_at > $1 order by starts_at desc limit 1"
        ))
        .bind(now)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some(s) = row.map(season) else {
            return Ok(None);
        };
        let (xp, level) = self.track_level(account, &s.track_id).await?;
        let st: Option<(bool, Value)> =
            sqlx::query_as("select premium, claimed from musubi.season_state where account_id = $1 and season_id = $2")
                .bind(account.0)
                .bind(&s.id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        let (premium, claimed) = st.unwrap_or((false, json!([])));
        Ok(Some(json!({
            "season": s, "xp": xp, "level": level, "premium": premium, "claimed": claimed,
        })))
    }

    async fn track_level(&self, account: AccountId, track_id: &str) -> Result<(i64, i32)> {
        let row: Option<(Value, i64)> = sqlx::query_as(
            "select t.levels, coalesce(p.xp, 0) from musubi.progression_tracks t
             left join musubi.progress p on p.track_id = t.id and p.account_id = $1 where t.id = $2",
        )
        .bind(account.0)
        .bind(track_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some((levels, xp)) = row else {
            return Err(Error::NotFound(format!("track {track_id}")));
        };
        let levels: Vec<Level> = serde_json::from_value(levels).unwrap_or_default();
        Ok((xp, level_for(&levels, xp)))
    }

    pub async fn grant_premium(&self, account: AccountId, season_id: &str) -> Result<()> {
        let exists: Option<(String,)> =
            sqlx::query_as("select id from musubi.seasons where id = $1")
                .bind(season_id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        if exists.is_none() {
            return Err(Error::NotFound(format!("season {season_id}")));
        }
        sqlx::query(
            "insert into musubi.season_state (account_id, season_id, premium) values ($1, $2, true)
             on conflict (account_id, season_id) do update set premium = true",
        )
        .bind(account.0)
        .bind(season_id)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Claims one tier (`free` | `premium`) of one level; requires the level on the track and, for
    /// premium, the premium pass.
    pub async fn claim_season(
        &self,
        account: AccountId,
        season_id: &str,
        level: i32,
        tier: &str,
        now: OffsetDateTime,
    ) -> Result<Vec<RewardEntry>> {
        if !matches!(tier, "free" | "premium") {
            return Err(Error::InvalidArgument("tier: free|premium".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<SeasonRow> =
            sqlx::query_as(&format!("select {COLS} from musubi.seasons where id = $1"))
                .bind(season_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some(s) = row.map(season) else {
            return Err(Error::NotFound(format!("season {season_id}")));
        };
        if now < s.starts_at || now >= s.ends_at {
            return Err(Error::PermissionDenied("season is not active".into()));
        }
        let Some(def) = s.levels.iter().find(|l| l.level == level) else {
            return Err(Error::NotFound(format!("season level {level}")));
        };
        let table_id = match tier {
            "free" => def.free.as_deref(),
            _ => def.premium.as_deref(),
        };
        let Some(table_id) = table_id else {
            return Err(Error::NotFound(format!(
                "no {tier} reward at level {level}"
            )));
        };
        let (_, reached) = self.track_level(account, &s.track_id).await?;
        if reached < level {
            return Err(Error::PermissionDenied(format!(
                "level {level} not reached (at {reached})"
            )));
        }
        sqlx::query("insert into musubi.season_state (account_id, season_id) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(season_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let (premium, mut claimed): (bool, Value) =
            sqlx::query_as("select premium, claimed from musubi.season_state where account_id = $1 and season_id = $2 for update")
                .bind(account.0)
                .bind(season_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if tier == "premium" && !premium {
            return Err(Error::PermissionDenied("premium pass required".into()));
        }
        let key = format!("{tier}:{level}");
        let list = claimed
            .as_array_mut()
            .ok_or_else(|| Error::Internal("claimed".into()))?;
        if list.iter().any(|v| v == &json!(key)) {
            return Err(Error::Conflict("already claimed".into()));
        }
        let Some(table) = Self::load_table(&mut tx, table_id).await? else {
            return Err(Error::NotFound(format!("reward table {table_id}")));
        };
        Self::apply_rewards(
            &mut tx,
            account,
            &table.entries,
            "season.claim",
            &format!("{season_id}:{key}"),
        )
        .await?;
        list.push(json!(key));
        sqlx::query(
            "update musubi.season_state set claimed = $3 where account_id = $1 and season_id = $2",
        )
        .bind(account.0)
        .bind(season_id)
        .bind(&claimed)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(table.entries)
    }
}
