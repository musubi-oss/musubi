//! Tournaments: leaderboards with start/end, active duration per reset window, size and submission caps.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::{
    parse_cron, validate_id, window, Operator, Record, RecordList, SortOrder, WriteRecord,
};

/// A rank band of a tournament window and the reward table every owner in it receives by mail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prize {
    pub from_rank: i32,
    pub to_rank: i32,
    /// Player tournaments: the reward table mailed to every owner in the band.
    #[serde(default)]
    pub reward_table: String,
    /// Team tournaments: currency and amount paid into each group's treasury.
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub amount: Option<i64>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
}

type PrizeRow = (
    i32,
    i32,
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
);

/// Records of a team tournament are owned by groups; a member needs these role bits to write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamRules {
    #[serde(default = "default_team_perm")]
    pub perm: i64,
}

fn default_team_perm() -> i64 {
    8
}

#[derive(Debug, Clone, Serialize)]
pub struct Tournament {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: i32,
    pub sort_order: SortOrder,
    pub operator: Operator,
    pub reset_schedule: Option<String>,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub duration: i32,
    pub max_size: i32,
    pub max_num_score: i32,
    pub join_required: bool,
    pub authoritative: bool,
    /// Joining needs the join code given at creation (the code itself is never returned).
    pub private: bool,
    /// Participants in the current window.
    pub size: i64,
    /// Whether a new participant could join/submit right now.
    pub can_enter: bool,
    /// Unix seconds when the current active phase ends (0 = not active).
    pub end_active: i64,
    pub next_reset: i64,
    pub prev_reset: i64,
    /// Filled by `get` and `create`; empty in listings.
    #[serde(default)]
    pub prizes: Vec<Prize>,
    /// Set for team tournaments (filled by `get` and `create`).
    #[serde(default)]
    pub team: Option<TeamRules>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTournament {
    pub id: String,
    /// Makes the tournament private: `join` must present this code.
    #[serde(default)]
    pub join_code: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category: i32,
    #[serde(default)]
    pub sort_order: SortOrder,
    #[serde(default)]
    pub operator: Operator,
    #[serde(default)]
    pub reset_schedule: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Unix seconds; 0 = now.
    #[serde(default)]
    pub start_time: i64,
    #[serde(default)]
    pub end_time: Option<i64>,
    /// Active seconds per window (0 = the whole window). Required when there is no reset schedule and no end.
    #[serde(default)]
    pub duration: i32,
    #[serde(default)]
    pub max_size: i32,
    #[serde(default)]
    pub max_num_score: i32,
    #[serde(default)]
    pub join_required: bool,
    #[serde(default)]
    pub authoritative: bool,
    #[serde(default)]
    pub prizes: Vec<Prize>,
    #[serde(default)]
    pub team: Option<TeamRules>,
}

#[derive(Debug, Clone, Default)]
pub struct TournamentFilter {
    pub category_start: Option<i32>,
    pub category_end: Option<i32>,
    /// Only tournaments whose [start, end) overlaps [start_time, end_time].
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
}

#[derive(Clone)]
pub struct TournamentService {
    pub(crate) db: PgPool,
}

type Row = (
    String,
    String,
    String,
    i32,
    i16,
    i16,
    Option<String>,
    serde_json::Value,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
    i32,
    i32,
    i32,
    bool,
    bool,
);
const COLS: &str = "id, title, description, category, sort_order, operator, reset_schedule, metadata, created_at, start_time, end_time, duration, max_size, max_num_score, join_required, authoritative";

/// Phase of a tournament at `now`: current window [ws, we) (we = epoch when it never resets) and the
/// active deadline inside it.
struct Phase {
    window_start: OffsetDateTime,
    expiry: OffsetDateTime,
    end_active: Option<OffsetDateTime>,
    active: bool,
}

fn phase(r: &Row, now: OffsetDateTime) -> Result<Phase> {
    let start = r.9;
    let end = r.10;
    let (prev, next) = window(r.6.as_deref(), now)?;
    let window_start = if prev > start { prev } else { start };
    let expiry = if r.6.is_some() {
        next
    } else {
        OffsetDateTime::UNIX_EPOCH
    };
    let duration = r.11;
    let mut end_active = if duration > 0 {
        Some(window_start + time::Duration::seconds(duration as i64))
    } else {
        None
    };
    if let Some(e) = end {
        end_active = Some(end_active.map_or(e, |a| a.min(e)));
    }
    if r.6.is_some() {
        end_active = Some(end_active.map_or(next, |a| a.min(next)));
    }
    let started = now >= start;
    let ended = end.is_some_and(|e| now >= e);
    let in_active = end_active.is_none_or(|a| now < a);
    Ok(Phase {
        window_start,
        expiry,
        end_active,
        active: started && !ended && in_active,
    })
}

async fn size_of(db: &PgPool, id: &str, expiry: OffsetDateTime) -> Result<i64> {
    sqlx::query_scalar("select count(*) from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2")
        .bind(id)
        .bind(expiry)
        .fetch_one(db)
        .await
        .map_err(Error::internal)
}

fn to_tournament(r: Row, p: &Phase, size: i64) -> Tournament {
    let ts = |t: OffsetDateTime| {
        if t == OffsetDateTime::UNIX_EPOCH {
            0
        } else {
            t.unix_timestamp()
        }
    };
    let max_size = r.12;
    Tournament {
        id: r.0.clone(),
        title: r.1.clone(),
        description: r.2.clone(),
        category: r.3,
        sort_order: if r.4 == 0 {
            SortOrder::Asc
        } else {
            SortOrder::Desc
        },
        operator: match r.5 {
            1 => Operator::Set,
            2 => Operator::Incr,
            3 => Operator::Decr,
            _ => Operator::Best,
        },
        reset_schedule: r.6.clone(),
        metadata: r.7.clone(),
        create_time: r.8,
        start_time: r.9.unix_timestamp(),
        end_time: r.10.map(|t| t.unix_timestamp()),
        duration: r.11,
        max_size,
        max_num_score: r.13,
        join_required: r.14,
        authoritative: r.15,
        private: false,
        size,
        can_enter: p.active && (max_size == 0 || size < max_size as i64),
        end_active: if p.active {
            p.end_active.map(ts).unwrap_or(0)
        } else {
            0
        },
        next_reset: ts(p.expiry),
        prev_reset: ts(p.window_start),
        prizes: Vec::new(),
        team: None,
    }
}

impl TournamentService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    pub async fn create(&self, c: CreateTournament) -> Result<Tournament> {
        validate_id(&c.id)?;
        if let Some(s) = &c.reset_schedule {
            parse_cron(s)?;
        }
        if c.title.chars().count() > 256 || c.description.chars().count() > 1024 {
            return Err(Error::InvalidArgument(
                "title ≤ 256, description ≤ 1024 chars".into(),
            ));
        }
        Self::validate_prizes(&c.prizes)?;
        if c.duration < 0 || c.max_size < 0 || c.max_num_score < 0 {
            return Err(Error::InvalidArgument(
                "duration, max_size, max_num_score must be ≥ 0".into(),
            ));
        }
        if c.reset_schedule.is_none() && c.end_time.is_none() && c.duration == 0 {
            return Err(Error::InvalidArgument(
                "set duration, end_time or reset_schedule".into(),
            ));
        }
        if let Some(m) = &c.metadata {
            if !m.is_object() {
                return Err(Error::InvalidArgument("metadata must be an object".into()));
            }
        }
        let start = if c.start_time <= 0 {
            OffsetDateTime::now_utc()
        } else {
            OffsetDateTime::from_unix_timestamp(c.start_time)
                .map_err(|_| Error::InvalidArgument("bad start_time".into()))?
        };
        let end = match c.end_time {
            Some(e) if e > 0 => {
                let e = OffsetDateTime::from_unix_timestamp(e)
                    .map_err(|_| Error::InvalidArgument("bad end_time".into()))?;
                if e <= start {
                    return Err(Error::InvalidArgument(
                        "end_time must be after start_time".into(),
                    ));
                }
                Some(e)
            }
            _ => None,
        };
        let res = sqlx::query_as::<_, Row>(&format!(
            "insert into musubi.leaderboards (id, is_tournament, title, description, category, sort_order, operator, reset_schedule, metadata,
                 start_time, end_time, duration, max_size, max_num_score, join_required, authoritative, join_code)
             values ($1, true, $2, $3, $4, $5, $6, $7, coalesce($8, '{{}}'::jsonb), $9, $10, $11, $12, $13, $14, $15, $16)
             returning {COLS}"
        ))
        .bind(&c.id)
        .bind(&c.title)
        .bind(&c.description)
        .bind(c.category)
        .bind(c.sort_order as i16)
        .bind(c.operator as i16)
        .bind(&c.reset_schedule)
        .bind(&c.metadata)
        .bind(start)
        .bind(end)
        .bind(c.duration)
        .bind(c.max_size)
        .bind(c.max_num_score)
        .bind(c.join_required)
        .bind(c.authoritative)
        .bind(&c.join_code)
        .fetch_one(&self.db)
        .await;
        let row = match res {
            Ok(r) => r,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(Error::AlreadyExists("tournament exists".into()))
            }
            Err(e) => return Err(Error::internal(e)),
        };
        self.insert_prizes(&c.id, &c.prizes).await?;
        if let Some(team) = &c.team {
            sqlx::query("insert into musubi.tournament_team (tournament_id, perm) values ($1, $2)")
                .bind(&c.id)
                .bind(team.perm)
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
        }
        let p = phase(&row, OffsetDateTime::now_utc())?;
        let mut t = to_tournament(row, &p, 0);
        t.private = c.join_code.is_some();
        t.prizes = c.prizes;
        t.team = c.team;
        Ok(t)
    }

    pub async fn delete(&self, id: &str) -> Result<()> {
        let r = sqlx::query("delete from musubi.leaderboards where id = $1 and is_tournament")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("tournament".into()));
        }
        Ok(())
    }

    async fn row(&self, id: &str) -> Result<Row> {
        sqlx::query_as::<_, Row>(&format!(
            "select {COLS} from musubi.leaderboards where id = $1 and is_tournament"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("tournament".into()))
    }

    pub async fn get(&self, id: &str) -> Result<Tournament> {
        let r = self.row(id).await?;
        let p = phase(&r, OffsetDateTime::now_utc())?;
        let prizes = self.prizes(id).await?;
        let team = self.team(id).await?;
        let size = size_of(&self.db, id, p.expiry).await?;
        let mut t = to_tournament(r, &p, size);
        t.private = self.join_code(id).await?.is_some();
        t.prizes = prizes;
        t.team = team;
        Ok(t)
    }

    pub async fn prizes(&self, id: &str) -> Result<Vec<Prize>> {
        let rows: Vec<PrizeRow> = sqlx::query_as(
            "select from_rank, to_rank, reward_table, title, body, currency, amount from musubi.tournament_prizes
             where tournament_id = $1 order by from_rank",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(
                |(from_rank, to_rank, reward_table, title, body, currency, amount)| Prize {
                    from_rank,
                    to_rank,
                    reward_table,
                    currency,
                    amount,
                    title,
                    body,
                },
            )
            .collect())
    }

    pub async fn team(&self, id: &str) -> Result<Option<TeamRules>> {
        let perm: Option<i64> =
            sqlx::query_scalar("select perm from musubi.tournament_team where tournament_id = $1")
                .bind(id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(perm.map(|perm| TeamRules { perm }))
    }

    fn validate_prizes(prizes: &[Prize]) -> Result<()> {
        let mut sorted: Vec<&Prize> = prizes.iter().collect();
        sorted.sort_by_key(|p| p.from_rank);
        let mut last_to = 0;
        for p in sorted {
            if p.from_rank < 1 || p.to_rank < p.from_rank {
                return Err(Error::InvalidArgument(format!(
                    "prize band {}..{} is invalid",
                    p.from_rank, p.to_rank
                )));
            }
            if p.from_rank <= last_to {
                return Err(Error::InvalidArgument(format!(
                    "prize band {}..{} overlaps the previous one",
                    p.from_rank, p.to_rank
                )));
            }
            let money = p.currency.is_some() || p.amount.is_some();
            if p.reward_table.is_empty() == !money {
                return Err(Error::InvalidArgument(
                    "a prize is either a reward_table (players) or currency + amount (teams)"
                        .into(),
                ));
            }
            if money
                && (p.currency.as_deref().unwrap_or("").is_empty() || p.amount.unwrap_or(0) <= 0)
            {
                return Err(Error::InvalidArgument(
                    "team prize needs currency and a positive amount".into(),
                ));
            }
            last_to = p.to_rank;
        }
        Ok(())
    }

    async fn insert_prizes(&self, id: &str, prizes: &[Prize]) -> Result<()> {
        for p in prizes {
            sqlx::query(
                "insert into musubi.tournament_prizes (tournament_id, from_rank, to_rank, reward_table, title, body, currency, amount)
                 values ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(id)
            .bind(p.from_rank)
            .bind(p.to_rank)
            .bind(&p.reward_table)
            .bind(&p.title)
            .bind(&p.body)
            .bind(&p.currency)
            .bind(p.amount)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        }
        Ok(())
    }

    /// Reserve the payout of one finished window; false when it was already claimed (another
    /// node, an earlier sweep, or a manual `end`).
    pub async fn claim_payout(&self, id: &str, expiry: i64) -> Result<bool> {
        let exp = OffsetDateTime::from_unix_timestamp(expiry)
            .map_err(|_| Error::InvalidArgument("bad expiry".into()))?;
        let r = sqlx::query(
            "insert into musubi.tournament_payouts (tournament_id, expiry_time) values ($1, $2) on conflict do nothing",
        )
        .bind(id)
        .bind(exp)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn record_payout(&self, id: &str, expiry: i64, paid: i32) -> Result<()> {
        let exp = OffsetDateTime::from_unix_timestamp(expiry)
            .map_err(|_| Error::InvalidArgument("bad expiry".into()))?;
        sqlx::query("update musubi.tournament_payouts set paid = $3 where tournament_id = $1 and expiry_time = $2")
            .bind(id)
            .bind(exp)
            .bind(paid)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Join code of a private tournament (sqlx tuples stop at 16 columns, so it is read apart).
    async fn join_code(&self, id: &str) -> Result<Option<String>> {
        sqlx::query_scalar("select join_code from musubi.leaderboards where id = $1")
            .bind(id)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)
    }

    pub async fn list(
        &self,
        f: TournamentFilter,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<(Vec<Tournament>, Option<String>)> {
        let limit = limit.clamp(1, 100) as i64;
        let to_t = |v: Option<i64>| {
            v.filter(|x| *x > 0)
                .map(|x| {
                    OffsetDateTime::from_unix_timestamp(x)
                        .map_err(|_| Error::InvalidArgument("bad time".into()))
                })
                .transpose()
        };
        let (fs, fe) = (to_t(f.start_time)?, to_t(f.end_time)?);
        let rows = sqlx::query_as::<_, Row>(&format!(
            "select {COLS} from musubi.leaderboards where is_tournament
               and ($1::int is null or category >= $1) and ($2::int is null or category <= $2)
               and ($3::timestamptz is null or end_time is null or end_time > $3)
               and ($4::timestamptz is null or start_time <= $4)
               and ($5::text is null or id > $5)
             order by id limit $6"
        ))
        .bind(f.category_start)
        .bind(f.category_end)
        .bind(fs)
        .bind(fe)
        .bind(cursor)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let now = OffsetDateTime::now_utc();
        let mut out = Vec::new();
        for r in rows.into_iter().take(limit as usize) {
            let p = phase(&r, now)?;
            let size = size_of(&self.db, &r.0, p.expiry).await?;
            let mut t = to_tournament(r, &p, size);
            t.private = self.join_code(&t.id).await?.is_some();
            out.push(t);
        }
        let cursor = more.then(|| out.last().map(|t| t.id.clone())).flatten();
        Ok((out, cursor))
    }

    /// Reserve a slot in the current window (records a zero-submission row).
    pub async fn join(
        &self,
        id: &str,
        owner: AccountId,
        username: Option<&str>,
        code: Option<&str>,
    ) -> Result<()> {
        let r = self.row(id).await?;
        if let Some(expected) = self.join_code(id).await? {
            if code != Some(expected.as_str()) {
                return Err(Error::PermissionDenied(
                    "tournament is private: a join code is required".into(),
                ));
            }
        }
        let p = phase(&r, OffsetDateTime::now_utc())?;
        if !p.active {
            return Err(Error::Unavailable("tournament is not active".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query("select 1 from musubi.leaderboards where id = $1 for update")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let exists: Option<i64> = sqlx::query_scalar("select 1::bigint from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3")
            .bind(id)
            .bind(p.expiry)
            .bind(owner.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
        if exists.is_some() {
            return Ok(());
        }
        if r.12 > 0 {
            let size: i64 = sqlx::query_scalar("select count(*) from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2")
                .bind(id)
                .bind(p.expiry)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
            if size >= r.12 as i64 {
                return Err(Error::ResourceExhausted("tournament is full".into()));
            }
        }
        sqlx::query("insert into musubi.leaderboard_records (leaderboard_id, owner_id, username, score, subscore, num_score, expiry_time) values ($1, $2, $3, 0, 0, 0, $4)")
            .bind(id)
            .bind(owner.0)
            .bind(username)
            .bind(p.expiry)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)
    }

    pub async fn write(
        &self,
        lb: &crate::LeaderboardService,
        id: &str,
        owner: AccountId,
        username: Option<&str>,
        w: WriteRecord,
        server: bool,
    ) -> Result<Record> {
        let r = self.row(id).await?;
        let p = phase(&r, OffsetDateTime::now_utc())?;
        if !p.active {
            return Err(Error::Unavailable("tournament is not active".into()));
        }
        let joined: Option<i32> = sqlx::query_scalar("select num_score from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2 and owner_id = $3")
            .bind(id)
            .bind(p.expiry)
            .bind(owner.0)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
        match joined {
            None if r.14 && !server => {
                return Err(Error::PermissionDenied("join the tournament first".into()))
            }
            // a private tournament has to be joined explicitly, with its code
            None => self.join(id, owner, username, None).await?,
            Some(n) if r.13 > 0 && n >= r.13 => {
                return Err(Error::ResourceExhausted("submission limit reached".into()))
            }
            Some(_) => {}
        }
        lb.write(id, owner, username, w, server).await
    }

    pub async fn records(
        &self,
        lb: &crate::LeaderboardService,
        id: &str,
        owners: &[AccountId],
        limit: usize,
        cursor: Option<&str>,
        expiry: Option<i64>,
    ) -> Result<RecordList> {
        self.row(id).await?;
        lb.records(id, owners, limit, cursor, expiry, None).await
    }

    pub async fn around(
        &self,
        lb: &crate::LeaderboardService,
        id: &str,
        owner: AccountId,
        limit: usize,
        expiry: Option<i64>,
    ) -> Result<RecordList> {
        self.row(id).await?;
        lb.around(id, owner, limit, expiry).await
    }
}
