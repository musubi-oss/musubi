//! Product analytics (competitive P0-26 / P0-22 / P0-35): client telemetry ingest with a schema
//! registry and sampling, the `analytics_events` table, KPI queries (DAU/MAU, retention cohorts,
//! funnels, revenue, event volumes) and export sinks (JSONL files, ClickHouse, webhook).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use musubi_core::config::{AnalyticsConfig, AnalyticsSink};
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const SCHEMA_TTL: Duration = Duration::from_secs(30);
const QUEUE_CAP: usize = 50_000;
const SINK_BATCH: usize = 2_000;

type SchemaCache = Option<(Instant, Arc<HashMap<String, Schema>>)>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Property names every event must carry.
    #[serde(default)]
    pub required: Vec<String>,
    /// Keep this percentage of events (deterministic per session / device / account).
    #[serde(default = "hundred")]
    pub sampling_pct: i32,
}

fn yes() -> bool {
    true
}

fn hundred() -> i32 {
    100
}

/// One event as sent by a client.
#[derive(Debug, Clone, Deserialize)]
pub struct Incoming {
    pub name: String,
    /// Unix seconds or milliseconds (values above 1e12 are milliseconds); default = now.
    #[serde(default)]
    pub at: Option<i64>,
    #[serde(default)]
    pub properties: Value,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub country: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IngestReport {
    pub accepted: usize,
    pub sampled_out: usize,
    pub rejected: Vec<Value>,
}

/// A stored / exported event.
type EventPropRow = (
    String,
    i64,
    OffsetDateTime,
    OffsetDateTime,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    Option<f64>,
    f64,
    Option<String>,
    OffsetDateTime,
);

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub id: Uuid,
    pub at: i64,
    pub name: String,
    pub account_id: Option<Uuid>,
    pub device_id: Option<String>,
    pub session_id: Option<String>,
    pub platform: Option<String>,
    pub country: Option<String>,
    pub properties: Value,
    pub source: String,
}

pub struct Analytics {
    db: PgPool,
    pub cfg: AnalyticsConfig,
    http: reqwest::Client,
    queue: Mutex<Vec<Row>>,
    schemas: Mutex<SchemaCache>,
    sampling: Mutex<Option<(std::time::Instant, Arc<SamplingPolicy>)>>,
}

/// Title-wide sampling (Pragma `globalWeight` / `alwaysEnabledIds`): the effective keep rate of
/// an event is `schema.sampling_pct × global_pct / 100` (unregistered events: `global_pct`),
/// except names in `always_enabled`, which are always kept. Clients fetch the same policy
/// (`GET /v1/telemetry/sampling`) and skip sending what the server would drop anyway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SamplingPolicy {
    pub global_pct: i32,
    pub always_enabled: Vec<String>,
    /// Unix seconds of the last change — poll cheaply / cache key.
    pub version: i64,
}

impl Default for SamplingPolicy {
    fn default() -> Self {
        Self {
            global_pct: 100,
            always_enabled: Vec::new(),
            version: 0,
        }
    }
}

impl SamplingPolicy {
    /// Effective keep percentage for `name` given its schema rate (100 when unregistered).
    pub fn effective_pct(&self, name: &str, schema_pct: i32) -> i32 {
        if self.always_enabled.iter().any(|n| n == name) {
            return 100;
        }
        (schema_pct.clamp(0, 100) * self.global_pct.clamp(0, 100)) / 100
    }
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' || c == ':')
}

fn bucket(key: &str) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % 100) as u32
}

impl Analytics {
    pub fn new(db: PgPool, cfg: AnalyticsConfig) -> Arc<Self> {
        Arc::new(Self {
            db,
            cfg,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_default(),
            queue: Mutex::new(Vec::new()),
            schemas: Mutex::new(None),
            sampling: Mutex::new(None),
        })
    }

    /// Periodic sink flush (every `flush_secs`).
    pub fn spawn_flusher(self: &Arc<Self>) {
        if self.cfg.sinks.is_empty() {
            return;
        }
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(me.cfg.flush_secs.max(1)));
            loop {
                tick.tick().await;
                me.flush().await;
            }
        });
    }

    pub fn invalidate_sampling(&self) {
        *self.sampling.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Current sampling policy (cached 30 s per node).
    pub async fn sampling(&self) -> Result<Arc<SamplingPolicy>> {
        if let Some((at, p)) = &*self.sampling.lock().unwrap_or_else(|e| e.into_inner()) {
            if at.elapsed() < Duration::from_secs(30) {
                return Ok(p.clone());
            }
        }
        let row: Option<(i32, Vec<String>, OffsetDateTime)> = sqlx::query_as(
            "select global_pct, always_enabled, updated_at from musubi.telemetry_sampling where id = 1",
        )
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let p = Arc::new(match row {
            Some((global_pct, always_enabled, at)) => SamplingPolicy {
                global_pct,
                always_enabled,
                version: at.unix_timestamp(),
            },
            None => SamplingPolicy::default(),
        });
        *self.sampling.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((std::time::Instant::now(), p.clone()));
        Ok(p)
    }

    pub async fn save_sampling(&self, p: &SamplingPolicy) -> Result<Arc<SamplingPolicy>> {
        if !(0..=100).contains(&p.global_pct) {
            return Err(Error::InvalidArgument("global_pct: 0..=100".into()));
        }
        if p.always_enabled.len() > 256 || p.always_enabled.iter().any(|n| !valid_name(n)) {
            return Err(Error::InvalidArgument(
                "always_enabled: at most 256 valid event names".into(),
            ));
        }
        let mut names = p.always_enabled.clone();
        names.sort();
        names.dedup();
        sqlx::query(
            "insert into musubi.telemetry_sampling (id, global_pct, always_enabled, updated_at)
             values (1, $1, $2, now())
             on conflict (id) do update set global_pct = excluded.global_pct,
               always_enabled = excluded.always_enabled, updated_at = now()",
        )
        .bind(p.global_pct)
        .bind(&names)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        self.invalidate_sampling();
        self.sampling().await
    }

    pub fn invalidate_schemas(&self) {
        *self.schemas.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub async fn schemas(&self) -> Result<Arc<HashMap<String, Schema>>> {
        if let Some((at, s)) = &*self.schemas.lock().unwrap_or_else(|e| e.into_inner()) {
            if at.elapsed() < SCHEMA_TTL {
                return Ok(s.clone());
            }
        }
        let rows: Vec<(String, String, bool, Vec<String>, i32)> = sqlx::query_as(
            "select name, description, enabled, required, sampling_pct from musubi.analytics_schemas",
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let map: HashMap<String, Schema> = rows
            .into_iter()
            .map(|(name, description, enabled, required, sampling_pct)| {
                (
                    name.clone(),
                    Schema {
                        name,
                        description,
                        enabled,
                        required,
                        sampling_pct,
                    },
                )
            })
            .collect();
        let map = Arc::new(map);
        *self.schemas.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), map.clone()));
        Ok(map)
    }

    pub async fn save_schema(&self, s: &Schema) -> Result<Schema> {
        if !valid_name(&s.name) {
            return Err(Error::InvalidArgument(
                "name: [A-Za-z0-9._:-]{1,128}".into(),
            ));
        }
        if !(0..=100).contains(&s.sampling_pct) {
            return Err(Error::InvalidArgument("sampling_pct: 0..=100".into()));
        }
        if s.required.len() > 64 {
            return Err(Error::InvalidArgument(
                "required: at most 64 properties".into(),
            ));
        }
        sqlx::query(
            "insert into musubi.analytics_schemas (name, description, enabled, required, sampling_pct)
             values ($1, $2, $3, $4, $5)
             on conflict (name) do update set description = excluded.description, enabled = excluded.enabled,
                required = excluded.required, sampling_pct = excluded.sampling_pct, updated_at = now()",
        )
        .bind(&s.name)
        .bind(&s.description)
        .bind(s.enabled)
        .bind(&s.required)
        .bind(s.sampling_pct)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        self.invalidate_schemas();
        Ok(s.clone())
    }

    pub async fn delete_schema(&self, name: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.analytics_schemas where name = $1")
            .bind(name)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        self.invalidate_schemas();
        Ok(n > 0)
    }

    /// Validate, sample and store a client batch.
    pub async fn ingest(
        &self,
        batch: Vec<Incoming>,
        account: Option<AccountId>,
        device_id: Option<String>,
        source: &str,
    ) -> Result<IngestReport> {
        if !self.cfg.enabled {
            return Err(Error::PermissionDenied("analytics disabled".into()));
        }
        if batch.len() > self.cfg.max_batch {
            return Err(Error::InvalidArgument(format!(
                "at most {} events per request",
                self.cfg.max_batch
            )));
        }
        let schemas = self.schemas().await?;
        let sampling = self.sampling().await?;
        let now = OffsetDateTime::now_utc();
        let now_s = now.unix_timestamp();
        let mut report = IngestReport::default();
        let mut rows: Vec<Row> = Vec::with_capacity(batch.len());
        for (i, e) in batch.into_iter().enumerate() {
            let reject = |reason: &str| json!({ "index": i, "name": e.name, "reason": reason });
            if !valid_name(&e.name) {
                report.rejected.push(reject("invalid name"));
                continue;
            }
            if !e.properties.is_null() && !e.properties.is_object() {
                report.rejected.push(reject("properties must be an object"));
                continue;
            }
            let props = if e.properties.is_null() {
                json!({})
            } else {
                e.properties.clone()
            };
            if props.to_string().len() > self.cfg.max_event_bytes {
                report.rejected.push(reject("properties too large"));
                continue;
            }
            let schema = schemas.get(&e.name);
            let schema_pct = match schema {
                None if !self.cfg.accept_unregistered => {
                    report.rejected.push(reject("unregistered event"));
                    continue;
                }
                Some(s) if !s.enabled => {
                    report.rejected.push(reject("event disabled"));
                    continue;
                }
                Some(s) => {
                    if let Some(missing) =
                        s.required.iter().find(|r| props.get(r.as_str()).is_none())
                    {
                        report
                            .rejected
                            .push(reject(&format!("missing property {missing}")));
                        continue;
                    }
                    s.sampling_pct
                }
                None => 100,
            };
            let pct = sampling.effective_pct(&e.name, schema_pct);
            if pct < 100 {
                let key = e
                    .session_id
                    .clone()
                    .or_else(|| e.device_id.clone())
                    .or_else(|| device_id.clone())
                    .or_else(|| account.map(|a| a.to_string()))
                    .unwrap_or_else(|| Uuid::now_v7().to_string());
                if bucket(&format!("{}/{key}", e.name)) as i32 >= pct {
                    report.sampled_out += 1;
                    continue;
                }
            }
            let mut at = e.at.unwrap_or(now_s);
            if at > 1_000_000_000_000 {
                at /= 1000;
            }
            if at <= 0 || at > now_s + 60 {
                at = now_s;
            }
            rows.push(Row {
                id: Uuid::now_v7(),
                at,
                name: e.name,
                account_id: account.map(|a| a.0),
                device_id: e.device_id.or_else(|| device_id.clone()),
                session_id: e.session_id,
                platform: e.platform,
                country: e.country,
                properties: props,
                source: source.to_string(),
            });
        }
        report.accepted = rows.len();
        self.store(&rows).await?;
        self.enqueue(rows);
        Ok(report)
    }

    async fn store(&self, rows: &[Row]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let ats: Vec<OffsetDateTime> = rows
            .iter()
            .map(|r| {
                OffsetDateTime::from_unix_timestamp(r.at)
                    .unwrap_or_else(|_| OffsetDateTime::now_utc())
            })
            .collect();
        sqlx::query(
            "insert into musubi.analytics_events (id, at, name, account_id, device_id, session_id, platform, country, properties, source)
             select * from unnest($1::uuid[], $2::timestamptz[], $3::text[], $4::uuid[], $5::text[], $6::text[], $7::text[], $8::text[], $9::jsonb[], $10::text[])",
        )
        .bind(rows.iter().map(|r| r.id).collect::<Vec<_>>())
        .bind(ats)
        .bind(rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>())
        .bind(rows.iter().map(|r| r.account_id).collect::<Vec<Option<Uuid>>>())
        .bind(rows.iter().map(|r| r.device_id.clone()).collect::<Vec<Option<String>>>())
        .bind(rows.iter().map(|r| r.session_id.clone()).collect::<Vec<Option<String>>>())
        .bind(rows.iter().map(|r| r.platform.clone()).collect::<Vec<Option<String>>>())
        .bind(rows.iter().map(|r| r.country.clone()).collect::<Vec<Option<String>>>())
        .bind(rows.iter().map(|r| r.properties.clone()).collect::<Vec<Value>>())
        .bind(rows.iter().map(|r| r.source.clone()).collect::<Vec<_>>())
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        self.fold_event_props(rows).await
    }

    /// Fold a stored batch into `player_event_props`: one upsert per (account, event) pair with
    /// the batch pre-aggregated, so a burst of the same event costs one row write.
    async fn fold_event_props(&self, rows: &[Row]) -> Result<()> {
        #[derive(Default)]
        struct Agg {
            count: i64,
            first_at: i64,
            last_at: i64,
            v_first: Option<f64>,
            v_high: Option<f64>,
            v_low: Option<f64>,
            v_last: Option<f64>,
            v_sum: f64,
            last_session: Option<String>,
        }
        let mut aggs: HashMap<(Uuid, String), Agg> = HashMap::new();
        let mut order: Vec<(Uuid, String)> = Vec::new();
        for r in rows {
            let Some(acc) = r.account_id else { continue };
            let key = (acc, r.name.clone());
            let a = aggs.entry(key.clone()).or_insert_with(|| {
                order.push(key);
                Agg {
                    first_at: r.at,
                    last_at: r.at,
                    ..Agg::default()
                }
            });
            a.count += 1;
            a.first_at = a.first_at.min(r.at);
            if r.at >= a.last_at {
                a.last_at = r.at;
                if r.session_id.is_some() {
                    a.last_session = r.session_id.clone();
                }
            }
            if let Some(v) = r.properties.get("value").and_then(Value::as_f64) {
                if v.is_finite() {
                    a.v_first.get_or_insert(v);
                    a.v_high = Some(a.v_high.map_or(v, |h| h.max(v)));
                    a.v_low = Some(a.v_low.map_or(v, |l| l.min(v)));
                    a.v_last = Some(v);
                    a.v_sum += v;
                }
            }
        }
        if order.is_empty() {
            return Ok(());
        }
        let ts = |t: i64| {
            OffsetDateTime::from_unix_timestamp(t).unwrap_or_else(|_| OffsetDateTime::now_utc())
        };
        let get = |k: &(Uuid, String)| &aggs[k];
        sqlx::query(
            "insert into musubi.player_event_props
                (account_id, event, count, seen_first, seen_last, value_first, value_high, value_low, value_last, value_sum, last_session_id, updated_at)
             select * , now() from unnest($1::uuid[], $2::text[], $3::bigint[], $4::timestamptz[], $5::timestamptz[], $6::float8[], $7::float8[], $8::float8[], $9::float8[], $10::float8[], $11::text[])
             on conflict (account_id, event) do update set
                count = musubi.player_event_props.count + excluded.count,
                seen_first = least(musubi.player_event_props.seen_first, excluded.seen_first),
                seen_last = greatest(musubi.player_event_props.seen_last, excluded.seen_last),
                value_first = coalesce(musubi.player_event_props.value_first, excluded.value_first),
                value_high = greatest(musubi.player_event_props.value_high, excluded.value_high),
                value_low = least(musubi.player_event_props.value_low, excluded.value_low),
                value_last = coalesce(excluded.value_last, musubi.player_event_props.value_last),
                value_sum = musubi.player_event_props.value_sum + excluded.value_sum,
                last_session_id = case when excluded.seen_last >= musubi.player_event_props.seen_last
                    then coalesce(excluded.last_session_id, musubi.player_event_props.last_session_id)
                    else musubi.player_event_props.last_session_id end,
                updated_at = now()",
        )
        .bind(order.iter().map(|k| k.0).collect::<Vec<_>>())
        .bind(order.iter().map(|k| k.1.clone()).collect::<Vec<_>>())
        .bind(order.iter().map(|k| get(k).count).collect::<Vec<_>>())
        .bind(order.iter().map(|k| ts(get(k).first_at)).collect::<Vec<_>>())
        .bind(order.iter().map(|k| ts(get(k).last_at)).collect::<Vec<_>>())
        .bind(order.iter().map(|k| get(k).v_first).collect::<Vec<Option<f64>>>())
        .bind(order.iter().map(|k| get(k).v_high).collect::<Vec<Option<f64>>>())
        .bind(order.iter().map(|k| get(k).v_low).collect::<Vec<Option<f64>>>())
        .bind(order.iter().map(|k| get(k).v_last).collect::<Vec<Option<f64>>>())
        .bind(order.iter().map(|k| get(k).v_sum).collect::<Vec<f64>>())
        .bind(order.iter().map(|k| get(k).last_session.clone()).collect::<Vec<Option<String>>>())
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Computed per-event properties of one account (`<event>Count/SeenFirst/…` in Hiro terms),
    /// newest activity first, plus the session that produced the most recent event overall.
    pub async fn event_props(&self, account: Uuid, event: Option<&str>) -> Result<Value> {
        let rows: Vec<EventPropRow> = sqlx::query_as(
            "select event, count, seen_first, seen_last, value_first, value_high, value_low, value_last, value_sum, last_session_id, updated_at
             from musubi.player_event_props
             where account_id = $1 and ($2::text is null or event = $2)
             order by seen_last desc, event",
        )
        .bind(account)
        .bind(event)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let rfc = |t: OffsetDateTime| {
            t.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default()
        };
        let last_active_session_id = rows.first().and_then(|r| r.9.clone());
        let events: Vec<Value> = rows
            .iter()
            .map(|r| {
                json!({
                    "event": r.0, "count": r.1,
                    "seen_first": rfc(r.2), "seen_last": rfc(r.3),
                    "value_first": r.4, "value_high": r.5, "value_low": r.6, "value_last": r.7,
                    "value_sum": r.8, "last_session_id": r.9, "updated_at": rfc(r.10),
                })
            })
            .collect();
        Ok(json!({
            "account_id": account,
            "last_active_session_id": last_active_session_id,
            "events": events,
        }))
    }

    /// Drop the aggregates of an account (all events or one); the raw events stay.
    pub async fn reset_event_props(&self, account: Uuid, event: Option<&str>) -> Result<u64> {
        sqlx::query(
            "delete from musubi.player_event_props where account_id = $1 and ($2::text is null or event = $2)",
        )
        .bind(account)
        .bind(event)
        .execute(&self.db)
        .await
        .map(|r| r.rows_affected())
        .map_err(Error::internal)
    }

    /// Copy of a server-side domain event (when `mirror_domain_events`).
    pub fn push_domain(self: &Arc<Self>, kind: &str, account: Option<AccountId>, payload: Value) {
        if !self.cfg.enabled || !self.cfg.mirror_domain_events {
            return;
        }
        let row = Row {
            id: Uuid::now_v7(),
            at: OffsetDateTime::now_utc().unix_timestamp(),
            name: kind.to_string(),
            account_id: account.map(|a| a.0),
            device_id: None,
            session_id: None,
            platform: None,
            country: None,
            properties: payload,
            source: "server".into(),
        };
        let me = self.clone();
        tokio::spawn(async move {
            if let Err(e) = me.store(std::slice::from_ref(&row)).await {
                tracing::warn!(error = %e, "analytics mirror failed");
            }
            me.enqueue(vec![row]);
        });
    }

    fn enqueue(&self, rows: Vec<Row>) {
        if self.cfg.sinks.is_empty() || rows.is_empty() {
            return;
        }
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.extend(rows);
        if q.len() > QUEUE_CAP {
            let drop = q.len() - QUEUE_CAP;
            q.drain(..drop);
            tracing::warn!(dropped = drop, "analytics sink queue overflow");
        }
    }

    /// Push queued events to every sink; returns events delivered per sink.
    pub async fn flush(&self) -> Vec<Value> {
        let batch: Vec<Row> = {
            let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
            let n = q.len().min(SINK_BATCH);
            q.drain(..n).collect()
        };
        let mut out = Vec::new();
        for sink in &self.cfg.sinks {
            let rows: Vec<&Row> = batch
                .iter()
                .filter(|r| sink.names.is_empty() || sink.names.contains(&r.name))
                .collect();
            let res = if rows.is_empty() {
                Ok(0)
            } else {
                self.deliver(sink, &rows).await
            };
            match res {
                Ok(n) => out.push(json!({ "sink": sink.name, "kind": sink.kind, "delivered": n })),
                Err(e) => {
                    tracing::warn!(sink = %sink.name, error = %e, "analytics sink failed");
                    out.push(
                        json!({ "sink": sink.name, "kind": sink.kind, "error": e.to_string() }),
                    );
                }
            }
        }
        out
    }

    async fn deliver(&self, sink: &AnalyticsSink, rows: &[&Row]) -> Result<usize> {
        match sink.kind.as_str() {
            "jsonl" => {
                let dir = sink
                    .path
                    .as_deref()
                    .ok_or_else(|| Error::InvalidArgument("jsonl sink needs path".into()))?;
                std::fs::create_dir_all(dir).map_err(Error::internal)?;
                let day = OffsetDateTime::now_utc().date();
                let file = std::path::Path::new(dir).join(format!(
                    "{}-{day}.jsonl",
                    if sink.name.is_empty() {
                        "events"
                    } else {
                        &sink.name
                    }
                ));
                let mut body = String::new();
                for r in rows {
                    body.push_str(&serde_json::to_string(r).map_err(Error::internal)?);
                    body.push('\n');
                }
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(file)
                    .map_err(Error::internal)?;
                f.write_all(body.as_bytes()).map_err(Error::internal)?;
                Ok(rows.len())
            }
            "clickhouse" => {
                let url = sink
                    .url
                    .as_deref()
                    .ok_or_else(|| Error::InvalidArgument("clickhouse sink needs url".into()))?;
                let table = sink.table.as_deref().unwrap_or("analytics_events");
                let mut body = String::new();
                for r in rows {
                    body.push_str(&serde_json::to_string(r).map_err(Error::internal)?);
                    body.push('\n');
                }
                let mut req = self
                    .http
                    .post(url)
                    .query(&[("query", format!("INSERT INTO {table} FORMAT JSONEachRow"))])
                    .body(body);
                for (k, v) in &sink.headers {
                    req = req.header(k, v);
                }
                let res = req.send().await.map_err(Error::internal)?;
                if !res.status().is_success() {
                    return Err(Error::Internal(format!("clickhouse {}", res.status())));
                }
                Ok(rows.len())
            }
            "webhook" => {
                let url = sink
                    .url
                    .as_deref()
                    .ok_or_else(|| Error::InvalidArgument("webhook sink needs url".into()))?;
                let mut req = self.http.post(url).json(&json!({ "events": rows }));
                for (k, v) in &sink.headers {
                    req = req.header(k, v);
                }
                let res = req.send().await.map_err(Error::internal)?;
                if !res.status().is_success() {
                    return Err(Error::Internal(format!("webhook {}", res.status())));
                }
                Ok(rows.len())
            }
            other => Err(Error::InvalidArgument(format!("unknown sink kind {other}"))),
        }
    }

    // ------------------------------------------------------------ queries

    pub async fn export(
        &self,
        name: Option<&str>,
        account: Option<Uuid>,
        after: Option<i64>,
        before: Option<i64>,
        cursor: Option<Uuid>,
        limit: i64,
    ) -> Result<Vec<Row>> {
        type R = (
            Uuid,
            OffsetDateTime,
            String,
            Option<Uuid>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Value,
            String,
        );
        let rows: Vec<R> = sqlx::query_as(
            "select id, at, name, account_id, device_id, session_id, platform, country, properties, source
             from musubi.analytics_events
             where ($1::text is null or name = $1) and ($2::uuid is null or account_id = $2)
               and ($3::bigint is null or at >= to_timestamp($3)) and ($4::bigint is null or at < to_timestamp($4))
               and ($5::uuid is null or id > $5)
             order by id limit $6",
        )
        .bind(name)
        .bind(account)
        .bind(after)
        .bind(before)
        .bind(cursor)
        .bind(limit.clamp(1, 10_000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    at,
                    name,
                    account_id,
                    device_id,
                    session_id,
                    platform,
                    country,
                    properties,
                    source,
                )| Row {
                    id,
                    at: at.unix_timestamp(),
                    name,
                    account_id,
                    device_id,
                    session_id,
                    platform,
                    country,
                    properties,
                    source,
                },
            )
            .collect())
    }

    /// Daily / monthly active users (distinct account, else device) for the last `days`.
    pub async fn active_users(&self, days: i64) -> Result<Value> {
        let days = days.clamp(1, 365);
        let daily: Vec<(time::Date, i64, i64)> = sqlx::query_as(
            "select at::date as d, count(distinct coalesce(account_id::text, device_id)) as users, count(*) as events
             from musubi.analytics_events where at >= now() - make_interval(days => $1::int) and source = 'client'
             group by d order by d",
        )
        .bind(days as i32)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let (mau, wau): (i64, i64) = sqlx::query_as(
            "select count(distinct coalesce(account_id::text, device_id)) filter (where at >= now() - interval '30 days'),
                    count(distinct coalesce(account_id::text, device_id)) filter (where at >= now() - interval '7 days')
             from musubi.analytics_events where at >= now() - interval '30 days' and source = 'client'",
        )
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let today = daily.last().map(|(_, u, _)| *u).unwrap_or(0);
        Ok(json!({
            "days": days, "dau": today, "wau": wau, "mau": mau,
            "stickiness": if mau > 0 { today as f64 / mau as f64 } else { 0.0 },
            "daily": daily.iter().map(|(d, u, e)| json!({ "day": d.to_string(), "users": u, "events": e })).collect::<Vec<_>>(),
        }))
    }

    /// Retention cohorts by first-seen day: D1 / D3 / D7 / D14 / D30 for cohorts of the last `days`.
    pub async fn retention(&self, days: i64) -> Result<Value> {
        let days = days.clamp(1, 180);
        type R = (time::Date, i64, i64, i64, i64, i64, i64);
        let rows: Vec<R> = sqlx::query_as(
            "with u as (
                select coalesce(account_id::text, device_id) as uid, at::date as d
                from musubi.analytics_events where source = 'client' and coalesce(account_id::text, device_id) is not null
                  and at >= now() - make_interval(days => ($1::int + 31))
                group by 1, 2
             ), first as (select uid, min(d) as d0 from u group by uid)
             select f.d0, count(distinct f.uid),
                    count(distinct f.uid) filter (where u.d = f.d0 + 1),
                    count(distinct f.uid) filter (where u.d = f.d0 + 3),
                    count(distinct f.uid) filter (where u.d = f.d0 + 7),
                    count(distinct f.uid) filter (where u.d = f.d0 + 14),
                    count(distinct f.uid) filter (where u.d = f.d0 + 30)
             from first f join u on u.uid = f.uid
             where f.d0 >= current_date - $1::int
             group by f.d0 order by f.d0 desc",
        )
        .bind(days as i32)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let rate = |n: i64, size: i64| {
            if size > 0 {
                n as f64 / size as f64
            } else {
                0.0
            }
        };
        Ok(json!({
            "days": days,
            "cohorts": rows.iter().map(|(d0, size, d1, d3, d7, d14, d30)| json!({
                "cohort": d0.to_string(), "size": size,
                "d1": rate(*d1, *size), "d3": rate(*d3, *size), "d7": rate(*d7, *size),
                "d14": rate(*d14, *size), "d30": rate(*d30, *size),
            })).collect::<Vec<_>>(),
        }))
    }

    /// Ordered funnel: users who did step 1, then step 2 after it, … within the last `days`.
    pub async fn funnel(&self, steps: &[String], days: i64) -> Result<Value> {
        if steps.len() < 2 || steps.len() > 8 {
            return Err(Error::InvalidArgument("steps: 2..8 event names".into()));
        }
        let days = days.clamp(1, 365);
        let mut select = String::from(
            "with e as (select coalesce(account_id::text, device_id) as uid, name, at from musubi.analytics_events
                        where at >= now() - make_interval(days => $1::int) and name = any($2)
                          and coalesce(account_id::text, device_id) is not null),
             s0 as (select uid, min(at) as t0 from e where name = $2[1] group by uid)",
        );
        for i in 1..steps.len() {
            // earliest occurrence of step i after the user's step i-1
            select.push_str(&format!(
                ", s{i} as (select p.*, (select min(e2.at) from e e2 where e2.uid = p.uid and e2.name = $2[{}] and e2.at > p.t{}) as t{i} from s{} p)",
                i + 1,
                i - 1,
                i - 1
            ));
        }
        let last = steps.len() - 1;
        let conds: Vec<String> = (0..steps.len())
            .map(|i| format!("count(*) filter (where t{i} is not null)"))
            .collect();
        select.push_str(&format!(" select {} from s{last}", conds.join(", ")));
        let row: sqlx::postgres::PgRow = sqlx::query(&select)
            .bind(days as i32)
            .bind(steps)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
        use sqlx::Row as _;
        let counts: Vec<i64> = (0..steps.len())
            .map(|i| row.try_get::<i64, _>(i).unwrap_or(0))
            .collect();
        let first = counts.first().copied().unwrap_or(0);
        Ok(json!({
            "days": days,
            "steps": steps.iter().enumerate().map(|(i, s)| json!({
                "step": s, "users": counts[i],
                "conversion_from_start": if first > 0 { counts[i] as f64 / first as f64 } else { 0.0 },
                "conversion_from_previous": if i == 0 { 1.0 } else if counts[i - 1] > 0 { counts[i] as f64 / counts[i - 1] as f64 } else { 0.0 },
            })).collect::<Vec<_>>(),
        }))
    }

    /// Store revenue per day and currency (`musubi.purchases`), with buyers and ARPDAU
    /// against analytics DAU.
    pub async fn revenue(&self, days: i64) -> Result<Value> {
        let days = days.clamp(1, 365);
        type R = (time::Date, String, i64, i64, i64);
        let rows: Vec<R> = sqlx::query_as(
            "select created_at::date as d, currency, coalesce(sum(amount), 0)::bigint, count(*), count(distinct account_id)
             from musubi.purchases where created_at >= now() - make_interval(days => $1::int)
             group by d, currency order by d, currency",
        )
        .bind(days as i32)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let dau: Vec<(time::Date, i64)> = sqlx::query_as(
            "select at::date, count(distinct coalesce(account_id::text, device_id)) from musubi.analytics_events
             where at >= now() - make_interval(days => $1::int) and source = 'client' group by 1",
        )
        .bind(days as i32)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let dau: HashMap<time::Date, i64> = dau.into_iter().collect();
        Ok(json!({
            "days": days,
            "daily": rows.iter().map(|(d, cur, amount, purchases, buyers)| {
                let users = dau.get(d).copied().unwrap_or(0);
                json!({ "day": d.to_string(), "currency": cur, "amount": amount, "purchases": purchases, "buyers": buyers,
                        "arpdau": if users > 0 { *amount as f64 / users as f64 } else { 0.0 },
                        "arppu": if *buyers > 0 { *amount as f64 / *buyers as f64 } else { 0.0 } })
            }).collect::<Vec<_>>(),
        }))
    }

    /// Event volumes by name for the last `days`.
    pub async fn event_counts(&self, days: i64) -> Result<Value> {
        let days = days.clamp(1, 365);
        let rows: Vec<(String, i64, i64)> = sqlx::query_as(
            "select name, count(*), count(distinct coalesce(account_id::text, device_id))
             from musubi.analytics_events where at >= now() - make_interval(days => $1::int)
             group by name order by 2 desc limit 500",
        )
        .bind(days as i32)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(json!({
            "days": days,
            "events": rows.iter().map(|(n, c, u)| json!({ "name": n, "count": c, "users": u })).collect::<Vec<_>>(),
        }))
    }
}
