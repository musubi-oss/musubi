//! Leaderboards: definitions, score submission with operators, ranked listing and reset windows.

pub mod bracket;
pub mod rating;
pub mod tournament;
pub use rating::{
    MatchResult, Rating, RatingChange, RatingParams, RatingQueue, RatingService, RatingSystem,
    ResultApplied, UpsertRatingQueue,
};
pub use tournament::{CreateTournament, Tournament, TournamentFilter, TournamentService};

use std::str::FromStr;

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Operator {
    /// Keep the better score according to the sort order.
    #[default]
    Best,
    Set,
    Incr,
    Decr,
}

#[derive(Debug, Clone, Serialize)]
pub struct Leaderboard {
    pub id: String,
    pub authoritative: bool,
    pub sort_order: SortOrder,
    pub operator: Operator,
    pub reset_schedule: Option<String>,
    pub metadata: serde_json::Value,
    /// Players compete inside buckets of this many (0 = everyone in one scope). A bucket is
    /// assigned on the first score of a window and kept for that window.
    pub bucket_size: i32,
    /// Tiers with promotion/demotion at every reset (needs `reset_schedule`).
    pub league: Option<League>,
    /// Race-to-target cohorts (needs `bucket_size`).
    pub race: Option<Race>,
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,
    /// Start of the next reset window (unix seconds), 0 if the board never resets.
    pub next_reset: i64,
    pub prev_reset: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateLeaderboard {
    pub id: String,
    #[serde(default)]
    pub authoritative: bool,
    #[serde(default)]
    pub sort_order: SortOrder,
    #[serde(default)]
    pub operator: Operator,
    #[serde(default)]
    pub reset_schedule: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub bucket_size: i32,
    #[serde(default)]
    pub league: Option<League>,
    #[serde(default)]
    pub race: Option<Race>,
}

/// Race-to-target cohorts (Hiro score-based event leaderboards): a cohort (bucket) finishes as
/// soon as `target_players` of its members reach `score_target`, after which nobody in it can
/// post; every member has `time_limit_secs` from their first score (0 = the whole window); a
/// player whose cohort finished (or who finished / ran out of time) may re-roll into a fresh
/// cohort up to `max_rolls` times, waiting `roll_cooldown_secs` between rolls.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Race {
    pub score_target: i64,
    #[serde(default = "one_player")]
    pub target_players: i32,
    #[serde(default)]
    pub time_limit_secs: i64,
    #[serde(default)]
    pub max_rolls: i32,
    #[serde(default)]
    pub roll_cooldown_secs: i64,
}

fn one_player() -> i32 {
    1
}

impl Race {
    fn validate(&self) -> Result<()> {
        if self.target_players < 1 || self.target_players > 10_000 {
            return Err(Error::InvalidArgument(
                "race.target_players must be in 1..=10000".into(),
            ));
        }
        if self.time_limit_secs < 0 || self.roll_cooldown_secs < 0 {
            return Err(Error::InvalidArgument(
                "race.time_limit_secs/roll_cooldown_secs must be non-negative".into(),
            ));
        }
        if !(0..=1000).contains(&self.max_rolls) {
            return Err(Error::InvalidArgument(
                "race.max_rolls must be in 0..=1000".into(),
            ));
        }
        Ok(())
    }

    fn reached(&self, order: SortOrder, score: i64) -> bool {
        match order {
            SortOrder::Desc => score >= self.score_target,
            SortOrder::Asc => score <= self.score_target,
        }
    }
}

/// A player's cohort card on a race board.
#[derive(Debug, Clone, Serialize)]
pub struct Cohort {
    pub tier: i16,
    pub bucket: Option<i32>,
    pub members: i64,
    pub finishers: i64,
    pub closed: bool,
    pub closed_at: Option<i64>,
    pub finished: bool,
    pub score_target: i64,
    pub target_players: i32,
    /// Seconds this player may still post (None = no limit or no record yet).
    pub time_left_secs: Option<i64>,
    pub rolls: i32,
    pub rolls_left: i32,
    pub cooldown_left_secs: i64,
    pub can_reroll: bool,
}

/// League rules: at each reset the top `promote_pct` of every (tier, bucket) group moves one tier
/// up and the bottom `demote_pct` one tier down; tier 0 is the first name. Per-tier `overrides`
/// replace the percentages / add score targets for one tier; `promote_score` / `demote_score`
/// promote or demote anyone past the target regardless of rank (score-target races);
/// `idle_demote` drops players who posted nothing all window one tier; `rest_secs` keeps the
/// board closed for writes at the start of each window; `ending_soon_secs` flags the last stretch;
/// `rewards` pays a reward table per tier when the window closes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct League {
    pub tiers: Vec<String>,
    pub promote_pct: f64,
    pub demote_pct: f64,
    #[serde(default)]
    pub overrides: std::collections::BTreeMap<String, TierRule>,
    #[serde(default)]
    pub promote_score: Option<i64>,
    #[serde(default)]
    pub demote_score: Option<i64>,
    #[serde(default)]
    pub idle_demote: bool,
    #[serde(default)]
    pub rest_secs: i64,
    #[serde(default)]
    pub ending_soon_secs: i64,
    #[serde(default)]
    pub rewards: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TierRule {
    pub promote_pct: Option<f64>,
    pub demote_pct: Option<f64>,
    pub promote_score: Option<i64>,
    pub demote_score: Option<i64>,
}

/// Effective rule for one tier.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct Rule {
    pub promote_pct: f64,
    pub demote_pct: f64,
    pub promote_score: Option<i64>,
    pub demote_score: Option<i64>,
}

impl League {
    fn validate(&self) -> Result<()> {
        if self.tiers.len() < 2 || self.tiers.len() > 64 {
            return Err(Error::InvalidArgument("league needs 2..=64 tiers".into()));
        }
        let pct_ok = |p: f64| (0.0..=50.0).contains(&p);
        if !pct_ok(self.promote_pct) || !pct_ok(self.demote_pct) {
            return Err(Error::InvalidArgument(
                "promote_pct/demote_pct must be in 0..=50".into(),
            ));
        }
        for (k, r) in &self.overrides {
            if !k.parse::<usize>().is_ok_and(|i| i < self.tiers.len()) {
                return Err(Error::InvalidArgument(format!(
                    "overrides: {k} is not a tier index"
                )));
            }
            if r.promote_pct.is_some_and(|p| !pct_ok(p)) || r.demote_pct.is_some_and(|p| !pct_ok(p))
            {
                return Err(Error::InvalidArgument(format!(
                    "overrides.{k}: percentages must be in 0..=50"
                )));
            }
        }
        for k in self.rewards.keys() {
            if !k.parse::<usize>().is_ok_and(|i| i < self.tiers.len()) {
                return Err(Error::InvalidArgument(format!(
                    "rewards: {k} is not a tier index"
                )));
            }
        }
        if self.rest_secs < 0 || self.ending_soon_secs < 0 {
            return Err(Error::InvalidArgument(
                "rest_secs/ending_soon_secs must be non-negative".into(),
            ));
        }
        Ok(())
    }

    pub fn rule(&self, tier: i16) -> Rule {
        let o = self.overrides.get(&tier.to_string());
        Rule {
            promote_pct: o.and_then(|o| o.promote_pct).unwrap_or(self.promote_pct),
            demote_pct: o.and_then(|o| o.demote_pct).unwrap_or(self.demote_pct),
            promote_score: o.and_then(|o| o.promote_score).or(self.promote_score),
            demote_score: o.and_then(|o| o.demote_score).or(self.demote_score),
        }
    }

    /// Where a player ranked `rn` of `n` with `score` goes at the end of the window.
    pub fn decide(&self, tier: i16, rn: i64, n: i64, score: i64, desc: bool) -> Move {
        let top = self.tiers.len() as i16 - 1;
        let r = self.rule(tier);
        let up = ((n as f64) * r.promote_pct / 100.0).ceil() as i64;
        let down = ((n as f64) * r.demote_pct / 100.0).ceil() as i64;
        let passes = |t: Option<i64>, promote: bool| {
            t.is_some_and(|t| {
                if desc == promote {
                    score >= t
                } else {
                    score <= t
                }
            })
        };
        if tier < top && ((rn <= up && r.promote_pct > 0.0) || passes(r.promote_score, true)) {
            Move::Promote
        } else if tier > 0
            && ((rn > n - down && r.demote_pct > 0.0) || passes(r.demote_score, false))
        {
            Move::Demote
        } else {
            Move::Stay
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Move {
    Promote,
    Demote,
    Stay,
}

/// What one rollover did.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Rollover {
    pub leaderboard_id: String,
    pub expiry_time: i64,
    pub promoted: u32,
    pub demoted: u32,
    pub idle_demoted: u32,
    /// (account, reward table) for every participant of a tier with a reward.
    #[serde(skip)]
    pub rewards: Vec<(AccountId, String)>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Record {
    pub leaderboard_id: String,
    pub owner_id: AccountId,
    pub username: Option<String>,
    pub score: i64,
    pub subscore: i64,
    pub num_score: i32,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,
    pub expiry_time: i64,
    /// Scope of the record: league tier and bucket (0/0 on plain boards).
    pub tier: i16,
    pub bucket: i32,
    /// 1-based rank within the current window and scope.
    pub rank: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordList {
    pub records: Vec<Record>,
    pub owner_records: Vec<Record>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WriteRecord {
    pub score: i64,
    #[serde(default)]
    pub subscore: i64,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Override the leaderboard operator for this write (server/authoritative use).
    #[serde(default)]
    pub operator: Option<Operator>,
}

#[derive(Clone)]
pub struct LeaderboardService {
    db: PgPool,
}

type BoardRow = (
    String,
    bool,
    i16,
    i16,
    Option<String>,
    serde_json::Value,
    OffsetDateTime,
    i32,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
);
const BOARD_COLS: &str =
    "id, authoritative, sort_order, operator, reset_schedule, metadata, created_at, bucket_size, league, race";
type RecordRow = (
    String,
    Uuid,
    Option<String>,
    i64,
    i64,
    i32,
    serde_json::Value,
    OffsetDateTime,
    OffsetDateTime,
    OffsetDateTime,
    i16,
    i32,
    i64,
);
const RECORD_COLS: &str = "leaderboard_id, owner_id, username, score, subscore, num_score, metadata, created_at, updated_at, expiry_time, tier, bucket";

pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return Err(Error::InvalidArgument(
            "leaderboard id must be 1..=128 chars of [A-Za-z0-9_.:-]".into(),
        ));
    }
    Ok(())
}

/// Window boundaries for a cron reset schedule at `now`: (window start = previous fire or epoch, next fire).
/// The schedule fires in UTC unless prefixed crontab-style with `CRON_TZ=<IANA zone>` (or `TZ=`),
/// e.g. `CRON_TZ=Asia/Tokyo 0 0 * * *` resets at local midnight, DST included.
pub fn window(
    schedule: Option<&str>,
    now: OffsetDateTime,
) -> Result<(OffsetDateTime, OffsetDateTime)> {
    let Some(s) = schedule else {
        return Ok((OffsetDateTime::UNIX_EPOCH, OffsetDateTime::UNIX_EPOCH));
    };
    let (sched, tz) = parse_cron(s)?;
    let now_c = chrono::DateTime::<chrono::Utc>::from_timestamp(now.unix_timestamp(), 0)
        .ok_or_else(|| Error::Internal("bad time".into()))?
        .with_timezone(&tz);
    let next = sched
        .after(&now_c)
        .next()
        .ok_or_else(|| Error::InvalidArgument("schedule never fires".into()))?;
    // cron 0.12 has no `before`; walk forward from a bounded start to find the latest fire <= now.
    let mut prev: Option<chrono::DateTime<chrono_tz::Tz>> = None;
    let probe = now_c - chrono::Duration::days(366 * 2);
    for fire in sched.after(&probe) {
        if fire > now_c {
            break;
        }
        prev = Some(fire);
    }
    let to_t = |d: chrono::DateTime<chrono_tz::Tz>| {
        OffsetDateTime::from_unix_timestamp(d.timestamp()).unwrap_or(OffsetDateTime::UNIX_EPOCH)
    };
    Ok((
        prev.map(to_t).unwrap_or(OffsetDateTime::UNIX_EPOCH),
        to_t(next),
    ))
}

/// `[CRON_TZ=<zone> ]<5-field crontab>` → schedule + zone (UTC by default).
pub(crate) fn parse_cron(s: &str) -> Result<(cron::Schedule, chrono_tz::Tz)> {
    let mut rest = s.trim();
    let mut tz = chrono_tz::UTC;
    for prefix in ["CRON_TZ=", "TZ="] {
        if let Some(r) = rest.strip_prefix(prefix) {
            let (zone, tail) = r.split_once(char::is_whitespace).ok_or_else(|| {
                Error::InvalidArgument(
                    "reset_schedule: CRON_TZ=<zone> must be followed by a cron expression".into(),
                )
            })?;
            tz = zone.parse().map_err(|_| {
                Error::InvalidArgument(format!("reset_schedule: unknown time zone {zone}"))
            })?;
            rest = tail.trim();
            break;
        }
    }
    // Accept standard 5-field crontab; the `cron` crate wants seconds first.
    let fields = rest.split_whitespace().count();
    let spec = match fields {
        5 => format!("0 {rest}"),
        6 | 7 => rest.to_string(),
        _ => {
            return Err(Error::InvalidArgument(
                "reset_schedule must be a 5-field cron expression, optionally prefixed with CRON_TZ=<zone>".into(),
            ))
        }
    };
    cron::Schedule::from_str(&spec)
        .map(|sched| (sched, tz))
        .map_err(|e| Error::InvalidArgument(format!("bad reset_schedule: {e}")))
}

/// Race boards: open the cohort row, refuse writes into a finished cohort or past the player's
/// time limit.
#[allow(clippy::too_many_arguments)]
async fn race_gate(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    board: &Leaderboard,
    race: &Race,
    expiry: OffsetDateTime,
    tier: i16,
    bucket: i32,
    owner: AccountId,
    now: OffsetDateTime,
) -> Result<()> {
    sqlx::query(
        "insert into musubi.leaderboard_cohorts (leaderboard_id, expiry_time, tier, bucket)
         values ($1, $2, $3, $4) on conflict do nothing",
    )
    .bind(&board.id)
    .bind(expiry)
    .bind(tier)
    .bind(bucket)
    .execute(&mut **tx)
    .await
    .map_err(Error::internal)?;
    let closed: Option<Option<OffsetDateTime>> = sqlx::query_scalar(
        "select closed_at from musubi.leaderboard_cohorts
         where leaderboard_id = $1 and expiry_time = $2 and tier = $3 and bucket = $4",
    )
    .bind(&board.id)
    .bind(expiry)
    .bind(tier)
    .bind(bucket)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Error::internal)?;
    if closed.flatten().is_some() {
        return Err(Error::Conflict("cohort finished".into()));
    }
    if race.time_limit_secs > 0 {
        let created: Option<OffsetDateTime> = sqlx::query_scalar(
            "select created_at from musubi.leaderboard_records
             where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3",
        )
        .bind(&board.id)
        .bind(expiry)
        .bind(owner.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        if created.is_some_and(|c| (now - c).whole_seconds() >= race.time_limit_secs) {
            return Err(Error::Conflict("race time limit over".into()));
        }
    }
    Ok(())
}

/// Recount a cohort's finishers; closes it when `target_players` reached the target. Returns
/// whether it closed just now.
async fn refresh_cohort(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    board: &Leaderboard,
    race: &Race,
    expiry: OffsetDateTime,
    tier: i16,
    bucket: i32,
) -> Result<bool> {
    let finishers: i64 = sqlx::query_scalar(
        "select count(*) from musubi.leaderboard_records
         where leaderboard_id = $1 and expiry_time = $2 and tier = $3 and bucket = $4
           and case when $6 then score >= $5 else score <= $5 end",
    )
    .bind(&board.id)
    .bind(expiry)
    .bind(tier)
    .bind(bucket)
    .bind(race.score_target)
    .bind(matches!(board.sort_order, SortOrder::Desc))
    .fetch_one(&mut **tx)
    .await
    .map_err(Error::internal)?;
    let closed_now: Option<bool> = sqlx::query_scalar(
        "update musubi.leaderboard_cohorts
         set finishers = $5,
             closed_at = case when closed_at is null and $5 >= $6 then now() else closed_at end
         where leaderboard_id = $1 and expiry_time = $2 and tier = $3 and bucket = $4
         returning closed_at is not null and $5 >= $6 and closed_at >= now()",
    )
    .bind(&board.id)
    .bind(expiry)
    .bind(tier)
    .bind(bucket)
    .bind(finishers as i32)
    .bind(race.target_players)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Error::internal)?;
    Ok(closed_now.unwrap_or(false))
}

fn to_board(r: BoardRow, now: OffsetDateTime) -> Result<Leaderboard> {
    let (prev, next) = window(r.4.as_deref(), now)?;
    Ok(Leaderboard {
        id: r.0,
        authoritative: r.1,
        sort_order: if r.2 == 0 {
            SortOrder::Asc
        } else {
            SortOrder::Desc
        },
        operator: match r.3 {
            1 => Operator::Set,
            2 => Operator::Incr,
            3 => Operator::Decr,
            _ => Operator::Best,
        },
        reset_schedule: r.4,
        bucket_size: r.7,
        league: r
            .8
            .map(serde_json::from_value)
            .transpose()
            .map_err(Error::internal)?,
        race: r
            .9
            .map(serde_json::from_value)
            .transpose()
            .map_err(Error::internal)?,
        metadata: r.5,
        create_time: r.6,
        next_reset: if next == OffsetDateTime::UNIX_EPOCH {
            0
        } else {
            next.unix_timestamp()
        },
        prev_reset: if prev == OffsetDateTime::UNIX_EPOCH {
            0
        } else {
            prev.unix_timestamp()
        },
    })
}

fn to_record(r: RecordRow) -> Record {
    Record {
        leaderboard_id: r.0,
        owner_id: AccountId(r.1),
        username: r.2,
        score: r.3,
        subscore: r.4,
        num_score: r.5,
        metadata: r.6,
        create_time: r.7,
        update_time: r.8,
        expiry_time: if r.9 == OffsetDateTime::UNIX_EPOCH {
            0
        } else {
            r.9.unix_timestamp()
        },
        tier: r.10,
        bucket: r.11,
        rank: r.12,
    }
}

impl LeaderboardService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    pub async fn create(&self, c: CreateLeaderboard) -> Result<Leaderboard> {
        validate_id(&c.id)?;
        if let Some(s) = &c.reset_schedule {
            parse_cron(s)?;
        }
        if let Some(m) = &c.metadata {
            if !m.is_object() {
                return Err(Error::InvalidArgument("metadata must be an object".into()));
            }
        }
        if c.bucket_size < 0 || c.bucket_size == 1 {
            return Err(Error::InvalidArgument(
                "bucket_size must be 0 (global) or at least 2".into(),
            ));
        }
        if let Some(l) = &c.league {
            l.validate()?;
            if c.reset_schedule.is_none() {
                return Err(Error::InvalidArgument(
                    "a league needs a reset_schedule to roll over on".into(),
                ));
            }
        }
        if let Some(r) = &c.race {
            r.validate()?;
            if c.bucket_size < 2 {
                return Err(Error::InvalidArgument(
                    "a race needs bucket_size >= 2 (cohorts)".into(),
                ));
            }
        }
        let league_json = c
            .league
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(Error::internal)?;
        let race_json = c
            .race
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(Error::internal)?;
        let res = sqlx::query_as::<_, BoardRow>(
            &format!(
                "insert into musubi.leaderboards (id, authoritative, sort_order, operator, reset_schedule, metadata, bucket_size, league, race)
                 values ($1, $2, $3, $4, $5, coalesce($6, '{{}}'::jsonb), $7, $8, $9)
                 returning {BOARD_COLS}"
            ),
        )
        .bind(&c.id)
        .bind(c.authoritative)
        .bind(c.sort_order as i16)
        .bind(c.operator as i16)
        .bind(&c.reset_schedule)
        .bind(&c.metadata)
        .bind(c.bucket_size)
        .bind(league_json)
        .bind(race_json)
        .fetch_one(&self.db)
        .await;
        match res {
            Ok(r) => to_board(r, OffsetDateTime::now_utc()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(Error::AlreadyExists("leaderboard exists".into()))
            }
            Err(e) => Err(Error::internal(e)),
        }
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let r = sqlx::query("delete from musubi.leaderboards where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("leaderboard".into()));
        }
        Ok(())
    }

    pub async fn get(&self, id: &str) -> Result<Leaderboard> {
        let r = sqlx::query_as::<_, BoardRow>(&format!(
            "select {BOARD_COLS} from musubi.leaderboards where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("leaderboard".into()))?;
        to_board(r, OffsetDateTime::now_utc())
    }

    pub async fn list(
        &self,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<Leaderboard>, Option<String>)> {
        let limit = limit.clamp(1, 100) as i64;
        let rows = sqlx::query_as::<_, BoardRow>(&format!(
            "select {BOARD_COLS} from musubi.leaderboards where ($1::text is null or id > $1) order by id limit $2"),
        )
        .bind(cursor)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let now = OffsetDateTime::now_utc();
        let boards: Vec<Leaderboard> = rows
            .into_iter()
            .take(limit as usize)
            .map(|r| to_board(r, now))
            .collect::<Result<_>>()?;
        let cursor = more.then(|| boards.last().map(|b| b.id.clone())).flatten();
        Ok((boards, cursor))
    }

    /// Submit a score. `server` bypasses the `authoritative` flag and may override the operator.
    pub async fn write(
        &self,
        id: &str,
        owner: AccountId,
        username: Option<&str>,
        w: WriteRecord,
        server: bool,
    ) -> Result<Record> {
        Ok(self.write_ext(id, owner, username, w, server).await?.0)
    }

    /// `write` plus whether this score just finished the player's race cohort.
    pub async fn write_ext(
        &self,
        id: &str,
        owner: AccountId,
        username: Option<&str>,
        w: WriteRecord,
        server: bool,
    ) -> Result<(Record, bool)> {
        let board = self.get(id).await?;
        if board.authoritative && !server {
            return Err(Error::PermissionDenied(
                "leaderboard is authoritative".into(),
            ));
        }
        if let Some(m) = &w.metadata {
            if !m.is_object() || serde_json::to_vec(m)?.len() > 16 * 1024 {
                return Err(Error::InvalidArgument(
                    "metadata must be an object ≤ 16 KB".into(),
                ));
            }
        }
        let op = match (w.operator, server) {
            (Some(o), true) => o,
            (Some(_), false) => {
                return Err(Error::PermissionDenied(
                    "operator override is server-only".into(),
                ))
            }
            (None, _) => board.operator,
        };
        let now = OffsetDateTime::now_utc();
        let (start, expiry) = window(board.reset_schedule.as_deref(), now)?;
        if let Some(l) = &board.league {
            if l.rest_secs > 0 && start != OffsetDateTime::UNIX_EPOCH {
                let until = start + time::Duration::seconds(l.rest_secs);
                if now < until {
                    return Err(Error::Unavailable(format!(
                        "league is resting until {}",
                        until.unix_timestamp()
                    )));
                }
            }
        }
        let better = match board.sort_order {
            SortOrder::Desc => "(excluded.score, excluded.subscore) > (r.score, r.subscore)",
            SortOrder::Asc => "(excluded.score, excluded.subscore) < (r.score, r.subscore)",
        };
        let update = match op {
            Operator::Best => format!(
                "score = case when {better} then excluded.score else r.score end,
                 subscore = case when {better} then excluded.subscore else r.subscore end"
            ),
            Operator::Set => "score = excluded.score, subscore = excluded.subscore".into(),
            Operator::Incr => {
                "score = r.score + excluded.score, subscore = r.subscore + excluded.subscore".into()
            }
            Operator::Decr => {
                "score = r.score - excluded.score, subscore = r.subscore - excluded.subscore".into()
            }
        };
        let sql = format!(
            "insert into musubi.leaderboard_records as r (leaderboard_id, owner_id, username, score, subscore, num_score, metadata, expiry_time, tier, bucket)
             values ($1, $2, $3, $4, $5, 1, coalesce($6, '{{}}'::jsonb), $7, $8, $9)
             on conflict (leaderboard_id, expiry_time, owner_id) do update set {update},
                 num_score = r.num_score + 1, username = coalesce(excluded.username, r.username),
                 metadata = coalesce($6, r.metadata), updated_at = now()
             returning {RECORD_COLS}, 0::bigint"
        );
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let (tier, bucket) = self.scope_for_write(&mut tx, &board, expiry, owner).await?;
        if let Some(race) = &board.race {
            race_gate(&mut tx, &board, race, expiry, tier, bucket, owner, now).await?;
        }
        let row = sqlx::query_as::<_, RecordRow>(&sql)
            .bind(id)
            .bind(owner.0)
            .bind(username)
            .bind(w.score)
            .bind(w.subscore)
            .bind(&w.metadata)
            .bind(expiry)
            .bind(tier)
            .bind(bucket)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let mut closed = false;
        if let Some(race) = &board.race {
            if race.reached(board.sort_order, row.3) {
                closed = refresh_cohort(&mut tx, &board, race, expiry, tier, bucket).await?;
            }
        }
        tx.commit().await.map_err(Error::internal)?;
        let mut rec = to_record(row);
        rec.rank = self.rank_of(&board, expiry, owner).await?;
        Ok((rec, closed))
    }

    /// The player's race cohort card for the current window.
    pub async fn cohort(&self, id: &str, owner: AccountId) -> Result<Cohort> {
        let board = self.get(id).await?;
        let Some(race) = &board.race else {
            return Err(Error::InvalidArgument("leaderboard has no race".into()));
        };
        let now = OffsetDateTime::now_utc();
        let (_, expiry) = window(board.reset_schedule.as_deref(), now)?;
        let rec: Option<(i16, i32, i64, OffsetDateTime)> = sqlx::query_as(
            "select tier, bucket, score, created_at from musubi.leaderboard_records
             where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3",
        )
        .bind(id)
        .bind(expiry)
        .bind(owner.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let roll: Option<(i32, OffsetDateTime)> = sqlx::query_as(
            "select rolls, last_roll_at from musubi.leaderboard_rolls
             where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3",
        )
        .bind(id)
        .bind(expiry)
        .bind(owner.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let rolls = roll.map_or(0, |r| r.0);
        let cooldown_left = roll
            .map(|(_, at)| race.roll_cooldown_secs - (now - at).whole_seconds())
            .unwrap_or(0)
            .max(0);
        let mut c = Cohort {
            tier: rec.map_or(0, |r| r.0),
            bucket: rec.map(|r| r.1),
            members: 0,
            finishers: 0,
            closed: false,
            closed_at: None,
            finished: false,
            score_target: race.score_target,
            target_players: race.target_players,
            time_left_secs: None,
            rolls,
            rolls_left: (race.max_rolls - rolls).max(0),
            cooldown_left_secs: cooldown_left,
            can_reroll: false,
        };
        let Some((tier, bucket, score, created)) = rec else {
            return Ok(c);
        };
        let (members, finishers, closed_at): (i64, i64, Option<OffsetDateTime>) = sqlx::query_as(
            "select (select count(*) from musubi.leaderboard_records r
                     where r.leaderboard_id = $1 and r.expiry_time = $2 and r.tier = $3 and r.bucket = $4),
                    (select count(*) from musubi.leaderboard_records r
                     where r.leaderboard_id = $1 and r.expiry_time = $2 and r.tier = $3 and r.bucket = $4
                       and case when $6 then r.score >= $5 else r.score <= $5 end),
                    (select closed_at from musubi.leaderboard_cohorts c
                     where c.leaderboard_id = $1 and c.expiry_time = $2 and c.tier = $3 and c.bucket = $4)",
        )
        .bind(id)
        .bind(expiry)
        .bind(tier)
        .bind(bucket)
        .bind(race.score_target)
        .bind(matches!(board.sort_order, SortOrder::Desc))
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        c.members = members;
        c.finishers = finishers;
        c.closed = closed_at.is_some();
        c.closed_at = closed_at.map(|t| t.unix_timestamp());
        c.finished = race.reached(board.sort_order, score);
        if race.time_limit_secs > 0 {
            c.time_left_secs =
                Some((race.time_limit_secs - (now - created).whole_seconds()).max(0));
        }
        let over = c.closed || c.finished || c.time_left_secs == Some(0);
        c.can_reroll = over && c.rolls_left > 0 && c.cooldown_left_secs == 0;
        Ok(c)
    }

    /// Re-roll into a fresh cohort: the player's record is reset to zero scores in the newest
    /// open bucket of their tier; allowed once their cohort finished, they reached the target or
    /// their time ran out, within `max_rolls` and after `roll_cooldown_secs`.
    pub async fn reroll(&self, id: &str, owner: AccountId) -> Result<Cohort> {
        let board = self.get(id).await?;
        let Some(race) = board.race.clone() else {
            return Err(Error::InvalidArgument("leaderboard has no race".into()));
        };
        let before = self.cohort(id, owner).await?;
        if before.bucket.is_none() {
            return Err(Error::NotFound("no record to re-roll".into()));
        }
        if !(before.closed || before.finished || before.time_left_secs == Some(0)) {
            return Err(Error::Conflict("cohort still running".into()));
        }
        if before.rolls_left == 0 {
            return Err(Error::Conflict("no rolls left".into()));
        }
        if before.cooldown_left_secs > 0 {
            return Err(Error::Conflict(format!(
                "roll cooldown: {} s left",
                before.cooldown_left_secs
            )));
        }
        let now = OffsetDateTime::now_utc();
        let (_, expiry) = window(board.reset_schedule.as_deref(), now)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let old: Option<(i16, i32, Option<String>)> = sqlx::query_as(
            "delete from musubi.leaderboard_records
             where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3
             returning tier, bucket, username",
        )
        .bind(id)
        .bind(expiry)
        .bind(owner.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((tier, old_bucket, username)) = old else {
            return Err(Error::NotFound("no record to re-roll".into()));
        };
        refresh_cohort(&mut tx, &board, &race, expiry, tier, old_bucket).await?;
        let (tier, bucket) = self.scope_for_write(&mut tx, &board, expiry, owner).await?;
        sqlx::query(
            "insert into musubi.leaderboard_records (leaderboard_id, owner_id, username, score, subscore, num_score, expiry_time, tier, bucket)
             values ($1, $2, $3, 0, 0, 0, $4, $5, $6)",
        )
        .bind(id)
        .bind(owner.0)
        .bind(&username)
        .bind(expiry)
        .bind(tier)
        .bind(bucket)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.leaderboard_cohorts (leaderboard_id, expiry_time, tier, bucket)
             values ($1, $2, $3, $4) on conflict do nothing",
        )
        .bind(id)
        .bind(expiry)
        .bind(tier)
        .bind(bucket)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.leaderboard_rolls (leaderboard_id, expiry_time, owner_id, rolls, last_roll_at)
             values ($1, $2, $3, 1, now())
             on conflict (leaderboard_id, expiry_time, owner_id) do update
                 set rolls = musubi.leaderboard_rolls.rolls + 1, last_roll_at = now()",
        )
        .bind(id)
        .bind(expiry)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        self.cohort(id, owner).await
    }

    pub async fn delete_record(&self, id: &str, owner: AccountId, server: bool) -> Result<()> {
        let board = self.get(id).await?;
        if board.authoritative && !server {
            return Err(Error::PermissionDenied(
                "leaderboard is authoritative".into(),
            ));
        }
        let (_, expiry) = window(board.reset_schedule.as_deref(), OffsetDateTime::now_utc())?;
        sqlx::query("delete from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3")
            .bind(id)
            .bind(expiry)
            .bind(owner.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    async fn rank_of(
        &self,
        board: &Leaderboard,
        expiry: OffsetDateTime,
        owner: AccountId,
    ) -> Result<i64> {
        let cmp = match board.sort_order {
            SortOrder::Desc => "(o.score, o.subscore, -extract(epoch from o.updated_at)) > (r.score, r.subscore, -extract(epoch from r.updated_at)) or ((o.score, o.subscore, o.updated_at) = (r.score, r.subscore, r.updated_at) and o.owner_id < r.owner_id)",
            SortOrder::Asc => "(o.score, o.subscore, extract(epoch from o.updated_at)) < (r.score, r.subscore, extract(epoch from r.updated_at)) or ((o.score, o.subscore, o.updated_at) = (r.score, r.subscore, r.updated_at) and o.owner_id < r.owner_id)",
        };
        let sql = format!(
            "select 1 + count(*) from musubi.leaderboard_records o, musubi.leaderboard_records r
             where r.leaderboard_id = $1 and r.expiry_time = $2 and r.owner_id = $3
               and o.leaderboard_id = r.leaderboard_id and o.expiry_time = r.expiry_time
               and o.tier = r.tier and o.bucket = r.bucket and ({cmp})"
        );
        let rank: i64 = sqlx::query_scalar(&sql)
            .bind(&board.id)
            .bind(expiry)
            .bind(owner.0)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(rank)
    }

    /// Ranked page of the current window (or a past window when `expiry` is given), plus the
    /// requested owners' records with their ranks.
    pub async fn records(
        &self,
        id: &str,
        owners: &[AccountId],
        limit: usize,
        cursor: Option<&str>,
        expiry: Option<i64>,
        viewer: Option<AccountId>,
    ) -> Result<RecordList> {
        let board = self.get(id).await?;
        let limit = limit.clamp(1, 100) as i64;
        let expiry = match expiry {
            Some(0) | None => window(board.reset_schedule.as_deref(), OffsetDateTime::now_utc())?.1,
            Some(e) => OffsetDateTime::from_unix_timestamp(e)
                .map_err(|_| Error::InvalidArgument("bad expiry".into()))?,
        };
        let offset: i64 = match cursor {
            Some(c) => c
                .parse()
                .map_err(|_| Error::InvalidArgument("bad cursor".into()))?,
            None => 0,
        };
        let order = match board.sort_order {
            SortOrder::Desc => "score desc, subscore desc, updated_at asc, owner_id asc",
            SortOrder::Asc => "score asc, subscore asc, updated_at asc, owner_id asc",
        };
        // scoped boards page the viewer's (tier, bucket); a viewer without a record this window
        // sees their tier's first bucket page (or nothing on a bucketed board)
        let scope = if board.bucket_size > 0 || board.league.is_some() {
            let viewer = viewer.or_else(|| owners.first().copied());
            match self.scope_of(&board, expiry, viewer).await? {
                Some(sc) => Some(sc),
                None => {
                    return Ok(RecordList {
                        records: vec![],
                        owner_records: vec![],
                        next_cursor: None,
                        prev_cursor: None,
                    })
                }
            }
        } else {
            None
        };
        let (tier, bucket) = scope.unwrap_or((0, 0));
        let scoped = "and tier = $5 and bucket = $6";
        let sql = format!(
            "select {RECORD_COLS}, row_number() over (order by {order}) as rank
             from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2 {scoped}
             order by rank offset $3 limit $4"
        );
        let rows = sqlx::query_as::<_, RecordRow>(&sql)
            .bind(id)
            .bind(expiry)
            .bind(offset)
            .bind(limit + 1)
            .bind(tier)
            .bind(bucket)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let records: Vec<Record> = rows
            .into_iter()
            .take(limit as usize)
            .map(to_record)
            .collect();
        let owner_records = if owners.is_empty() {
            vec![]
        } else {
            let ids: Vec<Uuid> = owners.iter().map(|o| o.0).collect();
            let sql = format!(
                "select * from (select {RECORD_COLS}, row_number() over (partition by tier, bucket order by {order}) as rank
                 from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2) x
                 where owner_id = any($3) order by rank"
            );
            sqlx::query_as::<_, RecordRow>(&sql)
                .bind(id)
                .bind(expiry)
                .bind(&ids)
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?
                .into_iter()
                .map(to_record)
                .collect()
        };
        Ok(RecordList {
            records,
            owner_records,
            next_cursor: more.then(|| (offset + limit).to_string()),
            prev_cursor: (offset > 0).then(|| (offset - limit).max(0).to_string()),
        })
    }

    /// Records around an owner: the owner's rank page centred on them.
    pub async fn around(
        &self,
        id: &str,
        owner: AccountId,
        limit: usize,
        expiry: Option<i64>,
    ) -> Result<RecordList> {
        let board = self.get(id).await?;
        let limit = limit.clamp(1, 100) as i64;
        let exp = match expiry {
            Some(0) | None => window(board.reset_schedule.as_deref(), OffsetDateTime::now_utc())?.1,
            Some(e) => OffsetDateTime::from_unix_timestamp(e)
                .map_err(|_| Error::InvalidArgument("bad expiry".into()))?,
        };
        let rank = self.rank_of(&board, exp, owner).await?;
        let exists: Option<i64> = sqlx::query_scalar("select 1::bigint from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3")
            .bind(id)
            .bind(exp)
            .bind(owner.0)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
        if exists.is_none() {
            return Err(Error::NotFound("owner has no record".into()));
        }
        let offset = (rank - 1 - limit / 2).max(0);
        self.records(
            id,
            &[owner],
            limit as usize,
            Some(&offset.to_string()),
            Some(exp.unix_timestamp()),
            Some(owner),
        )
        .await
    }

    /// (tier, bucket) of a viewer for a window: their record's scope, else their tier with the
    /// first bucket (plain league), else None (bucketed board, no record yet).
    async fn scope_of(
        &self,
        board: &Leaderboard,
        expiry: OffsetDateTime,
        viewer: Option<AccountId>,
    ) -> Result<Option<(i16, i32)>> {
        let Some(viewer) = viewer else {
            return Ok((board.bucket_size == 0).then_some((0, 0)));
        };
        let rec: Option<(i16, i32)> = sqlx::query_as(
            "select tier, bucket from musubi.leaderboard_records
             where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3",
        )
        .bind(&board.id)
        .bind(expiry)
        .bind(viewer.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        if let Some(sc) = rec {
            return Ok(Some(sc));
        }
        if board.bucket_size > 0 {
            return Ok(None);
        }
        Ok(Some((self.tier_of(&board.id, viewer).await?, 0)))
    }

    /// League tier of a player (0 when unknown or no league).
    pub async fn tier_of(&self, id: &str, owner: AccountId) -> Result<i16> {
        let t: Option<i16> = sqlx::query_scalar(
            "select tier from musubi.leaderboard_tiers where leaderboard_id = $1 and owner_id = $2",
        )
        .bind(id)
        .bind(owner.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(t.unwrap_or(0))
    }

    /// Scope a new score lands in. An existing record keeps its scope; otherwise the player's tier
    /// and, on bucketed boards, the newest bucket of that tier with room (under a per-window lock
    /// so two first scores cannot both open bucket N+1).
    async fn scope_for_write(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        board: &Leaderboard,
        expiry: OffsetDateTime,
        owner: AccountId,
    ) -> Result<(i16, i32)> {
        if board.bucket_size == 0 && board.league.is_none() {
            return Ok((0, 0));
        }
        let existing: Option<(i16, i32)> = sqlx::query_as(
            "select tier, bucket from musubi.leaderboard_records
             where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3",
        )
        .bind(&board.id)
        .bind(expiry)
        .bind(owner.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        if let Some(sc) = existing {
            return Ok(sc);
        }
        let tier = if board.league.is_some() {
            self.tier_of(&board.id, owner).await?
        } else {
            0
        };
        if board.bucket_size == 0 {
            return Ok((tier, 0));
        }
        sqlx::query("select pg_advisory_xact_lock(hashtext($1), $2)")
            .bind(format!("{}:{}", board.id, expiry.unix_timestamp()))
            .bind(tier as i32)
            .execute(&mut **tx)
            .await
            .map_err(Error::internal)?;
        let last: Option<(i32, i64)> = sqlx::query_as(
            "select bucket, count(*) from musubi.leaderboard_records
             where leaderboard_id = $1 and expiry_time = $2 and tier = $3
             group by bucket order by bucket desc limit 1",
        )
        .bind(&board.id)
        .bind(expiry)
        .bind(tier)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
        if board.race.is_some() {
            // race boards: the newest *open* cohort with room, else a new one past the newest
            let open: Option<(i32, i64)> = sqlx::query_as(
                "select r.bucket, count(*) from musubi.leaderboard_records r
                 where r.leaderboard_id = $1 and r.expiry_time = $2 and r.tier = $3
                   and not exists (select 1 from musubi.leaderboard_cohorts c
                                   where c.leaderboard_id = r.leaderboard_id and c.expiry_time = r.expiry_time
                                     and c.tier = r.tier and c.bucket = r.bucket and c.closed_at is not null)
                 group by r.bucket order by r.bucket desc limit 1",
            )
            .bind(&board.id)
            .bind(expiry)
            .bind(tier)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
            let newest = last.map_or(0, |(b, _)| b);
            return Ok((
                tier,
                match open {
                    Some((b, n)) if n < board.bucket_size as i64 => b,
                    _ => newest + 1,
                },
            ));
        }
        let bucket = match last {
            Some((b, n)) if n < board.bucket_size as i64 => b,
            Some((b, _)) => b + 1,
            None => 1,
        };
        Ok((tier, bucket))
    }

    /// End-of-window league move. `expiry` names the window to close (default: the last finished
    /// one); passing the current window's expiry ends the season early. Idempotent per window.
    pub async fn rollover(&self, id: &str, expiry: Option<i64>) -> Result<Option<Rollover>> {
        let board = self.get(id).await?;
        let Some(league) = &board.league else {
            return Err(Error::InvalidArgument("leaderboard has no league".into()));
        };
        let now = OffsetDateTime::now_utc();
        let exp = match expiry {
            Some(e) => OffsetDateTime::from_unix_timestamp(e)
                .map_err(|_| Error::InvalidArgument("bad expiry".into()))?,
            None => window(board.reset_schedule.as_deref(), now)?.0,
        };
        if exp == OffsetDateTime::UNIX_EPOCH {
            return Ok(None);
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let done = sqlx::query(
            "insert into musubi.leaderboard_rollovers (leaderboard_id, expiry_time) values ($1, $2)
             on conflict do nothing",
        )
        .bind(id)
        .bind(exp)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if done.rows_affected() == 0 {
            return Ok(None);
        }
        let order = match board.sort_order {
            SortOrder::Desc => "score desc, subscore desc, updated_at asc, owner_id asc",
            SortOrder::Asc => "score asc, subscore asc, updated_at asc, owner_id asc",
        };
        let desc = board.sort_order == SortOrder::Desc;
        let rows: Vec<(Uuid, i16, i64, i64, i64)> = sqlx::query_as(&format!(
            "select owner_id, tier, row_number() over (partition by tier, bucket order by {order}),
                    count(*) over (partition by tier, bucket), score
             from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2"
        ))
        .bind(id)
        .bind(exp)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let (mut promoted, mut demoted, mut idle_demoted) = (0u32, 0u32, 0u32);
        let mut rewards = Vec::new();
        let mut moves: Vec<(Uuid, i16)> = Vec::new();
        for (owner, tier, rn, n, score) in rows {
            if let Some(t) = league.rewards.get(&tier.to_string()) {
                rewards.push((AccountId(owner), t.clone()));
            }
            match league.decide(tier, rn, n, score, desc) {
                Move::Promote => {
                    promoted += 1;
                    moves.push((owner, tier + 1));
                }
                Move::Demote => {
                    demoted += 1;
                    moves.push((owner, tier - 1));
                }
                Move::Stay => {}
            }
        }
        if league.idle_demote {
            let idle: Vec<(Uuid, i16)> = sqlx::query_as(
                "select t.owner_id, t.tier from musubi.leaderboard_tiers t
                 where t.leaderboard_id = $1 and t.tier > 0 and not exists (
                     select 1 from musubi.leaderboard_records r
                     where r.leaderboard_id = $1 and r.expiry_time = $2 and r.owner_id = t.owner_id)",
            )
            .bind(id)
            .bind(exp)
            .fetch_all(&mut *tx)
            .await
            .map_err(Error::internal)?;
            for (owner, tier) in idle {
                idle_demoted += 1;
                moves.push((owner, tier - 1));
            }
        }
        for (owner, new_tier) in moves {
            sqlx::query(
                "insert into musubi.leaderboard_tiers (leaderboard_id, owner_id, tier) values ($1, $2, $3)
                 on conflict (leaderboard_id, owner_id) do update set tier = excluded.tier, updated_at = now()",
            )
            .bind(id)
            .bind(owner)
            .bind(new_tier)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        sqlx::query(
            "update musubi.leaderboard_rollovers set promoted = $3, demoted = $4
             where leaderboard_id = $1 and expiry_time = $2",
        )
        .bind(id)
        .bind(exp)
        .bind(promoted as i32)
        .bind((demoted + idle_demoted) as i32)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Some(Rollover {
            leaderboard_id: id.to_string(),
            expiry_time: exp.unix_timestamp(),
            promoted,
            demoted,
            idle_demoted,
            rewards,
        }))
    }

    /// League card of a player: tier, window, phase flags and where the current rank would send
    /// them at the end of the window.
    pub async fn tier_status(
        &self,
        id: &str,
        owner: AccountId,
        now: OffsetDateTime,
    ) -> Result<serde_json::Value> {
        let board = self.get(id).await?;
        let Some(league) = board.league.clone() else {
            return Err(Error::NotFound("leaderboard has no league".into()));
        };
        let (start, end) = window(board.reset_schedule.as_deref(), now)?;
        // the league standing lives in leaderboard_tiers; this window's record (if any) only
        // places the player in a bucket while it still belongs to that tier
        let tier = self.tier_of(id, owner).await?;
        let bucket = match self.scope_of(&board, end, Some(owner)).await? {
            Some((t, b)) if t == tier => b,
            _ => -1,
        };
        let order = match board.sort_order {
            SortOrder::Desc => "score desc, subscore desc, updated_at asc, owner_id asc",
            SortOrder::Asc => "score asc, subscore asc, updated_at asc, owner_id asc",
        };
        let mine: Option<(i64, i64, i64)> = sqlx::query_as(&format!(
            "select rn, n, score from (
                 select owner_id, row_number() over (order by {order}) as rn, count(*) over () as n, score
                 from musubi.leaderboard_records
                 where leaderboard_id = $1 and expiry_time = $2 and tier = $3 and bucket = $4) x
             where owner_id = $5"
        ))
        .bind(id)
        .bind(end)
        .bind(tier)
        .bind(bucket)
        .bind(owner.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let rule = league.rule(tier);
        let desc = board.sort_order == SortOrder::Desc;
        let projected = match mine {
            Some((rn, n, score)) => league.decide(tier, rn, n, score, desc),
            None if league.idle_demote && tier > 0 => Move::Demote,
            None => Move::Stay,
        };
        let resting_until = (league.rest_secs > 0 && start != OffsetDateTime::UNIX_EPOCH)
            .then(|| start + time::Duration::seconds(league.rest_secs));
        let seconds_left = (end != OffsetDateTime::UNIX_EPOCH).then(|| (end - now).whole_seconds());
        let n = mine.map(|m| m.1).unwrap_or(0);
        Ok(serde_json::json!({
            "tier": tier,
            "name": league.tiers.get(tier as usize),
            "tiers": league.tiers,
            "bucket": bucket,
            "window_start": (start != OffsetDateTime::UNIX_EPOCH).then(|| start.unix_timestamp()),
            "window_end": (end != OffsetDateTime::UNIX_EPOCH).then(|| end.unix_timestamp()),
            "seconds_left": seconds_left,
            "ending_soon": seconds_left.is_some_and(|s| league.ending_soon_secs > 0 && s <= league.ending_soon_secs),
            "resting": resting_until.is_some_and(|u| now < u),
            "resting_until": resting_until.map(|u| u.unix_timestamp()),
            "rank": mine.map(|m| m.0),
            "group_size": n,
            "score": mine.map(|m| m.2),
            "projected": projected,
            "idle": mine.is_none(),
            "promote_cut": ((n as f64) * rule.promote_pct / 100.0).ceil() as i64,
            "demote_cut": ((n as f64) * rule.demote_pct / 100.0).ceil() as i64,
            "rule": rule,
            "reward_table": league.rewards.get(&tier.to_string()),
        }))
    }

    pub async fn unended_windows(&self) -> Result<Vec<(String, i64, bool)>> {
        let rows: Vec<(String, Option<String>, bool, OffsetDateTime)> = sqlx::query_as(
            "select id, reset_schedule, is_tournament, created_at from musubi.leaderboards where reset_schedule is not null",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let now = OffsetDateTime::now_utc();
        let mut out = Vec::new();
        for (id, schedule, is_t, created) in rows {
            let (prev, _) = window(schedule.as_deref(), now)?;
            // a window that ended before the board existed has nothing to announce
            if prev == OffsetDateTime::UNIX_EPOCH || prev <= created {
                continue;
            }
            let done: Option<i64> = sqlx::query_scalar(
                "select 1::bigint from musubi.leaderboard_ends where leaderboard_id = $1 and expiry_time = $2",
            )
            .bind(&id)
            .bind(prev)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
            if done.is_none() {
                out.push((id, prev.unix_timestamp(), is_t));
            }
        }
        Ok(out)
    }

    /// Mark a finished window as announced; false when someone else already did.
    pub async fn claim_end(&self, id: &str, expiry: i64) -> Result<bool> {
        let exp = OffsetDateTime::from_unix_timestamp(expiry)
            .map_err(|_| Error::InvalidArgument("bad expiry".into()))?;
        let r = sqlx::query(
            "insert into musubi.leaderboard_ends (leaderboard_id, expiry_time) values ($1, $2) on conflict do nothing",
        )
        .bind(id)
        .bind(exp)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    /// Roll over every league board whose last window has finished (sweep).
    pub async fn rollover_due(&self) -> Result<Vec<Rollover>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "select id from musubi.leaderboards where league is not null and reset_schedule is not null",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for id in ids {
            if let Some(r) = self.rollover(&id, None).await? {
                out.push(r);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn league_rules_overrides_score_targets_and_moves() {
        let mut l = League {
            tiers: vec!["bronze".into(), "silver".into(), "gold".into()],
            promote_pct: 20.0,
            demote_pct: 20.0,
            overrides: Default::default(),
            promote_score: Some(1000),
            demote_score: None,
            idle_demote: true,
            rest_secs: 0,
            ending_soon_secs: 0,
            rewards: Default::default(),
        };
        l.overrides.insert(
            "1".into(),
            TierRule {
                demote_pct: Some(50.0),
                ..Default::default()
            },
        );
        assert!(l.validate().is_ok());
        assert_eq!(l.rule(1).demote_pct, 50.0);
        assert_eq!(l.rule(0).demote_pct, 20.0);
        assert_eq!(l.decide(0, 2, 10, 5, true), Move::Promote);
        assert_eq!(l.decide(0, 3, 10, 5, true), Move::Stay);
        assert_eq!(l.decide(0, 10, 10, 5, true), Move::Stay, "bottom tier");
        assert_eq!(
            l.decide(1, 6, 10, 5, true),
            Move::Demote,
            "override: bottom half"
        );
        assert_eq!(l.decide(1, 5, 10, 5, true), Move::Stay);
        assert_eq!(
            l.decide(0, 9, 10, 1200, true),
            Move::Promote,
            "score target beats rank"
        );
        assert_eq!(l.decide(2, 1, 10, 5000, true), Move::Stay, "top tier");
        assert_eq!(
            l.decide(0, 9, 10, 900, false),
            Move::Promote,
            "asc board: at or below target"
        );
        l.overrides.insert("7".into(), TierRule::default());
        assert!(l.validate().is_err(), "override for a missing tier");
    }

    #[test]
    fn cron_windows() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(); // 2023-11-14 22:13:20 UTC
        let (prev, next) = window(Some("0 0 * * *"), now).unwrap();
        assert_eq!(prev.unix_timestamp(), 1_699_920_000); // 2023-11-14 00:00
        assert_eq!(next.unix_timestamp(), 1_700_006_400); // 2023-11-15 00:00
        assert!(window(Some("bad"), now).is_err());
        // local midnight in Tokyo: 2023-11-15 00:00 JST = 2023-11-14 15:00 UTC
        let (prev, next) = window(Some("CRON_TZ=Asia/Tokyo 0 0 * * *"), now).unwrap();
        assert_eq!(prev.unix_timestamp(), 1_699_974_000);
        assert_eq!(next.unix_timestamp(), 1_700_060_400);
        // New York, across the DST change: 2023-11-05 02:00 EDT → EST; midnight stays midnight
        let (prev, next) = window(Some("TZ=America/New_York 0 0 * * *"), now).unwrap();
        assert_eq!(prev.unix_timestamp(), 1_699_938_000); // 2023-11-14 00:00 EST = 05:00 UTC
        assert_eq!(next.unix_timestamp(), 1_700_024_400); // 2023-11-15 00:00 EST
        assert!(window(Some("CRON_TZ=Mars/Olympus 0 0 * * *"), now).is_err());
        assert!(window(Some("CRON_TZ=Asia/Tokyo"), now).is_err());
        assert_eq!(
            window(None, now).unwrap(),
            (OffsetDateTime::UNIX_EPOCH, OffsetDateTime::UNIX_EPOCH)
        );
    }
}
