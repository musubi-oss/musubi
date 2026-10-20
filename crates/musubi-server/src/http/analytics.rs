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
        .route("/v1/telemetry/attribution", post(attribute))
        .route("/v1/account/attribution", get(my_attribution))
        .route(
            "/v1/server/accounts/{id}/attribution",
            get(account_attribution).delete(forget_attribution),
        )
        .route("/v1/server/analytics/attribution", get(attribution_cohorts))
        .route(
            "/v1/server/analytics/reports",
            get(list_reports).post(generate_report),
        )
        .route("/v1/server/analytics/reports/latest", get(latest_report))
        .route(
            "/v1/server/analytics/reports/by-date/{date}",
            get(report_by_date),
        )
        .route(
            "/v1/server/analytics/reports/{id}",
            get(get_report).delete(delete_report),
        )
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

/// `POST /v1/telemetry/attribution {source, medium, campaign, content, term, network, click_id,
/// installed_at}` — a UA touch (install referrer, MMP postback relayed by the client, deep link).
/// First touch stays, last touch follows; mirrored as a `platform_ua_attribution` analytics event.
async fn attribute(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let Some(obj) = body.as_object() else {
        return Err(Error::InvalidArgument("body must be an object".into()).into());
    };
    let mut touch = serde_json::Map::new();
    for (k, v) in obj {
        if !crate::analytics::ATTRIBUTION_KEYS.contains(&k.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "unknown attribution key {k}; allowed: {}",
                crate::analytics::ATTRIBUTION_KEYS.join(", ")
            ))
            .into());
        }
        let Some(sv) = v.as_str() else {
            return Err(Error::InvalidArgument(format!("{k} must be a string")).into());
        };
        let sv = sv.trim();
        if sv.is_empty() || sv.len() > 256 {
            return Err(Error::InvalidArgument(format!("{k} must be 1..=256 chars")).into());
        }
        touch.insert(k.clone(), json!(sv));
    }
    if !touch.contains_key("source") && !touch.contains_key("campaign") {
        return Err(Error::InvalidArgument("source or campaign is required".into()).into());
    }
    let touch = Value::Object(touch);
    let doc = s.analytics.attribute(me.account_id.0, &touch).await?;
    s.analytics.push_domain(
        "platform_ua_attribution",
        Some(me.account_id),
        touch.clone(),
    );
    s.events
        .emit("account.attributed", Some(me.account_id), touch)
        .await;
    Ok(Json(doc))
}

async fn my_attribution(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.analytics
            .attribution(me.account_id.0)
            .await?
            .unwrap_or_else(|| json!({ "account_id": me.account_id.0, "first": null, "last": null, "touches": 0 })),
    ))
}

async fn account_attribution(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, id).await?;
    match s.analytics.attribution(id).await? {
        Some(v) => Ok(Json(v)),
        None => Err(Error::NotFound("no attribution recorded for this account".into()).into()),
    }
}

async fn forget_attribution(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, id).await?;
    if !s.analytics.forget_attribution(id).await? {
        return Err(Error::NotFound("no attribution recorded for this account".into()).into());
    }
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize, Default)]
struct CohortQ {
    group_by: Option<String>,
    touch: Option<String>,
    since: Option<String>,
    limit: Option<i64>,
}

/// `GET /v1/server/analytics/attribution?group_by=source|medium|campaign|network|content|term&touch=first|last&since&limit`
async fn attribution_cohorts(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<CohortQ>,
) -> Result<Json<Value>, ApiError> {
    let group_by = q.group_by.unwrap_or_else(|| "campaign".into());
    if !["source", "medium", "campaign", "content", "term", "network"].contains(&group_by.as_str())
    {
        return Err(Error::InvalidArgument(
            "group_by must be source|medium|campaign|content|term|network".into(),
        )
        .into());
    }
    let touch = q.touch.unwrap_or_else(|| "first".into());
    if !["first", "last"].contains(&touch.as_str()) {
        return Err(Error::InvalidArgument("touch must be first|last".into()).into());
    }
    let since = q
        .since
        .as_deref()
        .map(|t| {
            time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                .map_err(|_| Error::InvalidArgument("since must be RFC 3339".into()))
        })
        .transpose()?;
    let limit = q.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(Error::InvalidArgument("limit must be 1..=1000".into()).into());
    }
    let groups = s
        .analytics
        .attribution_cohorts(&group_by, &touch, since, limit)
        .await?;
    Ok(Json(
        json!({ "group_by": group_by, "touch": touch, "groups": groups }),
    ))
}

fn parse_date(d: &str) -> Result<time::Date, ApiError> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(d, &fmt)
        .map_err(|_| Error::InvalidArgument("date must be YYYY-MM-DD".into()).into())
}

fn period_for(kind: &str, period_days: Option<i64>) -> Result<i64, ApiError> {
    let p = match (kind, period_days) {
        ("daily", None) => 1,
        ("weekly", None) => 7,
        ("adhoc", None) => 1,
        (_, Some(p)) if (1..=365).contains(&p) => p,
        _ => {
            return Err(Error::InvalidArgument("period_days must be 1..=365".into()).into());
        }
    };
    Ok(p)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GenerateBody {
    kind: Option<String>,
    for_date: Option<String>,
    period_days: Option<i64>,
    replace: bool,
}

/// `POST /v1/server/analytics/reports {kind: daily|weekly|adhoc, for_date?, period_days?, replace}`
/// — build (or return the existing) snapshot; the leader does `daily` for yesterday and `weekly`
/// on Mondays by itself.
async fn generate_report(
    State(s): State<AppState>,
    _k: ServerKey,
    Json(b): Json<GenerateBody>,
) -> Result<Json<Value>, ApiError> {
    let kind = b.kind.unwrap_or_else(|| "adhoc".into());
    if !["daily", "weekly", "adhoc"].contains(&kind.as_str()) {
        return Err(Error::InvalidArgument("kind must be daily|weekly|adhoc".into()).into());
    }
    let for_date = match b.for_date.as_deref() {
        Some(d) => parse_date(d)?,
        None => time::OffsetDateTime::now_utc().date(),
    };
    let period = period_for(&kind, b.period_days)?;
    let v = s
        .analytics
        .generate_report(&kind, for_date, period, b.replace)
        .await?;
    s.events
        .emit(
            "analytics.report_generated",
            None,
            json!({ "id": v["id"], "kind": kind, "for_date": for_date.to_string(), "period_days": period }),
        )
        .await;
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
struct ReportsQ {
    kind: Option<String>,
    before: Option<String>,
    limit: Option<i64>,
}

fn check_kind(kind: &Option<String>) -> Result<(), ApiError> {
    if kind
        .as_deref()
        .is_some_and(|k| !["daily", "weekly", "adhoc"].contains(&k))
    {
        return Err(Error::InvalidArgument("kind must be daily|weekly|adhoc".into()).into());
    }
    Ok(())
}

/// `GET /v1/server/analytics/reports?kind&before&limit` — latest report headers (≤ 20 default).
async fn list_reports(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ReportsQ>,
) -> Result<Json<Value>, ApiError> {
    check_kind(&q.kind)?;
    let before = q.before.as_deref().map(parse_date).transpose()?;
    let limit = q.limit.unwrap_or(20);
    if !(1..=100).contains(&limit) {
        return Err(Error::InvalidArgument("limit must be 1..=100".into()).into());
    }
    let reports = s
        .analytics
        .reports(q.kind.as_deref(), before, limit)
        .await?;
    Ok(Json(json!({ "reports": reports })))
}

/// `GET /v1/server/analytics/reports/latest?kind` — the newest report with its body.
async fn latest_report(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ReportsQ>,
) -> Result<Json<Value>, ApiError> {
    check_kind(&q.kind)?;
    let heads = s.analytics.reports(q.kind.as_deref(), None, 1).await?;
    let Some(h) = heads.first() else {
        return Err(Error::NotFound("no report yet".into()).into());
    };
    let id = h["id"]
        .as_str()
        .and_then(|i| Uuid::parse_str(i).ok())
        .unwrap_or_default();
    match s.analytics.report(id).await? {
        Some(v) => Ok(Json(v)),
        None => Err(Error::NotFound("no report yet".into()).into()),
    }
}

/// `GET /v1/server/analytics/reports/by-date/{YYYY-MM-DD}?kind&period_days` — the report covering
/// that start date (defaults: `daily`, 1 day).
async fn report_by_date(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(date): Path<String>,
    Query(q): Query<GenerateBody>,
) -> Result<Json<Value>, ApiError> {
    let kind = q.kind.unwrap_or_else(|| "daily".into());
    if !["daily", "weekly", "adhoc"].contains(&kind.as_str()) {
        return Err(Error::InvalidArgument("kind must be daily|weekly|adhoc".into()).into());
    }
    let d = parse_date(&date)?;
    let period = period_for(&kind, q.period_days)?;
    match s.analytics.report_by(&kind, d, period).await? {
        Some(v) => Ok(Json(v)),
        None => Err(Error::NotFound(format!("no {kind} report for {date}")).into()),
    }
}

async fn get_report(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    match s.analytics.report(id).await? {
        Some(v) => Ok(Json(v)),
        None => Err(Error::NotFound(format!("report {id}")).into()),
    }
}

async fn delete_report(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    if !s.analytics.delete_report(id).await? {
        return Err(Error::NotFound(format!("report {id}")).into());
    }
    Ok(Json(json!({ "deleted": true })))
}
