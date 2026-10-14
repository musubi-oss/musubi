//! Leaderboards: definitions, score submission with operators, ranked listing and reset windows.

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
    /// 1-based rank within the current window.
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
);
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
    i64,
);
const RECORD_COLS: &str = "leaderboard_id, owner_id, username, score, subscore, num_score, metadata, created_at, updated_at, expiry_time";

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
pub fn window(
    schedule: Option<&str>,
    now: OffsetDateTime,
) -> Result<(OffsetDateTime, OffsetDateTime)> {
    let Some(s) = schedule else {
        return Ok((OffsetDateTime::UNIX_EPOCH, OffsetDateTime::UNIX_EPOCH));
    };
    let sched = parse_cron(s)?;
    let now_c = chrono::DateTime::<chrono::Utc>::from_timestamp(now.unix_timestamp(), 0)
        .ok_or_else(|| Error::Internal("bad time".into()))?;
    let next = sched
        .after(&now_c)
        .next()
        .ok_or_else(|| Error::InvalidArgument("schedule never fires".into()))?;
    // cron 0.12 has no `before`; walk back from a bounded start to find the latest fire <= now.
    let mut prev = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap();
    let mut probe = now_c - chrono::Duration::days(366 * 2);
    for fire in sched.after(&probe) {
        if fire > now_c {
            break;
        }
        prev = fire;
        probe = fire;
    }
    let _ = probe;
    let to_t = |d: chrono::DateTime<chrono::Utc>| {
        OffsetDateTime::from_unix_timestamp(d.timestamp()).unwrap_or(OffsetDateTime::UNIX_EPOCH)
    };
    Ok((to_t(prev), to_t(next)))
}

fn parse_cron(s: &str) -> Result<cron::Schedule> {
    // Accept standard 5-field crontab; the `cron` crate wants seconds first.
    let fields = s.split_whitespace().count();
    let spec = match fields {
        5 => format!("0 {s}"),
        6 | 7 => s.to_string(),
        _ => {
            return Err(Error::InvalidArgument(
                "reset_schedule must be a 5-field cron expression".into(),
            ))
        }
    };
    cron::Schedule::from_str(&spec)
        .map_err(|e| Error::InvalidArgument(format!("bad reset_schedule: {e}")))
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
        rank: r.10,
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
        let res = sqlx::query_as::<_, BoardRow>(
            "insert into musubi.leaderboards (id, authoritative, sort_order, operator, reset_schedule, metadata)
             values ($1, $2, $3, $4, $5, coalesce($6, '{}'::jsonb))
             returning id, authoritative, sort_order, operator, reset_schedule, metadata, created_at",
        )
        .bind(&c.id)
        .bind(c.authoritative)
        .bind(c.sort_order as i16)
        .bind(c.operator as i16)
        .bind(&c.reset_schedule)
        .bind(&c.metadata)
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
        let r = sqlx::query_as::<_, BoardRow>("select id, authoritative, sort_order, operator, reset_schedule, metadata, created_at from musubi.leaderboards where id = $1")
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
        let rows = sqlx::query_as::<_, BoardRow>(
            "select id, authoritative, sort_order, operator, reset_schedule, metadata, created_at from musubi.leaderboards where ($1::text is null or id > $1) order by id limit $2",
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
        let (_, expiry) = window(board.reset_schedule.as_deref(), now)?;
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
            "insert into musubi.leaderboard_records as r (leaderboard_id, owner_id, username, score, subscore, num_score, metadata, expiry_time)
             values ($1, $2, $3, $4, $5, 1, coalesce($6, '{{}}'::jsonb), $7)
             on conflict (leaderboard_id, expiry_time, owner_id) do update set {update},
                 num_score = r.num_score + 1, username = coalesce(excluded.username, r.username),
                 metadata = coalesce($6, r.metadata), updated_at = now()
             returning {RECORD_COLS}, 0::bigint"
        );
        let row = sqlx::query_as::<_, RecordRow>(&sql)
            .bind(id)
            .bind(owner.0)
            .bind(username)
            .bind(w.score)
            .bind(w.subscore)
            .bind(&w.metadata)
            .bind(expiry)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
        let mut rec = to_record(row);
        rec.rank = self.rank_of(&board, expiry, owner).await?;
        Ok(rec)
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
               and o.leaderboard_id = r.leaderboard_id and o.expiry_time = r.expiry_time and ({cmp})"
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
        let sql = format!(
            "select {RECORD_COLS}, row_number() over (order by {order}) as rank
             from musubi.leaderboard_records where leaderboard_id = $1 and expiry_time = $2
             order by rank offset $3 limit $4"
        );
        let rows = sqlx::query_as::<_, RecordRow>(&sql)
            .bind(id)
            .bind(expiry)
            .bind(offset)
            .bind(limit + 1)
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
                "select * from (select {RECORD_COLS}, row_number() over (order by {order}) as rank
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
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_windows() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap(); // 2023-11-14 22:13:20 UTC
        let (prev, next) = window(Some("0 0 * * *"), now).unwrap();
        assert_eq!(prev.unix_timestamp(), 1_699_920_000); // 2023-11-14 00:00
        assert_eq!(next.unix_timestamp(), 1_700_006_400); // 2023-11-15 00:00
        assert!(window(Some("bad"), now).is_err());
        assert_eq!(
            window(None, now).unwrap(),
            (OffsetDateTime::UNIX_EPOCH, OffsetDateTime::UNIX_EPOCH)
        );
    }
}
