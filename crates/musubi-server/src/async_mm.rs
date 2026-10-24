//! Asynchronous PvP matchmaking (P1-55): a player picks a defender from the rating ladder of a
//! queue and plays against their stored base/deck/team while they are offline. Rules per queue
//! (spread, cooldowns, attacks per day, shields, revenge); shields make a player unattackable;
//! results feed the rating queue.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use time::OffsetDateTime;
use uuid::Uuid;

pub const RESULTS: [&str; 3] = ["win", "loss", "draw"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShieldPrice {
    pub currency: String,
    pub per_hour: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Rules {
    pub enabled: bool,
    /// Display-rating distance for the first pick and the widest allowed.
    pub spread: f64,
    pub max_spread: f64,
    /// Default/maximum opponents per request.
    pub count: usize,
    pub max_count: usize,
    /// Defenders with fewer rated games are not offered.
    pub min_games: i32,
    /// Same attacker → same defender no more often than this.
    pub defender_cooldown_secs: i64,
    /// An attack not resolved within this expires (no rating change).
    pub attack_ttl_secs: i64,
    /// 0 = unlimited.
    pub attacks_per_day: i64,
    pub max_open: i64,
    /// Beaten defender gets this long of shield (0 = none).
    pub shield_on_loss_secs: i64,
    /// A shield can never reach further than this into the future.
    pub shield_max_secs: i64,
    /// Starting an attack drops the attacker's own shield.
    pub attack_breaks_shield: bool,
    /// Players may buy shield time with a wallet currency.
    pub shield_price: Option<ShieldPrice>,
    /// How long a beaten defender may strike back; revenge ignores shield and cooldown.
    pub revenge_ttl_secs: i64,
    pub revenge_ignores_shield: bool,
}

impl Default for Rules {
    fn default() -> Self {
        Rules {
            enabled: true,
            spread: 200.0,
            max_spread: 1000.0,
            count: 3,
            max_count: 10,
            min_games: 0,
            defender_cooldown_secs: 3_600,
            attack_ttl_secs: 900,
            attacks_per_day: 0,
            max_open: 1,
            shield_on_loss_secs: 0,
            shield_max_secs: 7 * 86_400,
            attack_breaks_shield: true,
            shield_price: None,
            revenge_ttl_secs: 86_400,
            revenge_ignores_shield: true,
        }
    }
}

impl Rules {
    pub fn validate(&self) -> Result<()> {
        if !(self.spread > 0.0 && self.max_spread >= self.spread) {
            return Err(Error::InvalidArgument(
                "spread must be > 0 and max_spread >= spread".into(),
            ));
        }
        if self.count == 0 || self.max_count < self.count || self.max_count > 50 {
            return Err(Error::InvalidArgument(
                "count must be 1..=max_count and max_count <= 50".into(),
            ));
        }
        if self.attack_ttl_secs <= 0 || self.max_open <= 0 {
            return Err(Error::InvalidArgument(
                "attack_ttl_secs and max_open must be > 0".into(),
            ));
        }
        if self.defender_cooldown_secs < 0
            || self.attacks_per_day < 0
            || self.shield_on_loss_secs < 0
            || self.shield_max_secs <= 0
            || self.revenge_ttl_secs < 0
            || self.min_games < 0
        {
            return Err(Error::InvalidArgument("negative rule value".into()));
        }
        if let Some(p) = &self.shield_price {
            if p.currency.is_empty() || p.per_hour <= 0 {
                return Err(Error::InvalidArgument(
                    "shield_price needs a currency and per_hour > 0".into(),
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Shield {
    pub account_id: AccountId,
    pub queue_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub until: OffsetDateTime,
    pub seconds_left: i64,
    pub source: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Attack {
    pub id: Uuid,
    pub queue_id: String,
    pub attacker: AccountId,
    pub defender: AccountId,
    pub state: String,
    pub result: Option<String>,
    pub revenge_of: Option<Uuid>,
    pub data: Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
}

fn attack_row(r: sqlx::postgres::PgRow) -> Attack {
    Attack {
        id: r.get("id"),
        queue_id: r.get("queue_id"),
        attacker: AccountId(r.get("attacker")),
        defender: AccountId(r.get("defender")),
        state: r.get("state"),
        result: r.get("result"),
        revenge_of: r.get("revenge_of"),
        data: r.get("data"),
        created_at: r.get("created_at"),
        expires_at: r.get("expires_at"),
        resolved_at: r.get("resolved_at"),
    }
}

const ATTACK_COLS: &str =
    "id, queue_id, attacker, defender, state, result, revenge_of, data, created_at, expires_at, resolved_at";

#[derive(Clone)]
pub struct AsyncMatchService {
    db: PgPool,
}

impl AsyncMatchService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    // ------------------------------------------------------------------ rules

    pub async fn put_rules(&self, queue: &str, rules: Rules) -> Result<Rules> {
        rules.validate()?;
        sqlx::query(
            "insert into musubi.async_mm_rules (queue_id, rules) values ($1, $2)
             on conflict (queue_id) do update set rules = excluded.rules, updated_at = now()",
        )
        .bind(queue)
        .bind(serde_json::to_value(&rules).map_err(Error::internal)?)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rules)
    }

    pub async fn rules(&self, queue: &str) -> Result<Rules> {
        let r = sqlx::query("select rules from musubi.async_mm_rules where queue_id = $1")
            .bind(queue)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .ok_or_else(|| {
                Error::NotFound(format!("queue {queue} has no async matchmaking rules"))
            })?;
        serde_json::from_value(r.get::<Value, _>("rules")).map_err(Error::internal)
    }

    pub async fn all_rules(&self) -> Result<Vec<(String, Rules)>> {
        let rows =
            sqlx::query("select queue_id, rules from musubi.async_mm_rules order by queue_id")
                .fetch_all(&self.db)
                .await
                .map_err(Error::internal)?;
        rows.into_iter()
            .map(|r| {
                Ok((
                    r.get("queue_id"),
                    serde_json::from_value(r.get::<Value, _>("rules")).map_err(Error::internal)?,
                ))
            })
            .collect()
    }

    pub async fn delete_rules(&self, queue: &str) -> Result<bool> {
        let r = sqlx::query("delete from musubi.async_mm_rules where queue_id = $1")
            .bind(queue)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    // ---------------------------------------------------------------- shields

    pub async fn shield(&self, queue: &str, account: AccountId) -> Result<Option<Shield>> {
        let r = sqlx::query(
            "select until, source from musubi.async_mm_shields
             where account_id = $1 and queue_id = $2 and until > now()",
        )
        .bind(account.0)
        .bind(queue)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.map(|r| {
            let until: OffsetDateTime = r.get("until");
            Shield {
                account_id: account,
                queue_id: queue.into(),
                until,
                seconds_left: (until - OffsetDateTime::now_utc()).whole_seconds().max(0),
                source: r.get("source"),
            }
        }))
    }

    /// Extend (or start) a shield by `secs`, never past `rules.shield_max_secs` from now.
    pub async fn grant_shield(
        &self,
        rules: &Rules,
        queue: &str,
        account: AccountId,
        secs: i64,
        source: &str,
    ) -> Result<Shield> {
        if secs <= 0 {
            return Err(Error::InvalidArgument("shield seconds must be > 0".into()));
        }
        let now = OffsetDateTime::now_utc();
        let base = self
            .shield(queue, account)
            .await?
            .map(|s| s.until)
            .unwrap_or(now)
            .max(now);
        let until = (base + time::Duration::seconds(secs))
            .min(now + time::Duration::seconds(rules.shield_max_secs));
        sqlx::query(
            "insert into musubi.async_mm_shields (account_id, queue_id, until, source)
             values ($1, $2, $3, $4)
             on conflict (account_id, queue_id) do update set until = excluded.until, source = excluded.source",
        )
        .bind(account.0)
        .bind(queue)
        .bind(until)
        .bind(source)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(Shield {
            account_id: account,
            queue_id: queue.into(),
            until,
            seconds_left: (until - now).whole_seconds().max(0),
            source: source.into(),
        })
    }

    pub async fn drop_shield(&self, queue: &str, account: AccountId) -> Result<bool> {
        let r = sqlx::query(
            "delete from musubi.async_mm_shields where account_id = $1 and queue_id = $2",
        )
        .bind(account.0)
        .bind(queue)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    pub async fn set_opt_out(&self, queue: &str, account: AccountId, out: bool) -> Result<()> {
        if out {
            sqlx::query(
                "insert into musubi.async_mm_optouts (account_id, queue_id) values ($1, $2)
                 on conflict do nothing",
            )
        } else {
            sqlx::query(
                "delete from musubi.async_mm_optouts where account_id = $1 and queue_id = $2",
            )
        }
        .bind(account.0)
        .bind(queue)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn opted_out(&self, queue: &str, account: AccountId) -> Result<bool> {
        let r = sqlx::query(
            "select 1 from musubi.async_mm_optouts where account_id = $1 and queue_id = $2",
        )
        .bind(account.0)
        .bind(queue)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.is_some())
    }

    // ------------------------------------------------------------- candidates

    /// Nearest-rating accounts (by native `mu`) of the queue's season that are attackable by
    /// `me`: not shielded, not opted out, enough games, not under this attacker's cooldown or in
    /// one of their open attacks. Over-fetches so the caller can filter by display spread.
    pub async fn nearest(
        &self,
        rules: &Rules,
        queue: &str,
        season: i32,
        me: AccountId,
        my_mu: Option<f64>,
        limit: i64,
    ) -> Result<Vec<AccountId>> {
        let rows = sqlx::query(
            "select r.account_id from musubi.ratings r
             where r.queue_id = $1 and r.season = $2 and r.account_id <> $3 and r.games >= $4
               and not exists (select 1 from musubi.async_mm_optouts o
                               where o.account_id = r.account_id and o.queue_id = $1)
               and not exists (select 1 from musubi.async_mm_shields s
                               where s.account_id = r.account_id and s.queue_id = $1 and s.until > now())
               and not exists (select 1 from musubi.async_mm_attacks a
                               where a.queue_id = $1 and a.attacker = $3 and a.defender = r.account_id
                                 and (a.state = 'open'
                                      or a.created_at > now() - make_interval(secs => $5)))
             order by case when $6::float8 is null then 0 else abs(r.mu - $6) end, random()
             limit $7",
        )
        .bind(queue)
        .bind(season)
        .bind(me.0)
        .bind(rules.min_games)
        .bind(rules.defender_cooldown_secs as f64)
        .bind(my_mu)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| AccountId(r.get("account_id")))
            .collect())
    }

    // ---------------------------------------------------------------- attacks

    pub async fn expire(&self, queue: &str) -> Result<u64> {
        let r = sqlx::query(
            "update musubi.async_mm_attacks set state = 'expired'
             where queue_id = $1 and state = 'open' and expires_at <= now()",
        )
        .bind(queue)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.rows_affected())
    }

    pub async fn attack(&self, id: Uuid) -> Result<Attack> {
        let sql = format!("select {ATTACK_COLS} from musubi.async_mm_attacks where id = $1");
        sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(attack_row)
            .ok_or_else(|| Error::NotFound(format!("attack {id} does not exist")))
    }

    pub async fn open_by(&self, queue: &str, attacker: AccountId) -> Result<Vec<Attack>> {
        let sql = format!(
            "select {ATTACK_COLS} from musubi.async_mm_attacks
             where queue_id = $1 and attacker = $2 and state = 'open' order by created_at"
        );
        let rows = sqlx::query(&sql)
            .bind(queue)
            .bind(attacker.0)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(rows.into_iter().map(attack_row).collect())
    }

    pub async fn attacks_today(&self, queue: &str, attacker: AccountId) -> Result<i64> {
        let r = sqlx::query(
            "select count(*) as n from musubi.async_mm_attacks
             where queue_id = $1 and attacker = $2 and created_at > now() - interval '1 day'",
        )
        .bind(queue)
        .bind(attacker.0)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.get("n"))
    }

    pub async fn last_against(
        &self,
        queue: &str,
        attacker: AccountId,
        defender: AccountId,
    ) -> Result<Option<Attack>> {
        let sql = format!(
            "select {ATTACK_COLS} from musubi.async_mm_attacks
             where queue_id = $1 and attacker = $2 and defender = $3
             order by created_at desc limit 1"
        );
        Ok(sqlx::query(&sql)
            .bind(queue)
            .bind(attacker.0)
            .bind(defender.0)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(attack_row))
    }

    /// Can `me` still strike back for `a`? Resolved attack on me, within `revenge_ttl_secs`,
    /// not yet avenged.
    pub async fn revenge_open(&self, rules: &Rules, me: AccountId, a: &Attack) -> Result<bool> {
        if a.defender != me || a.state != "resolved" || rules.revenge_ttl_secs == 0 {
            return Ok(false);
        }
        let Some(at) = a.resolved_at else {
            return Ok(false);
        };
        if OffsetDateTime::now_utc() - at > time::Duration::seconds(rules.revenge_ttl_secs) {
            return Ok(false);
        }
        let r = sqlx::query("select 1 from musubi.async_mm_attacks where revenge_of = $1")
            .bind(a.id)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(r.is_none())
    }

    /// Start an attack after every rule check; returns the open attack.
    pub async fn start(
        &self,
        rules: &Rules,
        queue: &str,
        me: AccountId,
        defender: AccountId,
        revenge_of: Option<Uuid>,
        data: Value,
    ) -> Result<Attack> {
        if !rules.enabled {
            return Err(Error::PermissionDenied(format!(
                "async matchmaking is disabled for {queue}"
            )));
        }
        if me == defender {
            return Err(Error::InvalidArgument("cannot attack yourself".into()));
        }
        self.expire(queue).await?;
        let mut revenge = false;
        if let Some(rid) = revenge_of {
            let orig = self.attack(rid).await?;
            if orig.queue_id != queue || orig.attacker != defender {
                return Err(Error::InvalidArgument(
                    "revenge_of must be an attack by this defender on you in this queue".into(),
                ));
            }
            if !self.revenge_open(rules, me, &orig).await? {
                return Err(Error::Conflict(
                    "revenge window closed or already used".into(),
                ));
            }
            revenge = true;
        }
        if self.opted_out(queue, defender).await? {
            return Err(Error::PermissionDenied(
                "defender opted out of attacks".into(),
            ));
        }
        if !(revenge && rules.revenge_ignores_shield) {
            if let Some(s) = self.shield(queue, defender).await? {
                return Err(Error::Conflict(format!(
                    "defender is shielded for {} s",
                    s.seconds_left
                )));
            }
        }
        // The per-attacker budgets (max_open, attacks_per_day, cooldown) are counted and the
        // attack inserted under one advisory lock per (queue, attacker): without it N concurrent
        // requests all pass the checks and the attacker gets N times the budget.
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
            .bind(format!("async_mm:{queue}:{}", me.0))
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let open: i64 = sqlx::query_scalar(
            "select count(*) from musubi.async_mm_attacks
             where queue_id = $1 and attacker = $2 and state = 'open'",
        )
        .bind(queue)
        .bind(me.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if open >= rules.max_open {
            return Err(Error::Conflict(format!(
                "{open} open attack(s) already; resolve them first"
            )));
        }
        if rules.attacks_per_day > 0 {
            let today: i64 = sqlx::query_scalar(
                "select count(*) from musubi.async_mm_attacks
                 where queue_id = $1 and attacker = $2 and created_at > now() - interval '1 day'",
            )
            .bind(queue)
            .bind(me.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if today >= rules.attacks_per_day {
                return Err(Error::ResourceExhausted(format!(
                    "attack limit {} per day reached",
                    rules.attacks_per_day
                )));
            }
        }
        if !revenge {
            let last: Option<(String, OffsetDateTime)> = sqlx::query_as(
                "select state, created_at from musubi.async_mm_attacks
                 where queue_id = $1 and attacker = $2 and defender = $3
                 order by created_at desc limit 1",
            )
            .bind(queue)
            .bind(me.0)
            .bind(defender.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if let Some((state, created_at)) = last {
                let since = OffsetDateTime::now_utc() - created_at;
                if state == "open" {
                    return Err(Error::Conflict(
                        "you already have an open attack on them".into(),
                    ));
                }
                if since < time::Duration::seconds(rules.defender_cooldown_secs) {
                    return Err(Error::Conflict(format!(
                        "defender cooldown: {} s left",
                        rules.defender_cooldown_secs - since.whole_seconds()
                    )));
                }
            }
        }
        if rules.attack_breaks_shield {
            sqlx::query(
                "delete from musubi.async_mm_shields where account_id = $1 and queue_id = $2",
            )
            .bind(me.0)
            .bind(queue)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let id = Uuid::now_v7();
        let sql = format!(
            "insert into musubi.async_mm_attacks (id, queue_id, attacker, defender, revenge_of, data, expires_at)
             values ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7))
             returning {ATTACK_COLS}"
        );
        let r = sqlx::query(&sql)
            .bind(id)
            .bind(queue)
            .bind(me.0)
            .bind(defender.0)
            .bind(revenge_of)
            .bind(data)
            .bind(rules.attack_ttl_secs as f64)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(d) if d.is_unique_violation() => {
                    Error::Conflict("revenge already taken".into())
                }
                e => Error::internal(e),
            })?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(attack_row(r))
    }

    /// Record the outcome (`result` is from the attacker's side). Only open, unexpired attacks.
    pub async fn resolve(&self, id: Uuid, result: &str, data: Option<Value>) -> Result<Attack> {
        if !RESULTS.contains(&result) {
            return Err(Error::InvalidArgument(
                "result must be win, loss or draw".into(),
            ));
        }
        let a = self.attack(id).await?;
        self.expire(&a.queue_id).await?;
        let sql = format!(
            "update musubi.async_mm_attacks
             set state = 'resolved', result = $2, resolved_at = now(), data = coalesce($3, data)
             where id = $1 and state = 'open' returning {ATTACK_COLS}"
        );
        sqlx::query(&sql)
            .bind(id)
            .bind(result)
            .bind(data)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?
            .map(attack_row)
            .ok_or_else(|| {
                Error::Conflict(format!(
                    "attack {id} is {}",
                    if a.state == "open" {
                        "expired"
                    } else {
                        a.state.as_str()
                    }
                ))
            })
    }

    pub async fn list(
        &self,
        queue: &str,
        account: Option<AccountId>,
        role: &str,
        state: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Attack>> {
        let sql = format!(
            "select {ATTACK_COLS} from musubi.async_mm_attacks
             where queue_id = $1
               and ($2::uuid is null or ($3 = 'attacker' and attacker = $2)
                    or ($3 = 'defender' and defender = $2) or ($3 = 'any' and (attacker = $2 or defender = $2)))
               and ($4::text is null or state = $4)
             order by created_at desc limit $5"
        );
        let rows = sqlx::query(&sql)
            .bind(queue)
            .bind(account.map(|a| a.0))
            .bind(role)
            .bind(state)
            .bind(limit.clamp(1, 200))
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(rows.into_iter().map(attack_row).collect())
    }

    pub fn ranks(result: &str) -> Vec<u32> {
        match result {
            "win" => vec![1, 2],
            "loss" => vec![2, 1],
            _ => vec![1, 1],
        }
    }

    pub fn summary(a: &Attack) -> Value {
        json!({ "id": a.id, "queue": a.queue_id, "attacker": a.attacker, "defender": a.defender,
                "state": a.state, "result": a.result })
    }
}
