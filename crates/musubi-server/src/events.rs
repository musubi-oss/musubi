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
    /// LiveOps (Zanshin) sink: every account-bound event is also tracked there.
    liveops:
        std::sync::Arc<std::sync::OnceLock<std::sync::Arc<musubi_integrations::ZanshinClient>>>,
    /// SMTP sink for `account.email_verification` / `account.password_reset`.
    mailer: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<musubi_integrations::Mailer>>>,
    analytics: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::analytics::Analytics>>>,
    /// Per-account `{experiment_id: variant}` of running experiments, stamped onto account-bound
    /// events so analytics can split every metric by variant (cached for 60 s).
    experiments: ExperimentCtxCache,
    /// Per-account `service` session baggage (latest write per key across live sessions),
    /// stamped as `baggage` onto account-bound events (cached for 60 s).
    baggage: ExperimentCtxCache,
}

const EXPERIMENT_CTX_TTL: Duration = Duration::from_secs(60);

/// account → (cached at, `{experiment_id: variant}`)
type ExperimentCtxCache = std::sync::Arc<
    std::sync::Mutex<
        std::collections::HashMap<AccountId, (std::time::Instant, std::sync::Arc<Value>)>,
    >,
>;

/// Which events become mail: (template, to, subject, text).
fn mail_for(
    m: &musubi_integrations::Mailer,
    kind: &str,
    p: &Value,
) -> Option<(&'static str, String, String, String)> {
    let to = p["email"].as_str()?.to_string();
    let token = p["token"].as_str()?;
    let c = m.config();
    let minutes = (p["expires_in_secs"].as_i64().unwrap_or(0) / 60).max(1);
    let (template, subject, action, link) = match kind {
        "account.email_verification" => (
            "verify",
            format!("{}: confirm your e-mail", c.product),
            "confirm your e-mail address",
            musubi_integrations::Mailer::link(&c.verify_url, token),
        ),
        "account.password_reset" => (
            "reset",
            format!("{}: reset your password", c.product),
            "reset your password",
            musubi_integrations::Mailer::link(&c.reset_url, token),
        ),
        _ => return None,
    };
    let text = match link {
        Some(l) => format!(
            "Open this link to {action} for {}:\n\n{l}\n\nIt is valid for {minutes} minutes. If you did not request this, ignore this message.\n",
            c.product
        ),
        None => format!(
            "Your {} code to {action}:\n\n{token}\n\nIt is valid for {minutes} minutes. If you did not request this, ignore this message.\n",
            c.product
        ),
    };
    Some((template, to, subject, text))
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
                // a signed POST must land on the configured endpoint, not wherever it redirects
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest client"),
            liveops: Default::default(),
            mailer: Default::default(),
            analytics: Default::default(),
            experiments: Default::default(),
            baggage: Default::default(),
        }
    }

    /// Forget cached `service` baggage of an account (set / cleared by the server).
    pub fn invalidate_baggage(&self, account: AccountId) {
        self.baggage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&account);
    }

    /// `service` session baggage of the account's live sessions, cached for 60 s.
    pub async fn baggage_context(&self, account: AccountId) -> std::sync::Arc<Value> {
        if let Some((at, v)) = self
            .baggage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&account)
        {
            if at.elapsed() < EXPERIMENT_CTX_TTL {
                return v.clone();
            }
        }
        let m = crate::http::baggage::service_context(&self.db, account)
            .await
            .unwrap_or_default();
        let v = std::sync::Arc::new(Value::Object(m));
        let mut g = self.baggage.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|_, (at, _)| at.elapsed() < EXPERIMENT_CTX_TTL);
        g.insert(account, (std::time::Instant::now(), v.clone()));
        v
    }

    /// Forget cached experiment context (assignments or experiment status changed).
    pub fn invalidate_experiments(&self, account: Option<AccountId>) {
        let mut g = self.experiments.lock().unwrap_or_else(|e| e.into_inner());
        match account {
            Some(a) => {
                g.remove(&a);
            }
            None => g.clear(),
        }
    }

    /// `{experiment_id: variant}` for the account's running experiments (empty object when none).
    pub async fn experiment_context(&self, account: AccountId) -> std::sync::Arc<Value> {
        if let Some((at, v)) = self
            .experiments
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&account)
        {
            if at.elapsed() < EXPERIMENT_CTX_TTL {
                return v.clone();
            }
        }
        let rows: Vec<(String, String)> = sqlx::query_as(
            "select a.experiment_id, a.variant from musubi.experiment_assignments a
             join musubi.experiments e on e.id = a.experiment_id
             where a.account_id = $1 and e.status = 'running' order by a.experiment_id",
        )
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();
        let v: serde_json::Map<String, Value> = rows
            .into_iter()
            .map(|(e, v)| (e, Value::String(v)))
            .collect();
        let v = std::sync::Arc::new(Value::Object(v));
        let mut g = self.experiments.lock().unwrap_or_else(|e| e.into_inner());
        g.retain(|_, (at, _)| at.elapsed() < EXPERIMENT_CTX_TTL);
        g.insert(account, (std::time::Instant::now(), v.clone()));
        v
    }

    /// Forward account-bound events to LiveOps as analytics events (`external_id` = account id).
    pub fn set_liveops(&self, c: std::sync::Arc<musubi_integrations::ZanshinClient>) {
        let _ = self.liveops.set(c);
    }

    /// Send verification / reset mails over SMTP (in addition to the event itself).
    pub fn set_mailer(&self, m: std::sync::Arc<musubi_integrations::Mailer>) {
        let _ = self.mailer.set(m);
    }

    /// Record an event and queue it for every subscribed webhook. Never fails the caller's
    /// request: errors are logged.
    pub async fn emit(
        &self,
        kind: &str,
        account: Option<AccountId>,
        payload: Value,
    ) -> Option<Uuid> {
        let mut payload = payload;
        if let Some(a) = account {
            if kind != "experiment.exposed"
                && payload.is_object()
                && payload.get("experiments").is_none()
            {
                let ctx = self.experiment_context(a).await;
                if ctx.as_object().is_some_and(|m| !m.is_empty()) {
                    payload["experiments"] = (*ctx).clone();
                }
            }
            if payload.is_object() && payload.get("baggage").is_none() {
                let ctx = self.baggage_context(a).await;
                if ctx.as_object().is_some_and(|m| !m.is_empty()) {
                    payload["baggage"] = (*ctx).clone();
                }
            }
        }
        if let (Some(l), Some(a)) = (self.liveops.get(), account) {
            l.track(&a.to_string(), kind, redacted(kind, &payload));
        }
        if let Some(m) = self.mailer.get() {
            if let Some(mail) = mail_for(m, kind, &payload) {
                let (m, ev, acct) = (m.clone(), self.clone(), account);
                tokio::spawn(async move {
                    let (template, to, subject, text) = mail;
                    let res = m
                        .send(musubi_integrations::Mail {
                            to: &to,
                            subject: &subject,
                            text: &text,
                        })
                        .await;
                    match res {
                        Ok(()) => {
                            let _ = ev
                                .emit_inner(
                                    "email.sent",
                                    acct,
                                    json!({ "to": to, "template": template }),
                                )
                                .await;
                        }
                        Err(e) => {
                            tracing::warn!(template, error = %e, "mail failed");
                            let _ = ev.emit_inner(
                                "email.failed",
                                acct,
                                json!({ "to": to, "template": template, "error": e.to_string() }),
                            )
                            .await;
                        }
                    }
                });
            }
        }
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
             select id, $1 from musubi.webhooks where enabled and (cardinality(kinds) = 0 or $2 = any(kinds)
                 or exists (select 1 from unnest(kinds) k where k like '%*' and $2 like replace(replace(k, '_', '\\_'), '*', '%')))",
        )
        .bind(id)
        .bind(kind)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        if let Some(a) = self.analytics.get() {
            a.push_domain(kind, account, redacted(kind, &payload));
        }
        Ok(id)
    }

    /// Mirror domain events into analytics (`[analytics] mirror_domain_events`).
    pub fn set_analytics(&self, a: std::sync::Arc<crate::analytics::Analytics>) {
        let _ = self.analytics.set(a);
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
            let secret = match musubi_core::secrets::open(&secret) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!(webhook = %wid, error = %e, "webhook secret cannot be opened; delivery skipped");
                    continue;
                }
            };
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
        .route(
            "/v1/admin/webhooks/{id}/deliveries/{event_id}/redeliver",
            post(redeliver),
        )
        .route("/v1/server/events", get(pull_events))
        .route("/v1/server/webhooks/dispatch", post(dispatch_now))
}

const MAX_WEBHOOK_URL: usize = 2048;
const MAX_WEBHOOK_NAME: usize = 128;
const MAX_WEBHOOK_KINDS: usize = 64;
const MAX_WEBHOOK_SECRET: usize = 1024;

/// Hosts that must never receive a webhook even though they parse as public names.
const BLOCKED_HOSTS: &[&str] = &["metadata.google.internal", "metadata"];

/// An absolute `http(s)` URL with a host, no credentials and no link-local / metadata / unroutable
/// literal target. Loopback and RFC 1918 stay allowed: operators (the only principals that can
/// register hooks) commonly point them at in-cluster services.
pub(crate) fn validate_webhook_url(url: &str) -> Result<()> {
    if url.is_empty() || url.len() > MAX_WEBHOOK_URL {
        return Err(Error::InvalidArgument(format!(
            "url must be 1..={MAX_WEBHOOK_URL} chars"
        )));
    }
    let u = reqwest::Url::parse(url)
        .map_err(|_| Error::InvalidArgument("url must be an absolute http(s) URL".into()))?;
    if !matches!(u.scheme(), "http" | "https") {
        return Err(Error::InvalidArgument("url must be http(s)".into()));
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(Error::InvalidArgument(
            "url must not carry credentials".into(),
        ));
    }
    let Some(host) = u.host_str().filter(|h| !h.is_empty()) else {
        return Err(Error::InvalidArgument("url must have a host".into()));
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let ip = bare
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| ip.to_canonical());
    if ip.is_none() {
        let d = host.trim_end_matches('.').to_ascii_lowercase();
        if BLOCKED_HOSTS.contains(&d.as_str()) {
            return Err(Error::InvalidArgument("url host is not allowed".into()));
        }
    }
    if let Some(ip) = ip {
        let bad = match ip {
            std::net::IpAddr::V4(a) => {
                a.is_unspecified()
                    || a.is_link_local()
                    || a.is_broadcast()
                    || a.is_multicast()
                    || a.is_documentation()
            }
            std::net::IpAddr::V6(a) => {
                a.is_unspecified()
                    || a.is_multicast()
                    || (a.segments()[0] & 0xffc0) == 0xfe80
                    || a.segments()[0] == 0x2001 && a.segments()[1] == 0x0db8
            }
        };
        if bad {
            return Err(Error::InvalidArgument(
                "url host is not a routable address".into(),
            ));
        }
    }
    Ok(())
}

fn validate_webhook_kinds(kinds: &[String]) -> Result<()> {
    if kinds.len() > MAX_WEBHOOK_KINDS {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_WEBHOOK_KINDS} kinds"
        )));
    }
    if kinds
        .iter()
        .any(|k| k.is_empty() || k.len() > MAX_WEBHOOK_NAME)
    {
        return Err(Error::InvalidArgument(format!(
            "each kind must be 1..={MAX_WEBHOOK_NAME} chars"
        )));
    }
    Ok(())
}

/// The copy of a payload that goes to third-party sinks (LiveOps, analytics): one-time secrets
/// that `account.*` events carry for the mailer / `GET /v1/server/events` are dropped.
fn redacted(kind: &str, payload: &Value) -> Value {
    let mut v = payload.clone();
    if let Some(o) = v.as_object_mut() {
        o.remove("token");
        if kind.starts_with("account.") {
            o.remove("code");
        }
    }
    v
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
    validate_webhook_url(&b.url)?;
    if b.name.is_empty() || b.name.len() > MAX_WEBHOOK_NAME {
        return Err(
            Error::InvalidArgument(format!("name must be 1..={MAX_WEBHOOK_NAME} chars")).into(),
        );
    }
    validate_webhook_kinds(&b.kinds)?;
    if b.secret.len() < 16 {
        return Err(Error::InvalidArgument("secret must be at least 16 chars".into()).into());
    }
    if b.secret.len() > MAX_WEBHOOK_SECRET {
        return Err(Error::InvalidArgument(format!(
            "secret must be at most {MAX_WEBHOOK_SECRET} chars"
        ))
        .into());
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.webhooks (id, name, url, secret, kinds) values ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(&b.name)
    .bind(&b.url)
    .bind(musubi_core::secrets::seal(&b.secret))
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
    if let Some(u) = &b.url {
        validate_webhook_url(u)?;
    }
    if let Some(k) = &b.kinds {
        validate_webhook_kinds(k)?;
    }
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

/// Put a delivery back in the queue — a dead one after `MAX_ATTEMPTS`, or an already delivered
/// one the receiver lost; attempts and backoff start over and the next dispatch cycle sends it.
async fn redeliver(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((id, event_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query(
        "update musubi.webhook_deliveries
         set attempts = 0, dead = false, next_at = now(), delivered_at = null, last_status = null, last_error = null
         where webhook_id = $1 and event_id = $2",
    )
    .bind(id)
    .bind(event_id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("delivery not found".into()).into());
    }
    Ok(Json(json!({ "queued": true })))
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

/// `node.joined` / `node.dead` from SWIM membership and `node.left` from voluntary departures go
/// to the outbox so ops webhooks see cluster incidents (bus clusters only).
pub fn start_cluster_events(state: crate::app::AppState) {
    let Some(c) = state.cluster.clone() else {
        return;
    };
    // node.joined / node.dead from SWIM, node.left from voluntary departures
    let ev = state.events.clone();
    let mut members = c.member_events();
    let mut bus = c.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                m = members.recv() => match m {
                    Ok(m) => {
                        let kind = match m.state {
                            "alive" => "node.joined",
                            "dead" => "node.dead",
                            _ => continue,
                        };
                        ev.emit(kind, None, serde_json::json!({ "node_id": m.id })).await;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                },
                b = bus.recv() => match b {
                    Ok(b) if b.topic == musubi_cluster::LEAVE_TOPIC => {
                        ev.emit("node.left", None, serde_json::json!({ "node_id": b.payload })).await;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                },
            }
        }
    });
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn webhook_url_accepts_public_and_cluster_targets() {
        for u in [
            "https://hooks.example.com/musubi",
            "http://127.0.0.1:8080/ok",
            "http://10.0.0.5/hook",
            "http://[::1]:9/x",
            "https://example.com./trailing-dot",
        ] {
            assert!(validate_webhook_url(u).is_ok(), "{u}");
        }
    }

    #[test]
    fn webhook_url_rejects_unroutable_or_dangerous_targets() {
        for u in [
            "",
            "ftp://nope",
            "javascript:alert(1)",
            "http://",
            "http://user:pw@example.com/x",
            "http://user@example.com/x",
            "http://169.254.169.254/latest/meta-data/",
            "http://[fe80::1]/x",
            "http://[::ffff:169.254.169.254]/x",
            "http://0.0.0.0/x",
            "http://224.0.0.1/x",
            "http://255.255.255.255/x",
            "http://metadata.google.internal/computeMetadata/v1/",
            "http://METADATA/",
            "not a url",
        ] {
            assert!(validate_webhook_url(u).is_err(), "{u}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_WEBHOOK_URL));
        assert!(validate_webhook_url(&long).is_err());
    }

    #[test]
    fn webhook_kinds_are_capped() {
        assert!(validate_webhook_kinds(&[]).is_ok());
        assert!(validate_webhook_kinds(&["account.*".into()]).is_ok());
        assert!(validate_webhook_kinds(&[String::new()]).is_err());
        assert!(validate_webhook_kinds(&["k".repeat(MAX_WEBHOOK_NAME + 1)]).is_err());
        let many: Vec<String> = (0..=MAX_WEBHOOK_KINDS).map(|i| format!("k{i}")).collect();
        assert!(validate_webhook_kinds(&many).is_err());
    }

    #[test]
    fn third_party_copies_drop_one_time_secrets() {
        let p = json!({ "account_id": "a", "email": "e@x", "token": "secret", "code": "123456" });
        let r = redacted("account.mfa_code", &p);
        assert!(r.get("token").is_none());
        assert!(r.get("code").is_none());
        assert_eq!(r["email"], "e@x");
        let r = redacted(
            "ban_reason.updated",
            &json!({ "code": "toxic", "token": "t" }),
        );
        assert_eq!(r["code"], "toxic");
        assert!(r.get("token").is_none());
        assert_eq!(redacted("x", &json!("scalar")), json!("scalar"));
    }

    #[test]
    fn signature_is_stable() {
        let sig = sign("s3cret", 1_700_000_000, "{}");
        assert!(sig.starts_with("t=1700000000,v1="));
        assert_eq!(sig.len(), "t=1700000000,v1=".len() + 64);
    }
}
