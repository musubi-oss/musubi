//! Host pool behind the `ssh` fleet provider and its capacity pools (P1-11): register / drain /
//! probe / remove bare-metal hosts, relay spot termination notices, inspect node-level capacity.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_fleet::hosts::{HostFilter, NewHost, PatchHost};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/fleets/hosts", get(list).post(register))
        .route("/v1/server/fleets/hosts/classes", get(classes))
        .route("/v1/server/fleets/hosts/notice", post(notice_by_addr))
        .route(
            "/v1/server/fleets/hosts/{id}",
            get(get_one).patch(patch).delete(delete),
        )
        .route("/v1/server/fleets/hosts/{id}/drain", post(drain))
        .route("/v1/server/fleets/hosts/{id}/undrain", post(undrain))
        .route("/v1/server/fleets/hosts/{id}/probe", post(probe))
        .route(
            "/v1/server/fleets/hosts/{id}/termination-notice",
            post(notice),
        )
        .route("/v1/server/fleets/capacity", get(capacity))
        .route("/v1/server/fleets/capacity/reconcile", post(reconcile))
}

fn parse(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id)
        .map_err(|_| musubi_core::Error::InvalidArgument("bad host id".into()).into())
}

fn hosts(s: &AppState) -> Result<std::sync::Arc<musubi_fleet::hosts::HostRegistry>, ApiError> {
    s.fleets.capacity().map(|c| c.hosts.clone()).ok_or_else(|| {
        musubi_core::Error::InvalidArgument(
            "host pool not enabled: configure [fleet.ssh] hosts or [fleet.capacity] pools".into(),
        )
        .into()
    })
}

/// `GET /v1/server/fleets/hosts?state&region&class&pool&source&include_gone` — hosts with
/// current occupancy (`used`, `free`).
async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(f): Query<HostFilter>,
) -> Result<Json<Value>, ApiError> {
    let reg = hosts(&s)?;
    reg.refresh().await?;
    let used = reg.used().await?;
    let items: Vec<Value> = reg
        .list(&f)
        .await?
        .into_iter()
        .map(|h| {
            let u = used.get(&h.id).copied().unwrap_or(0);
            let mut v = json!(h);
            v["used"] = json!(u);
            v["free"] = json!((h.capacity as i64 - u).max(0));
            v
        })
        .collect();
    Ok(Json(json!({ "hosts": items, "count": items.len() })))
}

/// `POST /v1/server/fleets/hosts {addr, name?, public_addr?, user?, port?, key?, capacity?,
/// region?, class?, labels?, spot?, cost_per_hour?, state?: ready|provisioning}`.
async fn register(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(mut n): Json<NewHost>,
) -> Result<Json<Value>, ApiError> {
    n.source = Some("manual".into());
    n.pool = n.pool.filter(|p| !p.is_empty());
    if let Some(p) = &n.pool {
        let cap = s.fleets.capacity();
        if cap.as_ref().and_then(|c| c.pool(p)).is_none() {
            return Err(musubi_core::Error::InvalidArgument(format!("unknown pool {p}")).into());
        }
    }
    n.external_id = None;
    Ok(Json(json!(s.fleets.register_host(n).await?)))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let reg = hosts(&s)?;
    let h = reg.get(id).await?;
    let servers = s.fleets.host_servers(id).await?;
    let mut v = json!(h);
    v["used"] = json!(servers.len());
    v["free"] = json!((h.capacity as i64 - servers.len() as i64).max(0));
    v["servers"] = json!(servers
        .iter()
        .map(|g| json!({ "id": g.id, "fleet_id": g.fleet_id, "state": g.state, "players": g.players }))
        .collect::<Vec<_>>());
    Ok(Json(v))
}

async fn patch(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(p): Json<PatchHost>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    Ok(Json(json!(hosts(&s)?.patch(id, p).await?)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DeleteQ {
    force: bool,
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<DeleteQ>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let id = parse(&id)?;
    let v = s.fleets.delete_host(id, q.force).await?;
    Ok((StatusCode::OK, Json(v)))
}

async fn drain(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(s.fleets.drain_host(parse(&id)?, true).await?)))
}

async fn undrain(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!(s.fleets.drain_host(parse(&id)?, false).await?)))
}

async fn probe(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.probe_host(parse(&id)?).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NoticeBody {
    /// Seconds until the machine disappears (AWS spot: 120, GCP: 30, Azure: 30). Default 120.
    deadline_secs: Option<u64>,
    reason: Option<String>,
    /// `POST …/hosts/notice`: host by ssh address or cloud id (what an on-host watcher knows).
    addr: Option<String>,
}

/// `POST /v1/server/fleets/hosts/{id}/termination-notice {deadline_secs?, reason?}` — spot /
/// preemption: idle servers stop now, busy ones get `server.preempted` + the deadline.
async fn notice(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<NoticeBody>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    Ok(Json(
        s.fleets
            .preempt_host(
                id,
                b.deadline_secs.unwrap_or(120),
                b.reason.as_deref().unwrap_or("spot_termination"),
            )
            .await?,
    ))
}

/// `POST /v1/server/fleets/hosts/notice {addr, deadline_secs?, reason?}` — same, keyed by the
/// host's address / external id (for a watcher script on the machine reading the cloud
/// metadata endpoint).
async fn notice_by_addr(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<NoticeBody>,
) -> Result<Json<Value>, ApiError> {
    let addr = b
        .addr
        .as_deref()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| musubi_core::Error::InvalidArgument("addr required".into()))?;
    let reg = hosts(&s)?;
    reg.refresh().await?;
    let h = reg
        .by_addr(addr)
        .ok_or_else(|| musubi_core::Error::NotFound(format!("host {addr}")))?;
    Ok(Json(
        s.fleets
            .preempt_host(
                h.id,
                b.deadline_secs.unwrap_or(120),
                b.reason.as_deref().unwrap_or("spot_termination"),
            )
            .await?,
    ))
}

/// `GET /v1/server/fleets/hosts/classes` — hardware classes: hosts, slots, cost per hour.
async fn classes(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    let v = s.fleets.capacity_summary().await?;
    Ok(Json(json!({ "classes": v["classes"] })))
}

/// `GET /v1/server/fleets/capacity` — per pool: hosts by state, slots total/used/free,
/// starved starts, last scale action, cost; plus unpooled hosts and the whole picture.
async fn capacity(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.capacity_summary().await?))
}

/// `POST /v1/server/fleets/capacity/reconcile` — run one capacity pass now (the reconciler
/// runs it after every fleet pass).
async fn reconcile(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.fleets.capacity_pass().await?))
}
