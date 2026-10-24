//! Audit log: every mutating call to `/v1/admin/*` and `/v1/server/*` is recorded with the actor
//! (`X-Musubi-Actor`, which the console sets to the operator; otherwise `server-key`), route,
//! status, client IP and the request body with secrets redacted. Written off the request path.

use std::time::Instant;

use axum::body::Body;
use axum::extract::{MatchedPath, Query, Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;
use axum::Json;
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

const MAX_BODY: usize = 64 * 1024;
/// `X-Musubi-Actor` is free text from the console; longer or non-printable values fall back to
/// the key name so the audit log cannot be stuffed or forged with control characters.
const MAX_ACTOR: usize = 128;

/// Operator and server-key mutations are audited; the simulator data-plane of a world
/// (`/v1/server/worlds/{w}/spawn|apply|lease|...`, hundreds of calls a second per world) is
/// game state, not an operator action, and would only flood the log and the pool. Creating and
/// deleting worlds stays audited.
/// `X-Musubi-Actor` when it is present, printable and within [`MAX_ACTOR`].
fn actor_header(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-musubi-actor")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|a| !a.is_empty() && a.len() <= MAX_ACTOR && !a.chars().any(char::is_control))
        .map(str::to_string)
}

fn audited(method: &Method, path: &str) -> bool {
    if !matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        return false;
    }
    if let Some(rest) = path.strip_prefix("/v1/server/worlds/") {
        if rest.contains('/') {
            return false;
        }
    }
    path.starts_with("/v1/admin/") || path.starts_with("/v1/server/")
}

struct Row {
    actor: String,
    method: String,
    route: String,
    path: String,
    status: i32,
    ip: String,
    body: Option<Value>,
    duration_ms: i32,
}

/// One writer for the whole process: rows queue on a bounded channel and are written over a
/// single connection, so a burst of audited calls never takes the pool from the request path;
/// when the queue is full the row is dropped with a warning rather than slowing the request.
#[derive(Clone)]
pub struct Sink(tokio::sync::mpsc::Sender<Row>);

impl Sink {
    /// One writer task per app instance (a process-wide static would bind the writer to the first
    /// runtime that touched it — fatal under a test binary with one runtime per test).
    pub fn new(db: &sqlx::PgPool) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Row>(10_000);
        let db = db.clone();
        tokio::spawn(async move {
            while let Some(first) = rx.recv().await {
                let mut batch = vec![first];
                while batch.len() < 256 {
                    match rx.try_recv() {
                        Ok(r) => batch.push(r),
                        Err(_) => break,
                    }
                }
                let mut conn = match db.acquire().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, dropped = batch.len(), "audit writer: no connection");
                        continue;
                    }
                };
                for r in batch {
                    let res = sqlx::query(
                        "insert into musubi.audit_log (id, actor, method, route, path, status, ip, body, duration_ms)
                         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
                    )
                    .bind(Uuid::now_v7())
                    .bind(&r.actor)
                    .bind(&r.method)
                    .bind(&r.route)
                    .bind(&r.path)
                    .bind(r.status)
                    .bind(&r.ip)
                    .bind(&r.body)
                    .bind(r.duration_ms)
                    .execute(&mut *conn)
                    .await;
                    if let Err(e) = res {
                        tracing::warn!(error = %e, route = r.route, "audit write failed");
                    }
                }
            }
        });
        Sink(tx)
    }
}

pub fn redact(v: &mut Value) {
    if let Value::Object(m) = v {
        for (k, child) in m.iter_mut() {
            let lk = k.to_ascii_lowercase();
            if ["key", "secret", "password", "token", "pem"]
                .iter()
                .any(|w| lk.contains(w))
                && !lk.ends_with("_id")
            {
                *child = Value::String("***".into());
            } else {
                redact(child);
            }
        }
    } else if let Value::Array(a) = v {
        a.iter_mut().for_each(redact);
    }
}

pub async fn record(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if !audited(req.method(), &path) {
        return next.run(req).await;
    }
    let method = req.method().as_str().to_string();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| path.clone());
    let actor = actor_header(req.headers()).unwrap_or_else(|| {
        // issued keys are audited under their name; the config key stays `server-key`
        req.headers()
            .get("x-musubi-server-key")
            .and_then(|v| v.to_str().ok())
            .and_then(|k| s.keys.lookup(k))
            .map(|c| format!("key:{}", c.name))
            .unwrap_or_else(|| "server-key".to_string())
    });
    let ip = {
        let h = |n: &str| {
            req.headers()
                .get(n)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        h("x-forwarded-for")
            .or_else(|| h("x-real-ip"))
            .or_else(|| {
                req.extensions()
                    .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                    .map(|c| c.0.ip().to_string())
            })
            .unwrap_or_default()
    };
    // Binary uploads (WASM modules, build artifacts and their chunks) are not buffered: only
    // their size is recorded.
    let binary = path.starts_with("/v1/admin/runtime/modules/")
        || path == "/v1/admin/content/import"
        || (path.starts_with("/v1/server/builds/")
            && (method == "PUT" || path.contains("/chunks/")));
    let content_length = req
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let (parts, body) = req.into_parts();
    let (body, mut body_json) = if binary {
        (body, Some(serde_json::json!({ "bytes": content_length })))
    } else {
        match axum::body::to_bytes(body, MAX_BODY).await {
            Ok(b) => {
                let parsed = serde_json::from_slice::<Value>(&b).ok();
                (Body::from(b), parsed)
            }
            Err(_) => {
                return axum::response::IntoResponse::into_response((
                    axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                    "audited request body exceeds 64 KiB",
                ))
            }
        }
    };
    if let Some(b) = &mut body_json {
        redact(b);
    }
    let started = Instant::now();
    let res = next.run(Request::from_parts(parts, body)).await;
    let status = res.status().as_u16() as i32;
    let duration_ms = started.elapsed().as_millis() as i32;
    let row = Row {
        actor,
        method,
        route: route.clone(),
        path,
        status,
        ip,
        body: body_json,
        duration_ms,
    };
    if s.audit.0.try_send(row).is_err() {
        tracing::warn!(route, "audit queue full: entry dropped");
    }
    res
}

#[derive(Deserialize)]
pub struct AuditQ {
    actor: Option<String>,
    route: Option<String>,
    path_prefix: Option<String>,
    limit: Option<i64>,
    cursor: Option<Uuid>,
}

/// Newest first; `cursor` = id of the last row seen.
pub async fn list(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<AuditQ>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(t) order by t.id desc), '[]'::jsonb) from (
            select id, at, actor, method, route, path, status, ip, body, duration_ms
            from musubi.audit_log
            where ($1::text is null or actor = $1)
              and ($2::text is null or route = $2)
              and ($3::text is null or path like $3 || '%')
              and ($4::uuid is null or id < $4)
            order by id desc limit $5) t",
    )
    .bind(&q.actor)
    .bind(&q.route)
    .bind(&q.path_prefix)
    .bind(q.cursor)
    .bind(limit)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let cursor = rows
        .as_array()
        .and_then(|a| a.last())
        .and_then(|r| r["id"].as_str())
        .map(String::from);
    Ok(Json(json!({ "entries": rows, "cursor": cursor })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_header_is_bounded_and_printable() {
        let h = |v: &str| {
            let mut m = axum::http::HeaderMap::new();
            m.insert("x-musubi-actor", v.parse().unwrap());
            actor_header(&m)
        };
        assert_eq!(h(" ops@example ").as_deref(), Some("ops@example"));
        assert!(h("").is_none());
        assert!(h(&"a".repeat(MAX_ACTOR + 1)).is_none());
        assert!(h("line\tbreak").is_none());
        assert!(actor_header(&axum::http::HeaderMap::new()).is_none());
    }

    #[test]
    fn simulator_data_plane_is_not_audited() {
        let p = Method::POST;
        assert!(audited(&p, "/v1/server/worlds"));
        assert!(audited(&Method::DELETE, "/v1/server/worlds/arena"));
        assert!(audited(&p, "/v1/admin/accounts/x/ban"));
        assert!(!audited(&p, "/v1/server/worlds/arena/apply"));
        assert!(!audited(&p, "/v1/server/worlds/arena/handover/ack"));
        assert!(!audited(&Method::GET, "/v1/admin/accounts/x"));
    }
}
