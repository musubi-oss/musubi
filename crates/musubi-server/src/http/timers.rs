//! Durable one-shot timers for modules (plan 3.6 host API): `timer.schedule` stores a call —
//! an RPC export (`rpc: "name"`) or a hook export (`hook: "name"` → `hook_name`) with a JSON
//! payload — due at a time; the node holding the `schedules` lease runs due timers every second
//! with a server context (`vars.timer_id`, `vars.timer_key`), retrying failures with backoff
//! (2^attempts seconds, dropped after `MAX_ATTEMPTS`). `key` makes scheduling idempotent: a
//! second `timer.schedule` with the same key moves the timer instead of adding one.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{Error, Result};
use musubi_runtime::{CallContext, Runtime};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub const MAX_ATTEMPTS: i32 = 5;
const MAX_DELAY_SECS: i64 = 366 * 86_400;

type Row = (
    Uuid,
    Option<String>,
    String,
    Value,
    i64,
    i32,
    Option<String>,
);

fn doc((id, key, target, payload, due_at, attempts, last_error): Row) -> Value {
    json!({
        "id": id, "key": key, "target": target, "payload": payload,
        "due_at": due_at, "attempts": attempts, "last_error": last_error,
    })
}

/// `target`: `rpc:<id>` or `hook:<name>`.
pub fn target_of(req: &Value) -> Result<String> {
    let ident = |s: &str| {
        !s.is_empty()
            && s.len() <= 128
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    };
    match (req["rpc"].as_str(), req["hook"].as_str()) {
        (Some(r), None) if ident(r) => Ok(format!("rpc:{r}")),
        (None, Some(h)) if ident(h) => Ok(format!("hook:{}", h.trim_start_matches("hook_"))),
        _ => Err(Error::InvalidArgument(
            "timer: exactly one of `rpc` (export rpc_<id>) or `hook` (export hook_<name>)".into(),
        )),
    }
}

/// Due time from `at` (unix seconds) or `in_secs`, at most a year ahead.
pub fn due_of(req: &Value) -> Result<i64> {
    let now = chrono::Utc::now().timestamp();
    let due = match (req["at"].as_i64(), req["in_secs"].as_i64()) {
        (Some(at), None) => at,
        (None, Some(d)) if d >= 0 => now.saturating_add(d),
        _ => {
            return Err(Error::InvalidArgument(
                "timer: exactly one of `at` (unix seconds) or `in_secs` (>= 0)".into(),
            ))
        }
    };
    if due > now + MAX_DELAY_SECS {
        return Err(Error::InvalidArgument("timer: at most a year ahead".into()));
    }
    Ok(due.max(now))
}

pub async fn schedule(
    db: &PgPool,
    target: &str,
    payload: Value,
    due_at: i64,
    key: Option<&str>,
) -> Result<Value> {
    if let Some(k) = key {
        if k.is_empty() || k.len() > 200 {
            return Err(Error::InvalidArgument("timer key: 1..200 chars".into()));
        }
    }
    if serde_json::to_vec(&payload).map(|v| v.len()).unwrap_or(0) > 64 << 10 {
        return Err(Error::InvalidArgument(
            "timer payload: at most 64 KiB".into(),
        ));
    }
    let id = Uuid::now_v7();
    let row: (Uuid, i64) = sqlx::query_as(
        "insert into musubi.timers (id, key, target, payload, due_at)
         values ($1, $2, $3, $4, to_timestamp($5))
         on conflict (key) do update set target = excluded.target, payload = excluded.payload,
             due_at = excluded.due_at, attempts = 0, last_error = null
         returning id, extract(epoch from due_at)::bigint",
    )
    .bind(id)
    .bind(key)
    .bind(target)
    .bind(&payload)
    .bind(due_at as f64)
    .fetch_one(db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({ "id": row.0, "key": key, "due_at": row.1, "target": target }))
}

/// Cancel by `id` or `key`; `true` when something was removed.
pub async fn cancel(db: &PgPool, id: Option<Uuid>, key: Option<&str>) -> Result<bool> {
    let n = match (id, key) {
        (Some(id), _) => {
            sqlx::query("delete from musubi.timers where id = $1")
                .bind(id)
                .execute(db)
                .await
        }
        (None, Some(k)) => {
            sqlx::query("delete from musubi.timers where key = $1")
                .bind(k)
                .execute(db)
                .await
        }
        _ => return Err(Error::InvalidArgument("timer: `id` or `key`".into())),
    }
    .map_err(Error::internal)?
    .rows_affected();
    Ok(n > 0)
}

/// Run every due timer once (claimed with `for update skip locked`, so concurrent sweepers
/// never double-fire). Returns `(fired, failed)`.
pub async fn sweep(db: &PgPool, runtime: &Arc<Runtime>) -> Result<(usize, usize)> {
    let due: Vec<Row> = sqlx::query_as(
        "update musubi.timers t set attempts = t.attempts + 1
         from (select id from musubi.timers where due_at <= now()
               order by due_at limit 100 for update skip locked) d
         where t.id = d.id
         returning t.id, t.key, t.target, t.payload, extract(epoch from t.due_at)::bigint,
                   t.attempts, t.last_error",
    )
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let (mut fired, mut failed) = (0, 0);
    for (id, key, target, payload, _due, attempts, _) in due {
        let mut vars = serde_json::Map::new();
        vars.insert("timer_id".into(), json!(id));
        if let Some(k) = &key {
            vars.insert("timer_key".into(), json!(k));
        }
        let ctx = CallContext {
            server: true,
            vars,
            ..Default::default()
        };
        let bytes = serde_json::to_vec(&payload).unwrap_or_default();
        let res = match target.split_once(':') {
            Some(("rpc", rpc)) => runtime.call(rpc, ctx, bytes).await.map(|_| ()),
            Some(("hook", hook)) => match runtime.hook(&format!("hook_{hook}"), ctx, bytes).await {
                None => Err(Error::NotFound(format!("no module exports hook_{hook}"))),
                Some(r) => r.map(|_| ()),
            },
            _ => Err(Error::InvalidArgument(format!("bad timer target {target}"))),
        };
        match res {
            Ok(()) => {
                fired += 1;
                sqlx::query("delete from musubi.timers where id = $1")
                    .bind(id)
                    .execute(db)
                    .await
                    .map_err(Error::internal)?;
            }
            Err(e) => {
                failed += 1;
                if attempts >= MAX_ATTEMPTS {
                    tracing::warn!(timer = %id, target, error = %e, "timer dropped after retries");
                    sqlx::query("delete from musubi.timers where id = $1")
                        .bind(id)
                        .execute(db)
                        .await
                        .map_err(Error::internal)?;
                } else {
                    tracing::warn!(timer = %id, target, attempts, error = %e, "timer failed, retrying");
                    sqlx::query(
                        "update musubi.timers set due_at = now() + make_interval(secs => $2),
                             last_error = $3 where id = $1",
                    )
                    .bind(id)
                    .bind((1i64 << attempts.min(10)) as f64)
                    .bind(e.to_string())
                    .execute(db)
                    .await
                    .map_err(Error::internal)?;
                }
            }
        }
    }
    Ok((fired, failed))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/runtime/timers", get(list))
        .route(
            "/v1/admin/runtime/timers/sweep",
            axum::routing::post(sweep_now),
        )
        .route(
            "/v1/admin/runtime/timers/{id}",
            axum::routing::delete(delete_one),
        )
}

#[derive(Deserialize)]
struct ListQ {
    #[serde(default)]
    limit: Option<i64>,
}

async fn list(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(
        "select id, key, target, payload, extract(epoch from due_at)::bigint, attempts, last_error
         from musubi.timers order by due_at limit $1",
    )
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "timers": rows.into_iter().map(doc).collect::<Vec<_>>() }),
    ))
}

async fn delete_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "deleted": cancel(&s.db, Some(id), None).await? }),
    ))
}

/// Run due timers on this node right now (the lease holder also sweeps every second).
async fn sweep_now(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let (fired, failed) = sweep(&s.db, &s.runtime).await?;
    Ok(Json(json!({ "fired": fired, "failed": failed })))
}
