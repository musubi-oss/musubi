//! Quest pools: a rotation that hands every player a random, weighted pick of goals per period
//! (daily / weekly / custom length, reset hour, optionally in the player's local time). The pick
//! is deterministic per account × pool × period × generation, so every node agrees without a
//! coordination step; `accelerate` bumps the generation (everyone gets a fresh set now) and
//! `retire` closes the pool to new periods.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::quests::{Objective, QuestEventResult};
use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct PoolGoal {
    pub id: String,
    pub name: String,
    pub description: String,
    pub objectives: Vec<Objective>,
    pub rewards: Option<String>,
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QuestPool {
    pub id: String,
    pub name: String,
    pub description: String,
    /// `daily` | `weekly` | `custom` (`period_seconds`).
    pub kind: String,
    pub period_seconds: i64,
    /// Hour of day (0..23) at which the period rolls over.
    pub reset_hour: i32,
    /// Roll over at `reset_hour` in the player's own timezone instead of UTC.
    pub local_time: bool,
    /// Goals handed out per period.
    pub pick: i32,
    pub goals: Vec<PoolGoal>,
    pub rerolls_per_period: i32,
    pub generation: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub starts_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    pub enabled: bool,
    pub metadata: Value,
}

impl Default for QuestPool {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            kind: "daily".into(),
            period_seconds: 86_400,
            reset_hour: 0,
            local_time: false,
            pick: 3,
            goals: Vec::new(),
            rerolls_per_period: 1,
            generation: 0,
            starts_at: None,
            ends_at: None,
            enabled: true,
            metadata: json!({}),
        }
    }
}

impl QuestPool {
    pub fn period(&self) -> i64 {
        match self.kind.as_str() {
            "daily" => 86_400,
            "weekly" => 604_800,
            _ => self.period_seconds.max(60),
        }
    }

    /// Start of the period containing `now`, honouring the reset hour and (when `local_time`)
    /// the player's UTC offset in minutes.
    pub fn period_start(&self, now: OffsetDateTime, tz_offset_min: i64) -> OffsetDateTime {
        let p = self.period();
        let shift = self.reset_hour as i64 * 3600
            - if self.local_time {
                tz_offset_min * 60
            } else {
                0
            };
        let t = now.unix_timestamp() - shift;
        OffsetDateTime::from_unix_timestamp(t.div_euclid(p) * p + shift)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    pub fn period_key(&self, start: OffsetDateTime) -> String {
        format!("{}:{}", self.generation, start.unix_timestamp())
    }

    fn live(&self, now: OffsetDateTime) -> bool {
        self.enabled
            && self.starts_at.is_none_or(|t| t <= now)
            && self.ends_at.is_none_or(|t| t > now)
    }

    fn goal(&self, id: &str) -> Option<&PoolGoal> {
        self.goals.iter().find(|g| g.id == id)
    }
}

/// splitmix64 over a sha256 seed — stable across platforms and releases.
pub struct Rng(u64);

impl Rng {
    fn seeded(parts: &[&str]) -> Self {
        let mut h = Sha256::new();
        for p in parts {
            h.update(p.as_bytes());
            h.update([0]);
        }
        let d = h.finalize();
        Self(u64::from_le_bytes(d[..8].try_into().unwrap()))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

/// Weighted sample without replacement of `pick` goal ids from `goals` minus `exclude`.
pub fn sample<'a>(
    goals: &'a [PoolGoal],
    pick: usize,
    exclude: &[String],
    rng: &mut Rng,
) -> Vec<&'a PoolGoal> {
    let mut pool: Vec<&PoolGoal> = goals.iter().filter(|g| !exclude.contains(&g.id)).collect();
    let mut out = Vec::with_capacity(pick);
    while out.len() < pick && !pool.is_empty() {
        let total: u64 = pool.iter().map(|g| g.weight.max(1) as u64).sum();
        let mut r = rng.below(total);
        let mut idx = pool.len() - 1;
        for (i, g) in pool.iter().enumerate() {
            let w = g.weight.max(1) as u64;
            if r < w {
                idx = i;
                break;
            }
            r -= w;
        }
        out.push(pool.swap_remove(idx));
    }
    out
}

fn goal_complete(g: &PoolGoal, progress: &Value) -> bool {
    g.objectives
        .iter()
        .all(|o| progress[&o.event].as_i64().unwrap_or(0) >= o.target)
}

type PoolRow = (
    String,
    String,
    String,
    String,
    i64,
    i32,
    bool,
    i32,
    Value,
    i32,
    i32,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    bool,
    Value,
);
type SlotRow = (
    i32,
    String,
    Value,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    i32,
);

const COLS: &str = "id, name, description, kind, period_seconds, reset_hour, local_time, pick, goals, rerolls_per_period, generation, starts_at, ends_at, enabled, metadata";

fn pool(r: PoolRow) -> QuestPool {
    QuestPool {
        id: r.0,
        name: r.1,
        description: r.2,
        kind: r.3,
        period_seconds: r.4,
        reset_hour: r.5,
        local_time: r.6,
        pick: r.7,
        goals: serde_json::from_value(r.8).unwrap_or_default(),
        rerolls_per_period: r.9,
        generation: r.10,
        starts_at: r.11,
        ends_at: r.12,
        enabled: r.13,
        metadata: r.14,
    }
}

fn rfc(t: OffsetDateTime) -> Value {
    t.format(&time::format_description::well_known::Rfc3339)
        .map(Value::String)
        .unwrap_or(Value::Null)
}

impl EconomyService {
    pub async fn upsert_quest_pool(&self, mut p: QuestPool) -> Result<QuestPool> {
        crate::check_code(&p.id)?;
        if !matches!(p.kind.as_str(), "daily" | "weekly" | "custom") {
            return Err(Error::InvalidArgument("kind: daily|weekly|custom".into()));
        }
        if p.kind == "custom" && p.period_seconds < 60 {
            return Err(Error::InvalidArgument("period_seconds: >= 60".into()));
        }
        if !(0..24).contains(&p.reset_hour) {
            return Err(Error::InvalidArgument("reset_hour: 0..23".into()));
        }
        if p.goals.is_empty() || p.pick <= 0 || p.pick as usize > p.goals.len() {
            return Err(Error::InvalidArgument(
                "pick: 1..=goals.len(), goals non-empty".into(),
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for g in &p.goals {
            crate::check_code(&g.id)?;
            if !seen.insert(g.id.as_str()) {
                return Err(Error::InvalidArgument(format!("duplicate goal {}", g.id)));
            }
            if g.objectives.is_empty()
                || g.objectives
                    .iter()
                    .any(|o| o.event.is_empty() || o.target <= 0)
            {
                return Err(Error::InvalidArgument(format!(
                    "goal {}: objectives non-empty, event + positive target",
                    g.id
                )));
            }
        }
        if p.starts_at.zip(p.ends_at).is_some_and(|(a, b)| b <= a) {
            return Err(Error::InvalidArgument(
                "ends_at must be after starts_at".into(),
            ));
        }
        p.rerolls_per_period = p.rerolls_per_period.max(0);
        let row: PoolRow = sqlx::query_as(&format!(
            "insert into musubi.quest_pools (id, name, description, kind, period_seconds, reset_hour, local_time, pick, goals,
                                             rerolls_per_period, starts_at, ends_at, enabled, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             on conflict (id) do update set name = excluded.name, description = excluded.description, kind = excluded.kind,
                 period_seconds = excluded.period_seconds, reset_hour = excluded.reset_hour, local_time = excluded.local_time,
                 pick = excluded.pick, goals = excluded.goals, rerolls_per_period = excluded.rerolls_per_period,
                 starts_at = excluded.starts_at, ends_at = excluded.ends_at, enabled = excluded.enabled,
                 metadata = excluded.metadata, updated_at = now()
             returning {COLS}"
        ))
        .bind(&p.id)
        .bind(&p.name)
        .bind(&p.description)
        .bind(&p.kind)
        .bind(p.period())
        .bind(p.reset_hour)
        .bind(p.local_time)
        .bind(p.pick)
        .bind(serde_json::to_value(&p.goals)?)
        .bind(p.rerolls_per_period)
        .bind(p.starts_at)
        .bind(p.ends_at)
        .bind(p.enabled)
        .bind(&p.metadata)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(pool(row))
    }

    pub async fn quest_pools(&self) -> Result<Vec<QuestPool>> {
        let rows: Vec<PoolRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.quest_pools order by id"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(pool).collect())
    }

    pub async fn quest_pool(&self, id: &str) -> Result<QuestPool> {
        let row: Option<PoolRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.quest_pools where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(pool)
            .ok_or_else(|| Error::NotFound(format!("quest pool {id}")))
    }

    /// `accelerate`: everyone gets a fresh pick immediately (new generation); `retire`: the pool
    /// stops handing out goals from now on (current assignments stay claimable until they reset).
    pub async fn control_quest_pool(
        &self,
        id: &str,
        action: &str,
        now: OffsetDateTime,
    ) -> Result<QuestPool> {
        let sql = match action {
            "accelerate" => "update musubi.quest_pools set generation = generation + 1, updated_at = now() where id = $1",
            "retire" => "update musubi.quest_pools set ends_at = $2, updated_at = now() where id = $1",
            _ => return Err(Error::InvalidArgument("action: accelerate|retire".into())),
        };
        let n = sqlx::query(sql)
            .bind(id)
            .bind(now)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("quest pool {id}")));
        }
        self.quest_pool(id).await
    }

    pub async fn delete_quest_pool(&self, id: &str) -> Result<()> {
        let n = sqlx::query("delete from musubi.quest_pools where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("quest pool {id}")));
        }
        Ok(())
    }

    /// Current period's assignments, created on first sight.
    async fn ensure_assignments(
        &self,
        account: AccountId,
        p: &QuestPool,
        key: &str,
    ) -> Result<Vec<SlotRow>> {
        let rows: Vec<SlotRow> = sqlx::query_as(
            "select slot, goal_id, progress, completed_at, claimed_at, rerolled from musubi.quest_assignments
             where account_id = $1 and pool_id = $2 and period_key = $3 order by slot",
        )
        .bind(account.0)
        .bind(&p.id)
        .bind(key)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        if !rows.is_empty() {
            return Ok(rows);
        }
        let mut rng = Rng::seeded(&[&account.0.to_string(), &p.id, key]);
        let picked = sample(&p.goals, p.pick as usize, &[], &mut rng);
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        for (slot, g) in picked.iter().enumerate() {
            sqlx::query(
                "insert into musubi.quest_assignments (account_id, pool_id, period_key, slot, goal_id)
                 values ($1, $2, $3, $4, $5) on conflict do nothing",
            )
            .bind(account.0)
            .bind(&p.id)
            .bind(key)
            .bind(slot as i32)
            .bind(&g.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)?;
        let rows: Vec<SlotRow> = sqlx::query_as(
            "select slot, goal_id, progress, completed_at, claimed_at, rerolled from musubi.quest_assignments
             where account_id = $1 and pool_id = $2 and period_key = $3 order by slot",
        )
        .bind(account.0)
        .bind(&p.id)
        .bind(key)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows)
    }

    fn view(p: &QuestPool, start: OffsetDateTime, key: &str, rows: &[SlotRow]) -> Value {
        let used: i32 = rows.iter().map(|r| r.5).sum();
        let goals: Vec<Value> = rows
            .iter()
            .map(|r| {
                let g = p.goal(&r.1).cloned().unwrap_or_default();
                json!({
                    "slot": r.0, "goal_id": r.1, "name": g.name, "description": g.description,
                    "objectives": g.objectives, "rewards": g.rewards, "progress": r.2,
                    "completed": r.3.is_some(), "claimed": r.4.is_some(), "rerolled": r.5 > 0,
                })
            })
            .collect();
        json!({
            "pool_id": p.id, "name": p.name, "description": p.description, "kind": p.kind,
            "period_key": key, "period_start": rfc(start),
            "resets_at": rfc(start + Duration::seconds(p.period())),
            "rerolls_left": (p.rerolls_per_period - used).max(0),
            "goals": goals, "metadata": p.metadata,
        })
    }

    /// Every live pool with this player's goals for the current period.
    pub async fn quest_pool_views(
        &self,
        account: AccountId,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for p in self.quest_pools().await? {
            if !p.live(now) {
                continue;
            }
            let start = p.period_start(now, tz_offset_min);
            let key = p.period_key(start);
            let rows = self.ensure_assignments(account, &p, &key).await?;
            out.push(Self::view(&p, start, &key, &rows));
        }
        Ok(out)
    }

    /// Swap one slot for a goal the player does not currently hold (progress resets).
    pub async fn reroll_pool_slot(
        &self,
        account: AccountId,
        pool_id: &str,
        slot: i32,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<Value> {
        let p = self.quest_pool(pool_id).await?;
        if !p.live(now) {
            return Err(Error::NotFound(format!("quest pool {pool_id}")));
        }
        let start = p.period_start(now, tz_offset_min);
        let key = p.period_key(start);
        let rows = self.ensure_assignments(account, &p, &key).await?;
        let Some(cur) = rows.iter().find(|r| r.0 == slot) else {
            return Err(Error::NotFound(format!("slot {slot}")));
        };
        if cur.3.is_some() {
            return Err(Error::Conflict("goal already completed".into()));
        }
        let used: i32 = rows.iter().map(|r| r.5).sum();
        if used >= p.rerolls_per_period {
            return Err(Error::ResourceExhausted(format!(
                "no rerolls left this period ({} per period)",
                p.rerolls_per_period
            )));
        }
        let held: Vec<String> = rows.iter().map(|r| r.1.clone()).collect();
        let mut rng = Rng::seeded(&[
            &account.0.to_string(),
            &p.id,
            &key,
            "reroll",
            &used.to_string(),
        ]);
        let Some(next) = sample(&p.goals, 1, &held, &mut rng).first().cloned() else {
            return Err(Error::ResourceExhausted(
                "no other goals left in the pool".into(),
            ));
        };
        sqlx::query(
            "update musubi.quest_assignments set goal_id = $5, progress = '{}'::jsonb, rerolled = rerolled + 1, assigned_at = now()
             where account_id = $1 and pool_id = $2 and period_key = $3 and slot = $4",
        )
        .bind(account.0)
        .bind(&p.id)
        .bind(&key)
        .bind(slot)
        .bind(&next.id)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let rows = self.ensure_assignments(account, &p, &key).await?;
        Ok(Self::view(&p, start, &key, &rows))
    }

    /// Progress every live pool goal of the player that counts `event`.
    pub async fn quest_pool_event(
        &self,
        account: AccountId,
        event: &str,
        delta: i64,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<QuestEventResult> {
        let mut res = QuestEventResult {
            updated: Vec::new(),
            completed: Vec::new(),
        };
        if event.is_empty() || delta <= 0 {
            return Ok(res);
        }
        for p in self.quest_pools().await? {
            if !p.live(now)
                || !p
                    .goals
                    .iter()
                    .any(|g| g.objectives.iter().any(|o| o.event == event))
            {
                continue;
            }
            let start = p.period_start(now, tz_offset_min);
            let key = p.period_key(start);
            let rows = self.ensure_assignments(account, &p, &key).await?;
            for r in rows {
                let Some(g) = p.goal(&r.1) else { continue };
                let Some(obj) = g.objectives.iter().find(|o| o.event == event) else {
                    continue;
                };
                if r.3.is_some() {
                    continue;
                }
                let mut progress = r.2.clone();
                let cur = progress[event].as_i64().unwrap_or(0);
                progress[event] = json!((cur + delta).min(obj.target));
                let done = goal_complete(g, &progress);
                sqlx::query(
                    "update musubi.quest_assignments set progress = $5, completed_at = $6
                     where account_id = $1 and pool_id = $2 and period_key = $3 and slot = $4 and completed_at is null",
                )
                .bind(account.0)
                .bind(&p.id)
                .bind(&key)
                .bind(r.0)
                .bind(&progress)
                .bind(done.then_some(now))
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
                let id = format!("{}/{}", p.id, r.0);
                res.updated.push(id.clone());
                if done {
                    res.completed.push(id);
                }
            }
        }
        Ok(res)
    }

    pub async fn claim_pool_slot(
        &self,
        account: AccountId,
        pool_id: &str,
        slot: i32,
        now: OffsetDateTime,
        tz_offset_min: i64,
    ) -> Result<(String, Vec<RewardEntry>)> {
        let p = self.quest_pool(pool_id).await?;
        let start = p.period_start(now, tz_offset_min);
        let key = p.period_key(start);
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<SlotRow> = sqlx::query_as(
            "select slot, goal_id, progress, completed_at, claimed_at, rerolled from musubi.quest_assignments
             where account_id = $1 and pool_id = $2 and period_key = $3 and slot = $4 for update",
        )
        .bind(account.0)
        .bind(&p.id)
        .bind(&key)
        .bind(slot)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(r) = row else {
            return Err(Error::NotFound(format!("slot {slot}")));
        };
        if r.4.is_some() {
            return Err(Error::Conflict("goal already claimed".into()));
        }
        if r.3.is_none() {
            return Err(Error::Conflict("goal not completed".into()));
        }
        let mut rewards = Vec::new();
        if let Some(t) = p.goal(&r.1).and_then(|g| g.rewards.clone()) {
            let Some(table) = Self::load_table(&mut tx, &t).await? else {
                return Err(Error::NotFound(format!("reward table {t}")));
            };
            Self::apply_rewards(
                &mut tx,
                account,
                &table.entries,
                "quest_pool.claim",
                &format!("{pool_id}:{key}:{slot}"),
            )
            .await?;
            rewards = table.entries;
        }
        sqlx::query(
            "update musubi.quest_assignments set claimed_at = $5
             where account_id = $1 and pool_id = $2 and period_key = $3 and slot = $4",
        )
        .bind(account.0)
        .bind(&p.id)
        .bind(&key)
        .bind(slot)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((r.1, rewards))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn goals(n: usize) -> Vec<PoolGoal> {
        (0..n)
            .map(|i| PoolGoal {
                id: format!("g{i}"),
                weight: if i == 0 { 100 } else { 1 },
                ..Default::default()
            })
            .collect()
    }

    #[test]
    fn sampling_is_deterministic_weighted_and_without_replacement() {
        let g = goals(6);
        let a = sample(&g, 3, &[], &mut Rng::seeded(&["acc", "pool", "0:1"]));
        let b = sample(&g, 3, &[], &mut Rng::seeded(&["acc", "pool", "0:1"]));
        assert_eq!(
            a.iter().map(|x| &x.id).collect::<Vec<_>>(),
            b.iter().map(|x| &x.id).collect::<Vec<_>>()
        );
        let ids: std::collections::HashSet<_> = a.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids.len(), 3, "no duplicates");
        let heavy_first = (0..200)
            .filter(|i| sample(&g, 1, &[], &mut Rng::seeded(&[&i.to_string()]))[0].id == "g0")
            .count();
        assert!(
            heavy_first > 150,
            "weight 100 vs 5×1 → g0 first ~95%: {heavy_first}"
        );
        let ex = sample(&g, 6, &["g0".into()], &mut Rng::seeded(&["x"]));
        assert_eq!(ex.len(), 5);
        assert!(ex.iter().all(|x| x.id != "g0"));
    }

    #[test]
    fn period_start_honours_reset_hour_and_local_time() {
        let mut p = QuestPool {
            reset_hour: 5,
            ..Default::default()
        };
        let day = 86_400;
        // 03:00 UTC belongs to the period that started yesterday 05:00
        let now = OffsetDateTime::from_unix_timestamp(10 * day + 3 * 3600).unwrap();
        assert_eq!(p.period_start(now, 0).unix_timestamp(), 9 * day + 5 * 3600);
        let now = OffsetDateTime::from_unix_timestamp(10 * day + 6 * 3600).unwrap();
        assert_eq!(p.period_start(now, 0).unix_timestamp(), 10 * day + 5 * 3600);
        // local time UTC+3: 03:00 UTC is 06:00 local → today's 05:00 local = 02:00 UTC
        p.local_time = true;
        let now = OffsetDateTime::from_unix_timestamp(10 * day + 3 * 3600).unwrap();
        assert_eq!(
            p.period_start(now, 180).unix_timestamp(),
            10 * day + 2 * 3600
        );
        p.kind = "weekly".into();
        assert_eq!(p.period(), 604_800);
        p.kind = "custom".into();
        p.period_seconds = 30;
        assert_eq!(p.period(), 60, "custom periods are clamped to a minute");
    }
}
