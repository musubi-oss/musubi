//! Client crash / error reports grouped by fingerprint with triage state (competitive P1-39) and
//! the per-player event history / customer-support timeline (P1-40).

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine as _;
use dashmap::DashMap;
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

/// Route body cap: a report with an inline minidump.
pub const CRASH_BODY_LIMIT: usize = 8 * 1024 * 1024;
const MAX_MESSAGE: usize = 4 * 1024;
const MAX_STACK: usize = 64 * 1024;
const MAX_BREADCRUMBS: usize = 100;
const MAX_BREADCRUMBS_BYTES: usize = 32 * 1024;
const MAX_TAGS: usize = 32;
const MAX_CUSTOM_BYTES: usize = 16 * 1024;
const MAX_FINGERPRINT: usize = 128;
const MAX_FIELD: usize = 128;
/// Built-in incident kinds; games add their own as `custom.<name>` (`[a-z0-9_-]`, ≤ 32) —
/// Metaplay-style custom incident reports (desyncs, asset-load failures, cheat suspicions…).
pub const KINDS: [&str; 5] = ["crash", "error", "hang", "connection", "exception"];
pub const CUSTOM_PREFIX: &str = "custom.";

pub fn valid_kind(k: &str) -> bool {
    if KINDS.contains(&k) {
        return true;
    }
    match k.strip_prefix(CUSTOM_PREFIX) {
        Some(n) => {
            !n.is_empty()
                && n.len() <= 32
                && n.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        }
        None => false,
    }
}
pub const STATUSES: [&str; 4] = ["open", "resolved", "ignored", "regressed"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/telemetry/crash", post(ingest))
        .route("/v1/telemetry/crash/anonymous", post(ingest_anonymous))
        .layer(DefaultBodyLimit::max(CRASH_BODY_LIMIT))
        .merge(
            Router::new()
                .route("/v1/server/crashes", get(list_groups))
                .route("/v1/server/crashes/summary", get(summary))
                .route("/v1/server/crashes/reports/{id}", get(report))
                .route("/v1/server/crashes/reports/{id}/minidump", get(minidump))
                .route(
                    "/v1/server/crashes/{fp}",
                    get(group).patch(update_group).delete(delete_group),
                )
                .route("/v1/server/crashes/{fp}/reports", get(group_reports))
                .route("/v1/admin/accounts/{id}/crashes", get(account_crashes))
                .route("/v1/admin/accounts/{id}/events", get(account_events))
                .route("/v1/admin/accounts/{id}/timeline", get(account_timeline))
                .route(
                    "/v1/admin/accounts/{id}/notes",
                    get(list_notes).post(add_note),
                )
                .route(
                    "/v1/admin/accounts/{id}/notes/{note_id}",
                    delete(delete_note),
                ),
        )
}

// ---------------------------------------------------------------------------------------------
// fingerprinting

/// Digits become `#`, whitespace collapses — so `thread 12 timed out after 3041ms` and
/// `thread 7 timed out after 2988ms` share one group.
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_num = false;
    for c in s.chars() {
        if c.is_ascii_digit() {
            if !in_num {
                out.push('#');
                in_num = true;
            }
            continue;
        }
        in_num = false;
        if c.is_whitespace() {
            if !out.ends_with(' ') {
                out.push(' ');
            }
        } else {
            out.push(c);
        }
    }
    out.trim().to_string()
}

/// Stable group key: kind + normalized first message line + the top five normalized frames
/// (hex addresses dropped). 32 hex chars.
pub fn fingerprint(kind: &str, message: &str, stack: &str) -> String {
    let mut h = Sha256::new();
    h.update(kind.as_bytes());
    h.update(b"\n");
    h.update(normalize(message.lines().next().unwrap_or("")).as_bytes());
    h.update(b"\n");
    for line in stack
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(5)
    {
        let words: Vec<&str> = line
            .split_whitespace()
            .filter(|w| !w.contains("0x") && !w.contains("0X"))
            .collect();
        h.update(normalize(&words.join(" ")).as_bytes());
        h.update(b"\n");
    }
    hex::encode(&h.finalize()[..16])
}

/// `1.2.10` > `1.2.9`; non-numeric segments compare as strings; a missing segment is lower.
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let parts = |s: &str| -> Vec<std::result::Result<u64, String>> {
        s.split(['.', '-', '+'])
            .filter(|p| !p.is_empty())
            .map(|p| p.parse::<u64>().map_err(|_| p.to_string()))
            .collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    for i in 0..pa.len().max(pb.len()) {
        let o = match (pa.get(i), pb.get(i)) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(Ok(x)), Some(Ok(y))) => x.cmp(y),
            (Some(Ok(_)), Some(Err(_))) => Ordering::Greater,
            (Some(Err(_)), Some(Ok(_))) => Ordering::Less,
            (Some(Err(x)), Some(Err(y))) => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

fn iso(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

fn ts(secs: Option<i64>, field: &str) -> Result<Option<OffsetDateTime>> {
    secs.map(|v| {
        OffsetDateTime::from_unix_timestamp(v).map_err(|_| Error::InvalidArgument(field.into()))
    })
    .transpose()
}

// ---------------------------------------------------------------------------------------------
// ingest

static RATE: LazyLock<DashMap<String, (Instant, u32)>> = LazyLock::new(DashMap::new);
static SPIKED: LazyLock<DashMap<String, Instant>> = LazyLock::new(DashMap::new);

fn check_rate(key: &str, per_minute: u32) -> Result<()> {
    if per_minute == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut e = RATE.entry(key.to_string()).or_insert((now, 0));
    if now.duration_since(e.0) >= Duration::from_secs(60) {
        *e = (now, 0);
    }
    if e.1 >= per_minute {
        return Err(Error::ResourceExhausted(
            "too many crash reports; retry later".into(),
        ));
    }
    e.1 += 1;
    drop(e);
    if RATE.len() > 100_000 {
        RATE.retain(|_, (t, _)| now.duration_since(*t) < Duration::from_secs(60));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ReportIn {
    pub kind: String,
    pub message: String,
    pub stack: String,
    pub fingerprint: Option<String>,
    pub platform: String,
    pub app_version: String,
    pub build: String,
    pub os: String,
    pub device: String,
    pub locale: String,
    pub device_id: Option<String>,
    pub session_id: Option<String>,
    pub at: Option<i64>,
    pub breadcrumbs: Vec<Value>,
    pub tags: HashMap<String, String>,
    pub custom: Value,
    pub minidump_b64: Option<String>,
}

fn field(v: &str, name: &str) -> Result<String> {
    let v = v.trim();
    if v.len() > MAX_FIELD || v.chars().any(char::is_control) {
        return Err(Error::InvalidArgument(format!(
            "{name}: up to {MAX_FIELD} printable chars"
        )));
    }
    Ok(v.to_string())
}

/// `POST /v1/telemetry/crash` — a logged-in client reports a crash/error/hang.
async fn ingest(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<ReportIn>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let v = store(&s, r, Some(me.account_id.0), None).await?;
    Ok((StatusCode::ACCEPTED, Json(v)))
}

/// `POST /v1/telemetry/crash/anonymous` with `X-Telemetry-Key` — pre-login crashes
/// (launcher, title screen); `device_id` is required.
async fn ingest_anonymous(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(r): Json<ReportIn>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let key = headers
        .get("x-telemetry-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !telemetry_key_ok(&s.analytics.cfg.telemetry_keys, key) {
        return Err(Error::Unauthenticated("telemetry key".into()).into());
    }
    let device = r
        .device_id
        .clone()
        .filter(|d| !d.is_empty() && d.len() <= 128)
        .ok_or_else(|| Error::InvalidArgument("device_id required".into()))?;
    let v = store(&s, r, None, Some(device)).await?;
    Ok((StatusCode::ACCEPTED, Json(v)))
}

async fn store(
    s: &AppState,
    r: ReportIn,
    account: Option<Uuid>,
    device_required: Option<String>,
) -> Result<Value> {
    let cfg = &s.analytics.cfg;
    let kind = r.kind.trim().to_ascii_lowercase();
    if !valid_kind(&kind) {
        return Err(Error::InvalidArgument(format!(
            "kind must be one of {} or custom.<name> ([a-z0-9_-], ≤ 32)",
            KINDS.join("|")
        )));
    }
    let message = r.message.trim();
    if message.is_empty() {
        return Err(Error::InvalidArgument("message required".into()));
    }
    let message = truncate(message, MAX_MESSAGE);
    let stack = truncate(r.stack.trim(), MAX_STACK);
    let fp = match r.fingerprint.as_deref().map(str::trim) {
        Some(f) if !f.is_empty() => {
            if f.len() > MAX_FINGERPRINT || f.chars().any(|c| c.is_whitespace() || c.is_control()) {
                return Err(Error::InvalidArgument(format!(
                    "fingerprint: up to {MAX_FINGERPRINT} chars, no whitespace"
                )));
            }
            f.to_string()
        }
        _ => fingerprint(&kind, &message, &stack),
    };
    if r.breadcrumbs.len() > MAX_BREADCRUMBS
        || serde_json::to_vec(&r.breadcrumbs)
            .map(|b| b.len())
            .unwrap_or(usize::MAX)
            > MAX_BREADCRUMBS_BYTES
    {
        return Err(Error::InvalidArgument(format!(
            "breadcrumbs: up to {MAX_BREADCRUMBS} entries / {MAX_BREADCRUMBS_BYTES} bytes"
        )));
    }
    if r.tags.len() > MAX_TAGS
        || r.tags
            .iter()
            .any(|(k, v)| k.is_empty() || k.len() > 64 || v.len() > 256)
    {
        return Err(Error::InvalidArgument(format!(
            "tags: up to {MAX_TAGS} entries, keys ≤ 64 and values ≤ 256 chars"
        )));
    }
    let custom = if r.custom.is_null() {
        json!({})
    } else {
        r.custom
    };
    if serde_json::to_vec(&custom)
        .map(|b| b.len())
        .unwrap_or(usize::MAX)
        > MAX_CUSTOM_BYTES
    {
        return Err(Error::InvalidArgument(format!(
            "custom: up to {MAX_CUSTOM_BYTES} bytes"
        )));
    }
    let minidump = match r.minidump_b64.as_deref().filter(|m| !m.is_empty()) {
        None => None,
        Some(_) if cfg.crash_max_minidump_bytes == 0 => {
            return Err(Error::InvalidArgument(
                "minidumps are not accepted on this deployment".into(),
            ))
        }
        Some(b64) => {
            if b64.len() / 4 * 3 > cfg.crash_max_minidump_bytes + 3 {
                return Err(Error::InvalidArgument(format!(
                    "minidump larger than {} bytes",
                    cfg.crash_max_minidump_bytes
                )));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|_| Error::InvalidArgument("minidump_b64 is not base64".into()))?;
            if bytes.len() > cfg.crash_max_minidump_bytes {
                return Err(Error::InvalidArgument(format!(
                    "minidump larger than {} bytes",
                    cfg.crash_max_minidump_bytes
                )));
            }
            Some(bytes)
        }
    };
    let device_id = device_required
        .or(r.device_id.clone())
        .filter(|d| !d.is_empty());
    let device_id = match device_id {
        Some(d) => Some(field(&d, "device_id")?),
        None => None,
    };
    let session_id = match r.session_id.as_deref() {
        Some(x) if !x.is_empty() => Some(field(x, "session_id")?),
        _ => None,
    };
    let platform = field(&r.platform, "platform")?;
    let app_version = field(&r.app_version, "app_version")?;
    let build = field(&r.build, "build")?;
    let os = field(&r.os, "os")?;
    let device = field(&r.device, "device")?;
    let locale = field(&r.locale, "locale")?;
    let rate_key = account
        .map(|a| a.to_string())
        .or_else(|| device_id.clone())
        .unwrap_or_else(|| "anonymous".into());
    check_rate(&rate_key, cfg.crash_per_minute)?;
    let now = OffsetDateTime::now_utc();
    let at = match ts(r.at, "at")? {
        Some(t) if t <= now + Duration::from_secs(300) => t,
        _ => now,
    };
    let tags = serde_json::to_value(&r.tags).unwrap_or(json!({}));

    // fold repeats from the same player/device inside the dedupe window
    let dup: Option<(Uuid, String)> = sqlx::query_as(
        "select id, (select status from musubi.crash_groups g where g.fingerprint = r.fingerprint)
         from musubi.crash_reports r
         where fingerprint = $1
           and (($2::uuid is not null and account_id = $2) or ($2 is null and device_id = $3))
           and received_at > now() - make_interval(secs => $4::float8)
         order by received_at desc limit 1",
    )
    .bind(&fp)
    .bind(account)
    .bind(&device_id)
    .bind(cfg.crash_dedupe_secs as f64)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if let Some((id, status)) = dup {
        sqlx::query("update musubi.crash_reports set duplicates = duplicates + 1 where id = $1")
            .bind(id)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
        sqlx::query(
            "update musubi.crash_groups set count = count + 1, last_seen = now(), updated_at = now()
             where fingerprint = $1",
        )
        .bind(&fp)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        return Ok(json!({
            "id": id, "fingerprint": fp, "deduplicated": true, "status": status
        }));
    }

    let id = Uuid::now_v7();
    let title = truncate(message.lines().next().unwrap_or(""), 200);
    let version_opt = (!app_version.is_empty()).then(|| app_version.clone());
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let existing: Option<(String, Option<String>)> = sqlx::query_as(
        "select status, resolved_in_version from musubi.crash_groups where fingerprint = $1 for update",
    )
    .bind(&fp)
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let (status, new_group, regressed) = match existing {
        None => {
            sqlx::query(
                "insert into musubi.crash_groups
                   (fingerprint, kind, title, status, count, first_seen, last_seen, first_version, last_version, sample_id)
                 values ($1, $2, $3, 'open', 1, now(), now(), $4, $4, $5)",
            )
            .bind(&fp)
            .bind(&kind)
            .bind(&title)
            .bind(&version_opt)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            ("open".to_string(), true, false)
        }
        Some((status, resolved_in)) => {
            let regressed = status == "resolved"
                && match (
                    resolved_in.as_deref().filter(|v| !v.is_empty()),
                    version_opt.as_deref(),
                ) {
                    (None, _) => true,
                    (Some(_), None) => false,
                    (Some(fixed), Some(v)) => version_cmp(v, fixed) != Ordering::Less,
                };
            let status = if regressed {
                "regressed".to_string()
            } else {
                status
            };
            sqlx::query(
                "update musubi.crash_groups
                 set count = count + 1, last_seen = now(), updated_at = now(), status = $2,
                     first_version = coalesce(first_version, $3),
                     last_version = coalesce($3, last_version),
                     sample_id = coalesce(sample_id, $4)
                 where fingerprint = $1",
            )
            .bind(&fp)
            .bind(&status)
            .bind(&version_opt)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            (status, false, regressed)
        }
    };
    sqlx::query(
        "insert into musubi.crash_reports
           (id, fingerprint, kind, account_id, device_id, session_id, platform, app_version, build,
            os, device, locale, message, stack, breadcrumbs, tags, custom, minidump, at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19)",
    )
    .bind(id)
    .bind(&fp)
    .bind(&kind)
    .bind(account)
    .bind(&device_id)
    .bind(&session_id)
    .bind(&platform)
    .bind(&app_version)
    .bind(&build)
    .bind(&os)
    .bind(&device)
    .bind(&locale)
    .bind(&message)
    .bind(&stack)
    .bind(Value::Array(r.breadcrumbs))
    .bind(&tags)
    .bind(&custom)
    .bind(&minidump)
    .bind(at)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;

    let acct = account.map(musubi_core::AccountId);
    if new_group {
        s.events
            .emit(
                "crash.new_group",
                acct,
                json!({"fingerprint": fp, "kind": kind, "title": title, "platform": platform, "app_version": app_version, "report_id": id}),
            )
            .await;
    }
    if regressed {
        s.events
            .emit(
                "crash.regressed",
                acct,
                json!({"fingerprint": fp, "kind": kind, "title": title, "app_version": app_version, "report_id": id}),
            )
            .await;
    }
    if cfg.crash_spike_per_hour > 0 {
        let (n,): (i64,) = sqlx::query_as(
            "select count(*) from musubi.crash_reports where fingerprint = $1 and received_at > now() - interval '1 hour'",
        )
        .bind(&fp)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
        let fresh = SPIKED
            .get(&fp)
            .map(|t| t.elapsed() < Duration::from_secs(3600))
            .unwrap_or(false);
        if n as u64 >= cfg.crash_spike_per_hour && !fresh {
            SPIKED.insert(fp.clone(), Instant::now());
            s.events
                .emit(
                    "crash.spike",
                    None,
                    json!({"fingerprint": fp, "kind": kind, "title": title, "last_hour": n, "threshold": cfg.crash_spike_per_hour}),
                )
                .await;
        }
    }
    Ok(json!({
        "id": id, "fingerprint": fp, "deduplicated": false, "status": status, "new_group": new_group
    }))
}

// ---------------------------------------------------------------------------------------------
// triage

#[derive(sqlx::FromRow)]
struct GroupRow {
    fingerprint: String,
    kind: String,
    title: String,
    status: String,
    count: i64,
    first_seen: OffsetDateTime,
    last_seen: OffsetDateTime,
    first_version: Option<String>,
    last_version: Option<String>,
    resolved_in_version: Option<String>,
    resolved_at: Option<OffsetDateTime>,
    assignee: String,
    notes: String,
    sample_id: Option<Uuid>,
    users: i64,
    total: i64,
}

impl GroupRow {
    fn json(&self) -> Value {
        json!({
            "fingerprint": self.fingerprint, "kind": self.kind, "title": self.title,
            "status": self.status, "count": self.count, "users": self.users,
            "first_seen": iso(self.first_seen), "last_seen": iso(self.last_seen),
            "first_version": self.first_version, "last_version": self.last_version,
            "resolved_in_version": self.resolved_in_version,
            "resolved_at": self.resolved_at.map(iso),
            "assignee": self.assignee, "notes": self.notes, "sample_id": self.sample_id,
        })
    }
}

const GROUP_COLS: &str = "g.fingerprint, g.kind, g.title, g.status, g.count, g.first_seen, g.last_seen, \
    g.first_version, g.last_version, g.resolved_in_version, g.resolved_at, g.assignee, g.notes, g.sample_id, \
    (select count(distinct coalesce(r.account_id::text, r.device_id)) from musubi.crash_reports r \
       where r.fingerprint = g.fingerprint and ($1::timestamptz is null or r.at > $1))::bigint as users";

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ListQ {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub platform: Option<String>,
    pub version: Option<String>,
    pub q: Option<String>,
    pub since_secs: Option<i64>,
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Longest look-back a `since_secs` query may ask for (10 years); anything wider is clamped so
/// the timestamp arithmetic cannot overflow (`OffsetDateTime - Duration` panics on overflow).
const MAX_SINCE_SECS: i64 = 10 * 365 * 86_400;

fn since(secs: Option<i64>) -> Option<OffsetDateTime> {
    secs.filter(|s| *s > 0)
        .map(|s| OffsetDateTime::now_utc() - Duration::from_secs(s.min(MAX_SINCE_SECS) as u64))
}

/// `X-Telemetry-Key` of the anonymous ingest route against the configured keys, compared in
/// constant time so a probing client learns nothing from response timing.
fn telemetry_key_ok(keys: &[String], key: &str) -> bool {
    use subtle::ConstantTimeEq as _;
    !key.is_empty()
        && keys
            .iter()
            .any(|k| bool::from(k.as_bytes().ct_eq(key.as_bytes())))
}

async fn list_groups_rows(db: &PgPool, q: &ListQ) -> Result<Vec<GroupRow>> {
    let statuses: Option<Vec<String>> = match q.status.as_deref().map(str::trim) {
        None | Some("") => Some(vec!["open".into(), "regressed".into()]),
        Some("all") => None,
        Some(s) => {
            let v: Vec<String> = s.split(',').map(|x| x.trim().to_string()).collect();
            if let Some(bad) = v.iter().find(|x| !STATUSES.contains(&x.as_str())) {
                return Err(Error::InvalidArgument(format!("status `{bad}`")));
            }
            Some(v)
        }
    };
    if let Some(k) = q.kind.as_deref() {
        if !valid_kind(k) {
            return Err(Error::InvalidArgument("kind".into()));
        }
    }
    let order = match q.sort.as_deref().unwrap_or("-last_seen") {
        "-last_seen" | "last_seen" => "g.last_seen desc",
        "-first_seen" => "g.first_seen desc",
        "first_seen" => "g.first_seen asc",
        "-count" | "count" => "g.count desc, g.last_seen desc",
        "-users" | "users" => "users desc, g.count desc",
        "title" => "g.title asc",
        other => {
            return Err(Error::InvalidArgument(format!(
                "sort `{other}`: -last_seen|-first_seen|first_seen|-count|-users|title"
            )))
        }
    };
    let sql = format!(
        "select {GROUP_COLS}, count(*) over()::bigint as total from musubi.crash_groups g
         where ($2::text[] is null or g.status = any($2))
           and ($3::text is null or g.kind = $3)
           and ($4::text is null or exists (select 1 from musubi.crash_reports r where r.fingerprint = g.fingerprint and r.platform = $4))
           and ($5::text is null or exists (select 1 from musubi.crash_reports r where r.fingerprint = g.fingerprint and r.app_version = $5))
           and ($6::text is null or g.title ilike '%' || $6 || '%')
           and ($1::timestamptz is null or g.last_seen > $1)
         order by {order} limit $7 offset $8"
    );
    sqlx::query_as::<_, GroupRow>(&sql)
        .bind(since(q.since_secs))
        .bind(statuses.as_deref())
        .bind(&q.kind)
        .bind(&q.platform)
        .bind(&q.version)
        .bind(q.q.as_deref().filter(|s| !s.is_empty()))
        .bind(q.limit.unwrap_or(50).clamp(1, 500))
        .bind(q.offset.unwrap_or(0).max(0))
        .fetch_all(db)
        .await
        .map_err(Error::internal)
}

/// `GET /v1/server/crashes` — groups (default: open + regressed) with counts and affected users.
async fn list_groups(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows = list_groups_rows(&s.db, &q).await?;
    let total = rows.first().map(|r| r.total).unwrap_or(0);
    Ok(Json(json!({
        "groups": rows.iter().map(GroupRow::json).collect::<Vec<_>>(),
        "total": total,
    })))
}

async fn load_group(db: &PgPool, fp: &str, since: Option<OffsetDateTime>) -> Result<GroupRow> {
    let sql = format!(
        "select {GROUP_COLS}, 0::bigint as total from musubi.crash_groups g where g.fingerprint = $2"
    );
    sqlx::query_as::<_, GroupRow>(&sql)
        .bind(since)
        .bind(fp)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("crash group".into()))
}

async fn breakdown(
    db: &PgPool,
    fp: &str,
    col: &str,
    since: Option<OffsetDateTime>,
) -> Result<Value> {
    let sql = format!(
        "select coalesce(nullif({col}, ''), 'unknown') as k, count(*)::bigint from musubi.crash_reports
         where fingerprint = $1 and ($2::timestamptz is null or at > $2)
         group by 1 order by 2 desc limit 25"
    );
    let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
        .bind(fp)
        .bind(since)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(k, n)| json!({"value": k, "count": n}))
        .collect())
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct SinceQ {
    pub since_secs: Option<i64>,
}

/// `GET /v1/server/crashes/{fp}` — the group, version/platform/OS breakdowns, hourly histogram
/// and the latest reports.
async fn group(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(fp): Path<String>,
    Query(q): Query<SinceQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let since = since(q.since_secs);
    let g = load_group(&s.db, &fp, since).await?;
    let versions = breakdown(&s.db, &fp, "app_version", since).await?;
    let platforms = breakdown(&s.db, &fp, "platform", since).await?;
    let oses = breakdown(&s.db, &fp, "os", since).await?;
    let hist: Vec<(OffsetDateTime, i64)> = sqlx::query_as(
        "select date_trunc('hour', at) as h, count(*)::bigint from musubi.crash_reports
         where fingerprint = $1 and at > now() - interval '48 hours' group by 1 order by 1",
    )
    .bind(&fp)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let recent = reports_rows(
        &s.db,
        &ReportsQ {
            fingerprint: Some(fp.clone()),
            limit: Some(10),
            ..Default::default()
        },
    )
    .await?;
    let mut v = g.json();
    v["versions"] = versions;
    v["platforms"] = platforms;
    v["oses"] = oses;
    v["hourly"] = hist
        .into_iter()
        .map(|(h, n)| json!({"hour": iso(h), "count": n}))
        .collect();
    v["recent"] = recent.iter().map(|r| r.json(false)).collect();
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct UpdateGroup {
    pub status: Option<String>,
    pub resolved_in_version: Option<String>,
    pub assignee: Option<String>,
    pub notes: Option<String>,
}

/// `PATCH /v1/server/crashes/{fp}` — triage: `status` open|resolved|ignored (`resolved` with
/// `resolved_in_version` reopens as `regressed` only when a report from that version or later
/// arrives), `assignee`, `notes`.
async fn update_group(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(fp): Path<String>,
    Json(b): Json<UpdateGroup>,
) -> std::result::Result<Json<Value>, ApiError> {
    if let Some(st) = b.status.as_deref() {
        if !["open", "resolved", "ignored"].contains(&st) {
            return Err(Error::InvalidArgument("status: open|resolved|ignored".into()).into());
        }
    }
    if let Some(v) = b.resolved_in_version.as_deref() {
        field(v, "resolved_in_version")?;
    }
    if let Some(a) = b.assignee.as_deref() {
        field(a, "assignee")?;
    }
    if b.notes.as_deref().map(|n| n.len() > 4000).unwrap_or(false) {
        return Err(Error::InvalidArgument("notes: up to 4000 chars".into()).into());
    }
    let n = sqlx::query(
        "update musubi.crash_groups set
             status = coalesce($2, status),
             resolved_at = case when $2 = 'resolved' then now() when $2 is null then resolved_at else null end,
             resolved_in_version = case when $2 = 'resolved' or $2 is null then coalesce($3, resolved_in_version) else null end,
             assignee = coalesce($4, assignee),
             notes = coalesce($5, notes),
             updated_at = now()
         where fingerprint = $1",
    )
    .bind(&fp)
    .bind(&b.status)
    .bind(b.resolved_in_version.as_deref().map(str::trim))
    .bind(b.assignee.as_deref().map(str::trim))
    .bind(&b.notes)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("crash group".into()).into());
    }
    if b.status.as_deref() == Some("resolved") {
        s.events
            .emit(
                "crash.resolved",
                None,
                json!({"fingerprint": fp, "resolved_in_version": b.resolved_in_version}),
            )
            .await;
    }
    Ok(Json(load_group(&s.db, &fp, None).await?.json()))
}

/// `DELETE /v1/server/crashes/{fp}` — drop the group and every report (minidumps included).
async fn delete_group(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(fp): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.crash_groups where fingerprint = $1")
        .bind(&fp)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("crash group".into()).into());
    }
    Ok(Json(json!({"deleted": true})))
}

#[derive(sqlx::FromRow)]
struct ReportRow {
    id: Uuid,
    fingerprint: String,
    kind: String,
    account_id: Option<Uuid>,
    device_id: Option<String>,
    session_id: Option<String>,
    platform: String,
    app_version: String,
    build: String,
    os: String,
    device: String,
    locale: String,
    message: String,
    stack: String,
    breadcrumbs: Value,
    tags: Value,
    custom: Value,
    has_minidump: bool,
    duplicates: i32,
    at: OffsetDateTime,
    received_at: OffsetDateTime,
}

const REPORT_COLS: &str = "id, fingerprint, kind, account_id, device_id, session_id, platform, app_version, build, \
    os, device, locale, message, stack, breadcrumbs, tags, custom, (minidump is not null) as has_minidump, \
    duplicates, at, received_at";

impl ReportRow {
    fn json(&self, full: bool) -> Value {
        let mut v = json!({
            "id": self.id, "fingerprint": self.fingerprint, "kind": self.kind,
            "account_id": self.account_id, "device_id": self.device_id, "session_id": self.session_id,
            "platform": self.platform, "app_version": self.app_version, "build": self.build,
            "os": self.os, "device": self.device, "locale": self.locale,
            "message": self.message, "tags": self.tags, "has_minidump": self.has_minidump,
            "duplicates": self.duplicates, "at": iso(self.at), "received_at": iso(self.received_at),
        });
        if full {
            v["stack"] = Value::String(self.stack.clone());
            v["breadcrumbs"] = self.breadcrumbs.clone();
            v["custom"] = self.custom.clone();
        }
        v
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct ReportsQ {
    #[serde(skip)]
    pub fingerprint: Option<String>,
    pub account_id: Option<Uuid>,
    pub version: Option<String>,
    pub platform: Option<String>,
    pub kind: Option<String>,
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

async fn reports_rows(db: &PgPool, q: &ReportsQ) -> Result<Vec<ReportRow>> {
    let sql = format!(
        "select {REPORT_COLS} from musubi.crash_reports
         where ($1::text is null or fingerprint = $1)
           and ($2::uuid is null or account_id = $2)
           and ($3::text is null or app_version = $3)
           and ($4::text is null or platform = $4)
           and ($5::text is null or kind = $5)
           and ($6::timestamptz is null or at < $6)
         order by at desc limit $7"
    );
    sqlx::query_as::<_, ReportRow>(&sql)
        .bind(&q.fingerprint)
        .bind(q.account_id)
        .bind(&q.version)
        .bind(&q.platform)
        .bind(&q.kind)
        .bind(ts(q.before, "before")?)
        .bind(q.limit.unwrap_or(50).clamp(1, 500))
        .fetch_all(db)
        .await
        .map_err(Error::internal)
}

fn reports_json(rows: &[ReportRow]) -> Value {
    json!({
        "reports": rows.iter().map(|r| r.json(false)).collect::<Vec<_>>(),
        "next_before": rows.last().map(|r| r.at.unix_timestamp()),
    })
}

/// `GET /v1/server/crashes/{fp}/reports?account_id&version&platform&before&limit`
async fn group_reports(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(fp): Path<String>,
    Query(mut q): Query<ReportsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    load_group(&s.db, &fp, None).await?;
    q.fingerprint = Some(fp);
    Ok(Json(reports_json(&reports_rows(&s.db, &q).await?)))
}

/// `GET /v1/admin/accounts/{id}/crashes` — one player's reports, newest first.
async fn account_crashes(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Query(mut q): Query<ReportsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    q.account_id = Some(id);
    q.fingerprint = None;
    Ok(Json(reports_json(&reports_rows(&s.db, &q).await?)))
}

async fn load_report(db: &PgPool, id: Uuid) -> Result<ReportRow> {
    let sql = format!("select {REPORT_COLS} from musubi.crash_reports where id = $1");
    sqlx::query_as::<_, ReportRow>(&sql)
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("crash report".into()))
}

/// `GET /v1/server/crashes/reports/{id}` — full report: stack, breadcrumbs, custom data.
async fn report(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(load_report(&s.db, id).await?.json(true)))
}

/// `GET /v1/server/crashes/reports/{id}/minidump` — the raw dump (`application/octet-stream`).
async fn minidump(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Response, ApiError> {
    let row: Option<(Option<Vec<u8>>,)> =
        sqlx::query_as("select minidump from musubi.crash_reports where id = $1")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    match row {
        None => Err(Error::NotFound("crash report".into()).into()),
        Some((None,)) => Err(Error::NotFound("report has no minidump".into()).into()),
        Some((Some(bytes),)) => Ok((
            [
                (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{id}.dmp\""),
                ),
            ],
            bytes,
        )
            .into_response()),
    }
}

/// `GET /v1/server/crashes/summary?since_secs=86400` — stability dashboard numbers:
/// reports/groups/affected users in the window, crash-free users among players seen in the
/// window, breakdowns by kind/platform/version, top groups.
async fn summary(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<SinceQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let window = q
        .since_secs
        .filter(|v| *v > 0)
        .unwrap_or(86_400)
        .min(MAX_SINCE_SECS);
    let from = OffsetDateTime::now_utc() - Duration::from_secs(window as u64);
    let (reports, groups, affected, affected_accounts, crashes): (i64, i64, i64, i64, i64) =
        sqlx::query_as(
            "select count(*)::bigint,
                    count(distinct fingerprint)::bigint,
                    count(distinct coalesce(account_id::text, device_id))::bigint,
                    count(distinct account_id)::bigint,
                    count(*) filter (where kind = 'crash')::bigint
             from musubi.crash_reports where at > $1",
        )
        .bind(from)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    let (active,): (i64,) = sqlx::query_as(
        "select count(distinct account_id)::bigint from musubi.sessions where last_seen_at > $1",
    )
    .bind(from)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let (crashed_active,): (i64,) = sqlx::query_as(
        "select count(distinct r.account_id)::bigint from musubi.crash_reports r
         where r.at > $1 and r.kind = 'crash' and r.account_id is not null
           and exists (select 1 from musubi.sessions se where se.account_id = r.account_id and se.last_seen_at > $1)",
    )
    .bind(from)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let crash_free = if active > 0 {
        Some(((active - crashed_active) as f64 / active as f64 * 10_000.0).round() / 100.0)
    } else {
        None
    };
    async fn by(db: &PgPool, col: &str, from: OffsetDateTime) -> Result<Value> {
        let sql = format!(
            "select coalesce(nullif({col}, ''), 'unknown'), count(*)::bigint from musubi.crash_reports
             where at > $1 group by 1 order by 2 desc limit 25"
        );
        let rows: Vec<(String, i64)> = sqlx::query_as(&sql)
            .bind(from)
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(k, n)| json!({"value": k, "count": n}))
            .collect())
    }
    let top = list_groups_rows(
        &s.db,
        &ListQ {
            status: Some("all".into()),
            since_secs: Some(window),
            sort: Some("-count".into()),
            limit: Some(10),
            ..Default::default()
        },
    )
    .await?;
    Ok(Json(json!({
        "window_secs": window,
        "reports": reports, "crashes": crashes, "groups": groups,
        "affected_users": affected, "affected_accounts": affected_accounts,
        "active_users": active, "crash_free_users_pct": crash_free,
        "by_kind": by(&s.db, "kind", from).await?,
        "by_platform": by(&s.db, "platform", from).await?,
        "by_version": by(&s.db, "app_version", from).await?,
        "top": top.iter().map(GroupRow::json).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------------------------
// per-player event history (P1-40)

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct EventsQ {
    pub kinds: Option<String>,
    pub prefix: Option<String>,
    pub q: Option<String>,
    pub after: Option<i64>,
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

/// `GET /v1/admin/accounts/{id}/events?kinds=a,b&prefix=match.&q=text&after&before&limit` —
/// every stored domain event about the player (`musubi.events`), newest first, paged by
/// `next_before`.
async fn account_events(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Query(q): Query<EventsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let kinds: Option<Vec<String>> = q.kinds.as_deref().map(|k| {
        k.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    });
    type Row = (Uuid, String, Value, OffsetDateTime);
    let rows: Vec<Row> = sqlx::query_as(
        "select id, kind, payload, created_at from musubi.events
         where account_id = $1
           and ($2::text[] is null or kind = any($2))
           and ($3::text is null or kind like $3 || '%')
           and ($4::text is null or payload::text ilike '%' || $4 || '%')
           and ($5::timestamptz is null or created_at > $5)
           and ($6::timestamptz is null or created_at < $6)
         order by created_at desc limit $7",
    )
    .bind(id)
    .bind(kinds.as_deref())
    .bind(q.prefix.as_deref().filter(|p| !p.is_empty()))
    .bind(q.q.as_deref().filter(|p| !p.is_empty()))
    .bind(ts(q.after, "after")?)
    .bind(ts(q.before, "before")?)
    .bind(q.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let next = rows.last().map(|r| r.3.unix_timestamp());
    let (kinds_seen,): (Vec<String>,) = sqlx::query_as(
        "select coalesce(array_agg(distinct kind order by kind), '{}') from musubi.events where account_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "events": rows.iter().map(|(id, kind, payload, at)| json!({
            "id": id, "kind": kind, "payload": payload, "at": iso(*at)
        })).collect::<Vec<_>>(),
        "next_before": next,
        "kinds": kinds_seen,
    })))
}

pub const TIMELINE_SOURCES: [&str; 7] = [
    "events",
    "telemetry",
    "sessions",
    "ledger",
    "crashes",
    "notes",
    "audit",
];

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct TimelineQ {
    pub sources: Option<String>,
    pub since_secs: Option<i64>,
    pub before: Option<i64>,
    pub limit: Option<i64>,
}

struct Item {
    at: OffsetDateTime,
    source: &'static str,
    kind: String,
    summary: String,
    data: Value,
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    truncate(&s, 160)
}

/// `GET /v1/admin/accounts/{id}/timeline?sources=events,telemetry,sessions,ledger,crashes,notes,audit&since_secs&before&limit`
/// — one merged, newest-first support timeline: domain events, client telemetry, logins
/// (sessions), wallet ledger, crash reports, support notes and admin actions that touched the
/// player. Each item: `{at, source, kind, summary, data}`; page with `next_before`.
async fn account_timeline(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Query(q): Query<TimelineQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let sources: Vec<String> = match q.sources.as_deref().map(str::trim) {
        None | Some("") => TIMELINE_SOURCES.iter().map(|s| s.to_string()).collect(),
        Some(list) => {
            let v: Vec<String> = list.split(',').map(|x| x.trim().to_string()).collect();
            if let Some(bad) = v.iter().find(|x| !TIMELINE_SOURCES.contains(&x.as_str())) {
                return Err(Error::InvalidArgument(format!(
                    "source `{bad}`: {}",
                    TIMELINE_SOURCES.join("|")
                ))
                .into());
            }
            v
        }
    };
    let has = |name: &str| sources.iter().any(|x| x == name);
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let before = ts(q.before, "before")?;
    let from = since(q.since_secs);
    let db = &s.db;
    let mut items: Vec<Item> = Vec::new();

    if has("events") || has("notes") {
        type Row = (Uuid, String, Value, OffsetDateTime);
        let rows: Vec<Row> = sqlx::query_as(
            "select id, kind, payload, created_at from musubi.events
             where account_id = $1
               and ($2::timestamptz is null or created_at < $2)
               and ($3::timestamptz is null or created_at > $3)
               and (($4 and kind <> 'support.note') or ($5 and kind = 'support.note'))
             order by created_at desc limit $6",
        )
        .bind(id)
        .bind(before)
        .bind(from)
        .bind(has("events"))
        .bind(has("notes"))
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (eid, kind, payload, at) in rows {
            let is_note = kind == "support.note";
            items.push(Item {
                at,
                source: if is_note { "notes" } else { "events" },
                summary: if is_note {
                    payload["text"].as_str().unwrap_or("").to_string()
                } else {
                    short(&payload)
                },
                kind,
                data: json!({"id": eid, "payload": payload}),
            });
        }
    }
    if has("telemetry") {
        type Row = (Uuid, String, Value, OffsetDateTime, Option<String>);
        let rows: Vec<Row> = sqlx::query_as(
            "select id, name, properties, at, platform from musubi.analytics_events
             where account_id = $1
               and ($2::timestamptz is null or at < $2)
               and ($3::timestamptz is null or at > $3)
             order by at desc limit $4",
        )
        .bind(id)
        .bind(before)
        .bind(from)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (eid, name, props, at, platform) in rows {
            items.push(Item {
                at,
                source: "telemetry",
                summary: short(&props),
                kind: name,
                data: json!({"id": eid, "properties": props, "platform": platform}),
            });
        }
    }
    if has("sessions") {
        type Row = (
            Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            OffsetDateTime,
            OffsetDateTime,
            Option<OffsetDateTime>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select id, device_id, user_agent, ip::text, created_at, last_seen_at, revoked_at
             from musubi.sessions
             where account_id = $1
               and ($2::timestamptz is null or created_at < $2)
               and ($3::timestamptz is null or created_at > $3)
             order by created_at desc limit $4",
        )
        .bind(id)
        .bind(before)
        .bind(from)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (sid, device, ua, ip, created, last_seen, revoked) in rows {
            items.push(Item {
                at: created,
                source: "sessions",
                kind: "session.created".into(),
                summary: format!(
                    "login from {} ({})",
                    ip.as_deref().unwrap_or("?"),
                    ua.as_deref().unwrap_or("unknown client")
                ),
                data: json!({
                    "id": sid, "device_id": device, "user_agent": ua, "ip": ip,
                    "last_seen_at": iso(last_seen), "revoked_at": revoked.map(iso),
                }),
            });
        }
    }
    if has("ledger") {
        type Row = (Uuid, String, i64, i64, String, String, OffsetDateTime);
        let rows: Vec<Row> = sqlx::query_as(
            "select id, currency, delta, balance_after, reason, ref, created_at from musubi.wallet_ledger
             where account_id = $1
               and ($2::timestamptz is null or created_at < $2)
               and ($3::timestamptz is null or created_at > $3)
             order by created_at desc limit $4",
        )
        .bind(id)
        .bind(before)
        .bind(from)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (lid, currency, delta, after, reason, r, at) in rows {
            items.push(Item {
                at,
                source: "ledger",
                kind: format!("wallet.{}", if delta >= 0 { "credit" } else { "debit" }),
                summary: format!("{delta:+} {currency} → {after} ({reason})"),
                data: json!({
                    "id": lid, "currency": currency, "delta": delta, "balance_after": after,
                    "reason": reason, "ref": r,
                }),
            });
        }
    }
    if has("crashes") {
        let rows = reports_rows(
            db,
            &ReportsQ {
                account_id: Some(id),
                before: q.before,
                limit: Some(limit),
                ..Default::default()
            },
        )
        .await?;
        for r in rows
            .into_iter()
            .filter(|r| from.map(|f| r.at > f).unwrap_or(true))
        {
            items.push(Item {
                at: r.at,
                source: "crashes",
                kind: format!("crash.{}", r.kind),
                summary: format!(
                    "{} [{} {}]",
                    truncate(r.message.lines().next().unwrap_or(""), 120),
                    r.platform,
                    r.app_version
                ),
                data: r.json(false),
            });
        }
    }
    if has("audit") {
        type Row = (Uuid, OffsetDateTime, String, String, String, i32);
        let rows: Vec<Row> = sqlx::query_as(
            "select id, at, actor, method, path, status from musubi.audit_log
             where path like '%' || $1::text || '%'
               and ($2::timestamptz is null or at < $2)
               and ($3::timestamptz is null or at > $3)
             order by at desc limit $4",
        )
        .bind(id.to_string())
        .bind(before)
        .bind(from)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (aid, at, actor, method, path, status) in rows {
            items.push(Item {
                at,
                source: "audit",
                kind: format!("admin.{}", method.to_ascii_lowercase()),
                summary: format!("{actor}: {method} {path} → {status}"),
                data: json!({"id": aid, "actor": actor, "method": method, "path": path, "status": status}),
            });
        }
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.at));
    items.truncate(limit as usize);
    let next = items.last().map(|i| i.at.unix_timestamp());
    Ok(Json(json!({
        "items": items.iter().map(|i| json!({
            "at": iso(i.at), "source": i.source, "kind": i.kind, "summary": i.summary, "data": i.data,
        })).collect::<Vec<_>>(),
        "sources": sources,
        "next_before": next,
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct NoteIn {
    pub text: String,
    pub tags: Vec<String>,
    pub author: Option<String>,
}

/// `POST /v1/admin/accounts/{id}/notes {text, tags, author}` — a support note; stored as the
/// `support.note` event so it shows up in the timeline and reaches webhooks/LiveOps.
async fn add_note(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Json(b): Json<NoteIn>,
) -> std::result::Result<(StatusCode, Json<Value>), ApiError> {
    let text = b.text.trim();
    if text.is_empty() || text.len() > 4000 {
        return Err(Error::InvalidArgument("text: 1..=4000 chars".into()).into());
    }
    if b.tags.len() > 16 || b.tags.iter().any(|t| t.is_empty() || t.len() > 32) {
        return Err(Error::InvalidArgument("tags: up to 16 entries of ≤ 32 chars".into()).into());
    }
    let (exists,): (bool,) =
        sqlx::query_as("select exists(select 1 from musubi.accounts where id = $1)")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if !exists {
        return Err(Error::NotFound("account".into()).into());
    }
    let eid = s
        .events
        .emit(
            "support.note",
            Some(musubi_core::AccountId(id)),
            json!({"text": text, "tags": b.tags, "author": b.author}),
        )
        .await;
    Ok((StatusCode::CREATED, Json(json!({"id": eid}))))
}

/// `GET /v1/admin/accounts/{id}/notes`
async fn list_notes(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    type Row = (Uuid, Value, OffsetDateTime);
    let rows: Vec<Row> = sqlx::query_as(
        "select id, payload, created_at from musubi.events
         where account_id = $1 and kind = 'support.note' order by created_at desc limit 500",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "notes": rows.iter().map(|(nid, p, at)| json!({
            "id": nid, "text": p["text"], "tags": p["tags"], "author": p["author"], "at": iso(*at)
        })).collect::<Vec<_>>()
    })))
}

/// `DELETE /v1/admin/accounts/{id}/notes/{note_id}`
async fn delete_note(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, note_id)): Path<(Uuid, Uuid)>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query(
        "delete from musubi.events where id = $1 and account_id = $2 and kind = 'support.note'",
    )
    .bind(note_id)
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("note".into()).into());
    }
    Ok(Json(json!({"deleted": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_clamps_huge_windows_instead_of_panicking() {
        assert!(since(None).is_none());
        assert!(since(Some(0)).is_none());
        assert!(since(Some(-5)).is_none());
        let far = since(Some(i64::MAX)).unwrap();
        let cap = since(Some(MAX_SINCE_SECS)).unwrap();
        assert!((far - cap).whole_seconds().abs() <= 1);
        let hour = since(Some(3600)).unwrap();
        assert!((OffsetDateTime::now_utc() - hour).whole_seconds() >= 3600);
    }

    #[test]
    fn telemetry_key_matches_any_configured_key_only() {
        let keys = vec!["alpha".to_string(), "beta-key".to_string()];
        assert!(telemetry_key_ok(&keys, "alpha"));
        assert!(telemetry_key_ok(&keys, "beta-key"));
        assert!(!telemetry_key_ok(&keys, "alph"));
        assert!(!telemetry_key_ok(&keys, "alphaa"));
        assert!(!telemetry_key_ok(&keys, "Alpha"));
        assert!(!telemetry_key_ok(&keys, ""));
        assert!(!telemetry_key_ok(&[], "alpha"));
    }

    #[test]
    fn fingerprint_ignores_numbers_and_addresses() {
        let a = fingerprint(
            "crash",
            "thread 12 timed out after 3041ms",
            "at Game::Tick (0x7ff123) game.cpp:120\nat main 0x400 main.cpp:12",
        );
        let b = fingerprint(
            "crash",
            "thread 7 timed out after 2988ms",
            "at Game::Tick (0x7ff9ab) game.cpp:131\n  at main 0x401 main.cpp:19",
        );
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        assert_ne!(
            a,
            fingerprint("error", "thread 7 timed out after 2988ms", "")
        );
        assert_ne!(
            a,
            fingerprint("crash", "thread 7 timed out after 2988ms", "at Other")
        );
    }

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(version_cmp("1.2.10", "1.2.9"), Ordering::Greater);
        assert_eq!(version_cmp("1.2", "1.2.0"), Ordering::Less);
        assert_eq!(version_cmp("2.0.0-beta", "2.0.0"), Ordering::Greater);
        assert_eq!(version_cmp("1.10", "1.9"), Ordering::Greater);
        assert_eq!(version_cmp("1.0.0", "1.0.0"), Ordering::Equal);
    }

    #[test]
    fn truncation_keeps_char_boundaries() {
        assert_eq!(truncate("привет", 3), "п…");
        assert_eq!(truncate("abc", 3), "abc");
    }
}
