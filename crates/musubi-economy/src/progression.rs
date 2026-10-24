//! Progression: XP tracks with level rewards, energy with lazy time regeneration, daily streaks
//! with a grace period. All time-dependent logic takes an explicit `now` so it is unit-testable.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

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
    /// Product of the active `xp` / `xp:<track>` modifiers applied to `delta`.
    pub xp_factor: f64,
    /// XP actually added after modifiers.
    pub delta_applied: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EnergyDef {
    pub id: String,
    pub name: String,
    pub max: i32,
    pub regen_seconds: i64,
    pub initial: i32,
    /// Units restored per `regen_seconds` tick (default 1).
    pub refill_count: i32,
    /// Grants may push the amount up to this cap (0 = `max`); regeneration never does.
    pub max_overfill: i32,
    /// Reward table granted on every spend (e.g. XP per stamina used).
    pub reward_on_spend: Option<String>,
}

impl Default for EnergyDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            max: 0,
            regen_seconds: 0,
            initial: -1,
            refill_count: 1,
            max_overfill: 0,
            reward_on_spend: None,
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
    pub refill_count: i32,
    pub max_overfill: i32,
    /// An `infinite` modifier is active: spends cost nothing.
    pub infinite: bool,
    pub modifiers: Vec<EnergyModifier>,
}

pub const ENERGY_MODIFIERS: &[&str] = &[
    "infinite",
    "set_refill_count",
    "set_refill_sec",
    "set_max_count",
];

/// A timed per-account override of an energy definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnergyModifier {
    pub id: Uuid,
    pub energy_id: String,
    pub kind: String,
    pub value: i64,
    pub expires_at: Option<i64>,
}

/// Applies modifiers to a definition; returns whether spends are free (`infinite`).
pub fn apply_modifiers(def: &mut EnergyDef, mods: &[EnergyModifier]) -> bool {
    let mut infinite = false;
    for m in mods {
        match m.kind.as_str() {
            "infinite" => infinite = true,
            "set_refill_count" => def.refill_count = m.value.clamp(1, i32::MAX as i64) as i32,
            "set_refill_sec" => def.regen_seconds = m.value.max(1),
            "set_max_count" => def.max = m.value.clamp(1, i32::MAX as i64) as i32,
            _ => {}
        }
    }
    infinite
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
    /// Periods are calendar buckets (`period_seconds` long, rolling over at `reset_hour`)
    /// instead of running from the last claim.
    pub calendar: bool,
    pub reset_hour: i32,
    /// Buckets roll over at `reset_hour` in the player's own timezone.
    pub local_time: bool,
    /// Cap on the count (0 = unbounded); further claims keep the count.
    pub max_count: i32,
    /// Lose this many per missed period instead of resetting to zero (0 = reset).
    pub decay: i32,
    /// Grant `repeat_rewards` every N-th count on top of milestones (0 = off).
    pub repeat_every: i32,
    pub repeat_rewards: Option<String>,
    pub enabled: bool,
}

impl Default for StreakDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            period_seconds: 86_400,
            grace_seconds: 86_400,
            milestones: Vec::new(),
            calendar: false,
            reset_hour: 0,
            local_time: false,
            max_count: 0,
            decay: 0,
            repeat_every: 0,
            repeat_rewards: None,
            enabled: true,
        }
    }
}

impl StreakDef {
    fn shift(&self, tz_offset_min: i64) -> i64 {
        self.reset_hour as i64 * 3600
            - if self.local_time {
                tz_offset_min * 60
            } else {
                0
            }
    }

    /// Calendar bucket index of `t`.
    pub fn bucket(&self, t: OffsetDateTime, tz_offset_min: i64) -> i64 {
        (t.unix_timestamp() - self.shift(tz_offset_min)).div_euclid(self.period_seconds)
    }

    pub fn bucket_start(&self, b: i64, tz_offset_min: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(
            b.saturating_mul(self.period_seconds)
                .saturating_add(self.shift(tz_offset_min)),
        )
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    fn grace_periods(&self) -> i64 {
        self.grace_seconds / self.period_seconds.max(1)
    }

    /// When the streak can next be claimed / when it breaks, given the last claim.
    pub fn window(
        &self,
        last: OffsetDateTime,
        tz_offset_min: i64,
    ) -> (OffsetDateTime, OffsetDateTime) {
        if self.calendar {
            let b = self.bucket(last, tz_offset_min);
            (
                self.bucket_start(b + 1, tz_offset_min),
                self.bucket_start(
                    b.saturating_add(2).saturating_add(self.grace_periods()),
                    tz_offset_min,
                ),
            )
        } else {
            (
                last + Duration::seconds(self.period_seconds),
                last + Duration::seconds(self.period_seconds + self.grace_seconds),
            )
        }
    }
}

fn rfc(t: OffsetDateTime) -> Value {
    t.format(&time::format_description::well_known::Rfc3339)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Advance {
    pub count: i32,
    pub missed: i32,
    pub reset: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreakClaim {
    pub streak_id: String,
    pub count: i32,
    pub rewards: Vec<RewardEntry>,
    #[serde(with = "time::serde::rfc3339")]
    pub next_claim_at: OffsetDateTime,
    pub missed: i32,
    pub reset: bool,
}

pub(crate) fn level_for(levels: &[Level], xp: i64) -> i32 {
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
    let ticks = elapsed / def.regen_seconds;
    let per_tick = def.refill_count.max(1) as i64;
    let units = ticks
        .saturating_mul(per_tick)
        .min((def.max - amount) as i64);
    let cur = amount + units as i32;
    if cur >= def.max {
        (cur, now)
    } else {
        (
            cur,
            updated_at + Duration::seconds(ticks * def.regen_seconds),
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
        refill_count: def.refill_count,
        max_overfill: def.max_overfill.max(def.max),
        infinite: false,
        modifiers: Vec::new(),
    }
}

/// Next streak count for a claim at `now`: Conflict inside the current period; beyond the grace
/// the count decays (or resets) by the missed periods, then grows by one up to `max_count`.
pub fn advance(
    def: &StreakDef,
    count: i32,
    last_claim: Option<OffsetDateTime>,
    now: OffsetDateTime,
    tz_offset_min: i64,
) -> Result<Advance> {
    let cap = |c: i32| {
        if def.max_count > 0 {
            c.min(def.max_count)
        } else {
            c
        }
    };
    let Some(last) = last_claim else {
        return Ok(Advance {
            count: cap(count.max(0) + 1),
            missed: 0,
            reset: false,
        });
    };
    let missed = if def.calendar {
        let cur = def.bucket(now, tz_offset_min);
        let prev = def.bucket(last, tz_offset_min);
        if cur <= prev {
            return Err(Error::Conflict(
                "streak already claimed for this period".into(),
            ));
        }
        let gap = cur - prev - 1;
        if gap <= def.grace_periods() {
            0
        } else {
            gap
        }
    } else {
        let elapsed = (now - last).whole_seconds();
        if elapsed < def.period_seconds {
            return Err(Error::Conflict(
                "streak already claimed for this period".into(),
            ));
        }
        if elapsed <= def.period_seconds + def.grace_seconds {
            0
        } else {
            let beyond = elapsed - def.period_seconds - def.grace_seconds;
            (beyond + def.period_seconds - 1) / def.period_seconds.max(1)
        }
    };
    let missed = missed.min(i32::MAX as i64) as i32;
    if missed == 0 {
        return Ok(Advance {
            count: cap(count + 1),
            missed: 0,
            reset: false,
        });
    }
    let base = if def.decay > 0 {
        (count - def.decay.saturating_mul(missed)).max(0)
    } else {
        0
    };
    Ok(Advance {
        count: cap(base + 1),
        missed,
        reset: true,
    })
}

type TrackRow = (String, String, Value);
type EnergyRow = (String, String, i32, i64, i32, i32, i32, Option<String>);
type StreakRow = (
    String,
    String,
    i64,
    i64,
    Value,
    bool,
    i32,
    bool,
    i32,
    i32,
    i32,
    Option<String>,
    bool,
);
type EnergyStateRow = (
    String,
    String,
    i32,
    i64,
    i32,
    i32,
    i32,
    Option<String>,
    Option<i32>,
    Option<OffsetDateTime>,
);

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
        refill_count: r.5,
        max_overfill: r.6,
        reward_on_spend: r.7,
    }
}
fn streak(r: StreakRow) -> StreakDef {
    StreakDef {
        id: r.0,
        name: r.1,
        period_seconds: r.2,
        grace_seconds: r.3,
        milestones: serde_json::from_value(r.4).unwrap_or_default(),
        calendar: r.5,
        reset_hour: r.6,
        local_time: r.7,
        max_count: r.8,
        decay: r.9,
        repeat_every: r.10,
        repeat_rewards: r.11,
        enabled: r.12,
    }
}

const STREAK_COLS: &str = "id, name, period_seconds, grace_seconds, milestones, calendar, reset_hour, local_time, max_count, decay, repeat_every, repeat_rewards, enabled";

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
        let xp_factor = Self::xp_factor(&mut tx, account, track_id).await?;
        let delta_applied = crate::modifiers::scale(delta, xp_factor);
        let previous_level = level_for(&t.levels, xp);
        let new_xp = xp.saturating_add(delta_applied);
        let new_level = level_for(&t.levels, new_xp);
        let mut rewards = Vec::new();
        let level_ups: Vec<i32> = (previous_level + 1..=new_level).collect();
        for lvl in &level_ups {
            if let Some(table_id) = t.levels[(*lvl - 1) as usize].rewards.as_deref() {
                let Some(table) = Self::load_table(&mut tx, table_id).await? else {
                    return Err(Error::NotFound(format!("reward table {table_id}")));
                };
                let applied = Self::apply_rewards_for(
                    &mut tx,
                    account,
                    Some(table_id),
                    &table.entries,
                    "progression.level",
                    &format!("{track_id}:{lvl}:{reason}"),
                )
                .await?;
                rewards.extend(applied);
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
            xp_factor,
            delta_applied,
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
            "insert into musubi.energies (id, name, max, regen_seconds, initial, refill_count, max_overfill, reward_on_spend)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (id) do update set name = excluded.name, max = excluded.max, regen_seconds = excluded.regen_seconds,
                initial = excluded.initial, refill_count = excluded.refill_count, max_overfill = excluded.max_overfill,
                reward_on_spend = excluded.reward_on_spend",
        )
        .bind(&d.id)
        .bind(&d.name)
        .bind(d.max)
        .bind(d.regen_seconds)
        .bind(d.initial.min(d.max))
        .bind(d.refill_count.max(1))
        .bind(d.max_overfill.max(0))
        .bind(&d.reward_on_spend)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(d)
    }

    pub async fn energies(&self) -> Result<Vec<EnergyDef>> {
        let rows: Vec<EnergyRow> = sqlx::query_as(
            "select id, name, max, regen_seconds, initial, refill_count, max_overfill, reward_on_spend from musubi.energies order by id",
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
            "select e.id, e.name, e.max, e.regen_seconds, e.initial, e.refill_count, e.max_overfill, e.reward_on_spend, s.amount, s.updated_at from musubi.energies e
             left join musubi.energy_state s on s.energy_id = e.id and s.account_id = $1 order by e.id",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mods = self.energy_modifiers(account, now).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let mut def = energy((r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7));
                let mine: Vec<EnergyModifier> = mods
                    .iter()
                    .filter(|m| m.energy_id == def.id)
                    .cloned()
                    .collect();
                let infinite = apply_modifiers(&mut def, &mine);
                let (amount, at) = match (r.8, r.9) {
                    (Some(a), Some(at)) => regenerate(a, at, &def, now),
                    _ => (def.initial.min(def.max), now),
                };
                let mut st = energy_state(&def, if infinite { def.max } else { amount }, at, now);
                st.infinite = infinite;
                st.modifiers = mine;
                st
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
        self.adjust_energy_at(account, energy_id, delta, now, None)
            .await
    }

    /// Spend (`delta < 0`) or grant energy. `at` (≤ now) books an offline spend at the moment it
    /// happened: regeneration runs to `at`, the spend applies, then regeneration continues to now.
    /// Modifiers apply (`infinite` makes spends free), grants may overfill up to `max_overfill`,
    /// and `reward_on_spend` is granted per spend.
    pub async fn adjust_energy_at(
        &self,
        account: AccountId,
        energy_id: &str,
        delta: i32,
        now: OffsetDateTime,
        at: Option<OffsetDateTime>,
    ) -> Result<EnergyState> {
        let mods = self.energy_modifiers(account, now).await?;
        let mine: Vec<EnergyModifier> = mods
            .into_iter()
            .filter(|m| m.energy_id == energy_id)
            .collect();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<EnergyRow> = sqlx::query_as(
            "select id, name, max, regen_seconds, initial, refill_count, max_overfill, reward_on_spend from musubi.energies where id = $1",
        )
        .bind(energy_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(mut def) = row.map(energy) else {
            return Err(Error::NotFound(format!("energy {energy_id}")));
        };
        let infinite = apply_modifiers(&mut def, &mine);
        let spend_at = match at {
            Some(a) if a > now => return Err(Error::InvalidArgument("at is in the future".into())),
            Some(a) => a,
            None => now,
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
        let spend_at = spend_at.max(at);
        let (cur, at) = regenerate(amount, at, &def, spend_at);
        let (next, at) = if delta < 0 {
            if infinite {
                (cur, at)
            } else {
                if cur + delta < 0 {
                    return Err(Error::ResourceExhausted(format!(
                        "not enough {energy_id}: {cur} available"
                    )));
                }
                let at = if cur >= def.max { spend_at } else { at };
                regenerate(cur + delta, at, &def, now)
            }
        } else {
            let cap = def.max_overfill.max(def.max);
            (cur.saturating_add(delta).min(cap).max(cur), at)
        };
        let at = if next >= def.max { now } else { at };
        sqlx::query("update musubi.energy_state set amount = $3, updated_at = $4 where account_id = $1 and energy_id = $2")
            .bind(account.0)
            .bind(energy_id)
            .bind(next)
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        if delta < 0 && !infinite {
            if let Some(table) = &def.reward_on_spend {
                let reference = format!("energy:{energy_id}:{}:{}", account.0, Uuid::now_v7());
                // a missing or empty table must not fail the spend itself
                let _ = self.grant_table(account, table, &reference).await;
            }
        }
        let mut st = energy_state(&def, if infinite { def.max } else { next }, at, now);
        st.infinite = infinite;
        st.modifiers = mine;
        Ok(st)
    }

    /// Active modifiers of an account (expired ones are ignored and pruned lazily).
    pub async fn energy_modifiers(
        &self,
        account: AccountId,
        now: OffsetDateTime,
    ) -> Result<Vec<EnergyModifier>> {
        let rows: Vec<(Uuid, String, String, i64, Option<OffsetDateTime>)> = sqlx::query_as(
            "select id, energy_id, kind, value, expires_at from musubi.energy_modifiers
             where account_id = $1 and (expires_at is null or expires_at > $2) order by created_at",
        )
        .bind(account.0)
        .bind(now)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(id, energy_id, kind, value, expires_at)| EnergyModifier {
                id,
                energy_id,
                kind,
                value,
                expires_at: expires_at.map(|t| t.unix_timestamp()),
            })
            .collect())
    }

    pub async fn add_energy_modifier(
        &self,
        account: AccountId,
        energy_id: &str,
        kind: &str,
        value: i64,
        duration_secs: Option<i64>,
        now: OffsetDateTime,
    ) -> Result<EnergyModifier> {
        if !ENERGY_MODIFIERS.contains(&kind) {
            return Err(Error::InvalidArgument(format!(
                "kind must be one of {ENERGY_MODIFIERS:?}"
            )));
        }
        if kind != "infinite" && value <= 0 {
            return Err(Error::InvalidArgument("value must be positive".into()));
        }
        if duration_secs.is_some_and(|d| d <= 0) {
            return Err(Error::InvalidArgument(
                "duration_secs must be positive".into(),
            ));
        }
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from musubi.energies where id = $1)")
                .bind(energy_id)
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
        if !exists {
            return Err(Error::NotFound(format!("energy {energy_id}")));
        }
        let id = Uuid::now_v7();
        let expires_at = duration_secs.map(|d| now + Duration::seconds(d));
        sqlx::query(
            "insert into musubi.energy_modifiers (id, account_id, energy_id, kind, value, expires_at) values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(account.0)
        .bind(energy_id)
        .bind(kind)
        .bind(value)
        .bind(expires_at)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "delete from musubi.energy_modifiers where expires_at is not null and expires_at <= $1",
        )
        .bind(now)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(EnergyModifier {
            id,
            energy_id: energy_id.into(),
            kind: kind.into(),
            value,
            expires_at: expires_at.map(|t| t.unix_timestamp()),
        })
    }

    pub async fn remove_energy_modifier(&self, id: Uuid) -> Result<()> {
        let n = sqlx::query("delete from musubi.energy_modifiers where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("modifier {id}")));
        }
        Ok(())
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
        if !(0..24).contains(&d.reset_hour) || d.max_count < 0 || d.decay < 0 || d.repeat_every < 0
        {
            return Err(Error::InvalidArgument(
                "reset_hour 0..23; max_count, decay, repeat_every non-negative".into(),
            ));
        }
        if d.repeat_every > 0 && d.repeat_rewards.is_none() {
            return Err(Error::InvalidArgument(
                "repeat_every needs repeat_rewards".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.streaks (id, name, period_seconds, grace_seconds, milestones, calendar, reset_hour, local_time,
                                         max_count, decay, repeat_every, repeat_rewards, enabled)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
             on conflict (id) do update set name = excluded.name, period_seconds = excluded.period_seconds,
             grace_seconds = excluded.grace_seconds, milestones = excluded.milestones, calendar = excluded.calendar,
             reset_hour = excluded.reset_hour, local_time = excluded.local_time, max_count = excluded.max_count,
             decay = excluded.decay, repeat_every = excluded.repeat_every, repeat_rewards = excluded.repeat_rewards,
             enabled = excluded.enabled",
        )
        .bind(&d.id)
        .bind(&d.name)
        .bind(d.period_seconds)
        .bind(d.grace_seconds)
        .bind(serde_json::to_value(&d.milestones)?)
        .bind(d.calendar)
        .bind(d.reset_hour)
        .bind(d.local_time)
        .bind(d.max_count)
        .bind(d.decay)
        .bind(d.repeat_every)
        .bind(&d.repeat_rewards)
        .bind(d.enabled)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(d)
    }

    pub async fn streaks(&self) -> Result<Vec<StreakDef>> {
        let rows: Vec<StreakRow> = sqlx::query_as(&format!(
            "select {STREAK_COLS} from musubi.streaks order by id"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(streak).collect())
    }

    async fn streak_def(&self, id: &str) -> Result<StreakDef> {
        let row: Option<StreakRow> = sqlx::query_as(&format!(
            "select {STREAK_COLS} from musubi.streaks where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(streak)
            .ok_or_else(|| Error::NotFound(format!("streak {id}")))
    }

    fn streak_view(
        def: &StreakDef,
        st: Option<(i32, Option<OffsetDateTime>, i32, i32, i32)>,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Value {
        let (count, last, claims, best, missed) = st.unwrap_or((0, None, 0, 0, 0));
        let window = last.map(|l| def.window(l, tz_offset_min));
        let claimable_at = window.map(|w| w.0);
        let expires_at = window.map(|w| w.1);
        let claimable = claimable_at.is_none_or(|t| t <= now);
        let broken = expires_at.is_some_and(|t| t <= now);
        let in_grace = !broken
            && claimable_at.is_some_and(|c| {
                if def.calendar {
                    def.bucket(now, tz_offset_min) > def.bucket(c, tz_offset_min)
                } else {
                    now - c >= Duration::seconds(def.period_seconds)
                }
            });
        let next_milestone = def
            .milestones
            .iter()
            .filter(|m| m.count > count)
            .map(|m| m.count)
            .min();
        json!({
            "streak_id": def.id,
            "name": def.name,
            "count": count,
            "best": best,
            "claims": claims,
            "missed": missed,
            "last_claim": last.map(rfc).unwrap_or(Value::Null),
            "claimable": claimable,
            "claimable_at": claimable_at.map(rfc).unwrap_or(Value::Null),
            "expires_at": expires_at.map(rfc).unwrap_or(Value::Null),
            "in_grace": in_grace,
            "broken": broken,
            "max_count": def.max_count,
            "max_count_reached": def.max_count > 0 && count >= def.max_count,
            "next_milestone": next_milestone,
            "milestones": def.milestones,
            "repeat_every": def.repeat_every,
            "period_seconds": def.period_seconds,
            "calendar": def.calendar,
        })
    }

    pub async fn streak_states(
        &self,
        account: AccountId,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<Vec<Value>> {
        let defs = self.streaks().await?;
        let states: Vec<(String, i32, Option<OffsetDateTime>, i32, i32, i32)> = sqlx::query_as(
            "select streak_id, count, last_claim, claims, best, missed from musubi.streak_state where account_id = $1",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(defs
            .iter()
            .filter(|d| d.enabled)
            .map(|d| {
                let st = states
                    .iter()
                    .find(|s| s.0 == d.id)
                    .map(|s| (s.1, s.2, s.3, s.4, s.5));
                Self::streak_view(d, st, now, tz_offset_min)
            })
            .collect())
    }

    pub async fn streak_state(
        &self,
        account: AccountId,
        streak_id: &str,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<Value> {
        let def = self.streak_def(streak_id).await?;
        let st: Option<(i32, Option<OffsetDateTime>, i32, i32, i32)> = sqlx::query_as(
            "select count, last_claim, claims, best, missed from musubi.streak_state where account_id = $1 and streak_id = $2",
        )
        .bind(account.0)
        .bind(streak_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(Self::streak_view(&def, st, now, tz_offset_min))
    }

    /// Operator reset (support, abuse): sets the count (default 0) and makes the streak claimable now.
    pub async fn reset_streak(
        &self,
        account: AccountId,
        streak_id: &str,
        count: Option<i32>,
    ) -> Result<()> {
        let def = self.streak_def(streak_id).await?;
        let count = count.unwrap_or(0).max(0);
        if def.max_count > 0 && count > def.max_count {
            return Err(Error::InvalidArgument(format!(
                "count: <= max_count {}",
                def.max_count
            )));
        }
        sqlx::query(
            "insert into musubi.streak_state (account_id, streak_id, count, last_claim) values ($1, $2, $3, null)
             on conflict (account_id, streak_id) do update set count = excluded.count, last_claim = null",
        )
        .bind(account.0)
        .bind(streak_id)
        .bind(count)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn claim_streak(
        &self,
        account: AccountId,
        streak_id: &str,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<StreakClaim> {
        let def = self.streak_def(streak_id).await?;
        if !def.enabled {
            return Err(Error::NotFound(format!("streak {streak_id}")));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query("insert into musubi.streak_state (account_id, streak_id) values ($1, $2) on conflict do nothing")
            .bind(account.0)
            .bind(streak_id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let (count, last, best): (i32, Option<OffsetDateTime>, i32) =
            sqlx::query_as("select count, last_claim, best from musubi.streak_state where account_id = $1 and streak_id = $2 for update")
                .bind(account.0)
                .bind(streak_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let adv = advance(&def, count, last, now, tz_offset_min)?;
        let count = adv.count;
        let mut rewards = Vec::new();
        let mut tables: Vec<String> = def
            .milestones
            .iter()
            .filter(|m| m.count == count)
            .map(|m| m.rewards.clone())
            .collect();
        if def.repeat_every > 0 && count % def.repeat_every == 0 {
            if let Some(t) = &def.repeat_rewards {
                tables.push(t.clone());
            }
        }
        for t in tables {
            let Some(table) = Self::load_table(&mut tx, &t).await? else {
                return Err(Error::NotFound(format!("reward table {t}")));
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
        sqlx::query(
            "update musubi.streak_state set count = $3, last_claim = $4, claims = claims + 1, best = $5, missed = missed + $6
             where account_id = $1 and streak_id = $2",
        )
        .bind(account.0)
        .bind(streak_id)
        .bind(count)
        .bind(now)
        .bind(best.max(count))
        .bind(adv.missed)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(StreakClaim {
            streak_id: streak_id.into(),
            count,
            rewards,
            next_claim_at: def.window(now, tz_offset_min).0,
            missed: adv.missed,
            reset: adv.reset,
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
            refill_count: 1,
            max_overfill: 0,
            reward_on_spend: None,
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
        let n = |c, l, now| advance(&d, c, l, now, 0).map(|a| a.count);
        assert_eq!(n(0, None, t0).unwrap(), 1);
        assert!(matches!(
            n(1, Some(t0), t0 + Duration::hours(23)),
            Err(Error::Conflict(_))
        ));
        assert_eq!(n(1, Some(t0), t0 + Duration::hours(30)).unwrap(), 2);
        assert_eq!(
            n(5, Some(t0), t0 + Duration::hours(47)).unwrap(),
            6,
            "inside grace"
        );
        let a = advance(&d, 5, Some(t0), t0 + Duration::hours(49), 0).unwrap();
        assert_eq!(
            (a.count, a.missed, a.reset),
            (1, 1, true),
            "grace exceeded → reset"
        );
    }

    #[test]
    fn streak_decay_cap_and_calendar_buckets() {
        let d = StreakDef {
            decay: 2,
            max_count: 7,
            ..Default::default()
        };
        let t0 = OffsetDateTime::UNIX_EPOCH;
        // 3 days missed beyond the grace → lose 2 × 3, then +1
        let a = advance(&d, 10, Some(t0), t0 + Duration::days(5), 0).unwrap();
        assert_eq!((a.count, a.missed), (5, 3));
        assert_eq!(
            advance(&d, 7, Some(t0), t0 + Duration::days(1), 0)
                .unwrap()
                .count,
            7,
            "capped"
        );
        let c = StreakDef {
            calendar: true,
            reset_hour: 4,
            grace_seconds: 0,
            ..Default::default()
        };
        // claimed 03:00 (yesterday's bucket), again at 05:00 the same day → next bucket, no gap
        let d0 = OffsetDateTime::from_unix_timestamp(100 * 86_400).unwrap();
        let last = d0 + Duration::hours(3);
        assert!(advance(
            &c,
            1,
            Some(last),
            d0 + Duration::hours(3) + Duration::minutes(30),
            0
        )
        .is_err());
        assert_eq!(
            advance(&c, 1, Some(last), d0 + Duration::hours(5), 0)
                .unwrap()
                .count,
            2
        );
        let a = advance(
            &c,
            1,
            Some(last),
            d0 + Duration::days(2) + Duration::hours(5),
            0,
        )
        .unwrap();
        assert_eq!(
            (a.count, a.missed, a.reset),
            (1, 2, true),
            "two buckets skipped"
        );
        // local time UTC+3: 02:00 UTC = 05:00 local → already today's bucket
        let l = StreakDef {
            local_time: true,
            ..c.clone()
        };
        assert_eq!(
            l.bucket(d0 + Duration::hours(2), 180),
            l.bucket(d0 + Duration::hours(20), 180)
        );
        assert_ne!(
            l.bucket(d0 + Duration::hours(2), 0),
            l.bucket(d0 + Duration::hours(20), 0)
        );
        let (next, exp) = c.window(last, 0);
        assert_eq!(next, d0 + Duration::hours(4));
        assert_eq!(exp, d0 + Duration::days(1) + Duration::hours(4));
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

    #[test]
    fn energy_and_streak_math_saturates_instead_of_overflowing() {
        let t0 = OffsetDateTime::UNIX_EPOCH;
        let huge = EnergyDef {
            refill_count: i32::MAX,
            ..def()
        };
        assert_eq!(regenerate(0, t0, &huge, t0 + Duration::days(40_000)).0, 10);
        let d = StreakDef {
            calendar: true,
            grace_seconds: i64::MAX,
            ..Default::default()
        };
        let (next, _) = d.window(t0 + Duration::days(1), 0);
        assert_eq!(next, t0 + Duration::days(2));
        assert_eq!(
            d.bucket_start(i64::MAX, 0),
            OffsetDateTime::UNIX_EPOCH,
            "out-of-range bucket degrades instead of panicking"
        );
    }
}
