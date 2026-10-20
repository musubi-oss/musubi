//! Player mailbox: messages with optional attached reward tables, read/claim/expire, bulk send.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{Postgres, QueryBuilder};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SendMail {
    pub account_ids: Vec<Uuid>,
    /// Send to every account (ignores `account_ids`).
    pub all: bool,
    /// Send to every account matching the filter (ignores `account_ids`); empty filter = all.
    pub audience: Option<Audience>,
    pub title: String,
    pub body: String,
    pub metadata: Value,
    /// Reward table attached to the message.
    pub rewards: Option<String>,
    /// Frozen reward entries attached inline (claimed as-is, no table lookup).
    pub reward_entries: Option<Vec<RewardEntry>>,
    /// Seconds until the attached rewards can be claimed.
    pub available_in_seconds: Option<i64>,
    pub expires_in_seconds: Option<i64>,
}

/// Audience filter for broadcasts: every listed condition must hold. Days are relative to `now`,
/// times are unix seconds, `platform` matches a linked identity provider (steam, psn, ...),
/// `metadata` is a JSON containment check on `accounts.metadata`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Audience {
    pub lang: Vec<String>,
    pub location: Vec<String>,
    pub platform: Vec<String>,
    pub age_bracket: Vec<String>,
    pub created_after: Option<i64>,
    pub created_before: Option<i64>,
    pub track: Option<String>,
    pub min_level: Option<i32>,
    pub max_level: Option<i32>,
    pub active_within_days: Option<i64>,
    pub inactive_days: Option<i64>,
    pub metadata: Option<Value>,
    /// Account age in days (relative to `now`).
    pub min_account_age_days: Option<i64>,
    pub max_account_age_days: Option<i64>,
    /// Deterministic percentage bucket 0..=99 of the account id (optionally salted) — cohorts,
    /// rollouts, A/B splits.
    pub bucket_min: Option<i32>,
    pub bucket_max: Option<i32>,
    pub bucket_salt: Option<String>,
    /// Always in / never in, regardless of the other rules.
    pub include: Vec<Uuid>,
    pub exclude: Vec<Uuid>,
    /// Telemetry-derived rules on `player_event_props` (count / value aggregates / recency).
    pub events: Vec<EventRule>,
    /// UA attribution: the account's first (default) or last touch matches every listed value set.
    pub attribution: Option<AttributionRule>,
    /// GeoIP: last known country (ISO 3166-1 alpha-2) at login is one of these.
    pub countries: Vec<String>,
    /// … and is none of these (accounts without a known location pass).
    pub not_countries: Vec<String>,
    /// Algebra: at least one of `any` holds, every one of `all` holds, `not` does not hold.
    pub any: Vec<Audience>,
    pub all: Vec<Audience>,
    pub not: Option<Box<Audience>>,
}

/// One telemetry-derived condition: the account's aggregate `field` of `event` lies in
/// `[min, max]`, optionally only counting accounts whose last such event is within / older than
/// `seen_within_days` / `not_seen_days`; `never: true` matches accounts that never sent the event.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct EventRule {
    pub event: String,
    /// `count` (default), `value_sum`, `value_high`, `value_low`, `value_last`, `value_first`.
    pub field: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub seen_within_days: Option<i64>,
    pub not_seen_days: Option<i64>,
    pub never: bool,
}

/// Match on `account_attribution`: every non-empty list must contain the touch's value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AttributionRule {
    pub source: Vec<String>,
    pub medium: Vec<String>,
    pub campaign: Vec<String>,
    pub network: Vec<String>,
    /// `first` (default) or `last`.
    pub touch: Option<String>,
}

impl AttributionRule {
    fn validate(&self) -> Result<()> {
        if self
            .touch
            .as_deref()
            .is_some_and(|t| !["first", "last"].contains(&t))
        {
            return Err(Error::InvalidArgument(
                "attribution.touch must be first|last".into(),
            ));
        }
        let lists = [&self.source, &self.medium, &self.campaign, &self.network];
        if lists.iter().all(|l| l.is_empty()) {
            return Err(Error::InvalidArgument(
                "attribution needs at least one of source/medium/campaign/network".into(),
            ));
        }
        if lists
            .iter()
            .any(|l| l.len() > 64 || l.iter().any(|v| v.is_empty() || v.len() > 256))
        {
            return Err(Error::InvalidArgument(
                "attribution lists: <= 64 values of 1..=256 chars".into(),
            ));
        }
        Ok(())
    }

    fn push(&self, qb: &mut QueryBuilder<'_, Postgres>) {
        let col = if self.touch.as_deref() == Some("last") {
            "last"
        } else {
            "first"
        };
        qb.push(
            " and exists (select 1 from musubi.account_attribution t where t.account_id = a.id",
        );
        for (key, vals) in [
            ("source", &self.source),
            ("medium", &self.medium),
            ("campaign", &self.campaign),
            ("network", &self.network),
        ] {
            if !vals.is_empty() {
                qb.push(format!(" and t.{col}->>'{key}' = any("))
                    .push_bind(vals.clone())
                    .push(")");
            }
        }
        qb.push(")");
    }
}

const EVENT_FIELDS: [&str; 6] = [
    "count",
    "value_sum",
    "value_high",
    "value_low",
    "value_last",
    "value_first",
];

impl EventRule {
    fn validate(&self) -> Result<()> {
        let ok_name = !self.event.is_empty()
            && self.event.len() <= 128
            && self
                .event
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-');
        if !ok_name {
            return Err(Error::InvalidArgument(
                "events[].event must be 1..=128 of [A-Za-z0-9_.-]".into(),
            ));
        }
        let field = self.field.as_deref().unwrap_or("count");
        if !EVENT_FIELDS.contains(&field) {
            return Err(Error::InvalidArgument(format!(
                "events[].field must be one of {}",
                EVENT_FIELDS.join("|")
            )));
        }
        if matches!((self.min, self.max), (Some(a), Some(b)) if a > b) {
            return Err(Error::InvalidArgument("events[].min must be <= max".into()));
        }
        for d in [self.seen_within_days, self.not_seen_days]
            .into_iter()
            .flatten()
        {
            if !(1..=3650).contains(&d) {
                return Err(Error::InvalidArgument(
                    "events[] days must be 1..=3650".into(),
                ));
            }
        }
        if self.never
            && (self.min.is_some()
                || self.max.is_some()
                || self.seen_within_days.is_some()
                || self.not_seen_days.is_some())
        {
            return Err(Error::InvalidArgument(
                "events[].never cannot be combined with bounds".into(),
            ));
        }
        Ok(())
    }

    fn push(&self, qb: &mut QueryBuilder<'_, Postgres>, now: OffsetDateTime) {
        if self.never {
            qb.push(" and not exists (select 1 from musubi.player_event_props e where e.account_id = a.id and e.event = ")
                .push_bind(self.event.clone())
                .push(")");
            return;
        }
        let field = self.field.as_deref().unwrap_or("count");
        qb.push(" and exists (select 1 from musubi.player_event_props e where e.account_id = a.id and e.event = ")
            .push_bind(self.event.clone());
        if let Some(min) = self.min {
            qb.push(format!(" and e.{field} >= ")).push_bind(min);
        }
        if let Some(max) = self.max {
            qb.push(format!(" and e.{field} <= ")).push_bind(max);
        }
        if let Some(d) = self.seen_within_days {
            qb.push(" and e.seen_last >= ")
                .push_bind(now - Duration::days(d));
        }
        if let Some(d) = self.not_seen_days {
            qb.push(" and e.seen_last < ")
                .push_bind(now - Duration::days(d));
        }
        qb.push(")");
    }
}

impl Audience {
    fn validate(&self) -> Result<()> {
        if self.events.len() > 16 {
            return Err(Error::InvalidArgument("at most 16 event rules".into()));
        }
        for e in &self.events {
            e.validate()?;
        }
        for (name, list) in [
            ("countries", &self.countries),
            ("not_countries", &self.not_countries),
        ] {
            if list.len() > 250
                || list
                    .iter()
                    .any(|c| c.len() != 2 || !c.chars().all(|ch| ch.is_ascii_alphabetic()))
            {
                return Err(Error::InvalidArgument(format!(
                    "{name}: <= 250 ISO 3166-1 alpha-2 codes"
                )));
            }
        }
        if let Some(a) = &self.attribution {
            a.validate()?;
        }
        if (self.min_level.is_some() || self.max_level.is_some()) && self.track.is_none() {
            return Err(Error::InvalidArgument(
                "min_level/max_level need track".into(),
            ));
        }
        for d in [self.active_within_days, self.inactive_days]
            .into_iter()
            .flatten()
        {
            if !(1..=3650).contains(&d) {
                return Err(Error::InvalidArgument("days must be 1..=3650".into()));
            }
        }
        if let Some(m) = &self.metadata {
            if !m.is_object() {
                return Err(Error::InvalidArgument("metadata must be an object".into()));
            }
        }
        Ok(())
    }

    pub fn validate_rules(&self) -> Result<()> {
        self.validate()?;
        if self.bucket_min.is_some_and(|b| !(0..=99).contains(&b))
            || self.bucket_max.is_some_and(|b| !(0..=99).contains(&b))
            || matches!((self.bucket_min, self.bucket_max), (Some(a), Some(b)) if a > b)
        {
            return Err(Error::InvalidArgument(
                "bucket_min/bucket_max must be 0..=99 with min <= max".into(),
            ));
        }
        if self.min_account_age_days.is_some_and(|d| d < 0)
            || self.max_account_age_days.is_some_and(|d| d < 0)
        {
            return Err(Error::InvalidArgument(
                "account age days must be ≥ 0".into(),
            ));
        }
        if self.any.len() + self.all.len() > 32 {
            return Err(Error::InvalidArgument("at most 32 nested rule sets".into()));
        }
        for c in self.any.iter().chain(self.all.iter()) {
            c.validate_rules()?;
        }
        if let Some(n) = &self.not {
            n.validate_rules()?;
        }
        Ok(())
    }

    fn push_where(&self, qb: &mut QueryBuilder<'_, Postgres>, now: OffsetDateTime) {
        qb.push(" where a.deleted_at is null and a.disabled_at is null");
        if !self.include.is_empty() {
            // included accounts bypass the rules (a bare list is just that list); excluded never match
            qb.push(" and (a.id = any(")
                .push_bind(self.include.clone())
                .push(")");
            if self.has_conditions() {
                qb.push(" or (true");
                self.push_conditions(qb, now);
                qb.push(")");
            }
            qb.push(")");
        } else {
            self.push_conditions(qb, now);
        }
        if !self.exclude.is_empty() {
            qb.push(" and a.id <> all(")
                .push_bind(self.exclude.clone())
                .push(")");
        }
    }

    /// Any rule besides the include / exclude lists?
    fn has_conditions(&self) -> bool {
        !(self.lang.is_empty()
            && self.location.is_empty()
            && self.platform.is_empty()
            && self.age_bracket.is_empty()
            && self.created_after.is_none()
            && self.created_before.is_none()
            && self.track.is_none()
            && self.active_within_days.is_none()
            && self.inactive_days.is_none()
            && self.metadata.is_none()
            && self.min_account_age_days.is_none()
            && self.max_account_age_days.is_none()
            && self.bucket_min.is_none()
            && self.bucket_max.is_none()
            && self.events.is_empty()
            && self.attribution.is_none()
            && self.countries.is_empty()
            && self.not_countries.is_empty()
            && self.any.is_empty()
            && self.all.is_empty()
            && self.not.is_none())
    }

    /// ` and <cond>` fragments for every rule (no `include`/`exclude`, those wrap the whole set).
    fn push_conditions(&self, qb: &mut QueryBuilder<'_, Postgres>, now: OffsetDateTime) {
        for (col, vals) in [
            ("a.lang", &self.lang),
            ("a.location", &self.location),
            ("a.age_bracket", &self.age_bracket),
        ] {
            if !vals.is_empty() {
                qb.push(format!(" and {col} = any("))
                    .push_bind(vals.clone())
                    .push(")");
            }
        }
        if !self.platform.is_empty() {
            qb.push(" and exists (select 1 from musubi.identities i where i.account_id = a.id and i.provider = any(")
                .push_bind(self.platform.clone())
                .push("))");
        }
        if let Some(t) = self
            .created_after
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
        {
            qb.push(" and a.created_at >= ").push_bind(t);
        }
        if let Some(t) = self
            .created_before
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
        {
            qb.push(" and a.created_at < ").push_bind(t);
        }
        if let Some(track) = &self.track {
            qb.push(" and exists (select 1 from musubi.progress p where p.account_id = a.id and p.track_id = ")
                .push_bind(track.clone());
            if let Some(min) = self.min_level {
                qb.push(" and p.level >= ").push_bind(min);
            }
            if let Some(max) = self.max_level {
                qb.push(" and p.level <= ").push_bind(max);
            }
            qb.push(")");
        }
        if let Some(d) = self.active_within_days {
            qb.push(" and exists (select 1 from musubi.sessions s where s.account_id = a.id and s.last_seen_at >= ")
                .push_bind(now - Duration::days(d))
                .push(")");
        }
        if let Some(d) = self.inactive_days {
            qb.push(" and not exists (select 1 from musubi.sessions s where s.account_id = a.id and s.last_seen_at >= ")
                .push_bind(now - Duration::days(d))
                .push(")");
        }
        if let Some(m) = &self.metadata {
            qb.push(" and a.metadata @> ").push_bind(m.clone());
        }
        if let Some(d) = self.min_account_age_days {
            qb.push(" and a.created_at <= ")
                .push_bind(now - Duration::days(d));
        }
        if let Some(d) = self.max_account_age_days {
            qb.push(" and a.created_at > ")
                .push_bind(now - Duration::days(d));
        }
        if self.bucket_min.is_some() || self.bucket_max.is_some() {
            let salt = self.bucket_salt.clone().unwrap_or_default();
            qb.push(" and (abs(hashtext(a.id::text || ")
                .push_bind(salt)
                .push(")) % 100) between ")
                .push_bind(self.bucket_min.unwrap_or(0))
                .push(" and ")
                .push_bind(self.bucket_max.unwrap_or(99));
        }
        for e in &self.events {
            e.push(qb, now);
        }
        if !self.countries.is_empty() {
            qb.push(" and exists (select 1 from musubi.account_geo g where g.account_id = a.id and g.country = any(")
                .push_bind(
                    self.countries
                        .iter()
                        .map(|c| c.to_ascii_uppercase())
                        .collect::<Vec<_>>(),
                )
                .push("))");
        }
        if !self.not_countries.is_empty() {
            qb.push(" and not exists (select 1 from musubi.account_geo g where g.account_id = a.id and g.country = any(")
                .push_bind(
                    self.not_countries
                        .iter()
                        .map(|c| c.to_ascii_uppercase())
                        .collect::<Vec<_>>(),
                )
                .push("))");
        }
        if let Some(a) = &self.attribution {
            a.push(qb);
        }
        if !self.any.is_empty() {
            qb.push(" and (");
            for (i, c) in self.any.iter().enumerate() {
                if i > 0 {
                    qb.push(" or ");
                }
                qb.push("(true");
                c.push_sub(qb, now);
                qb.push(")");
            }
            qb.push(")");
        }
        for c in &self.all {
            qb.push(" and (true");
            c.push_sub(qb, now);
            qb.push(")");
        }
        if let Some(n) = &self.not {
            qb.push(" and not (true");
            n.push_sub(qb, now);
            qb.push(")");
        }
    }

    /// A nested rule set: its own conditions plus its include / exclude lists.
    fn push_sub(&self, qb: &mut QueryBuilder<'_, Postgres>, now: OffsetDateTime) {
        if !self.include.is_empty() {
            qb.push(" and (a.id = any(")
                .push_bind(self.include.clone())
                .push(")");
            if self.has_conditions() {
                qb.push(" or (true");
                self.push_conditions(qb, now);
                qb.push(")");
            }
            qb.push(")");
        } else {
            self.push_conditions(qb, now);
        }
        if !self.exclude.is_empty() {
            qb.push(" and a.id <> all(")
                .push_bind(self.exclude.clone())
                .push(")");
        }
    }
}

impl Default for SendMail {
    fn default() -> Self {
        Self {
            account_ids: Vec::new(),
            all: false,
            audience: None,
            title: String::new(),
            body: String::new(),
            metadata: json!({}),
            rewards: None,
            expires_in_seconds: None,
            reward_entries: None,
            available_in_seconds: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Mail {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub metadata: Value,
    pub rewards: Option<String>,
    /// Inline rewards (deferred table outcome or server-attached entries).
    pub reward_entries: Option<Vec<RewardEntry>>,
    /// Rewards cannot be claimed before this.
    #[serde(with = "time::serde::rfc3339::option")]
    pub available_at: Option<OffsetDateTime>,
    pub read: bool,
    pub claimed: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type MailRow = (
    Uuid,
    String,
    String,
    Value,
    Option<String>,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    OffsetDateTime,
    Option<Value>,
    Option<OffsetDateTime>,
);

fn mail(r: MailRow) -> Mail {
    Mail {
        id: r.0,
        title: r.1,
        body: r.2,
        metadata: r.3,
        rewards: r.4,
        read: r.5.is_some(),
        claimed: r.6.is_some(),
        expires_at: r.7,
        created_at: r.8,
        reward_entries: r.9.and_then(|v| serde_json::from_value(v).ok()),
        available_at: r.10,
    }
}

const COLS: &str =
    "id, title, body, metadata, rewards, read_at, claimed_at, expires_at, created_at, reward_entries, available_at";

impl EconomyService {
    /// Freezes `rewards` into a mailbox message for `account` (deferred table delivery).
    pub(crate) async fn defer_rewards(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        account: AccountId,
        d: &crate::rewards::Deferred,
        rewards: &[RewardEntry],
        table_id: &str,
        reference: &str,
    ) -> Result<Uuid> {
        let now = OffsetDateTime::now_utc();
        let available_at = now + time::Duration::seconds(d.delay_secs);
        let expires_at = d
            .ttl_secs
            .map(|t| available_at + time::Duration::seconds(t));
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into musubi.mail (id, account_id, title, body, metadata, reward_entries, available_at, expires_at, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(id)
        .bind(account.0)
        .bind(&d.title)
        .bind(&d.body)
        .bind(serde_json::json!({ "table_id": table_id, "reference": reference }))
        .bind(serde_json::to_value(rewards)?)
        .bind(available_at)
        .bind(expires_at)
        .bind(now)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        Ok(id)
    }

    /// Returns the number of messages created.
    pub async fn send_mail(&self, m: SendMail, now: OffsetDateTime) -> Result<i64> {
        if m.title.is_empty() || m.title.len() > 200 || m.body.len() > 16 * 1024 {
            return Err(Error::InvalidArgument(
                "title 1..=200 chars, body <= 16 KiB".into(),
            ));
        }
        if !m.all
            && m.audience.is_none()
            && (m.account_ids.is_empty() || m.account_ids.len() > 10_000)
        {
            return Err(Error::InvalidArgument(
                "account_ids 1..=10000 (or all=true / audience)".into(),
            ));
        }
        let expires_at = match m.expires_in_seconds {
            Some(s) if s <= 0 => {
                return Err(Error::InvalidArgument(
                    "expires_in_seconds must be positive".into(),
                ))
            }
            Some(s) => Some(now + Duration::seconds(s)),
            None => None,
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        if let Some(t) = &m.rewards {
            if Self::load_table(&mut tx, t).await?.is_none() {
                return Err(Error::NotFound(format!("reward table {t}")));
            }
        }
        if let Some(es) = &m.reward_entries {
            if es.is_empty()
                || es
                    .iter()
                    .any(|e| e.amount <= 0 || e.currency.is_none() == e.item_id.is_none())
            {
                return Err(Error::InvalidArgument(
                    "reward_entries: amount > 0 and exactly one of currency / item_id".into(),
                ));
            }
        }
        let available_at = match m.available_in_seconds {
            Some(s) if s < 0 => {
                return Err(Error::InvalidArgument(
                    "available_in_seconds must be >= 0".into(),
                ))
            }
            Some(s) => Some(now + time::Duration::seconds(s)),
            None => None,
        };
        let reward_entries = m
            .reward_entries
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        let n = if m.all || m.audience.is_some() {
            let aud = m.audience.clone().unwrap_or_default();
            aud.validate()?;
            let mut qb = QueryBuilder::new(
                "insert into musubi.mail (id, account_id, title, body, metadata, rewards, expires_at, created_at, reward_entries, available_at) \
                 select gen_random_uuid(), a.id, ",
            );
            qb.push_bind(&m.title)
                .push(", ")
                .push_bind(&m.body)
                .push(", ")
                .push_bind(&m.metadata)
                .push(", ")
                .push_bind(&m.rewards)
                .push(", ")
                .push_bind(expires_at)
                .push(", ")
                .push_bind(now)
                .push(", ")
                .push_bind(&reward_entries)
                .push(", ")
                .push_bind(available_at)
                .push(" from musubi.accounts a");
            aud.push_where(&mut qb, now);
            qb.build()
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?
                .rows_affected()
        } else {
            sqlx::query(
                "insert into musubi.mail (id, account_id, title, body, metadata, rewards, expires_at, created_at, reward_entries, available_at)
                 select gen_random_uuid(), a.id, $2, $3, $4, $5, $6, $7, $8, $9 from unnest($1::uuid[]) as a(id)",
            )
            .bind(&m.account_ids)
            .bind(&m.title)
            .bind(&m.body)
            .bind(&m.metadata)
            .bind(&m.rewards)
            .bind(expires_at)
            .bind(now)
            .bind(&reward_entries)
            .bind(available_at)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?
            .rows_affected()
        };
        tx.commit().await.map_err(Error::internal)?;
        Ok(n as i64)
    }

    /// How many accounts a broadcast `audience` would reach right now.
    /// Names of the given audiences `account` belongs to (plan 5.12), in the given order.
    pub async fn audiences_of(
        &self,
        account: AccountId,
        audiences: &[(String, Audience)],
        now: OffsetDateTime,
    ) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for (name, aud) in audiences {
            if aud.validate().is_err() {
                continue;
            }
            let mut qb = QueryBuilder::new("select count(*) from musubi.accounts a");
            aud.push_where(&mut qb, now);
            qb.push(" and a.id = ").push_bind(account.0);
            let n: i64 = qb
                .build_query_scalar()
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
            if n > 0 {
                out.push(name.clone());
            }
        }
        Ok(out)
    }

    /// Accounts matching an audience right now (membership tracking), oldest first.
    pub async fn audience_accounts(
        &self,
        aud: &Audience,
        now: OffsetDateTime,
        limit: i64,
    ) -> Result<Vec<AccountId>> {
        aud.validate_rules()?;
        let mut qb = QueryBuilder::new("select a.id from musubi.accounts a");
        aud.push_where(&mut qb, now);
        qb.push(" order by a.created_at limit ").push_bind(limit);
        let rows: Vec<(Uuid,)> = qb
            .build_query_as()
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(rows.into_iter().map(|(id,)| AccountId(id)).collect())
    }

    pub async fn count_audience(&self, aud: &Audience, now: OffsetDateTime) -> Result<i64> {
        aud.validate()?;
        let mut qb = QueryBuilder::new("select count(*) from musubi.accounts a");
        aud.push_where(&mut qb, now);
        qb.build_query_scalar::<i64>()
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)
    }

    pub async fn mail(
        &self,
        account: AccountId,
        unread_only: bool,
        limit: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<Mail>> {
        let rows: Vec<MailRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.mail where account_id = $1 and (expires_at is null or expires_at > $4)
             and (not $2 or read_at is null) order by created_at desc, id desc limit $3"
        ))
        .bind(account.0)
        .bind(unread_only)
        .bind(limit.clamp(1, crate::MAX_LEDGER_PAGE))
        .bind(now)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(mail).collect())
    }

    pub async fn read_mail(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Mail> {
        let row: Option<MailRow> = sqlx::query_as(&format!(
            "update musubi.mail set read_at = coalesce(read_at, $3) where id = $1 and account_id = $2 returning {COLS}"
        ))
        .bind(id)
        .bind(account.0)
        .bind(now)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(mail).ok_or_else(|| Error::NotFound("mail".into()))
    }

    /// Claims attached rewards (granted in the same transaction) and marks the message read.
    pub async fn claim_mail(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(Mail, Vec<RewardEntry>)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<MailRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.mail where id = $1 and account_id = $2 for update"
        ))
        .bind(id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(m) = row.map(mail) else {
            return Err(Error::NotFound("mail".into()));
        };
        if m.claimed {
            return Err(Error::Conflict("mail already claimed".into()));
        }
        if m.expires_at.is_some_and(|e| e <= now) {
            return Err(Error::PermissionDenied("mail expired".into()));
        }
        if let Some(a) = m.available_at.filter(|a| *a > now) {
            return Err(Error::PermissionDenied(format!(
                "rewards claimable in {} s",
                (a - now).whole_seconds().max(1)
            )));
        }
        let mut rewards = Vec::new();
        if let Some(es) = &m.reward_entries {
            for e in es {
                if let Some(c) = &e.currency {
                    Self::adjust_tx(
                        &mut tx,
                        account,
                        c,
                        e.amount,
                        "mail.claim",
                        &id.to_string(),
                        None,
                        serde_json::json!({}),
                    )
                    .await?;
                }
                if let Some(i) = &e.item_id {
                    Self::grant_item_tx(
                        &mut tx,
                        account,
                        i,
                        e.amount,
                        Value::Object(Default::default()),
                    )
                    .await?;
                }
            }
            rewards = es.clone();
        } else if let Some(t) = &m.rewards {
            let Some(table) = Self::load_table(&mut tx, t).await? else {
                return Err(Error::NotFound(format!("reward table {t}")));
            };
            Self::apply_rewards(
                &mut tx,
                account,
                &table.entries,
                "mail.claim",
                &id.to_string(),
            )
            .await?;
            rewards = table.entries;
        }
        let row: MailRow = sqlx::query_as(&format!(
            "update musubi.mail set claimed_at = $2, read_at = coalesce(read_at, $2) where id = $1 returning {COLS}"
        ))
        .bind(id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((mail(row), rewards))
    }

    /// Deletes a message; refuses while unclaimed rewards are attached (unless expired).
    pub async fn delete_mail(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<()> {
        let row: Option<(Option<String>, Option<OffsetDateTime>, Option<OffsetDateTime>)> =
            sqlx::query_as("select coalesce(rewards, case when reward_entries is not null then 'inline' end), claimed_at, expires_at from musubi.mail where id = $1 and account_id = $2")
                .bind(id)
                .bind(account.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        let Some((rewards, claimed, expires)) = row else {
            return Err(Error::NotFound("mail".into()));
        };
        let expired = expires.is_some_and(|e| e <= now);
        if rewards.is_some() && claimed.is_none() && !expired {
            return Err(Error::Conflict("claim the attached rewards first".into()));
        }
        sqlx::query("delete from musubi.mail where id = $1 and account_id = $2")
            .bind(id)
            .bind(account.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }
}
