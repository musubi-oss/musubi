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

fn audited(method: &Method, path: &str) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) && (path.starts_with("/v1/admin/") || path.starts_with("/v1/server/"))
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
    let actor = req
        .headers()
        .get("x-musubi-actor")
        .and_then(|v| v.to_str().ok())
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
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
    // Binary uploads (WASM modules) are not buffered: only their size is recorded.
    let binary = path.starts_with("/v1/admin/runtime/modules/");
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
    let db = s.db.clone();
    tokio::spawn(async move {
        let r = sqlx::query(
            "insert into musubi.audit_log (id, actor, method, route, path, status, ip, body, duration_ms)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(Uuid::now_v7())
        .bind(&actor)
        .bind(&method)
        .bind(&route)
        .bind(&path)
        .bind(status)
        .bind(&ip)
        .bind(&body_json)
        .bind(duration_ms)
        .execute(&db)
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, route, "audit write failed");
        }
    });
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
