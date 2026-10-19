//! Config sources: keep a remote config in sync with a spreadsheet or file published on the web —
//! a Google Sheet "published to the web" as CSV (`…/export?format=csv&gid=0`), a CSV/JSON file in
//! a bucket or on a CDN. Designers edit the sheet, the server polls it (ETag/hash aware) and
//! stores a new config version only when the content changed; a manual `sync` is available too.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

const MAX_BODY: usize = 4 << 20;
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
const MIN_INTERVAL: i32 = 30;
const MAX_INTERVAL: i32 = 86_400;
const MAX_HEADERS: usize = 8;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/config_sources", get(list))
        .route(
            "/v1/server/configs/{name}/source",
            get(get_one).put(put).delete(delete),
        )
        .route(
            "/v1/server/configs/{name}/source/sync",
            axum::routing::post(sync_now),
        )
}

type Row = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i32,
    bool,
    Value,
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
    i32,
    OffsetDateTime,
);

const COLS: &str = "name, url, format, key, note, interval_secs, enabled, headers, etag, last_hash, last_sync_at, last_status, last_error, last_changed_at, syncs, updated_at";

fn doc(r: &Row) -> Value {
    let header_names: Vec<&str> =
        r.7.as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
    let next = if r.6 {
        Some(
            r.10.map(|t| t.unix_timestamp() + r.5 as i64)
                .unwrap_or_else(|| OffsetDateTime::now_utc().unix_timestamp()),
        )
    } else {
        None
    };
    json!({
        "name": r.0, "url": r.1, "format": r.2, "key": r.3, "note": r.4,
        "interval_secs": r.5, "enabled": r.6, "header_names": header_names,
        "etag": r.8, "last_hash": r.9,
        "last_sync_at": r.10.map(|t| t.unix_timestamp()),
        "last_status": r.11, "last_error": r.12,
        "last_changed_at": r.13.map(|t| t.unix_timestamp()),
        "syncs": r.14, "next_sync_at": next, "updated_at": r.15.unix_timestamp(),
    })
}

async fn load(s: &AppState, name: &str) -> Result<Row> {
    sqlx::query_as(&format!(
        "select {COLS} from musubi.config_sources where name = $1"
    ))
    .bind(name)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("config source {name}")))
}

/// `GET /v1/server/config_sources` — every source with its sync state.
async fn list(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.config_sources order by name"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let sources: Vec<Value> = rows.iter().map(doc).collect();
    Ok(Json(json!({ "sources": sources })))
}

async fn get_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(doc(&load(&s, &name).await?)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PutBody {
    url: String,
    format: Option<String>,
    key: Option<String>,
    note: Option<String>,
    interval_secs: Option<i32>,
    enabled: Option<bool>,
    headers: HashMap<String, String>,
}

fn guess_format(url: &str) -> &'static str {
    let u = url.to_ascii_lowercase();
    if u.contains("format=csv")
        || u.contains("output=csv")
        || u.split('?').next().is_some_and(|p| p.ends_with(".csv"))
    {
        "csv"
    } else {
        "json"
    }
}

/// `PUT /v1/server/configs/{name}/source {url, format?: csv|json, key?, note?, interval_secs?,
/// enabled?, headers?}` — bind the config to a URL. `format` defaults from the URL (`format=csv`,
/// `.csv` → csv, else json); `key` is the CSV column that becomes the object key (rows become an
/// array without it); `headers` (≤ 8, e.g. an auth token) are sent on every fetch and never shown
/// again, only their names. The first sync runs right away.
async fn put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    crate::http::configs::check_name(&name)?;
    let url = b.url.trim().to_string();
    if url.len() > 2048 || !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(
            Error::InvalidArgument("url must be http(s), at most 2048 chars".into()).into(),
        );
    }
    let format = b
        .format
        .map(|f| f.trim().to_ascii_lowercase())
        .unwrap_or_else(|| guess_format(&url).to_string());
    if format != "csv" && format != "json" {
        return Err(Error::InvalidArgument("format must be csv or json".into()).into());
    }
    let interval = b.interval_secs.unwrap_or(300);
    if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&interval) {
        return Err(Error::InvalidArgument(format!(
            "interval_secs must be {MIN_INTERVAL}..={MAX_INTERVAL}"
        ))
        .into());
    }
    if b.headers.len() > MAX_HEADERS
        || b.headers.iter().any(|(k, v)| {
            k.is_empty()
                || k.len() > 64
                || v.len() > 1024
                || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                || v.chars().any(|c| c == '\r' || c == '\n')
        })
    {
        return Err(Error::InvalidArgument(
            "headers: at most 8, names [A-Za-z0-9-]{1,64}, values <= 1024 chars".into(),
        )
        .into());
    }
    if b.key.as_ref().is_some_and(|k| k.is_empty() || k.len() > 64) {
        return Err(Error::InvalidArgument("key must be 1..=64 chars".into()).into());
    }
    if b.note.as_ref().is_some_and(|n| n.len() > 200) {
        return Err(Error::InvalidArgument("note exceeds 200 chars".into()).into());
    }
    let headers = Value::Object(
        b.headers
            .into_iter()
            .map(|(k, v)| (k, Value::String(v)))
            .collect(),
    );
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.config_sources (name, url, format, key, note, interval_secs, enabled, headers)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (name) do update set url = $2, format = $3, key = $4, note = $5,
             interval_secs = $6, enabled = $7, headers = $8, etag = null, last_hash = null,
             updated_at = now()
         returning {COLS}"
    ))
    .bind(&name)
    .bind(&url)
    .bind(&format)
    .bind(&b.key)
    .bind(&b.note)
    .bind(interval)
    .bind(b.enabled.unwrap_or(true))
    .bind(&headers)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "config.source_set",
            None,
            json!({ "name": name, "url": url, "format": format }),
        )
        .await;
    Ok(Json(doc(&row)))
}

async fn delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.config_sources where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("config source {name}")).into());
    }
    s.events
        .emit("config.source_removed", None, json!({ "name": name }))
        .await;
    Ok(Json(json!({ "name": name, "removed": true })))
}

/// `POST /v1/server/configs/{name}/source/sync` — fetch now (also for disabled sources); 200 with
/// `status: ok|unchanged|error` so operators see fetch/parse problems inline.
async fn sync_now(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = load(&s, &name).await?;
    let outcome = sync_one(&s, &row).await;
    let row = load(&s, &name).await?;
    let mut d = doc(&row);
    if let Some(o) = d.as_object_mut() {
        o.insert("status".into(), json!(outcome.status));
        o.insert("changed".into(), json!(outcome.changed));
        o.insert("version".into(), json!(outcome.version));
        o.insert("error".into(), json!(outcome.error));
    }
    Ok(Json(d))
}

struct Outcome {
    status: &'static str,
    changed: bool,
    version: Option<Value>,
    error: Option<String>,
}

async fn fetch(
    url: &str,
    headers: &Value,
    etag: Option<&str>,
) -> std::result::Result<Option<(String, Option<String>)>, String> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|e| e.to_string())?;
    let mut req = client.get(url);
    if let Some(m) = headers.as_object() {
        for (k, v) in m {
            if let Some(v) = v.as_str() {
                req = req.header(k.as_str(), v);
            }
        }
    }
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    let resp = req.send().await.map_err(|e| format!("fetch: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("fetch: HTTP {}", resp.status().as_u16()));
    }
    let new_etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.bytes().await.map_err(|e| format!("fetch: {e}"))?;
    if bytes.len() > MAX_BODY {
        return Err(format!("fetch: body {} bytes exceeds 4 MiB", bytes.len()));
    }
    let text =
        String::from_utf8(bytes.to_vec()).map_err(|_| "fetch: body is not UTF-8".to_string())?;
    Ok(Some((text, new_etag)))
}

async fn record(
    s: &AppState,
    name: &str,
    status: &str,
    error: Option<&str>,
    etag: Option<&str>,
    hash: Option<&str>,
    changed: bool,
) {
    let _ = sqlx::query(
        "update musubi.config_sources set last_sync_at = now(), last_status = $2, last_error = $3,
             etag = coalesce($4, etag), last_hash = coalesce($5, last_hash),
             last_changed_at = case when $6 then now() else last_changed_at end,
             syncs = syncs + 1
         where name = $1",
    )
    .bind(name)
    .bind(status)
    .bind(error)
    .bind(etag)
    .bind(hash)
    .bind(changed)
    .execute(&s.db)
    .await;
}

async fn sync_one(s: &AppState, r: &Row) -> Outcome {
    let name = r.0.as_str();
    let fail = |e: String| Outcome {
        status: "error",
        changed: false,
        version: None,
        error: Some(e),
    };
    let fetched = match fetch(&r.1, &r.7, r.8.as_deref()).await {
        Ok(f) => f,
        Err(e) => {
            record(s, name, "error", Some(&e), None, None, false).await;
            s.events
                .emit(
                    "config.source_failed",
                    None,
                    json!({ "name": name, "error": e }),
                )
                .await;
            return fail(e);
        }
    };
    let Some((text, etag)) = fetched else {
        record(s, name, "unchanged", None, None, None, false).await;
        return Outcome {
            status: "unchanged",
            changed: false,
            version: None,
            error: None,
        };
    };
    let hash = hex::encode(Sha256::digest(text.as_bytes()));
    if r.9.as_deref() == Some(hash.as_str()) {
        record(s, name, "unchanged", None, etag.as_deref(), None, false).await;
        return Outcome {
            status: "unchanged",
            changed: false,
            version: None,
            error: None,
        };
    }
    let parsed: std::result::Result<Value, String> = if r.2 == "csv" {
        crate::http::remote_config::csv_to_value(&text, r.3.as_deref()).map_err(|e| e.to_string())
    } else {
        serde_json::from_str(&text).map_err(|e| format!("json: {e}"))
    };
    let value = match parsed {
        Ok(v) => v,
        Err(e) => {
            record(s, name, "error", Some(&e), None, None, false).await;
            s.events
                .emit(
                    "config.source_failed",
                    None,
                    json!({ "name": name, "error": e }),
                )
                .await;
            return fail(e);
        }
    };
    let note =
        r.4.clone()
            .unwrap_or_else(|| format!("source sync: {}", r.1));
    match crate::http::configs::store(s, name, &value, Some(&note)).await {
        Ok(stored) => {
            record(s, name, "ok", None, etag.as_deref(), Some(&hash), true).await;
            let version = stored.get("version").cloned();
            s.events
                .emit(
                    "config.source_synced",
                    None,
                    json!({ "name": name, "version": version, "hash": hash }),
                )
                .await;
            Outcome {
                status: "ok",
                changed: true,
                version,
                error: None,
            }
        }
        Err(e) => {
            let msg = format!("store: {}", e.0);
            record(s, name, "error", Some(&msg), None, None, false).await;
            s.events
                .emit(
                    "config.source_failed",
                    None,
                    json!({ "name": name, "error": msg }),
                )
                .await;
            fail(msg)
        }
    }
}

/// One pass: sync every enabled source whose interval has elapsed. Returns the number synced.
pub async fn sweep(s: &AppState) -> usize {
    let rows: Vec<Row> = match sqlx::query_as(&format!(
        "select {COLS} from musubi.config_sources
         where enabled and (last_sync_at is null or last_sync_at + make_interval(secs => interval_secs) <= now())
         order by last_sync_at nulls first limit 50"
    ))
    .fetch_all(&s.db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "config sources: query failed");
            return 0;
        }
    };
    let n = rows.len();
    for r in &rows {
        let o = sync_one(s, r).await;
        if let Some(e) = o.error {
            tracing::warn!(source = %r.0, error = %e, "config source sync failed");
        }
    }
    n
}

/// Background poller (cluster-wide: only the lease holder runs it).
pub fn spawn_worker(state: AppState, held: impl Fn() -> bool + Send + 'static) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(10));
        loop {
            tick.tick().await;
            if !held() {
                continue;
            }
            sweep(&state).await;
        }
    });
}
