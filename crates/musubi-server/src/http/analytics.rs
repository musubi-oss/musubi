//! Product analytics routes (see `crate::analytics`).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::analytics::{Incoming, Schema};
use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/telemetry/events", post(ingest))
        .route("/v1/telemetry/anonymous", post(ingest_anonymous))
        .route("/v1/telemetry/sampling", get(client_sampling))
        .route("/v1/account/event_properties", get(my_event_props))
        .route(
            "/v1/server/accounts/{id}/event_properties",
            get(account_event_props).delete(reset_event_props),
        )
        .route(
            "/v1/server/accounts/{id}/event_properties/{event}",
            get(account_event_prop).delete(reset_event_prop),
        )
        .route(
            "/v1/server/analytics/sampling",
            get(get_sampling).put(put_sampling),
        )
        .route(
            "/v1/server/analytics/schemas",
            get(list_schemas).post(post_schema),
        )
        .route(
            "/v1/server/analytics/schemas/{name}",
            axum::routing::put(put_schema).delete(delete_schema),
        )
        .route("/v1/server/analytics/events", get(export))
        .route("/v1/server/analytics/flush", post(flush))
        .route("/v1/server/analytics/kpi/active_users", get(active_users))
        .route("/v1/server/analytics/kpi/retention", get(retention))
        .route("/v1/server/analytics/kpi/funnel", get(funnel))
        .route("/v1/server/analytics/kpi/revenue", get(revenue))
        .route("/v1/server/analytics/kpi/events", get(event_counts))
}

#[derive(Deserialize)]
struct Batch {
    events: Vec<Incoming>,
    #[serde(default)]
    device_id: Option<String>,
}

/// `POST /v1/telemetry/events {events: [{name, at?, properties, session_id?, platform?, country?}]}`
async fn ingest(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Batch>,
) -> std::result::Result<Json<Value>, ApiError> {
    let r = s
        .analytics
        .ingest(b.events, Some(me.account_id), b.device_id, "client")
        .await?;
    Ok(Json(serde_json::to_value(r).map_err(Error::internal)?))
}

/// `POST /v1/telemetry/anonymous` with `X-Telemetry-Key` — pre-login events (installs,
/// first-launch funnel); `device_id` is required.
async fn ingest_anonymous(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<Batch>,
) -> std::result::Result<Json<Value>, ApiError> {
    let key = headers
        .get("x-telemetry-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key.is_empty() || !s.analytics.cfg.telemetry_keys.iter().any(|k| k == key) {
        return Err(Error::Unauthenticated("telemetry key".into()).into());
    }
    let device = b
        .device_id
        .clone()
        .filter(|d| !d.is_empty() && d.len() <= 128)
        .ok_or_else(|| Error::InvalidArgument("device_id required".into()))?;
    let r = s
        .analytics
        .ingest(b.events, None, Some(device), "client")
        .await?;
    Ok(Json(serde_json::to_value(r).map_err(Error::internal)?))
}

async fn list_schemas(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let m = s.analytics.schemas().await?;
    let mut list: Vec<&Schema> = m.values().collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(json!({ "schemas": list })))
}

#[derive(Deserialize)]
struct SchemaBody {
    #[serde(default)]
    description: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    required: Vec<String>,
    #[serde(default = "hundred")]
    sampling_pct: i32,
}

fn yes() -> bool {
    true
}

fn hundred() -> i32 {
    100
}

async fn put_schema(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<SchemaBody>,
) -> std::result::Result<Json<Schema>, ApiError> {
    Ok(Json(
        s.analytics
            .save_schema(&Schema {
                name,
                description: b.description,
                enabled: b.enabled,
                required: b.required,
                sampling_pct: b.sampling_pct,
            })
            .await?,
    ))
}

/// `GET /v1/telemetry/sampling` — what the client may skip sending: global keep rate, events
/// that are always kept, and the effective per-event rates of registered schemas (`version`
/// changes whenever the policy does — poll it, drop the local copy on change).
async fn client_sampling(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = s.analytics.sampling().await?;
    let schemas = s.analytics.schemas().await?;
    let mut events: std::collections::BTreeMap<String, i32> = schemas
        .values()
        .filter(|sc| sc.enabled)
        .map(|sc| (sc.name.clone(), p.effective_pct(&sc.name, sc.sampling_pct)))
        .collect();
    for n in &p.always_enabled {
        events.insert(n.clone(), 100);
    }
    Ok(Json(json!({
        "global_pct": p.global_pct, "always_enabled": p.always_enabled, "version": p.version,
        "events": events,
    })))
}

async fn get_sampling(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<crate::analytics::SamplingPolicy>, ApiError> {
    Ok(Json((*s.analytics.sampling().await?).clone()))
}

/// `PUT /v1/server/analytics/sampling {global_pct, always_enabled[]}` — hot change; every node
/// picks it up within 30 s, clients on their next `GET /v1/telemetry/sampling`.
async fn put_sampling(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<crate::analytics::SamplingPolicy>,
) -> std::result::Result<Json<crate::analytics::SamplingPolicy>, ApiError> {
    let p = s.analytics.save_sampling(&b).await?;
    s.events
        .emit(
            "telemetry.sampling_changed",
            None,
            json!({ "global_pct": p.global_pct, "always_enabled": p.always_enabled, "version": p.version }),
        )
        .await;
    Ok(Json((*p).clone()))
}

async fn post_schema(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<Schema>,
) -> std::result::Result<Json<Schema>, ApiError> {
    Ok(Json(s.analytics.save_schema(&b).await?))
}

async fn delete_schema(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "removed": s.analytics.delete_schema(&name).await? }),
    ))
}

#[derive(Deserialize)]
struct ExportQ {
    name: Option<String>,
    account_id: Option<Uuid>,
    after: Option<i64>,
    before: Option<i64>,
    cursor: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    format: String,
}

fn default_limit() -> i64 {
    1000
}

/// `GET /v1/server/analytics/events?name&account_id&after&before&cursor&limit&format=json|jsonl`
/// — paged raw export (`next_cursor` = last id).
async fn export(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ExportQ>,
) -> std::result::Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let rows = s
        .analytics
        .export(
            q.name.as_deref(),
            q.account_id,
            q.after,
            q.before,
            q.cursor,
            q.limit,
        )
        .await?;
    let next = rows.last().map(|r| r.id);
    if q.format == "jsonl" {
        let mut body = String::new();
        for r in &rows {
            body.push_str(&serde_json::to_string(r).map_err(Error::internal)?);
            body.push('\n');
        }
        let mut res = body.into_response();
        res.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/x-ndjson"),
        );
        if let Some(n) = next {
            if let Ok(v) = axum::http::HeaderValue::from_str(&n.to_string()) {
                res.headers_mut().insert("x-next-cursor", v);
            }
        }
        return Ok(res);
    }
    Ok(Json(json!({ "events": rows, "next_cursor": next })).into_response())
}

async fn flush(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "sinks": s.analytics.flush().await })))
}

#[derive(Deserialize)]
struct DaysQ {
    #[serde(default = "default_days")]
    days: i64,
    steps: Option<String>,
}

fn default_days() -> i64 {
    30
}

async fn active_users(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<DaysQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(s.analytics.active_users(q.days).await?))
}

async fn retention(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<DaysQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(s.analytics.retention(q.days).await?))
}

/// `GET /v1/server/analytics/kpi/funnel?steps=app_open,tutorial_done,first_purchase&days=`
async fn funnel(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<DaysQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let steps: Vec<String> = q
        .steps
        .unwrap_or_default()
        .split(',')
        .filter(|x| !x.is_empty())
        .map(str::to_string)
        .collect();
    Ok(Json(s.analytics.funnel(&steps, q.days).await?))
}

async fn revenue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<DaysQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(s.analytics.revenue(q.days).await?))
}

async fn event_counts(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<DaysQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(s.analytics.event_counts(q.days).await?))
}

#[allow(dead_code)]
fn _unused(_: Result<()>) {}

/// `GET /v1/account/event_properties` — the caller's computed per-event properties.
async fn my_event_props(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.analytics.event_props(me.account_id.0, None).await?))
}

async fn account_exists(s: &AppState, id: Uuid) -> Result<(), ApiError> {
    let n: i64 = sqlx::query_scalar("select count(*) from musubi.accounts where id = $1")
        .bind(id)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    if n == 0 {
        return Err(Error::NotFound(format!("account {id}")).into());
    }
    Ok(())
}

/// `GET /v1/server/accounts/{id}/event_properties` — all computed event properties of an account.
async fn account_event_props(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, id).await?;
    Ok(Json(s.analytics.event_props(id, None).await?))
}

/// `GET /v1/server/accounts/{id}/event_properties/{event}` — one event's aggregate (404 if never seen).
async fn account_event_prop(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, event)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, id).await?;
    let mut v = s.analytics.event_props(id, Some(&event)).await?;
    let Some(row) = v["events"].as_array_mut().and_then(|a| a.pop()) else {
        return Err(Error::NotFound(format!("event {event} never seen for this account")).into());
    };
    Ok(Json(row))
}

/// `DELETE /v1/server/accounts/{id}/event_properties` — forget the aggregates (raw events stay).
async fn reset_event_props(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, id).await?;
    let n = s.analytics.reset_event_props(id, None).await?;
    Ok(Json(json!({ "deleted": n })))
}

async fn reset_event_prop(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, event)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, id).await?;
    let n = s.analytics.reset_event_props(id, Some(&event)).await?;
    if n == 0 {
        return Err(Error::NotFound(format!("event {event} never seen for this account")).into());
    }
    Ok(Json(json!({ "deleted": n })))
}
