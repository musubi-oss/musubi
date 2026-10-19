//! Per-server fleet metrics and alerts: agent ingest (`POST /v1/gameserver/metrics`), operator
//! history/summary and alert rules under `/v1/server/fleets/{id}/...`.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;
use crate::fleet_metrics::{Rule, RuleInput, Sample, Transition};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/gameserver/metrics", post(sdk_metrics))
        .route("/v1/server/fleets/{id}/metrics", get(fleet_summary))
        .route(
            "/v1/server/fleets/{id}/servers/{sid}/metrics",
            get(server_history),
        )
        .route("/v1/server/fleets/{id}/alerts", get(alerts))
        .route("/v1/server/fleets/{id}/alerts/sweep", post(sweep))
        .route("/v1/server/fleets/{id}/alerts/rules", get(rules))
        .route(
            "/v1/server/fleets/{id}/alerts/rules/{name}",
            axum::routing::put(put_rule).delete(delete_rule),
        )
}

fn self_id(headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let v = headers
        .get("x-musubi-server-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Id".into()))?;
    Uuid::parse_str(v).map_err(|_| Error::InvalidArgument("bad X-Musubi-Server-Id".into()).into())
}

/// Turn alert transitions into `fleet.alert_fired|resolved` events (webhooks, LiveOps).
pub async fn emit(s: &AppState, ts: &[Transition]) {
    for t in ts {
        s.events
            .emit(
                if t.kind == "fired" {
                    "fleet.alert_fired"
                } else {
                    "fleet.alert_resolved"
                },
                None,
                serde_json::to_value(&t.alert).unwrap_or_default(),
            )
            .await;
        tracing::warn!(
            fleet = %t.alert.fleet_id, server = %t.alert.server_id, rule = %t.alert.rule,
            value = ?t.alert.value, threshold = t.alert.threshold, "fleet alert {}", t.kind
        );
    }
}

/// `POST /v1/gameserver/metrics {cpu_pct, mem_bytes, mem_limit_bytes, net_in_bps, net_out_bps,
/// tick_ms, tick_p99_ms, tick_hz, players, capacity, custom: {k: number}}` — agent report; also
/// updates the server's occupancy when `players` is present. Returns alerts that changed state.
async fn sdk_metrics(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(sample): Json<Sample>,
) -> Result<Json<Value>, ApiError> {
    let sid = self_id(&headers)?;
    let gs = s.fleets.server(sid).await?;
    let ts = s.fleet_metrics.ingest(gs.fleet_id, sid, &sample).await?;
    if let Some(p) = sample.players {
        s.fleets.sdk_players(sid, p, sample.capacity).await?;
    }
    emit(&s, &ts).await;
    Ok(Json(json!({ "ok": true, "alerts": ts })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SummaryQuery {
    window_secs: Option<i64>,
    stale_secs: Option<i64>,
}

/// `GET /v1/server/fleets/{id}/metrics?window_secs=300&stale_secs=60` — latest sample per live
/// server, window aggregates, stale count, firing alerts.
async fn fleet_summary(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Query(q): Query<SummaryQuery>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.get(id).await?;
    Ok(Json(
        s.fleet_metrics
            .summary(id, q.window_secs.unwrap_or(300), q.stale_secs.unwrap_or(60))
            .await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct HistoryQuery {
    since_secs: Option<i64>,
    step_secs: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/server/fleets/{id}/servers/{sid}/metrics?since_secs&step_secs&limit` — newest first;
/// `step_secs` buckets (avg, max p99/players).
async fn server_history(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, sid)): Path<(Uuid, Uuid)>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let gs = s.fleets.server(sid).await?;
    if gs.fleet_id != id {
        return Err(Error::NotFound("server not in fleet".into()).into());
    }
    let latest = s.fleet_metrics.latest(sid).await?;
    let samples = s
        .fleet_metrics
        .history(
            sid,
            q.since_secs.unwrap_or(3600),
            q.step_secs.unwrap_or(0),
            q.limit.unwrap_or(500),
        )
        .await?;
    let alerts = s.fleet_metrics.alerts(id, Some(sid), "all", 50).await?;
    Ok(Json(json!({
        "server_id": sid, "state": gs.state, "latest": latest, "samples": samples, "alerts": alerts,
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AlertsQuery {
    state: Option<String>,
    server_id: Option<Uuid>,
    limit: Option<i64>,
}

/// `GET /v1/server/fleets/{id}/alerts?state=firing|pending|resolved|all&server_id&limit`.
async fn alerts(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Query(q): Query<AlertsQuery>,
) -> Result<Json<Value>, ApiError> {
    let alerts = s
        .fleet_metrics
        .alerts(
            id,
            q.server_id,
            q.state.as_deref().unwrap_or("firing"),
            q.limit.unwrap_or(100),
        )
        .await?;
    Ok(Json(json!({ "alerts": alerts })))
}

/// `POST /v1/server/fleets/{id}/alerts/sweep` — run the silence check / pruning now (the lease
/// holder does this every 15 s).
async fn sweep(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.get(id).await?;
    let ts = s.fleet_metrics.sweep(0).await?;
    emit(&s, &ts).await;
    let mine: Vec<&Transition> = ts.iter().filter(|t| t.alert.fleet_id == id).collect();
    Ok(Json(json!({ "transitions": mine })))
}

async fn rules(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "rules": s.fleet_metrics.rules(id).await? })))
}

/// `PUT /v1/server/fleets/{id}/alerts/rules/{name} {metric, op: gt|gte|lt|lte, threshold,
/// for_secs, severity: info|warning|critical, enabled}` — metrics: cpu_pct, mem_bytes, mem_pct,
/// net_in_bps, net_out_bps, tick_ms, tick_p99_ms, tick_hz, players, occupancy_pct, silence_secs,
/// custom.<key>.
async fn put_rule(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, name)): Path<(Uuid, String)>,
    Json(r): Json<RuleInput>,
) -> Result<Json<Rule>, ApiError> {
    s.fleets.get(id).await?;
    Ok(Json(s.fleet_metrics.put_rule(id, &name, r).await?))
}

async fn delete_rule(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, name)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "deleted": s.fleet_metrics.delete_rule(id, &name).await? }),
    ))
}
