//! Progression: XP tracks with level rewards, energy with lazy time regeneration, daily streaks
//! with a grace period. All time-dependent logic takes an explicit `now` so it is unit-testable.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Level {
    /// Cumulative XP required to reach this level.
    pub xp: i64,
    /// Reward table granted when the level is reached.
    pub rewards: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub levels: Vec<Level>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub track_id: String,
    pub xp: i64,
    pub level: i32,
    pub next_level_xp: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct XpResult {
    pub progress: Progress,
    pub previous_level: i32,
    pub level_ups: Vec<i32>,
    pub rewards: Vec<RewardEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EnergyDef {
    pub id: String,
    pub name: String,
    pub max: i32,
    pub regen_seconds: i64,
    pub initial: i32,
}

impl Default for EnergyDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            max: 0,
            regen_seconds: 0,
            initial: -1,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EnergyState {
    pub energy_id: String,
    pub amount: i32,
    pub max: i32,
    pub regen_seconds: i64,
    /// Seconds until the next unit regenerates (None when full).
    pub next_in_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Milestone {
    pub count: i32,
    pub rewards: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StreakDef {
    pub id: String,
    pub name: String,
    pub period_seconds: i64,
    pub grace_seconds: i64,
    pub milestones: Vec<Milestone>,
}

impl Default for StreakDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            period_seconds: 86_400,
            grace_seconds: 86_400,
            milestones: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StreakClaim {
    pub streak_id: String,
    pub count: i32,
    pub rewards: Vec<RewardEntry>,
    pub next_claim_at: OffsetDateTime,
}

fn level_for(levels: &[Level], xp: i64) -> i32 {
    levels.iter().filter(|l| l.xp <= xp).count() as i32
}

fn progress(track: &Track, xp: i64) -> Progress {
    let level = level_for(&track.levels, xp);
    Progress {
        track_id: track.id.clone(),
        xp,
        level,
        next_level_xp: track.levels.get(level as usize).map(|l| l.xp),
    }
}

/// Lazy regeneration: returns the current amount and the timestamp the regen clock continues from.
pub fn regenerate(
    amount: i32,
    updated_at: OffsetDateTime,
    def: &EnergyDef,
    now: OffsetDateTime,
) -> (i32, OffsetDateTime) {
    if amount >= def.max || def.regen_seconds <= 0 {
        return (amount, now);
    }
    let elapsed = (now - updated_at).whole_seconds().max(0);
    let units = (elapsed / def.regen_seconds).min((def.max - amount) as i64);
    let cur = amount + units as i32;
    if cur >= def.max {
        (cur, now)
    } else {
        (
            cur,
            updated_at + Duration::seconds(units * def.regen_seconds),
        )
    }
}

fn energy_state(
    def: &EnergyDef,
    amount: i32,
    updated_at: OffsetDateTime,
    now: OffsetDateTime,
) -> EnergyState {
    let next =
        (amount < def.max).then(|| (def.regen_seconds - (now - updated_at).whole_seconds()).max(0));
    EnergyState {
        energy_id: def.id.clone(),
        amount,
        max: def.max,
        regen_seconds: def.regen_seconds,
        next_in_seconds: next,
    }
}

/// Next streak count for a claim at `now`: Conflict inside the period, reset after period + grace.
pub fn next_streak(
    count: i32,
    last_claim: Option<OffsetDateTime>,
    def: &StreakDef,
    now: OffsetDateTime,
) -> Result<i32> {
    let Some(last) = last_claim else { return Ok(1) };
    let elapsed = (now - last).whole_seconds();
    if elapsed < def.period_seconds {
        return Err(Error::Conflict(
            "streak already claimed for this period".into(),
        ));
    }
    if elapsed > def.period_seconds + def.grace_seconds {
        return Ok(1);
    }
    Ok(count + 1)
}

type TrackRow = (String, String, Value);
type EnergyRow = (String, String, i32, i64, i32);
type StreakRow = (String, String, i64, i64, Value);
type EnergyStateRow = (
    String,
    String,
    i32,
    i64,
    i32,
    Option<i32>,
    Option<OffsetDateTime>,
);
type StreakStateRow = (String, i64, i64, Option<i32>, Option<OffsetDateTime>);

fn track(r: TrackRow) -> Track {
    Track {
        id: r.0,
        name: r.1,
        levels: serde_json::from_value(r.2).unwrap_or_default(),
    }
}
fn energy(r: EnergyRow) -> EnergyDef {
    EnergyDef {
        id: r.0,
        name: r.1,
        max: r.2,
        regen_seconds: r.3,
        initial: r.4,
    }
}
fn streak(r: StreakRow) -> StreakDef {
    StreakDef {
        id: r.0,
        name: r.1,
        period_seconds: r.2,
        grace_seconds: r.3,
        milestones: serde_json::from_value(r.4).unwrap_or_default(),
    }
}

impl EconomyService {
    // ----- XP tracks -----

    pub async fn upsert_track(&self, t: Track) -> Result<Track> {
        crate::check_code(&t.id)?;
        if t.levels.is_empty()
            || t.levels.windows(2).any(|w| w[1].xp <= w[0].xp)
            || t.levels[0].xp <= 0
        {
            return Err(Error::InvalidArgument(
                "levels must be non-empty with strictly increasing positive xp".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.progression_tracks (id, name, levels) values ($1, $2, $3)
             on conflict (id) do update set name = excluded.name, levels = excluded.levels",
        )
        .bind(&t.id)
        .bind(&t.name)
        .bind(serde_json::to_value(&t.levels)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(t)
    }

    pub async fn tracks(&self) -> Result<Vec<Track>> {
        let rows: Vec<TrackRow> =
            sqlx::query_as("select id, name, levels from musubi.progression_tracks order by id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows.into_iter().map(track).collect())
    }

    pub async fn progress(&self, account: AccountId) -> Result<Vec<Progress>> {
        let rows: Vec<(String, String, Value, i64)> = sqlx::query_as(
            "select t.id, t.name, t.levels, coalesce(p.xp, 0) from musubi.progression_tracks t
             left join musubi.progress p on p.track_id = t.id and p.account_id = $1 order by t.id",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| progress(&track((r.0, r.1, r.2)), r.3))
            .collect())
    }

    /// Adds XP; every level reached grants its reward table in the same transaction.
    pub async fn add_xp(
        &self,
        account: AccountId,
        track_id: &str,
        delta: i64,
        reason: &str,
    ) -> Result<XpResult> {
        if delta <= 0 {
            return Err(Error::InvalidArgument("delta must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<TrackRow> =
            sqlx::query_as("select id, name, levels from musubi.progression_tracks where id = $1")
                .bind(track_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some(t) = row.map(track) else {
            return Err(Error::NotFound(format!("track {track_id}")));
        };
        sqlx::query("insert into musubi.progress (account_id, track_id) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(track_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let (xp,): (i64,) = sqlx::query_as(
            "select xp from musubi.progress where account_id = $1 and track_id = $2 for update",
        )
        .bind(account.0)
        .bind(track_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let previous_level = level_for(&t.levels, xp);
        let new_xp = xp.saturating_add(delta);
        let new_level = level_for(&t.levels, new_xp);
        let mut rewards = Vec::new();
        let level_ups: Vec<i32> = (previous_level + 1..=new_level).collect();
        for lvl in &level_ups {
            if let Some(table_id) = t.levels[(*lvl - 1) as usize].rewards.as_deref() {
                let Some(table) = Self::load_table(&mut tx, table_id).await? else {
                    return Err(Error::NotFound(format!("reward table {table_id}")));
                };
                Self::apply_rewards(
                    &mut tx,
                    account,
                    &table.entries,
                    "progression.level",
                    &format!("{track_id}:{lvl}:{reason}"),
                )
                .await?;
                rewards.extend(table.entries);
            }
        }
        sqlx::query(
            "update musubi.progress set xp = $3, level = $4, updated_at = now() where account_id = $1 and track_id = $2",
        )
        .bind(account.0)
        .bind(track_id)
        .bind(new_xp)
        .bind(new_level)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(XpResult {
            progress: progress(&t, new_xp),
            previous_level,
            level_ups,
            rewards,
        })
    }

    // ----- energy -----

    pub async fn upsert_energy(&self, mut d: EnergyDef) -> Result<EnergyDef> {
        crate::check_code(&d.id)?;
        if d.max <= 0 || d.regen_seconds <= 0 {
            return Err(Error::InvalidArgument(
                "max and regen_seconds must be positive".into(),
            ));
        }
        if d.initial < 0 {
            d.initial = d.max;
        }
        sqlx::query(
            "insert into musubi.energies (id, name, max, regen_seconds, initial) values ($1, $2, $3, $4, $5)
             on conflict (id) do update set name = excluded.name, max = excluded.max, regen_seconds = excluded.regen_seconds, initial = excluded.initial",
        )
        .bind(&d.id)
        .bind(&d.name)
        .bind(d.max)
        .bind(d.regen_seconds)
        .bind(d.initial.min(d.max))
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(d)
    }

    pub async fn energies(&self) -> Result<Vec<EnergyDef>> {
        let rows: Vec<EnergyRow> = sqlx::query_as(
            "select id, name, max, regen_seconds, initial from musubi.energies order by id",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(energy).collect())
    }

    pub async fn energy(
        &self,
        account: AccountId,
        now: OffsetDateTime,
    ) -> Result<Vec<EnergyState>> {
        let rows: Vec<EnergyStateRow> = sqlx::query_as(
            "select e.id, e.name, e.max, e.regen_seconds, e.initial, s.amount, s.updated_at from musubi.energies e
             left join musubi.energy_state s on s.energy_id = e.id and s.account_id = $1 order by e.id",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let def = energy((r.0, r.1, r.2, r.3, r.4));
                let (amount, at) = match (r.5, r.6) {
                    (Some(a), Some(at)) => regenerate(a, at, &def, now),
                    _ => (def.initial, now),
                };
                energy_state(&def, amount, at, now)
            })
            .collect())
    }

    /// `delta < 0` spends (ResourceExhausted when short), `delta > 0` grants (capped at max).
    pub async fn adjust_energy(
        &self,
        account: AccountId,
        energy_id: &str,
        delta: i32,
        now: OffsetDateTime,
    ) -> Result<EnergyState> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<EnergyRow> = sqlx::query_as(
            "select id, name, max, regen_seconds, initial from musubi.energies where id = $1",
        )
        .bind(energy_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(def) = row.map(energy) else {
            return Err(Error::NotFound(format!("energy {energy_id}")));
        };
        sqlx::query(
            "insert into musubi.energy_state (account_id, energy_id, amount, updated_at) values ($1, $2, $3, $4) on conflict do nothing",
        )
        .bind(account.0)
        .bind(energy_id)
        .bind(def.initial)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let (amount, at): (i32, OffsetDateTime) =
            sqlx::query_as("select amount, updated_at from musubi.energy_state where account_id = $1 and energy_id = $2 for update")
                .bind(account.0)
                .bind(energy_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let (cur, at) = regenerate(amount, at, &def, now);
        let next = if delta < 0 {
            if cur + delta < 0 {
                return Err(Error::ResourceExhausted(format!(
                    "not enough {energy_id}: {cur} available"
                )));
            }
            cur + delta
        } else {
            (cur + delta).min(def.max)
        };
        let at = if cur >= def.max { now } else { at };
        sqlx::query("update musubi.energy_state set amount = $3, updated_at = $4 where account_id = $1 and energy_id = $2")
            .bind(account.0)
            .bind(energy_id)
            .bind(next)
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(energy_state(&def, next, at, now))
    }

    // ----- streaks -----

    pub async fn upsert_streak(&self, d: StreakDef) -> Result<StreakDef> {
        crate::check_code(&d.id)?;
        if d.period_seconds <= 0 || d.grace_seconds < 0 || d.milestones.iter().any(|m| m.count <= 0)
        {
            return Err(Error::InvalidArgument(
                "period must be positive, grace non-negative, milestone counts positive".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.streaks (id, name, period_seconds, grace_seconds, milestones) values ($1, $2, $3, $4, $5)
             on conflict (id) do update set name = excluded.name, period_seconds = excluded.period_seconds,
             grace_seconds = excluded.grace_seconds, milestones = excluded.milestones",
        )
        .bind(&d.id)
        .bind(&d.name)
        .bind(d.period_seconds)
        .bind(d.grace_seconds)
        .bind(serde_json::to_value(&d.milestones)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(d)
    }

    pub async fn streaks(&self) -> Result<Vec<StreakDef>> {
        let rows: Vec<StreakRow> =
            sqlx::query_as("select id, name, period_seconds, grace_seconds, milestones from musubi.streaks order by id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(rows.into_iter().map(streak).collect())
    }

    pub async fn streak_states(
        &self,
        account: AccountId,
        now: OffsetDateTime,
    ) -> Result<Vec<Value>> {
        let rows: Vec<StreakStateRow> = sqlx::query_as(
            "select d.id, d.period_seconds, d.grace_seconds, s.count, s.last_claim from musubi.streaks d
             left join musubi.streak_state s on s.streak_id = d.id and s.account_id = $1 order by d.id",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let claimable_at = r.4.map(|l| l + Duration::seconds(r.1));
                let expires_at = r.4.map(|l| l + Duration::seconds(r.1 + r.2));
                json!({
                    "streak_id": r.0,
                    "count": r.3.unwrap_or(0),
                    "last_claim": r.4,
                    "claimable": claimable_at.is_none_or(|t| t <= now),
                    "claimable_at": claimable_at,
                    "expires_at": expires_at,
                })
            })
            .collect())
    }

    pub async fn claim_streak(
        &self,
        account: AccountId,
        streak_id: &str,
        now: OffsetDateTime,
    ) -> Result<StreakClaim> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<StreakRow> =
            sqlx::query_as("select id, name, period_seconds, grace_seconds, milestones from musubi.streaks where id = $1")
                .bind(streak_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some(def) = row.map(streak) else {
            return Err(Error::NotFound(format!("streak {streak_id}")));
        };
        sqlx::query("insert into musubi.streak_state (account_id, streak_id) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(streak_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let (count, last): (i32, Option<OffsetDateTime>) =
            sqlx::query_as("select count, last_claim from musubi.streak_state where account_id = $1 and streak_id = $2 for update")
                .bind(account.0)
                .bind(streak_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let count = next_streak(count, last, &def, now)?;
        let mut rewards = Vec::new();
        for m in def.milestones.iter().filter(|m| m.count == count) {
            let Some(table) = Self::load_table(&mut tx, &m.rewards).await? else {
                return Err(Error::NotFound(format!("reward table {}", m.rewards)));
            };
            Self::apply_rewards(
                &mut tx,
                account,
                &table.entries,
                "streak.milestone",
                &format!("{streak_id}:{count}"),
            )
            .await?;
            rewards.extend(table.entries);
        }
        sqlx::query("update musubi.streak_state set count = $3, last_claim = $4 where account_id = $1 and streak_id = $2")
            .bind(account.0)
            .bind(streak_id)
            .bind(count)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(StreakClaim {
            streak_id: streak_id.into(),
            count,
            rewards,
            next_claim_at: now + Duration::seconds(def.period_seconds),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def() -> EnergyDef {
        EnergyDef {
            id: "e".into(),
            name: String::new(),
            max: 10,
            regen_seconds: 60,
            initial: 10,
        }
    }

    #[test]
    fn energy_regenerates_lazily_and_keeps_remainder() {
        let t0 = OffsetDateTime::UNIX_EPOCH;
        let (cur, at) = regenerate(3, t0, &def(), t0 + Duration::seconds(150));
        assert_eq!(cur, 5);
        assert_eq!(
            at,
            t0 + Duration::seconds(120),
            "30 s of progress toward the next unit is kept"
        );
        let (cur, at) = regenerate(3, t0, &def(), t0 + Duration::seconds(3600));
        assert_eq!(cur, 10);
        assert_eq!(
            at,
            t0 + Duration::seconds(3600),
            "full: clock restarts on the next spend"
        );
        assert_eq!(regenerate(10, t0, &def(), t0 + Duration::seconds(5)).0, 10);
    }

    #[test]
    fn streak_period_grace_and_reset() {
        let d = StreakDef::default();
        let t0 = OffsetDateTime::UNIX_EPOCH;
        assert_eq!(next_streak(0, None, &d, t0).unwrap(), 1);
        assert!(matches!(
            next_streak(1, Some(t0), &d, t0 + Duration::hours(23)),
            Err(Error::Conflict(_))
        ));
        assert_eq!(
            next_streak(1, Some(t0), &d, t0 + Duration::hours(30)).unwrap(),
            2
        );
        assert_eq!(
            next_streak(5, Some(t0), &d, t0 + Duration::hours(47)).unwrap(),
            6,
            "inside grace"
        );
        assert_eq!(
            next_streak(5, Some(t0), &d, t0 + Duration::hours(49)).unwrap(),
            1,
            "grace exceeded → reset"
        );
    }

    #[test]
    fn levels_from_cumulative_xp() {
        let t = Track {
            id: "t".into(),
            name: String::new(),
            levels: vec![
                Level {
                    xp: 100,
                    rewards: None,
                },
                Level {
                    xp: 250,
                    rewards: None,
                },
            ],
        };
        assert_eq!(progress(&t, 0).level, 0);
        assert_eq!(progress(&t, 0).next_level_xp, Some(100));
        assert_eq!(progress(&t, 120).level, 1);
        assert_eq!(progress(&t, 250).level, 2);
        assert_eq!(progress(&t, 999).next_level_xp, None);
    }
}
