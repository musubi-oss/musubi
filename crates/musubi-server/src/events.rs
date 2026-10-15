//! Event outbox + webhooks. `Events::emit` writes an event and fans it out to every enabled
//! webhook subscribed to its kind (same transaction); the leader's dispatcher posts due
//! deliveries with an HMAC signature and exponential backoff. Servers can also pull
//! `GET /v1/server/events?after=` instead of receiving pushes.

use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub const MAX_ATTEMPTS: i32 = 12;

#[derive(Clone)]
pub struct Events {
    db: PgPool,
    http: reqwest::Client,
}

fn backoff(attempts: i32) -> Duration {
    let secs = match attempts {
        0 | 1 => 10,
        2 => 30,
        3 => 120,
        4 => 600,
        5 => 1800,
        _ => 3600,
    };
    Duration::from_secs(secs)
}

/// Stripe-style `t=<unix>,v1=<hex hmac-sha256(secret, "<t>.<body>")>`.
pub fn sign(secret: &str, ts: i64, body: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(format!("{ts}.{body}").as_bytes());
    format!("t={ts},v1={}", hex::encode(mac.finalize().into_bytes()))
}

impl Events {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
        }
    }

    /// Record an event and queue it for every subscribed webhook. Never fails the caller's
    /// request: errors are logged.
    pub async fn emit(
        &self,
        kind: &str,
        account: Option<AccountId>,
        payload: Value,
    ) -> Option<Uuid> {
        match self.emit_inner(kind, account, payload).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::warn!(kind, error = %e, "event emit failed");
                None
            }
        }
    }

    async fn emit_inner(
        &self,
        kind: &str,
        account: Option<AccountId>,
        payload: Value,
    ) -> Result<Uuid> {
        let id = Uuid::now_v7();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.events (id, kind, account_id, payload) values ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(kind)
        .bind(account.map(|a| a.0))
        .bind(&payload)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.webhook_deliveries (webhook_id, event_id)
             select id, $1 from musubi.webhooks where enabled and (cardinality(kinds) = 0 or $2 = any(kinds))",
        )
        .bind(id)
        .bind(kind)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(id)
    }

    /// Deliver up to 50 due deliveries. Returns (delivered, failed).
    pub async fn dispatch_once(&self) -> Result<(usize, usize)> {
        type Row = (
            Uuid,
            Uuid,
            i32,
            String,
            String,
            String,
            Option<Uuid>,
            Value,
            time::OffsetDateTime,
        );
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let rows: Vec<Row> = sqlx::query_as(
            "select d.webhook_id, d.event_id, d.attempts, w.url, w.secret, e.kind, e.account_id, e.payload, e.created_at
             from musubi.webhook_deliveries d
             join musubi.webhooks w on w.id = d.webhook_id
             join musubi.events e on e.id = d.event_id
             where d.delivered_at is null and not d.dead and d.next_at <= now() and w.enabled
             order by d.next_at limit 50
             for update of d skip locked",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let (mut ok, mut failed) = (0, 0);
        for (wid, eid, attempts, url, secret, kind, account, payload, created) in rows {
            let body = json!({
                "id": eid,
                "kind": kind,
                "account_id": account,
                "payload": payload,
                "created_at": created.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
            })
            .to_string();
            let ts = time::OffsetDateTime::now_utc().unix_timestamp();
            let res = self
                .http
                .post(&url)
                .header("content-type", "application/json")
                .header("x-musubi-event", &kind)
                .header("x-musubi-event-id", eid.to_string())
                .header("x-musubi-signature", sign(&secret, ts, &body))
                .body(body)
                .send()
                .await;
            let (status, err) = match res {
                Ok(r) => (
                    Some(r.status().as_u16() as i32),
                    (!r.status().is_success()).then(|| format!("http {}", r.status())),
                ),
                Err(e) => (None, Some(e.to_string())),
            };
            if err.is_none() {
                ok += 1;
                sqlx::query(
                    "update musubi.webhook_deliveries set attempts = attempts + 1, delivered_at = now(), last_status = $3, last_error = null
                     where webhook_id = $1 and event_id = $2",
                )
                .bind(wid)
                .bind(eid)
                .bind(status)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            } else {
                failed += 1;
                let next = attempts + 1;
                sqlx::query(
                    "update musubi.webhook_deliveries set attempts = $3, next_at = now() + $4, dead = $5, last_status = $6, last_error = $7
                     where webhook_id = $1 and event_id = $2",
                )
                .bind(wid)
                .bind(eid)
                .bind(next)
                .bind(backoff(next))
                .bind(next >= MAX_ATTEMPTS)
                .bind(status)
                .bind(err.as_deref().map(|e| e.chars().take(500).collect::<String>()))
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            }
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok((ok, failed))
    }
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/admin/webhooks",
            get(list_webhooks).post(create_webhook),
        )
        .route(
            "/v1/admin/webhooks/{id}",
            axum::routing::delete(delete_webhook).patch(update_webhook),
        )
        .route("/v1/admin/webhooks/{id}/deliveries", get(deliveries))
        .route("/v1/admin/webhooks/{id}/test", post(test_webhook))
        .route("/v1/server/events", get(pull_events))
        .route("/v1/server/webhooks/dispatch", post(dispatch_now))
}

#[derive(Deserialize)]
struct NewWebhook {
    name: String,
    url: String,
    secret: String,
    #[serde(default)]
    kinds: Vec<String>,
}

async fn create_webhook(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(b): Json<NewWebhook>,
) -> Result<Json<Value>, ApiError> {
    if !(b.url.starts_with("http://") || b.url.starts_with("https://")) {
        return Err(Error::InvalidArgument("url must be http(s)".into()).into());
    }
    if b.secret.len() < 16 {
        return Err(Error::InvalidArgument("secret must be at least 16 chars".into()).into());
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.webhooks (id, name, url, secret, kinds) values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(&b.name)
    .bind(&b.url)
    .bind(&b.secret)
    .bind(&b.kinds)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
struct PatchWebhook {
    enabled: Option<bool>,
    url: Option<String>,
    kinds: Option<Vec<String>>,
}

async fn update_webhook(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(b): Json<PatchWebhook>,
) -> Result<Json<Value>, ApiError> {
    let r = sqlx::query(
        "update musubi.webhooks set enabled = coalesce($2, enabled), url = coalesce($3, url), kinds = coalesce($4, kinds) where id = $1",
    )
    .bind(id)
    .bind(b.enabled)
    .bind(&b.url)
    .bind(&b.kinds)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if r.rows_affected() == 0 {
        return Err(Error::NotFound("webhook".into()).into());
    }
    Ok(Json(json!({ "updated": true })))
}

async fn list_webhooks(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(w) - 'secret' order by w.created_at), '[]'::jsonb) from musubi.webhooks w",
    )
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "webhooks": rows })))
}

async fn delete_webhook(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let r = sqlx::query("delete from musubi.webhooks where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    if r.rows_affected() == 0 {
        return Err(Error::NotFound("webhook".into()).into());
    }
    Ok(Json(json!({ "deleted": true })))
}

async fn deliveries(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(d) || jsonb_build_object('kind', e.kind) order by d.event_id desc), '[]'::jsonb)
         from (select * from musubi.webhook_deliveries where webhook_id = $1 order by event_id desc limit 200) d
         join musubi.events e on e.id = d.event_id",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "deliveries": rows })))
}

/// Emit a `webhook.test` event for this hook only and deliver it right away.
async fn test_webhook(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let eid = Uuid::now_v7();
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    sqlx::query("insert into musubi.events (id, kind, payload) values ($1, 'webhook.test', $2)")
        .bind(eid)
        .bind(json!({ "webhook_id": id }))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    let r = sqlx::query("insert into musubi.webhook_deliveries (webhook_id, event_id) select id, $1 from musubi.webhooks where id = $2")
        .bind(eid)
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    if r.rows_affected() == 0 {
        return Err(Error::NotFound("webhook".into()).into());
    }
    tx.commit().await.map_err(Error::internal)?;
    let (ok, failed) = s.events.dispatch_once().await?;
    Ok(Json(
        json!({ "event_id": eid, "delivered": ok, "failed": failed }),
    ))
}

#[derive(Deserialize)]
struct PullQ {
    after: Option<Uuid>,
    kinds: Option<String>,
    limit: Option<i64>,
}

/// Ordered pull: events with id > `after` (uuid v7 = time-ordered), optional `kinds=a,b`.
async fn pull_events(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<PullQ>,
) -> Result<Json<Value>, ApiError> {
    let kinds: Option<Vec<String>> = q.kinds.map(|k| {
        k.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    });
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(e) order by e.id), '[]'::jsonb) from (
            select id, kind, account_id, payload, created_at from musubi.events
            where ($1::uuid is null or id > $1) and ($2::text[] is null or kind = any($2))
            order by id limit $3) e",
    )
    .bind(q.after)
    .bind(&kinds)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let cursor = rows
        .as_array()
        .and_then(|a| a.last())
        .and_then(|e| e["id"].as_str())
        .map(String::from);
    Ok(Json(json!({ "events": rows, "cursor": cursor })))
}

async fn dispatch_now(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let (ok, failed) = s.events.dispatch_once().await?;
    Ok(Json(json!({ "delivered": ok, "failed": failed })))
}
