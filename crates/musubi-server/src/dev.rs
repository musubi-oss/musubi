//! Developer tools: request recording with replay, and fixtures (seeded accounts with wallet,
//! inventory and player data) that can be applied idempotently and torn down.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap};
use axum::middleware::Next;
use axum::response::Response;
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;

const MAX_CAPTURE: usize = 64 * 1024;
pub const REPLAY_HEADER: &str = "x-musubi-replay";

pub struct DevTools {
    enabled: AtomicBool,
    sample: AtomicU32,
    counter: AtomicU64,
    prefix: RwLock<String>,
}

impl Default for DevTools {
    fn default() -> Self {
        Self::new()
    }
}

impl DevTools {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            sample: AtomicU32::new(100),
            counter: AtomicU64::new(0),
            prefix: RwLock::new("/v1/".into()),
        }
    }

    pub fn settings(&self) -> Value {
        json!({
            "enabled": self.enabled.load(Ordering::Relaxed),
            "sample_percent": self.sample.load(Ordering::Relaxed),
            "path_prefix": self.prefix.read().map(|p| p.clone()).unwrap_or_default(),
        })
    }

    pub fn configure(&self, enabled: bool, sample_percent: u32, path_prefix: &str) {
        self.enabled.store(enabled, Ordering::Relaxed);
        self.sample
            .store(sample_percent.min(100), Ordering::Relaxed);
        if let Ok(mut p) = self.prefix.write() {
            *p = if path_prefix.is_empty() {
                "/v1/".into()
            } else {
                path_prefix.to_string()
            };
        }
    }

    fn should_record(&self, path: &str, headers: &HeaderMap) -> bool {
        if !self.enabled.load(Ordering::Relaxed) || headers.contains_key(REPLAY_HEADER) {
            return false;
        }
        if path.starts_with("/v1/admin/dev/") {
            return false;
        }
        let ok = self
            .prefix
            .read()
            .map(|p| path.starts_with(p.as_str()))
            .unwrap_or(false);
        if !ok {
            return false;
        }
        let sample = self.sample.load(Ordering::Relaxed) as u64;
        sample >= 100 || (self.counter.fetch_add(1, Ordering::Relaxed) % 100) < sample
    }
}

fn text_of(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let cut = bytes.len().min(MAX_CAPTURE);
    Some(String::from_utf8_lossy(&bytes[..cut]).to_string())
}

/// Middleware: store sampled requests (and their responses) for later inspection and replay.
pub async fn record(State(s): State<AppState>, req: Request, next: Next) -> Response {
    if !s.dev.should_record(req.uri().path(), req.headers()) {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let body_bytes = axum::body::to_bytes(body, 8 << 20)
        .await
        .unwrap_or_default();
    let req = Request::from_parts(parts.clone(), Body::from(body_bytes.clone()));
    let started = Instant::now();
    let res = next.run(req).await;
    let (rparts, rbody) = res.into_parts();
    let rbytes = axum::body::to_bytes(rbody, 8 << 20)
        .await
        .unwrap_or_default();
    let res = Response::from_parts(rparts.clone(), Body::from(rbytes.clone()));
    let duration_ms = started.elapsed().as_millis() as i32;
    let account = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .and_then(|t| s.auth.signer().verify(t).ok())
        .map(|c| c.sub.0);
    let server_key = parts.headers.contains_key("x-musubi-server-key");
    let mut headers = serde_json::Map::new();
    for (k, v) in &parts.headers {
        let name = k.as_str();
        if matches!(
            name,
            "authorization" | "cookie" | "x-musubi-server-key" | "host" | "content-length"
        ) {
            continue;
        }
        if let Ok(v) = v.to_str() {
            headers.insert(name.to_string(), json!(v));
        }
    }
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| parts.uri.path().to_string());
    let method = parts.method.as_str().to_string();
    let status = rparts.status.as_u16() as i32;
    let db = s.db.clone();
    tokio::spawn(async move {
        let r = sqlx::query(
            "insert into musubi.http_recordings (id, method, path, headers, body, status, response, account_id, server_key, duration_ms)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(Uuid::now_v7())
        .bind(&method)
        .bind(&path)
        .bind(Value::Object(headers))
        .bind(text_of(&body_bytes))
        .bind(status)
        .bind(text_of(&rbytes))
        .bind(account)
        .bind(server_key)
        .bind(duration_ms)
        .execute(&db)
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "request recording failed");
        }
    });
    res
}

pub async fn recordings(db: &sqlx::PgPool, path: Option<&str>, limit: i64) -> Result<Vec<Value>> {
    type Row = (
        Uuid,
        String,
        String,
        i32,
        Option<Uuid>,
        bool,
        i32,
        time::OffsetDateTime,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select id, method, path, status, account_id, server_key, duration_ms, created_at
         from musubi.http_recordings where ($1::text is null or path like $1 || '%')
         order by created_at desc limit $2",
    )
    .bind(path)
    .bind(limit.clamp(1, 500))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(id, method, path, status, account, sk, ms, at)| {
            json!({ "id": id, "method": method, "path": path, "status": status, "account_id": account,
                "server_key": sk, "duration_ms": ms,
                "created_at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
        })
        .collect())
}

pub struct Recording {
    pub method: String,
    pub path: String,
    pub headers: Value,
    pub body: Option<String>,
    pub status: i32,
    pub response: Option<String>,
    pub account_id: Option<Uuid>,
    pub server_key: bool,
}

pub async fn recording(db: &sqlx::PgPool, id: Uuid) -> Result<Option<Recording>> {
    type Row = (
        String,
        String,
        Value,
        Option<String>,
        i32,
        Option<String>,
        Option<Uuid>,
        bool,
    );
    let r: Option<Row> = sqlx::query_as(
        "select method, path, headers, body, status, response, account_id, server_key
         from musubi.http_recordings where id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?;
    Ok(r.map(
        |(method, path, headers, body, status, response, account_id, server_key)| Recording {
            method,
            path,
            headers,
            body,
            status,
            response,
            account_id,
            server_key,
        },
    ))
}

/// Re-issue a recorded request against this node: server-key calls with the current key, player
/// calls with a 5-minute token minted for the recorded account.
pub async fn replay(s: &AppState, rec: &Recording) -> Result<Value> {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder()
        .method(rec.method.as_str())
        .uri(rec.path.as_str())
        .header(REPLAY_HEADER, "1");
    if let Some(h) = rec.headers.as_object() {
        for (k, v) in h {
            if let Some(v) = v.as_str() {
                req = req.header(k.as_str(), v);
            }
        }
    }
    if rec.server_key {
        req = req.header("x-musubi-server-key", &s.cfg.http.server_key);
    } else if let Some(acc) = rec.account_id {
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        let token = s.auth.signer().sign(&musubi_auth::Claims {
            sub: AccountId(acc),
            sid: musubi_core::SessionId(Uuid::now_v7()),
            iat: now,
            exp: now + 300,
            vars: Default::default(),
            name: None,
        })?;
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let req = req
        .body(Body::from(rec.body.clone().unwrap_or_default()))
        .map_err(Error::internal)?;
    let started = Instant::now();
    let res = crate::http::router(s.clone())
        .oneshot(req)
        .await
        .map_err(|e| Error::internal(format!("replay: {e}")))?;
    let status = res.status().as_u16() as i32;
    let bytes = axum::body::to_bytes(res.into_body(), 8 << 20)
        .await
        .unwrap_or_default();
    let text = text_of(&bytes);
    let body = text
        .as_deref()
        .and_then(|t| serde_json::from_str::<Value>(t).ok())
        .unwrap_or_else(|| json!(text));
    Ok(
        json!({ "status": status, "recorded_status": rec.status, "same_status": status == rec.status,
        "duration_ms": started.elapsed().as_millis() as i64, "body": body }),
    )
}

fn fixture_device(fixture: &str, username: &str) -> String {
    let raw = format!("fixture-{fixture}-{username}");
    let digest = ring::digest::digest(&ring::digest::SHA256, raw.as_bytes());
    let hex: String = digest.as_ref()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        "fixture-{hex}-{}",
        username.chars().take(80).collect::<String>()
    )
}

/// Apply a fixture: every account is created once (same device identity on re-apply), wallet
/// grants are idempotent per currency, inventory and storage are seeded for new accounts (or
/// when `reset`).
pub async fn apply_fixture(
    s: &AppState,
    name: &str,
    manifest: &Value,
    reset: bool,
) -> Result<Value> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::InvalidArgument(
            "fixture name: 1-64 chars of [A-Za-z0-9_-]".into(),
        ));
    }
    let accounts = manifest["accounts"]
        .as_array()
        .ok_or_else(|| Error::InvalidArgument("manifest.accounts must be an array".into()))?;
    if accounts.is_empty() || accounts.len() > 500 {
        return Err(Error::InvalidArgument(
            "1..=500 accounts per fixture".into(),
        ));
    }
    let mut out = Vec::new();
    for a in accounts {
        let username = a["username"]
            .as_str()
            .filter(|u| !u.is_empty())
            .ok_or_else(|| Error::InvalidArgument("account.username is required".into()))?;
        let device = fixture_device(name, username);
        let opts: musubi_auth::AuthOptions =
            serde_json::from_value(json!({ "create": true, "username": username }))?;
        let session = s.auth.authenticate_device(&device, opts).await?;
        let acc = session.account_id;
        let fresh = session.created || reset;
        if a.get("display_name").is_some() || a.get("metadata").is_some() {
            let upd: musubi_account::UpdateAccount = serde_json::from_value(json!({
                "display_name": a["display_name"], "metadata": a.get("metadata").cloned() }))?;
            s.accounts.update(acc, upd).await?;
        }
        let mut granted = Vec::new();
        if let Some(w) = a["wallet"].as_object() {
            for (cur, amount) in w {
                let amount = amount.as_i64().unwrap_or(0);
                if amount == 0 {
                    continue;
                }
                let key = format!("fixture:{name}:{username}:{cur}");
                let (e, _) = s
                    .economy
                    .adjust(
                        acc,
                        cur,
                        amount,
                        "fixture",
                        &format!("fixture:{name}"),
                        Some(&key),
                        json!({ "fixture": name }),
                    )
                    .await?;
                granted.push(json!({ "currency": cur, "balance": e.balance_after }));
            }
        }
        if fresh {
            if let Some(inv) = a["inventory"].as_array() {
                for i in inv {
                    let item = i["item_id"]
                        .as_str()
                        .ok_or_else(|| Error::InvalidArgument("inventory.item_id".into()))?;
                    let q = i["quantity"].as_i64().unwrap_or(1).max(1);
                    let e = s
                        .economy
                        .grant_item(
                            acc,
                            item,
                            q,
                            i.get("instance").cloned().unwrap_or(json!({})),
                        )
                        .await?;
                    granted.push(json!({ "item_id": item, "quantity": e.quantity }));
                }
            }
            if let Some(objs) = a["storage"].as_array() {
                let ops: Vec<musubi_storage::WriteOp> = objs
                    .iter()
                    .map(|o| serde_json::from_value(o.clone()).map_err(Error::from))
                    .collect::<Result<_>>()?;
                if !ops.is_empty() {
                    s.storage
                        .write(musubi_storage::Caller::Server, Some(acc.0), &ops)
                        .await?;
                }
            }
        }
        out.push(json!({ "username": username, "account_id": acc, "created": session.created, "seeded": fresh,
            "granted": granted }));
    }
    sqlx::query(
        "insert into musubi.fixtures (name, manifest, accounts, applied_at) values ($1, $2, $3, now())
         on conflict (name) do update set manifest = excluded.manifest, accounts = excluded.accounts, applied_at = now()",
    )
    .bind(name)
    .bind(manifest)
    .bind(Value::Array(out.clone()))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "fixture.applied",
            None,
            json!({ "name": name, "accounts": out.len() }),
        )
        .await;
    Ok(json!({ "name": name, "accounts": out }))
}

pub async fn fixtures(db: &sqlx::PgPool) -> Result<Vec<Value>> {
    let rows: Vec<(String, Value, time::OffsetDateTime)> =
        sqlx::query_as("select name, accounts, applied_at from musubi.fixtures order by name")
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(name, accounts, at)| {
            json!({ "name": name, "accounts": accounts.as_array().map(|a| a.len()).unwrap_or(0),
                "applied_at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default() })
        })
        .collect())
}

/// Tear a fixture down: purge every account it created.
pub async fn drop_fixture(s: &AppState, name: &str) -> Result<Option<Value>> {
    let row: Option<(Value,)> =
        sqlx::query_as("select accounts from musubi.fixtures where name = $1")
            .bind(name)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((accounts,)) = row else {
        return Ok(None);
    };
    let (mut purged, mut rows) = (0u64, 0u64);
    for a in accounts.as_array().into_iter().flatten() {
        if let Some(id) = a["account_id"]
            .as_str()
            .and_then(|v| Uuid::parse_str(v).ok())
        {
            if let Ok(n) = s.accounts.purge(AccountId(id)).await {
                purged += 1;
                rows += n;
            }
        }
    }
    sqlx::query("delete from musubi.fixtures where name = $1")
        .bind(name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.events
        .emit(
            "fixture.dropped",
            None,
            json!({ "name": name, "purged": purged, "rows": rows }),
        )
        .await;
    Ok(Some(
        json!({ "name": name, "purged": purged, "rows": rows }),
    ))
}
