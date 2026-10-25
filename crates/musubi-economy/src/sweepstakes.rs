//! Sweepstakes / raffles (PlayFab Sweepstakes parity): a definition sells entries for
//! `entry_cost` (per entry, any currencies) during `[starts_at, ends_at)`, capped at
//! `max_entries_per_player`; the draw — at `ends_at` by the sweeper, or forced by the operator —
//! picks `winners` distinct accounts weighted by their entries from a seeded, reproducible PRNG,
//! grants each `prize_table` and, optionally, `consolation_table` to everyone else. `cancel`
//! refunds every entry.

use std::collections::BTreeMap;

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::EconomyService;

pub const MAX_DURATION_SECS: i64 = 365 * 86_400;
pub const MAX_WINNERS: i32 = 1_000;
pub const MAX_ENTRIES_PER_PLAYER: i32 = 10_000;
/// Consolation prizes are granted inline; above this many entrants they are skipped.
pub const MAX_CONSOLATION: i64 = 10_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sweepstake {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Price of one entry, per currency.
    pub entry_cost: BTreeMap<String, i64>,
    pub max_entries_per_player: i32,
    pub winners: i32,
    /// Reward table granted to each winner.
    pub prize_table: String,
    /// Reward table granted to every entrant who did not win.
    pub consolation_table: Option<String>,
    pub starts_at: i64,
    pub ends_at: i64,
    /// `open` | `drawn` | `cancelled`.
    pub status: String,
    pub enabled: bool,
    pub metadata: Value,
    pub seed: Option<String>,
    pub drawn_at: Option<i64>,
    pub entrants: i64,
    pub entries: i64,
}

impl Default for Sweepstake {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            entry_cost: BTreeMap::new(),
            max_entries_per_player: 1,
            winners: 1,
            prize_table: String::new(),
            consolation_table: None,
            starts_at: 0,
            ends_at: 0,
            status: "open".into(),
            enabled: true,
            metadata: json!({}),
            seed: None,
            drawn_at: None,
            entrants: 0,
            entries: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub account_id: AccountId,
    pub entries: i32,
    pub paid: Value,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Winner {
    pub rank: i32,
    pub account_id: AccountId,
    pub rewards: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Drawn {
    pub sweepstake: Sweepstake,
    pub winners: Vec<Winner>,
    pub consolations: i64,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: String,
    name: String,
    description: String,
    entry_cost: Value,
    max_entries_per_player: i32,
    winners: i32,
    prize_table: String,
    consolation_table: Option<String>,
    starts_at: OffsetDateTime,
    ends_at: OffsetDateTime,
    status: String,
    enabled: bool,
    metadata: Value,
    seed: Option<String>,
    drawn_at: Option<OffsetDateTime>,
    entrants: i64,
    entries: i64,
}

const COLS: &str = "s.id, s.name, s.description, s.entry_cost, s.max_entries_per_player, s.winners,
    s.prize_table, s.consolation_table, s.starts_at, s.ends_at, s.status, s.enabled, s.metadata,
    s.seed, s.drawn_at,
    (select count(*) from musubi.sweepstake_entries e where e.sweepstake_id = s.id) as entrants,
    (select coalesce(sum(e.entries), 0)::bigint from musubi.sweepstake_entries e where e.sweepstake_id = s.id) as entries";

fn view(r: Row) -> Sweepstake {
    Sweepstake {
        id: r.id,
        name: r.name,
        description: r.description,
        entry_cost: serde_json::from_value(r.entry_cost).unwrap_or_default(),
        max_entries_per_player: r.max_entries_per_player,
        winners: r.winners,
        prize_table: r.prize_table,
        consolation_table: r.consolation_table,
        starts_at: r.starts_at.unix_timestamp(),
        ends_at: r.ends_at.unix_timestamp(),
        status: r.status,
        enabled: r.enabled,
        metadata: r.metadata,
        seed: r.seed,
        drawn_at: r.drawn_at.map(|t| t.unix_timestamp()),
        entrants: r.entrants,
        entries: r.entries,
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fn ts(secs: i64) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(secs)
        .map_err(|_| Error::InvalidArgument("timestamp out of range".into()))
}

/// SplitMix64 seeded from a string — the draw is reproducible from `seed` alone.
struct Rng(u64);

impl Rng {
    fn from_seed(seed: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in seed.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Self(h)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        if n <= 1 {
            return 0;
        }
        // rejection sampling keeps the draw unbiased
        let zone = u64::MAX - (u64::MAX % n);
        loop {
            let v = self.next();
            if v < zone {
                return v % n;
            }
        }
    }
}

/// Weighted draw without replacement: `pool` = (account, weight) sorted by account for
/// determinism; returns up to `k` distinct accounts.
pub fn pick_winners(mut pool: Vec<(Uuid, i64)>, k: usize, seed: &str) -> Vec<Uuid> {
    pool.sort_by_key(|(a, _)| *a);
    let mut rng = Rng::from_seed(seed);
    let mut out = Vec::new();
    while out.len() < k && !pool.is_empty() {
        let total: i64 = pool.iter().map(|(_, w)| (*w).max(0)).sum();
        if total <= 0 {
            break;
        }
        let mut r = rng.below(total as u64) as i64;
        let mut idx = pool.len() - 1;
        for (i, (_, w)) in pool.iter().enumerate() {
            if r < *w {
                idx = i;
                break;
            }
            r -= w;
        }
        out.push(pool.remove(idx).0);
    }
    out
}

impl EconomyService {
    pub async fn upsert_sweepstake(&self, mut d: Sweepstake) -> Result<Sweepstake> {
        if !valid_id(&d.id) {
            return Err(Error::InvalidArgument(
                "sweepstake id: 1..=64 of [a-z0-9_-]".into(),
            ));
        }
        if d.name.len() > 128 || d.description.len() > 2048 {
            return Err(Error::InvalidArgument(
                "name ≤ 128, description ≤ 2048".into(),
            ));
        }
        if d.winners < 1 || d.winners > MAX_WINNERS {
            return Err(Error::InvalidArgument(format!(
                "winners: 1 .. {MAX_WINNERS}"
            )));
        }
        if d.max_entries_per_player < 1 || d.max_entries_per_player > MAX_ENTRIES_PER_PLAYER {
            return Err(Error::InvalidArgument(format!(
                "max_entries_per_player: 1 .. {MAX_ENTRIES_PER_PLAYER}"
            )));
        }
        if d.prize_table.is_empty() {
            return Err(Error::InvalidArgument("prize_table is required".into()));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if d.starts_at == 0 {
            d.starts_at = now;
        }
        if d.ends_at <= d.starts_at {
            return Err(Error::InvalidArgument(
                "ends_at must be after starts_at".into(),
            ));
        }
        if d.ends_at - d.starts_at > MAX_DURATION_SECS {
            return Err(Error::InvalidArgument("duration: up to 365 days".into()));
        }
        for (cur, amount) in &d.entry_cost {
            if cur.is_empty() || *amount < 0 {
                return Err(Error::InvalidArgument(
                    "entry_cost: currency → non-negative amount".into(),
                ));
            }
        }
        if !d.metadata.is_object() {
            if d.metadata.is_null() {
                d.metadata = json!({});
            } else {
                return Err(Error::InvalidArgument("metadata must be an object".into()));
            }
        }
        for t in std::iter::once(&d.prize_table).chain(d.consolation_table.iter()) {
            let exists: bool = sqlx::query_scalar(
                "select exists(select 1 from musubi.reward_tables where id = $1)",
            )
            .bind(t)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
            if !exists {
                return Err(Error::NotFound(format!("reward table {t}")));
            }
        }
        let current: Option<String> =
            sqlx::query_scalar("select status from musubi.sweepstakes where id = $1")
                .bind(&d.id)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        if matches!(current.as_deref(), Some("drawn") | Some("cancelled")) {
            return Err(Error::Conflict(format!(
                "sweepstake {} is {}; create a new one",
                d.id,
                current.unwrap_or_default()
            )));
        }
        let row: Row = sqlx::query_as(&format!(
            "insert into musubi.sweepstakes as s (id, name, description, entry_cost, max_entries_per_player,
                 winners, prize_table, consolation_table, starts_at, ends_at, enabled, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             on conflict (id) do update set name = excluded.name, description = excluded.description,
                 entry_cost = excluded.entry_cost, max_entries_per_player = excluded.max_entries_per_player,
                 winners = excluded.winners, prize_table = excluded.prize_table,
                 consolation_table = excluded.consolation_table, starts_at = excluded.starts_at,
                 ends_at = excluded.ends_at, enabled = excluded.enabled, metadata = excluded.metadata,
                 updated_at = now()
             returning {COLS}"
        ))
        .bind(&d.id)
        .bind(&d.name)
        .bind(&d.description)
        .bind(json!(d.entry_cost))
        .bind(d.max_entries_per_player)
        .bind(d.winners)
        .bind(&d.prize_table)
        .bind(&d.consolation_table)
        .bind(ts(d.starts_at)?)
        .bind(ts(d.ends_at)?)
        .bind(d.enabled)
        .bind(&d.metadata)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(view(row))
    }

    pub async fn sweepstake(&self, id: &str) -> Result<Sweepstake> {
        let row: Option<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.sweepstakes s where s.id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(view)
            .ok_or_else(|| Error::NotFound(format!("sweepstake {id}")))
    }

    /// `all`: every definition (operators); otherwise enabled ones a player may see — open now or
    /// drawn within the last 30 days.
    pub async fn sweepstakes(&self, all: bool) -> Result<Vec<Sweepstake>> {
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.sweepstakes s
             where $1 or (s.enabled and ((s.status = 'open' and s.starts_at <= now())
                   or (s.status = 'drawn' and s.drawn_at > now() - interval '30 days')))
             order by s.ends_at desc, s.id limit 500"
        ))
        .bind(all)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(view).collect())
    }

    pub async fn delete_sweepstake(&self, id: &str) -> Result<()> {
        let n = sqlx::query("delete from musubi.sweepstakes where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        if n == 0 {
            return Err(Error::NotFound(format!("sweepstake {id}")));
        }
        Ok(())
    }

    /// Buy `count` entries: pays `entry_cost × count`, respects the per-player cap and the window.
    pub async fn enter_sweepstake(
        &self,
        account: AccountId,
        id: &str,
        count: i32,
    ) -> Result<Entry> {
        if count < 1 {
            return Err(Error::InvalidArgument("entries must be >= 1".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.sweepstakes s where s.id = $1 for update of s"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(d) = row.map(view) else {
            return Err(Error::NotFound(format!("sweepstake {id}")));
        };
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if !d.enabled || d.status != "open" || now < d.starts_at || now >= d.ends_at {
            return Err(Error::Conflict(format!(
                "sweepstake {id} is not accepting entries"
            )));
        }
        let have: i32 = sqlx::query_scalar(
            "select coalesce((select entries from musubi.sweepstake_entries where sweepstake_id = $1 and account_id = $2), 0)",
        )
        .bind(id)
        .bind(account.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let total = have
            .checked_add(count)
            .ok_or_else(|| Error::InvalidArgument("entries overflow".into()))?;
        if total > d.max_entries_per_player {
            return Err(Error::ResourceExhausted(format!(
                "at most {} entries per player (you have {have})",
                d.max_entries_per_player
            )));
        }
        let mut paid = BTreeMap::new();
        for (cur, amount) in &d.entry_cost {
            let total = amount
                .checked_mul(count as i64)
                .ok_or_else(|| Error::InvalidArgument("entry cost overflow".into()))?;
            if total == 0 {
                continue;
            }
            Self::adjust_tx(
                &mut tx,
                account,
                cur,
                -total,
                "sweepstake_entry",
                &format!("sweepstake:{id}"),
                None,
                json!({ "sweepstake": id, "entries": count }),
            )
            .await?;
            paid.insert(cur.clone(), total);
        }
        let (entries, paid_all, updated_at): (i32, Value, OffsetDateTime) = sqlx::query_as(
            "insert into musubi.sweepstake_entries (sweepstake_id, account_id, entries, paid)
             values ($1, $2, $3, $4)
             on conflict (sweepstake_id, account_id) do update set
                 entries = musubi.sweepstake_entries.entries + excluded.entries,
                 paid = (select coalesce(jsonb_object_agg(k, v), '{}'::jsonb) from (
                     select k, sum(v::bigint) as v from (
                         select key as k, value as v from jsonb_each_text(musubi.sweepstake_entries.paid)
                         union all
                         select key, value from jsonb_each_text(excluded.paid)) u group by k) agg),
                 updated_at = now()
             returning entries, paid, updated_at",
        )
        .bind(id)
        .bind(account.0)
        .bind(count)
        .bind(json!(paid))
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(Entry {
            account_id: account,
            entries,
            paid: paid_all,
            updated_at: updated_at.unix_timestamp(),
        })
    }

    pub async fn sweepstake_entries(&self, id: &str) -> Result<Vec<Entry>> {
        self.sweepstake(id).await?;
        let rows: Vec<(Uuid, i32, Value, OffsetDateTime)> = sqlx::query_as(
            "select account_id, entries, paid, updated_at from musubi.sweepstake_entries
             where sweepstake_id = $1 order by entries desc, account_id limit 10000",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(a, e, p, t)| Entry {
                account_id: AccountId(a),
                entries: e,
                paid: p,
                updated_at: t.unix_timestamp(),
            })
            .collect())
    }

    pub async fn sweepstake_entry_of(&self, id: &str, account: AccountId) -> Result<Option<Entry>> {
        let row: Option<(i32, Value, OffsetDateTime)> = sqlx::query_as(
            "select entries, paid, updated_at from musubi.sweepstake_entries where sweepstake_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(|(e, p, t)| Entry {
            account_id: account,
            entries: e,
            paid: p,
            updated_at: t.unix_timestamp(),
        }))
    }

    pub async fn sweepstake_winners(&self, id: &str) -> Result<Vec<Winner>> {
        let rows: Vec<(i32, Uuid, Value)> = sqlx::query_as(
            "select rank, account_id, rewards from musubi.sweepstake_winners where sweepstake_id = $1 order by rank",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(r, a, w)| Winner {
                rank: r,
                account_id: AccountId(a),
                rewards: w,
            })
            .collect())
    }

    /// Draw: `force` before `ends_at`; `seed` makes the result reproducible (defaults to a fresh
    /// UUID stored on the row). Winners are chosen and recorded in one transaction, prizes granted
    /// right after (each grant is its own ledger transaction keyed `sweepstake:{id}:{rank}`).
    pub async fn draw_sweepstake(
        &self,
        id: &str,
        seed: Option<String>,
        force: bool,
    ) -> Result<Drawn> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<Row> = sqlx::query_as(&format!(
            "select {COLS} from musubi.sweepstakes s where s.id = $1 for update of s"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(d) = row.map(view) else {
            return Err(Error::NotFound(format!("sweepstake {id}")));
        };
        if d.status != "open" {
            return Err(Error::Conflict(format!("sweepstake {id} is {}", d.status)));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        if now < d.ends_at && !force {
            return Err(Error::Conflict(format!(
                "sweepstake {id} ends at {}; pass force to draw early",
                d.ends_at
            )));
        }
        let pool: Vec<(Uuid, i64)> = sqlx::query_as(
            "select account_id, entries::bigint from musubi.sweepstake_entries where sweepstake_id = $1 and entries > 0",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let seed = seed
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| Uuid::now_v7().to_string());
        if seed.len() > 128 {
            return Err(Error::InvalidArgument("seed: up to 128 chars".into()));
        }
        let winners = pick_winners(pool.clone(), d.winners as usize, &seed);
        for (rank, a) in winners.iter().enumerate() {
            sqlx::query(
                "insert into musubi.sweepstake_winners (sweepstake_id, rank, account_id) values ($1, $2, $3)",
            )
            .bind(id)
            .bind(rank as i32 + 1)
            .bind(a)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        sqlx::query(
            "update musubi.sweepstakes set status = 'drawn', seed = $2, drawn_at = now(), updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(&seed)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let mut out = Vec::new();
        for (i, a) in winners.iter().enumerate() {
            let rank = i as i32 + 1;
            let rewards = self
                .grant_table(
                    AccountId(*a),
                    &d.prize_table,
                    &format!("sweepstake:{id}:{rank}"),
                )
                .await?;
            let rewards = json!(rewards);
            sqlx::query(
                "update musubi.sweepstake_winners set rewards = $3 where sweepstake_id = $1 and rank = $2",
            )
            .bind(id)
            .bind(rank)
            .bind(&rewards)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
            out.push(Winner {
                rank,
                account_id: AccountId(*a),
                rewards,
            });
        }
        let mut consolations = 0;
        if let Some(t) = &d.consolation_table {
            if pool.len() as i64 <= MAX_CONSOLATION {
                for (a, _) in pool.iter().filter(|(a, _)| !winners.contains(a)) {
                    self.grant_table(AccountId(*a), t, &format!("sweepstake:{id}:consolation"))
                        .await?;
                    consolations += 1;
                }
            }
        }
        Ok(Drawn {
            sweepstake: self.sweepstake(id).await?,
            winners: out,
            consolations,
        })
    }

    /// Cancel an open sweepstake and refund every entry.
    pub async fn cancel_sweepstake(&self, id: &str) -> Result<(Sweepstake, i64)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let status: Option<String> =
            sqlx::query_scalar("select status from musubi.sweepstakes where id = $1 for update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        match status.as_deref() {
            None => return Err(Error::NotFound(format!("sweepstake {id}"))),
            Some("open") => {}
            Some(st) => return Err(Error::Conflict(format!("sweepstake {id} is {st}"))),
        }
        let paid: Vec<(Uuid, Value)> = sqlx::query_as(
            "select account_id, paid from musubi.sweepstake_entries where sweepstake_id = $1",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let mut refunded = 0i64;
        for (a, p) in &paid {
            let m: BTreeMap<String, i64> = serde_json::from_value(p.clone()).unwrap_or_default();
            for (cur, amount) in m {
                if amount <= 0 {
                    continue;
                }
                Self::adjust_tx(
                    &mut tx,
                    AccountId(*a),
                    &cur,
                    amount,
                    "sweepstake_refund",
                    &format!("sweepstake:{id}"),
                    None,
                    json!({ "sweepstake": id }),
                )
                .await?;
                refunded += 1;
            }
        }
        sqlx::query(
            "update musubi.sweepstakes set status = 'cancelled', updated_at = now() where id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((self.sweepstake(id).await?, refunded))
    }

    /// Draw every enabled open sweepstake whose window has ended; returns the drawn ids.
    pub async fn draw_due_sweepstakes(&self) -> Result<Vec<String>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "select id from musubi.sweepstakes where status = 'open' and enabled and ends_at <= now() order by ends_at limit 100",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut drawn = Vec::new();
        for id in ids {
            match self.draw_sweepstake(&id, None, false).await {
                Ok(_) => drawn.push(id),
                Err(Error::Conflict(_)) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(drawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_is_deterministic_weighted_and_distinct() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let c = Uuid::from_u128(3);
        let pool = vec![(a, 1), (b, 1000), (c, 1)];
        let w1 = pick_winners(pool.clone(), 2, "seed");
        let w2 = pick_winners(pool.clone(), 2, "seed");
        assert_eq!(w1, w2);
        assert_eq!(w1.len(), 2);
        assert_ne!(w1[0], w1[1]);
        assert_eq!(w1[0], b, "heavy entrant wins first");
        assert_eq!(
            pick_winners(pool.clone(), 10, "x").len(),
            3,
            "capped by entrants"
        );
        assert!(pick_winners(vec![], 3, "x").is_empty());
        assert!(pick_winners(vec![(a, 0)], 1, "x").is_empty());
        let mut hits = 0;
        for i in 0..200 {
            if pick_winners(vec![(a, 1), (c, 1)], 1, &format!("s{i}"))[0] == a {
                hits += 1;
            }
        }
        assert!((60..=140).contains(&hits), "fair coin: {hits}/200");
    }
}
