//! Per-server fleet metrics: the gameserver agent reports CPU / memory / network / tick timing /
//! occupancy (`POST /v1/gameserver/metrics`); operators read per-server history and fleet-wide
//! summaries and define threshold alert rules (`cpu_pct > 90 for 60 s`, `tick_p99_ms > 50`,
//! `silence_secs > 30` = agent stopped reporting). Alerts have a breach → fired → resolved life
//! cycle; the caller turns the returned transitions into `fleet.alert_fired|resolved` events.

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const METRICS: &[&str] = &[
    "cpu_pct",
    "mem_bytes",
    "mem_pct",
    "net_in_bps",
    "net_out_bps",
    "tick_ms",
    "tick_p99_ms",
    "tick_hz",
    "players",
    "occupancy_pct",
    "silence_secs",
];

/// One report from the agent; every field optional so a server can send what it measures.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Sample {
    pub cpu_pct: Option<f64>,
    pub mem_bytes: Option<i64>,
    pub mem_limit_bytes: Option<i64>,
    pub net_in_bps: Option<i64>,
    pub net_out_bps: Option<i64>,
    pub tick_ms: Option<f64>,
    pub tick_p99_ms: Option<f64>,
    pub tick_hz: Option<f64>,
    pub players: Option<i32>,
    pub capacity: Option<i32>,
    /// Game-defined numeric gauges (`custom.<key>` in rules), ≤ 32 keys.
    pub custom: serde_json::Map<String, Value>,
}

impl Sample {
    pub fn validate(&self) -> Result<()> {
        let non_neg = |name: &str, v: Option<f64>| {
            if v.is_some_and(|x| !x.is_finite() || x < 0.0) {
                return Err(Error::InvalidArgument(format!("{name} must be >= 0")));
            }
            Ok(())
        };
        non_neg("cpu_pct", self.cpu_pct)?;
        non_neg("tick_ms", self.tick_ms)?;
        non_neg("tick_p99_ms", self.tick_p99_ms)?;
        non_neg("tick_hz", self.tick_hz)?;
        for (n, v) in [
            ("mem_bytes", self.mem_bytes),
            ("mem_limit_bytes", self.mem_limit_bytes),
            ("net_in_bps", self.net_in_bps),
            ("net_out_bps", self.net_out_bps),
        ] {
            if v.is_some_and(|x| x < 0) {
                return Err(Error::InvalidArgument(format!("{n} must be >= 0")));
            }
        }
        if self.players.is_some_and(|p| p < 0) || self.capacity.is_some_and(|c| c < 0) {
            return Err(Error::InvalidArgument(
                "players/capacity must be >= 0".into(),
            ));
        }
        if self.custom.len() > 32 {
            return Err(Error::InvalidArgument("at most 32 custom gauges".into()));
        }
        for (k, v) in &self.custom {
            if !valid_gauge_name(k) {
                return Err(Error::InvalidArgument(format!(
                    "bad custom gauge name {k:?}"
                )));
            }
            if !v.is_number() {
                return Err(Error::InvalidArgument(format!(
                    "custom gauge {k} must be a number"
                )));
            }
        }
        Ok(())
    }

    /// Value of a rule metric for this sample; `None` when the sample does not carry it.
    pub fn metric(&self, name: &str) -> Option<f64> {
        match name {
            "cpu_pct" => self.cpu_pct,
            "mem_bytes" => self.mem_bytes.map(|v| v as f64),
            "mem_pct" => match (self.mem_bytes, self.mem_limit_bytes) {
                (Some(m), Some(l)) if l > 0 => Some(m as f64 / l as f64 * 100.0),
                _ => None,
            },
            "net_in_bps" => self.net_in_bps.map(|v| v as f64),
            "net_out_bps" => self.net_out_bps.map(|v| v as f64),
            "tick_ms" => self.tick_ms,
            "tick_p99_ms" => self.tick_p99_ms,
            "tick_hz" => self.tick_hz,
            "players" => self.players.map(|v| v as f64),
            "occupancy_pct" => match (self.players, self.capacity) {
                (Some(p), Some(c)) if c > 0 => Some(p as f64 / c as f64 * 100.0),
                _ => None,
            },
            _ => name
                .strip_prefix("custom.")
                .and_then(|k| self.custom.get(k))
                .and_then(Value::as_f64),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleInput {
    pub metric: String,
    pub op: String,
    pub threshold: f64,
    pub for_secs: i32,
    pub severity: String,
    pub enabled: bool,
}

impl Default for RuleInput {
    fn default() -> Self {
        Self {
            metric: String::new(),
            op: "gt".into(),
            threshold: 0.0,
            for_secs: 0,
            severity: "warning".into(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Rule {
    pub fleet_id: Uuid,
    pub name: String,
    pub metric: String,
    pub op: String,
    pub threshold: f64,
    pub for_secs: i32,
    pub severity: String,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Alert {
    pub id: Uuid,
    pub fleet_id: Uuid,
    pub server_id: Uuid,
    pub rule: String,
    pub metric: String,
    pub severity: String,
    pub value: Option<f64>,
    pub threshold: f64,
    #[serde(with = "time::serde::rfc3339")]
    pub breach_since: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub fired_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub resolved_at: Option<OffsetDateTime>,
}

/// A fired/resolved transition produced by evaluation; the HTTP layer emits events for these.
#[derive(Debug, Clone, Serialize)]
pub struct Transition {
    pub kind: &'static str,
    pub alert: Alert,
}

/// Name of a game-defined gauge (`custom.<key>` in samples and rules): 1..=64 of `[A-Za-z0-9_.]`.
pub fn valid_gauge_name(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

pub fn valid_rule_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

fn breaches(op: &str, value: f64, threshold: f64) -> bool {
    match op {
        "gt" => value > threshold,
        "gte" => value >= threshold,
        "lt" => value < threshold,
        "lte" => value <= threshold,
        _ => false,
    }
}

pub struct FleetMetricsService {
    db: PgPool,
}

impl FleetMetricsService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Store a sample and evaluate the fleet's rules against it.
    pub async fn ingest(
        &self,
        fleet_id: Uuid,
        server_id: Uuid,
        s: &Sample,
    ) -> Result<Vec<Transition>> {
        s.validate()?;
        sqlx::query(
            "insert into musubi.server_metrics (server_id, fleet_id, cpu_pct, mem_bytes, mem_limit_bytes, net_in_bps,
                 net_out_bps, tick_ms, tick_p99_ms, tick_hz, players, custom)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(server_id)
        .bind(fleet_id)
        .bind(s.cpu_pct.map(|v| v as f32))
        .bind(s.mem_bytes)
        .bind(s.mem_limit_bytes)
        .bind(s.net_in_bps)
        .bind(s.net_out_bps)
        .bind(s.tick_ms.map(|v| v as f32))
        .bind(s.tick_p99_ms.map(|v| v as f32))
        .bind(s.tick_hz.map(|v| v as f32))
        .bind(s.players)
        .bind(Value::Object(s.custom.clone()))
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let rules = self.rules(fleet_id).await?;
        let mut out = Vec::new();
        for r in rules
            .iter()
            .filter(|r| r.enabled && r.metric != "silence_secs")
        {
            let Some(v) = s.metric(&r.metric) else {
                continue;
            };
            out.extend(
                self.evaluate(r, server_id, v, breaches(&r.op, v, r.threshold))
                    .await?,
            );
        }
        // the agent is talking again: resolve any silence alert
        if let Some(r) = rules
            .iter()
            .find(|r| r.enabled && r.metric == "silence_secs")
        {
            out.extend(self.evaluate(r, server_id, 0.0, false).await?);
        }
        Ok(out)
    }

    /// Breach → open (pending) alert; pending past `for_secs` → fired; no breach → pending
    /// dropped / fired resolved.
    async fn evaluate(
        &self,
        r: &Rule,
        server_id: Uuid,
        value: f64,
        breach: bool,
    ) -> Result<Vec<Transition>> {
        let mut out = Vec::new();
        let open = self.open_alert(server_id, &r.name).await?;
        let now = OffsetDateTime::now_utc();
        if breach {
            let a = match open {
                Some(a) => a,
                None => {
                    // `fleet_alerts_open_idx` allows one open alert per (server, rule): when two
                    // samples race, only one insert lands and the other must continue with the
                    // row that won, not with an id that never existed.
                    let inserted: Option<(Uuid, OffsetDateTime)> = sqlx::query_as(
                        "insert into musubi.fleet_alerts (id, fleet_id, server_id, rule, metric, severity, value, threshold)
                         values ($1, $2, $3, $4, $5, $6, $7, $8)
                         on conflict do nothing
                         returning id, breach_since",
                    )
                    .bind(Uuid::now_v7())
                    .bind(r.fleet_id)
                    .bind(server_id)
                    .bind(&r.name)
                    .bind(&r.metric)
                    .bind(&r.severity)
                    .bind(value)
                    .bind(r.threshold)
                    .fetch_optional(&self.db)
                    .await
                    .map_err(Error::internal)?;
                    match inserted {
                        Some((id, breach_since)) => Alert {
                            id,
                            fleet_id: r.fleet_id,
                            server_id,
                            rule: r.name.clone(),
                            metric: r.metric.clone(),
                            severity: r.severity.clone(),
                            value: Some(value),
                            threshold: r.threshold,
                            breach_since,
                            fired_at: None,
                            resolved_at: None,
                        },
                        None => self
                            .open_alert(server_id, &r.name)
                            .await?
                            .ok_or_else(|| Error::internal("open alert vanished"))?,
                    }
                }
            };
            let sustained = (now - a.breach_since).whole_seconds() >= r.for_secs as i64;
            // the row decides who fires: of two racing samples only the one whose update still
            // finds `fired_at` null reports the transition (one event, one webhook)
            let fired: Option<OffsetDateTime> = if a.fired_at.is_none() && sustained {
                sqlx::query_scalar(
                    "update musubi.fleet_alerts set value = $2, last_value_at = now(), fired_at = now()
                     where id = $1 and fired_at is null returning fired_at",
                )
                .bind(a.id)
                .bind(value)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?
            } else {
                None
            };
            if fired.is_none() {
                sqlx::query(
                    "update musubi.fleet_alerts set value = $2, last_value_at = now() where id = $1",
                )
                .bind(a.id)
                .bind(value)
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
            }
            if let Some(at) = fired {
                out.push(Transition {
                    kind: "fired",
                    alert: Alert {
                        value: Some(value),
                        fired_at: Some(at),
                        ..a
                    },
                });
            }
        } else if let Some(a) = open {
            if a.fired_at.is_some() {
                let resolved: Option<OffsetDateTime> = sqlx::query_scalar(
                    "update musubi.fleet_alerts set resolved_at = now(), value = $2
                     where id = $1 and resolved_at is null returning resolved_at",
                )
                .bind(a.id)
                .bind(value)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
                if let Some(at) = resolved {
                    out.push(Transition {
                        kind: "resolved",
                        alert: Alert {
                            value: Some(value),
                            resolved_at: Some(at),
                            ..a
                        },
                    });
                }
            } else {
                sqlx::query("delete from musubi.fleet_alerts where id = $1")
                    .bind(a.id)
                    .execute(&self.db)
                    .await
                    .map_err(Error::internal)?;
            }
        }
        Ok(out)
    }

    /// Evaluate `silence_secs` rules for every live server of every fleet that has one (the
    /// agent went quiet or never reported) and prune samples older than `retention_days`.
    /// Returns the transitions; meant for the lease holder's periodic loop.
    pub async fn sweep(&self, retention_days: i64) -> Result<Vec<Transition>> {
        let rules: Vec<Rule> = sqlx::query_as(
            "select fleet_id, name, metric, op, threshold, for_secs, severity, enabled, updated_at
             from musubi.fleet_alert_rules where enabled and metric = 'silence_secs'",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for r in &rules {
            let rows: Vec<(Uuid, Option<f64>)> = sqlx::query_as(
                "select g.id, extract(epoch from now() - coalesce(
                        (select max(at) from musubi.server_metrics m where m.server_id = g.id),
                        g.ready_at, g.created_at))::float8
                 from musubi.game_servers g
                 where g.fleet_id = $1 and g.state in ('ready', 'allocated')",
            )
            .bind(r.fleet_id)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
            for (sid, silence) in rows {
                let v = silence.unwrap_or(0.0).max(0.0);
                out.extend(
                    self.evaluate(r, sid, v, breaches(&r.op, v, r.threshold))
                        .await?,
                );
            }
        }
        // servers that are gone close their alerts
        sqlx::query(
            "update musubi.fleet_alerts a set resolved_at = now()
             where a.resolved_at is null and not exists (
                 select 1 from musubi.game_servers g where g.id = a.server_id and g.state in ('scheduled', 'starting', 'ready', 'allocated'))",
        )
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        if retention_days > 0 {
            sqlx::query(
                "delete from musubi.server_metrics where at < now() - make_interval(days => $1::int)",
            )
            .bind(retention_days as i32)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
            sqlx::query(
                "delete from musubi.fleet_alerts where resolved_at < now() - make_interval(days => $1::int)",
            )
            .bind(retention_days as i32)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        }
        Ok(out)
    }

    /// The one open (unresolved) alert of `rule` on `server_id`, if any.
    async fn open_alert(&self, server_id: Uuid, rule: &str) -> Result<Option<Alert>> {
        sqlx::query_as(
            "select id, fleet_id, server_id, rule, metric, severity, value, threshold, breach_since, fired_at, resolved_at
             from musubi.fleet_alerts where server_id = $1 and rule = $2 and resolved_at is null",
        )
        .bind(server_id)
        .bind(rule)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)
    }

    pub async fn rules(&self, fleet_id: Uuid) -> Result<Vec<Rule>> {
        sqlx::query_as(
            "select fleet_id, name, metric, op, threshold, for_secs, severity, enabled, updated_at
             from musubi.fleet_alert_rules where fleet_id = $1 order by name",
        )
        .bind(fleet_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)
    }

    pub async fn put_rule(&self, fleet_id: Uuid, name: &str, r: RuleInput) -> Result<Rule> {
        if !valid_rule_name(name) {
            return Err(Error::InvalidArgument("bad rule name".into()));
        }
        let custom_ok = r
            .metric
            .strip_prefix("custom.")
            .is_some_and(valid_gauge_name);
        if !METRICS.contains(&r.metric.as_str()) && !custom_ok {
            return Err(Error::InvalidArgument(format!(
                "unknown metric {:?}; one of {} or custom.<key>",
                r.metric,
                METRICS.join(", ")
            )));
        }
        if !["gt", "gte", "lt", "lte"].contains(&r.op.as_str()) {
            return Err(Error::InvalidArgument("op must be gt|gte|lt|lte".into()));
        }
        if !r.threshold.is_finite() {
            return Err(Error::InvalidArgument("threshold must be finite".into()));
        }
        if !(0..=86_400).contains(&r.for_secs) {
            return Err(Error::InvalidArgument("for_secs must be 0..=86400".into()));
        }
        if !["info", "warning", "critical"].contains(&r.severity.as_str()) {
            return Err(Error::InvalidArgument(
                "severity must be info|warning|critical".into(),
            ));
        }
        sqlx::query_as(
            "insert into musubi.fleet_alert_rules (fleet_id, name, metric, op, threshold, for_secs, severity, enabled)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (fleet_id, name) do update set metric = excluded.metric, op = excluded.op,
                 threshold = excluded.threshold, for_secs = excluded.for_secs, severity = excluded.severity,
                 enabled = excluded.enabled, updated_at = now()
             returning fleet_id, name, metric, op, threshold, for_secs, severity, enabled, updated_at",
        )
        .bind(fleet_id)
        .bind(name)
        .bind(&r.metric)
        .bind(&r.op)
        .bind(r.threshold)
        .bind(r.for_secs)
        .bind(&r.severity)
        .bind(r.enabled)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)
    }

    pub async fn delete_rule(&self, fleet_id: Uuid, name: &str) -> Result<bool> {
        let n =
            sqlx::query("delete from musubi.fleet_alert_rules where fleet_id = $1 and name = $2")
                .bind(fleet_id)
                .bind(name)
                .execute(&self.db)
                .await
                .map_err(Error::internal)?
                .rows_affected();
        sqlx::query(
            "update musubi.fleet_alerts set resolved_at = now() where fleet_id = $1 and rule = $2 and resolved_at is null",
        )
        .bind(fleet_id)
        .bind(name)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(n > 0)
    }

    /// `state`: `firing` (fired, unresolved), `pending`, `resolved`, `all`.
    pub async fn alerts(
        &self,
        fleet_id: Uuid,
        server_id: Option<Uuid>,
        state: &str,
        limit: i64,
    ) -> Result<Vec<Alert>> {
        let cond = match state {
            "firing" => "and fired_at is not null and resolved_at is null",
            "pending" => "and fired_at is null and resolved_at is null",
            "resolved" => "and resolved_at is not null",
            "all" => "",
            _ => {
                return Err(Error::InvalidArgument(
                    "state must be firing|pending|resolved|all".into(),
                ))
            }
        };
        sqlx::query_as(&format!(
            "select id, fleet_id, server_id, rule, metric, severity, value, threshold, breach_since, fired_at, resolved_at
             from musubi.fleet_alerts where fleet_id = $1 and ($2::uuid is null or server_id = $2) {cond}
             order by coalesce(fired_at, breach_since) desc limit $3"
        ))
        .bind(fleet_id)
        .bind(server_id)
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)
    }

    /// Latest sample of one server.
    pub async fn latest(&self, server_id: Uuid) -> Result<Option<Value>> {
        let rows = self.history(server_id, 0, 0, 1).await?;
        Ok(rows.into_iter().next())
    }

    /// Samples of one server, newest first. `step_secs > 0` averages into time buckets (max for
    /// players / p99), `since_secs` limits the window (0 = any).
    pub async fn history(
        &self,
        server_id: Uuid,
        since_secs: i64,
        step_secs: i64,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let limit = limit.clamp(1, 5000);
        // `since_secs <= 0` means the whole retention window; both knobs travel as bound
        // parameters, never spliced into the statement text
        let since_secs = since_secs.clamp(0, 10 * 365 * 86_400);
        let rows: Vec<Value> = if step_secs > 0 {
            let step = step_secs.clamp(1, 86_400);
            sqlx::query_scalar(
                "select to_jsonb(b) from (
                    select to_timestamp((floor(extract(epoch from at) / $4) * $4)::float8) as at,
                        count(*) as samples, avg(cpu_pct)::float8 as cpu_pct, max(cpu_pct)::float8 as cpu_pct_max,
                        avg(mem_bytes)::float8 as mem_bytes, max(mem_limit_bytes) as mem_limit_bytes,
                        avg(net_in_bps)::float8 as net_in_bps, avg(net_out_bps)::float8 as net_out_bps,
                        avg(tick_ms)::float8 as tick_ms, max(tick_p99_ms)::float8 as tick_p99_ms, avg(tick_hz)::float8 as tick_hz,
                        max(players) as players
                    from musubi.server_metrics
                    where server_id = $1 and ($3 = 0 or at > now() - make_interval(secs => $3::float8))
                    group by 1 order by 1 desc limit $2) b",
            )
            .bind(server_id)
            .bind(limit)
            .bind(since_secs)
            .bind(step)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?
        } else {
            sqlx::query_scalar(
                "select to_jsonb(m) - 'id' - 'fleet_id' - 'server_id' from musubi.server_metrics m
                 where server_id = $1 and ($3 = 0 or at > now() - make_interval(secs => $3::float8))
                 order by at desc limit $2",
            )
            .bind(server_id)
            .bind(limit)
            .bind(since_secs)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?
        };
        Ok(rows)
    }

    /// Fleet-wide picture: latest sample per live server (with `age_secs`), aggregates over the
    /// window and the count of servers that have not reported within `stale_secs`.
    pub async fn summary(
        &self,
        fleet_id: Uuid,
        window_secs: i64,
        stale_secs: i64,
    ) -> Result<Value> {
        let window = window_secs.clamp(10, 7 * 86_400);
        let stale_secs = stale_secs.clamp(1, 7 * 86_400);
        let servers: Vec<Value> = sqlx::query_scalar(
            "select to_jsonb(x) from (
                select g.id as server_id, g.state, g.players as reported_players, g.capacity,
                    m.at, extract(epoch from now() - m.at)::float8 as age_secs,
                    m.cpu_pct::float8 as cpu_pct, m.mem_bytes, m.mem_limit_bytes, m.net_in_bps, m.net_out_bps,
                    m.tick_ms::float8 as tick_ms, m.tick_p99_ms::float8 as tick_p99_ms, m.tick_hz::float8 as tick_hz,
                    m.players, m.custom
                from musubi.game_servers g
                left join lateral (select * from musubi.server_metrics s where s.server_id = g.id order by at desc limit 1) m on true
                where g.fleet_id = $1 and g.state in ('starting', 'ready', 'allocated')
                order by g.created_at) x",
        )
        .bind(fleet_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let agg: Value = sqlx::query_scalar(
            "select to_jsonb(a) from (
                select count(*) as samples, count(distinct server_id) as servers_reporting,
                    avg(cpu_pct)::float8 as cpu_pct_avg, max(cpu_pct)::float8 as cpu_pct_max,
                    avg(mem_bytes)::float8 as mem_bytes_avg, max(mem_bytes) as mem_bytes_max,
                    avg(tick_ms)::float8 as tick_ms_avg, max(tick_p99_ms)::float8 as tick_p99_ms_max,
                    sum(net_in_bps)::float8 / greatest(count(*), 1) as net_in_bps_avg,
                    sum(net_out_bps)::float8 / greatest(count(*), 1) as net_out_bps_avg
                from musubi.server_metrics where fleet_id = $1 and at > now() - make_interval(secs => $2::float8)) a",
        )
        .bind(fleet_id)
        .bind(window as f64)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let stale = servers
            .iter()
            .filter(|s| {
                s["state"] != "starting"
                    && s["age_secs"].as_f64().is_none_or(|a| a > stale_secs as f64)
            })
            .count();
        let players: i64 = servers
            .iter()
            .filter_map(|s| s["players"].as_i64().or(s["reported_players"].as_i64()))
            .sum();
        let firing: i64 = sqlx::query_scalar(
            "select count(*) from musubi.fleet_alerts where fleet_id = $1 and fired_at is not null and resolved_at is null",
        )
        .bind(fleet_id)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(json!({
            "fleet_id": fleet_id,
            "window_secs": window,
            "servers": servers,
            "live_servers": servers.len(),
            "stale_servers": stale,
            "players": players,
            "firing_alerts": firing,
            "aggregate": agg,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> Option<PgPool> {
        let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .ok()?;
        crate::db::migrate(&db).await.ok()?;
        Some(db)
    }

    async fn fleet_row(db: &PgPool) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query("insert into musubi.fleets (id, name) values ($1, $2)")
            .bind(id)
            .bind(format!("audit-metrics-{id}"))
            .execute(db)
            .await
            .unwrap();
        id
    }

    #[test]
    fn gauge_names_are_bounded() {
        assert!(valid_gauge_name("entities"));
        assert!(valid_gauge_name("ai.active_1"));
        assert!(!valid_gauge_name(""));
        assert!(!valid_gauge_name("bad key"));
        assert!(!valid_gauge_name("x'; drop"));
        assert!(!valid_gauge_name(&"a".repeat(65)));
    }

    #[tokio::test]
    async fn rule_custom_metric_key_is_validated() {
        let Some(db) = test_db().await else { return };
        let fleet = fleet_row(&db).await;
        let svc = FleetMetricsService::new(db);
        let input = |metric: &str| RuleInput {
            metric: metric.into(),
            op: "gt".into(),
            threshold: 1.0,
            for_secs: 0,
            severity: "warning".into(),
            enabled: true,
        };
        assert!(svc
            .put_rule(fleet, "ok", input("custom.entities"))
            .await
            .is_ok());
        assert!(svc
            .put_rule(fleet, "empty", input("custom."))
            .await
            .is_err());
        assert!(svc
            .put_rule(fleet, "space", input("custom.bad key"))
            .await
            .is_err());
        assert!(svc
            .put_rule(fleet, "long", input(&format!("custom.{}", "a".repeat(65))))
            .await
            .is_err());
        assert!(svc.put_rule(fleet, "unknown", input("nope")).await.is_err());
    }

    #[tokio::test]
    async fn history_filters_with_bound_parameters() {
        let Some(db) = test_db().await else { return };
        let fleet = fleet_row(&db).await;
        let svc = FleetMetricsService::new(db);
        let sid = Uuid::now_v7();
        let sample = Sample {
            cpu_pct: Some(42.0),
            players: Some(2),
            ..Default::default()
        };
        svc.ingest(fleet, sid, &sample).await.unwrap();
        assert_eq!(svc.history(sid, 0, 0, 10).await.unwrap().len(), 1);
        assert_eq!(svc.history(sid, -1, 0, 10).await.unwrap().len(), 1);
        assert_eq!(svc.history(sid, 3600, 0, 10).await.unwrap().len(), 1);
        assert_eq!(svc.history(sid, i64::MAX, 0, 10).await.unwrap().len(), 1);
        let bucketed = svc.history(sid, 3600, 60, 10).await.unwrap();
        assert_eq!(bucketed.len(), 1);
        assert_eq!(bucketed[0]["samples"], 1);
        assert_eq!(bucketed[0]["players"], 2);
        assert_eq!(svc.history(sid, 3600, i64::MAX, 10).await.unwrap().len(), 1);
        assert!(svc.summary(fleet, 300, -1).await.is_ok());
    }

    #[tokio::test]
    async fn concurrent_breaches_open_exactly_one_alert() {
        let Some(db) = test_db().await else { return };
        let fleet = fleet_row(&db).await;
        let svc = FleetMetricsService::new(db.clone());
        let rule = svc
            .put_rule(
                fleet,
                "cpu",
                RuleInput {
                    metric: "cpu_pct".into(),
                    op: "gt".into(),
                    threshold: 90.0,
                    for_secs: 0,
                    severity: "warning".into(),
                    enabled: true,
                },
            )
            .await
            .unwrap();
        let sid = Uuid::now_v7();
        let (a, b) = tokio::join!(
            svc.evaluate(&rule, sid, 95.0, true),
            svc.evaluate(&rule, sid, 96.0, true)
        );
        let transitions: Vec<Transition> = a.unwrap().into_iter().chain(b.unwrap()).collect();
        let open: Vec<Uuid> = sqlx::query_scalar(
            "select id from musubi.fleet_alerts where server_id = $1 and rule = 'cpu' and resolved_at is null",
        )
        .bind(sid)
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(open.len(), 1, "one open alert per (server, rule)");
        assert!(!transitions.is_empty());
        for t in &transitions {
            assert_eq!(t.kind, "fired");
            assert_eq!(
                t.alert.id, open[0],
                "transition must carry the stored alert id"
            );
        }
        // the losing sample must not have raised a second `fired`
        assert_eq!(transitions.len(), 1, "{transitions:?}");
        let (a, b) = tokio::join!(
            svc.evaluate(&rule, sid, 10.0, false),
            svc.evaluate(&rule, sid, 11.0, false)
        );
        let resolved: Vec<Transition> = a.unwrap().into_iter().chain(b.unwrap()).collect();
        assert_eq!(resolved.len(), 1, "{resolved:?}");
        assert_eq!(resolved[0].kind, "resolved");
        assert_eq!(resolved[0].alert.id, open[0]);
    }

    #[test]
    fn derived_metrics_and_custom_paths() {
        let s = Sample {
            mem_bytes: Some(512),
            mem_limit_bytes: Some(1024),
            players: Some(3),
            capacity: Some(4),
            custom: serde_json::from_value(json!({"entities": 1500})).unwrap(),
            ..Default::default()
        };
        assert_eq!(s.metric("mem_pct"), Some(50.0));
        assert_eq!(s.metric("occupancy_pct"), Some(75.0));
        assert_eq!(s.metric("custom.entities"), Some(1500.0));
        assert_eq!(s.metric("cpu_pct"), None);
        assert_eq!(s.metric("custom.nope"), None);
        assert!(s.validate().is_ok());
    }

    #[test]
    fn validation_rejects_bad_samples() {
        let mut s = Sample {
            cpu_pct: Some(-1.0),
            ..Default::default()
        };
        assert!(s.validate().is_err());
        s.cpu_pct = Some(f64::NAN);
        assert!(s.validate().is_err());
        s.cpu_pct = None;
        s.custom = serde_json::from_value(json!({"bad key": 1})).unwrap();
        assert!(s.validate().is_err());
        s.custom = serde_json::from_value(json!({"k": "str"})).unwrap();
        assert!(s.validate().is_err());
    }

    #[test]
    fn ops() {
        assert!(breaches("gt", 91.0, 90.0));
        assert!(!breaches("gt", 90.0, 90.0));
        assert!(breaches("gte", 90.0, 90.0));
        assert!(breaches("lt", 10.0, 20.0));
        assert!(breaches("lte", 20.0, 20.0));
        assert!(!breaches("eq", 1.0, 1.0));
    }
}
