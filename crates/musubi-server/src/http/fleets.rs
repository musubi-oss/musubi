//! Dedicated-server orchestration: control surface (server key) and the game-server SDK surface
//! (server key + `X-Musubi-Server-Id`, Agones-like: ready / health / allocate / shutdown / label / connect).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use musubi_fleet::{Allocation, CreateFleet, DemandSlot, Fleet, GameServer, ScalePolicy};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/fleets", get(list).post(create))
        .route("/v1/server/fleets/{id}", get(get_one).delete(delete))
        .route("/v1/server/fleets/{id}/scale", post(scale))
        .route(
            "/v1/server/fleets/providers/{name}/prepare",
            post(prepare_provider),
        )
        .route("/v1/server/fleets/{id}/allocate", post(allocate))
        .route("/v1/server/allocate", post(allocate_routed))
        .route("/v1/server/fleets/{id}/servers", get(servers))
        .route(
            "/v1/server/fleets/{id}/servers/{sid}/logs",
            get(server_logs),
        )
        .route("/v1/server/fleets/{id}/demand", get(demand))
        .route("/v1/server/fleets/{id}/builds", get(builds).post(add_build))
        .route(
            "/v1/server/fleets/{id}/builds/{version}",
            axum::routing::delete(retire_build),
        )
        .route(
            "/v1/server/fleets/{id}/builds/{version}/promote",
            post(promote_build),
        )
        .route("/v1/server/fleets/{id}/usage", get(usage))
        .route("/v1/server/fleets/{id}/rollout", get(rollout_get))
        .route("/v1/server/fleets/{id}/health", get(health_report))
        .route("/v1/server/fleets/{id}/rollouts", get(rollouts))
        .route("/v1/server/fleets/{id}/rollout/pause", post(rollout_pause))
        .route(
            "/v1/server/fleets/{id}/rollout/resume",
            post(rollout_resume),
        )
        .route(
            "/v1/server/fleets/{id}/rollout/rollback",
            post(rollout_rollback),
        )
        .route("/v1/server/fleets/{id}/events", get(fleet_events))
        .route("/v1/server/fleets/{id}/counters", get(fleet_counters))
        .route("/v1/server/gameservers", get(inventory))
        .route("/v1/server/gameservers/counts", get(inventory_counts))
        .route(
            "/v1/server/gameservers/{id}/counters",
            axum::routing::patch(op_counters),
        )
        .route(
            "/v1/server/gameservers/{id}/lists",
            axum::routing::patch(op_lists),
        )
        .route("/v1/gameserver/counters", post(sdk_counters))
        .route("/v1/gameserver/events", post(sdk_event))
        .route("/v1/gameserver/lists", post(sdk_lists))
        .route(
            "/v1/server/gameservers/{id}",
            get(server_get).delete(server_delete),
        )
        .route("/v1/gameserver", get(sdk_get))
        .route("/v1/gameserver/ready", post(sdk_ready))
        .route("/v1/gameserver/health", post(sdk_health))
        .route("/v1/gameserver/allocate", post(sdk_allocate))
        .route("/v1/gameserver/shutdown", post(sdk_shutdown))
        .route("/v1/gameserver/label", post(sdk_label))
        .route("/v1/gameserver/players", post(sdk_players))
        .route("/v1/gameserver/watch", get(sdk_watch))
        .route("/v1/gameserver/connect", post(sdk_connect))
        .route("/v1/gameserver/release", post(sdk_release))
        .route("/v1/gameserver/logs", post(sdk_logs))
}

fn parse(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad id".into()).into())
}

fn self_id(headers: &HeaderMap) -> Result<Uuid, ApiError> {
    let v = headers
        .get("x-musubi-server-id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Id".into()))?;
    parse(v)
}

async fn list(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "fleets": s.fleets.list().await?, "providers": s.fleets.providers() }),
    ))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<CreateFleet>,
) -> Result<Json<Fleet>, ApiError> {
    Ok(Json(s.fleets.create(c).await?))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Fleet>, ApiError> {
    Ok(Json(s.fleets.get(parse(&id)?).await?))
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.delete(parse(&id)?).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScaleReq {
    buffer: Option<i32>,
    max_servers: Option<i32>,
    min_servers: Option<i32>,
    policy: Option<ScalePolicy>,
}

async fn prepare_provider(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.prepare_provider(&name).await?))
}

async fn scale(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<ScaleReq>,
) -> Result<Json<Fleet>, ApiError> {
    Ok(Json(
        s.fleets
            .scale(
                parse(&id)?,
                r.buffer,
                r.max_servers,
                r.min_servers,
                r.policy,
            )
            .await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AllocateReq {
    payload: Value,
    /// Prefer a live server with free player slots (backfill) over a fresh one.
    backfill: bool,
    /// With nothing ready, start a server and wait up to this long for it (scale-to-zero).
    wait_secs: u64,
    /// `{labels, build_version, states, counters: {name: {min_available, min_count, max_count}},
    /// lists: {name: {contains, excludes, min_available}}}` — which servers may be taken.
    selector: musubi_fleet::AllocSelector,
    /// `{name: {delta, count, capacity}}` applied atomically with the allocation.
    counter_actions: std::collections::HashMap<String, musubi_fleet::CounterAction>,
    /// `{name: {add, remove, values, capacity}}` applied atomically with the allocation.
    list_actions: std::collections::HashMap<String, musubi_fleet::ListAction>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RoutedAllocateReq {
    /// A pool name or a fleet name.
    pool: String,
    /// Region hint (from the matchmaker's best region or the client's probes).
    region: Option<String>,
    payload: Value,
    backfill: bool,
    wait_secs: u64,
    selector: musubi_fleet::AllocSelector,
    counter_actions: std::collections::HashMap<String, musubi_fleet::CounterAction>,
    list_actions: std::collections::HashMap<String, musubi_fleet::ListAction>,
}

/// Allocate across a pool of fleets: the hinted region first, then any; `wait_secs` cold-starts a
/// server when every fleet is empty.
async fn allocate_routed(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<RoutedAllocateReq>,
) -> Result<Json<Allocation>, ApiError> {
    if r.pool.is_empty() {
        return Err(Error::InvalidArgument("pool is required".into()).into());
    }
    Ok(Json(
        s.fleets
            .allocate_routed_with(
                &r.pool,
                r.region.as_deref(),
                musubi_fleet::AllocateRequest {
                    payload: r.payload,
                    backfill: r.backfill,
                    selector: r.selector,
                    counter_actions: r.counter_actions,
                    list_actions: r.list_actions,
                },
                std::time::Duration::from_secs(r.wait_secs.min(300)),
            )
            .await?,
    ))
}

async fn allocate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<AllocateReq>,
) -> Result<Json<Allocation>, ApiError> {
    let fleet = s.fleets.get(parse(&id)?).await?;
    Ok(Json(
        s.fleets
            .allocate_routed_with(
                &fleet.name,
                None,
                musubi_fleet::AllocateRequest {
                    payload: r.payload,
                    backfill: r.backfill,
                    selector: r.selector,
                    counter_actions: r.counter_actions,
                    list_actions: r.list_actions,
                },
                std::time::Duration::from_secs(r.wait_secs.min(300)),
            )
            .await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServersQuery {
    state: Option<String>,
}

async fn servers(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<ServersQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "servers": s.fleets.servers(parse(&id)?, q.state.as_deref()).await? }),
    ))
}

/// Demand history (hour-of-week EWMA of allocations) and what the autoscaler wants right now.
async fn demand(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let f = s.fleets.get(id).await?;
    let now = time::OffsetDateTime::now_utc();
    let slots: Vec<DemandSlot> = s.fleets.demand(id).await?;
    let per_hour = s.fleets.forecast_per_hour(id, now).await?;
    let buffer = f.policy.buffer_at(now, f.buffer);
    Ok(Json(json!({
        "slot": musubi_fleet::demand_slot(now),
        "per_hour": per_hour,
        "buffer": buffer,
        "target_ready": musubi_fleet::target_ready(&f.policy, buffer, per_hour),
        "slots": slots,
    })))
}

async fn server_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.server(parse(&id)?).await?))
}

async fn server_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.sdk_shutdown(parse(&id)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn sdk_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.server(self_id(&headers)?).await?))
}

async fn sdk_ready(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.sdk_ready(self_id(&headers)?).await?))
}

/// `GET /v1/server/fleets/{id}/health` — per-server health status under the fleet's health
/// policy: healthy / suspect (missed checks) / unhealthy (about to be reaped) / starting / stuck.
async fn health_report(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.health_report(id).await?))
}

async fn sdk_health(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    s.fleets.sdk_health(self_id(&headers)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn sdk_allocate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.sdk_allocate(self_id(&headers)?).await?))
}

async fn sdk_shutdown(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    s.fleets.sdk_shutdown(self_id(&headers)?).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn sdk_label(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(labels): Json<Value>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.sdk_labels(self_id(&headers)?, labels).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PlayersReq {
    players: i32,
    capacity: Option<i32>,
}

/// `POST /v1/gameserver/players {players, capacity?}` — occupancy for backfill allocations.
async fn sdk_players(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(r): Json<PlayersReq>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(
        s.fleets
            .sdk_players(self_id(&headers)?, r.players, r.capacity)
            .await?,
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WatchQuery {
    /// Last `version` the caller saw; the call returns as soon as the server changed past it.
    since: Option<i64>,
    /// Long-poll budget, 1..=60 s (default 30).
    timeout_secs: Option<u64>,
}

/// `GET /v1/gameserver/watch?since=&timeout_secs=` — WatchGameServer: returns the server record
/// when it changes (state, labels, allocation, players) or when the timeout passes.
async fn sdk_watch(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Query(q): Query<WatchQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = self_id(&headers)?;
    let budget = std::time::Duration::from_secs(q.timeout_secs.unwrap_or(30).clamp(1, 60));
    let gs = s.fleets.watch(id, q.since, budget).await?;
    let changed = q.since.is_none_or(|v| gs.version > v);
    Ok(Json(json!({ "changed": changed, "server": gs })))
}

#[derive(Deserialize)]
struct ConnectReq {
    token: String,
}

async fn sdk_connect(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(r): Json<ConnectReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.fleets.sdk_connect(self_id(&headers)?, &r.token).await?,
    ))
}

async fn sdk_release(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(r): Json<ConnectReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.fleets.sdk_release(self_id(&headers)?, &r.token).await?,
    ))
}

async fn builds(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    s.fleets.get(id).await?;
    let builds: Vec<musubi_fleet::FleetBuild> = s.fleets.builds(id).await?;
    Ok(Json(json!({ "builds": builds })))
}

async fn add_build(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(c): Json<musubi_fleet::CreateBuild>,
) -> Result<Json<musubi_fleet::FleetBuild>, ApiError> {
    let id = parse(&id)?;
    let b = s.fleets.add_build(id, c).await?;
    Ok(Json(b))
}

async fn promote_build(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, version)): Path<(String, String)>,
) -> Result<Json<musubi_fleet::FleetBuild>, ApiError> {
    let id = parse(&id)?;
    let b = s.fleets.promote_build(id, &version).await?;
    Ok(Json(b))
}

async fn retire_build(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, version)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    s.fleets.retire_build(id, &version).await?;
    Ok(Json(json!({ "retired": version })))
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct UsageQuery {
    /// RFC 3339; default = 30 days ago.
    from: Option<String>,
    /// RFC 3339; default = now.
    to: Option<String>,
}

fn parse_time(
    s: Option<&str>,
    default: time::OffsetDateTime,
) -> Result<time::OffsetDateTime, ApiError> {
    match s {
        None => Ok(default),
        Some(s) => time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
            .map_err(|_| {
                musubi_core::Error::InvalidArgument("from/to must be RFC 3339".into()).into()
            }),
    }
}

async fn usage(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    axum::extract::Query(q): axum::extract::Query<UsageQuery>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let now = time::OffsetDateTime::now_utc();
    let to = parse_time(q.to.as_deref(), now)?;
    let from = parse_time(q.from.as_deref(), now - time::Duration::days(30))?;
    if from >= to {
        return Err(musubi_core::Error::InvalidArgument("from must be before to".into()).into());
    }
    let rows: Vec<musubi_fleet::UsageRow> = s.fleets.usage(id, from, to).await?;
    let total: f64 = rows.iter().map(|r| r.vcpu_hours).sum();
    Ok(Json(
        json!({ "from": from.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(), "to": to.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(), "rows": rows, "vcpu_hours": total }),
    ))
}

#[derive(Deserialize)]
struct LogsQ {
    #[serde(flatten)]
    filter: musubi_fleet::LogQuery,
    /// `provider` = raw tail straight from docker / kubernetes / ssh instead of the stored lines.
    source_kind: Option<String>,
}

/// `GET /v1/server/fleets/{id}/servers/{sid}/logs` — captured stdout/stderr + SDK lines of one
/// game server, newest first (`level`, `source`, `q`, `before`, `after`, `limit`); with
/// `source_kind=provider` the provider's own tail (`{"raw": "..."}`).
async fn server_logs(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, sid)): Path<(String, String)>,
    Query(q): Query<LogsQ>,
) -> Result<Json<Value>, ApiError> {
    let (fid, sid) = (parse(&id)?, parse(&sid)?);
    let mut filter = q.filter;
    if filter.limit == 0 {
        filter.limit = 200;
    }
    if q.source_kind.as_deref() == Some("provider") {
        let fleet = s.fleets.get(fid).await?;
        let raw = s.fleets.provider_logs(&fleet, sid, filter.limit).await?;
        return Ok(Json(json!({ "raw": raw, "supported": raw.is_some() })));
    }
    let lines = s.fleets.logs(fid, sid, &filter).await?;
    let next = lines.last().map(|l| l.at.unix_timestamp());
    Ok(Json(json!({ "lines": lines, "next_before": next })))
}

#[derive(Deserialize)]
struct SdkLogLine {
    #[serde(default)]
    at: Option<i64>,
    #[serde(default)]
    level: Option<String>,
    message: String,
}

#[derive(Deserialize)]
struct SdkLogsBody {
    lines: Vec<SdkLogLine>,
}

/// `POST /v1/gameserver/logs {lines: [{at?, level?, message}]}` — a game server pushes its own
/// log lines (any provider, including ones musubi does not run the process for).
async fn sdk_logs(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(b): Json<SdkLogsBody>,
) -> Result<Json<Value>, ApiError> {
    let sid = self_id(&headers)?;
    if b.lines.len() > 1000 {
        return Err(Error::InvalidArgument("at most 1000 lines per call".into()).into());
    }
    let gs = s.fleets.server(sid).await?;
    let sink = s.fleets.log_sink();
    let n = b.lines.len();
    for l in b.lines {
        let mut message = l.message;
        if message.len() > musubi_fleet::LOG_LINE_MAX {
            let mut cut = musubi_fleet::LOG_LINE_MAX;
            while !message.is_char_boundary(cut) {
                cut -= 1;
            }
            message.truncate(cut);
        }
        let level = match l.level.as_deref() {
            Some(l @ ("debug" | "info" | "warn" | "error")) => l.to_string(),
            Some(_) | None => musubi_fleet::level_of(&message, "sdk").to_string(),
        };
        let at =
            l.at.and_then(|t| time::OffsetDateTime::from_unix_timestamp(t).ok())
                .unwrap_or_else(time::OffsetDateTime::now_utc);
        let _ = sink.send(musubi_fleet::LogLine {
            server_id: sid,
            fleet_id: gs.fleet_id,
            at,
            source: "sdk".into(),
            level,
            message,
        });
    }
    Ok(Json(json!({ "accepted": n })))
}

/// `GET /v1/server/fleets/{id}/rollout` — latest rollout with live servers per build and progress.
async fn rollout_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.rollout_status(parse(&id)?).await?))
}

async fn rollouts(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "rollouts": s.fleets.rollouts(parse(&id)?, 50).await? }),
    ))
}

async fn rollout_pause(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<musubi_fleet::Rollout>, ApiError> {
    Ok(Json(s.fleets.pause_rollout(parse(&id)?).await?))
}

async fn rollout_resume(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<musubi_fleet::Rollout>, ApiError> {
    Ok(Json(s.fleets.resume_rollout(parse(&id)?).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RollbackReq {
    reason: String,
}

/// `POST /v1/server/fleets/{id}/rollout/rollback {reason?}` — abort and promote the previous build.
async fn rollout_rollback(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    body: Option<Json<RollbackReq>>,
) -> Result<Json<musubi_fleet::Rollout>, ApiError> {
    let reason = body.map(|b| b.0.reason).unwrap_or_default();
    let reason = if reason.is_empty() {
        "operator"
    } else {
        reason.as_str()
    };
    Ok(Json(s.fleets.rollback_rollout(parse(&id)?, reason).await?))
}

/// `GET /v1/server/fleets/{id}/events?code=server.terminated&server_id&after&limit` — lifecycle log.
async fn fleet_events(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<musubi_fleet::EventQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "events": s.fleets.events(parse(&id)?, &q).await? }),
    ))
}

/// Event-bus kind of a fleet log code: `server.*` → `gameserver.*`, else `fleet.<a>_<b>`.
pub fn event_kind(code: &str) -> String {
    match code.strip_prefix("server.") {
        Some(rest) => format!("gameserver.{rest}"),
        None => format!("fleet.{}", code.replace('.', "_")),
    }
}

/// Forward undispatched fleet log rows to the event bus (webhooks); the lease holder runs it.
pub async fn dispatch_fleet_events(s: &AppState) -> musubi_core::Result<usize> {
    let evs = s.fleets.undispatched_events(200).await?;
    if evs.is_empty() {
        return Ok(0);
    }
    for e in &evs {
        let mut payload = e.data.clone();
        if let Value::Object(m) = &mut payload {
            m.insert("fleet_id".into(), json!(e.fleet_id));
            m.insert("server_id".into(), json!(e.server_id));
            m.insert("message".into(), json!(e.message));
            m.insert("code".into(), json!(e.code));
        }
        s.events.emit(&event_kind(&e.code), None, payload).await;
    }
    let ids: Vec<i64> = evs.iter().map(|e| e.id).collect();
    s.fleets.mark_dispatched(&ids).await?;
    Ok(ids.len())
}

type CounterActions = std::collections::HashMap<String, musubi_fleet::CounterAction>;
type ListActions = std::collections::HashMap<String, musubi_fleet::ListAction>;

/// `POST /v1/gameserver/counters {name: {delta?, count?, capacity?}}` — the server reports its
/// counters (rooms, bots, …); over capacity → 429, below zero → 400.
async fn sdk_counters(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(a): Json<CounterActions>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(
        s.fleets.update_counters(self_id(&headers)?, &a).await?,
    ))
}

/// `POST /v1/gameserver/lists {name: {add?, remove?, values?, capacity?}}`.
async fn sdk_lists(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(a): Json<ListActions>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.update_lists(self_id(&headers)?, &a).await?))
}

async fn op_counters(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(a): Json<CounterActions>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.update_counters(parse(&id)?, &a).await?))
}

async fn op_lists(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(a): Json<ListActions>,
) -> Result<Json<GameServer>, ApiError> {
    Ok(Json(s.fleets.update_lists(parse(&id)?, &a).await?))
}

/// `GET /v1/server/fleets/{id}/counters` — aggregate counters/lists over live servers and the
/// `counter_target` shortfall.
async fn fleet_counters(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.fleet_counters(parse(&id)?).await?))
}

/// `GET /v1/server/gameservers?fleet_id|fleet|pool|region|provider&state=ready,allocated|all&build_version
/// &exit_reason&node_id&labels=k=v,k2=v2&min_age_secs&max_age_secs&min_players&free_slots&counter
/// &counter_min_available&list&list_contains&order&limit&offset` — cross-fleet inventory.
async fn inventory(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<musubi_fleet::ServerSearch>,
) -> Result<Json<Value>, ApiError> {
    let (servers, total) = s.fleets.search_servers(&q).await?;
    Ok(Json(json!({
        "servers": servers, "total": total,
        "limit": q.limit.unwrap_or(100).clamp(1, 1000), "offset": q.offset.unwrap_or(0).max(0),
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CountsQuery {
    group: Option<String>,
    #[serde(flatten)]
    search: musubi_fleet::ServerSearch,
}

/// `GET /v1/server/gameservers/counts?group=state|region|fleet|pool|provider|build|exit_reason|node&<filters>`.
async fn inventory_counts(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<CountsQuery>,
) -> Result<Json<Value>, ApiError> {
    let group = q.group.as_deref().unwrap_or("state");
    let rows = s.fleets.server_counts(&q.search, group).await?;
    let total: i64 = rows.iter().map(|r| r.1).sum();
    Ok(Json(json!({
        "group": group,
        "counts": rows.iter().map(|(k, n)| json!({ "key": k, "servers": n })).collect::<Vec<_>>(),
        "total": total,
    })))
}

#[derive(Deserialize)]
struct SdkEvent {
    kind: String,
    #[serde(default)]
    data: Value,
}

/// `POST /v1/gameserver/events {kind, data}` — a server-to-server callback: the game server's own
/// event (round ended, boss spawned, …) goes to the event bus as `gameserver.custom.<kind>`
/// (webhooks filter with `gameserver.custom.*`) and into the fleet's lifecycle log.
async fn sdk_event(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    headers: HeaderMap,
    Json(e): Json<SdkEvent>,
) -> Result<Json<Value>, ApiError> {
    let sid = self_id(&headers)?;
    if e.kind.is_empty()
        || e.kind.len() > 64
        || !e
            .kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
    {
        return Err(Error::InvalidArgument("kind must be 1..=64 of [a-z0-9_.]".into()).into());
    }
    if e.data.to_string().len() > 16 * 1024 {
        return Err(Error::InvalidArgument("data must be <= 16 KB".into()).into());
    }
    let gs = s.fleets.server(sid).await?;
    let kind = format!("gameserver.custom.{}", e.kind);
    let id = s
        .events
        .emit(
            &kind,
            None,
            json!({ "fleet_id": gs.fleet_id, "server_id": sid, "build_version": gs.build_version, "data": e.data }),
        )
        .await;
    s.fleets
        .record_event(
            gs.fleet_id,
            Some(sid),
            &format!("custom.{}", e.kind),
            format!("server event {}", e.kind),
            e.data,
        )
        .await;
    Ok(Json(json!({ "ok": true, "kind": kind, "event_id": id })))
}
