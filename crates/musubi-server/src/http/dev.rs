//! Developer tools HTTP: request recording/replay and fixtures (admin key).

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/admin/dev/recording",
            get(recording_get).put(recording_put),
        )
        .route(
            "/v1/admin/dev/recordings",
            get(recordings).delete(recordings_clear),
        )
        .route("/v1/admin/dev/recordings/{id}", get(recording_one))
        .route("/v1/admin/dev/recordings/{id}/replay", post(replay))
        .route("/v1/admin/dev/fixtures", get(fixtures).post(fixture_apply))
        .route("/v1/admin/dev/fixtures/{name}", delete(fixture_drop))
        .route(
            "/v1/admin/dev/netsim",
            get(netsim_get).put(netsim_put).delete(netsim_clear),
        )
        .route("/v1/admin/dev/netsim/reset", post(netsim_reset))
        .route(
            "/v1/admin/dev/loadtests",
            get(loadtest_list).post(loadtest_start),
        )
        .route(
            "/v1/admin/dev/loadtests/{id}",
            get(loadtest_get).delete(loadtest_delete),
        )
        .route("/v1/admin/dev/loadtests/{id}/stop", post(loadtest_stop))
        .route(
            "/v1/admin/dev/loadtests/{id}/compare/{base}",
            get(loadtest_compare),
        )
        .route(
            "/v1/admin/dev/netsim/sessions/{id}",
            axum::routing::put(netsim_session_put).delete(netsim_session_delete),
        )
}

async fn recording_get(_k: ServerKey, State(s): State<AppState>) -> Json<Value> {
    Json(s.dev.settings())
}

#[derive(Deserialize)]
struct RecordingReq {
    enabled: bool,
    #[serde(default = "hundred")]
    sample_percent: u32,
    #[serde(default)]
    path_prefix: String,
}

fn hundred() -> u32 {
    100
}

/// Turn request recording on/off: `path_prefix` (default `/v1/`) and `sample_percent` pick what
/// is kept; `/v1/admin/dev/*` and replays are never recorded.
async fn recording_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<RecordingReq>,
) -> Json<Value> {
    s.dev.configure(r.enabled, r.sample_percent, &r.path_prefix);
    Json(s.dev.settings())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    path: Option<String>,
    #[serde(default = "fifty")]
    limit: i64,
}

fn fifty() -> i64 {
    50
}

async fn recordings(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "recordings": crate::dev::recordings(&s.db, q.path.as_deref(), q.limit).await? }),
    ))
}

async fn recordings_clear(
    _k: ServerKey,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.http_recordings")
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "deleted": n })))
}

async fn recording_one(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rec = crate::dev::recording(&s.db, id)
        .await?
        .ok_or_else(|| Error::NotFound(format!("recording {id}")))?;
    let parse = |t: &Option<String>| {
        t.as_deref()
            .and_then(|x| serde_json::from_str::<Value>(x).ok())
            .unwrap_or_else(|| json!(t))
    };
    Ok(Json(
        json!({ "id": id, "method": rec.method, "path": rec.path, "headers": rec.headers,
        "body": parse(&rec.body), "status": rec.status, "response": parse(&rec.response),
        "account_id": rec.account_id, "server_key": rec.server_key }),
    ))
}

/// Re-run a recorded request on this node and compare the status with the recorded one.
async fn replay(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rec = crate::dev::recording(&s.db, id)
        .await?
        .ok_or_else(|| Error::NotFound(format!("recording {id}")))?;
    Ok(Json(crate::dev::replay(&s, &rec).await?))
}

async fn fixtures(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "fixtures": crate::dev::fixtures(&s.db).await? }),
    ))
}

#[derive(Deserialize)]
struct FixtureReq {
    name: String,
    #[serde(default)]
    reset: bool,
    #[serde(flatten)]
    manifest: Value,
}

/// Apply a fixture `{name, accounts: [{username, display_name?, metadata?, wallet?: {cur: n},
/// inventory?: [{item_id, quantity}], storage?: [{collection, key, value}]}], reset?}`.
async fn fixture_apply(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<FixtureReq>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::dev::apply_fixture(&s, &r.name, &r.manifest, r.reset).await?,
    ))
}

async fn fixture_drop(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match crate::dev::drop_fixture(&s, &name).await? {
        Some(v) => Ok(Json(v)),
        None => Err(Error::NotFound(format!("fixture {name}")).into()),
    }
}

// ---------- P1-62 simulated network conditions ----------

use crate::realtime::netsim;

/// `GET /v1/admin/dev/netsim` — global profile, per-session overrides and counters.
async fn netsim_get(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut v = netsim::snapshot();
    v["config_enabled"] = json!(s.cfg.dev.netsim.enabled);
    Ok(Json(v))
}

/// `PUT /v1/admin/dev/netsim {latency_ms, jitter_ms, loss_pct, direction}` — apply to every
/// realtime connection on this node (new frames immediately, existing sessions included).
async fn netsim_put(
    _k: ServerKey,
    State(_s): State<AppState>,
    Json(p): Json<netsim::Params>,
) -> Result<Json<Value>, ApiError> {
    p.validate()?;
    netsim::set_global(Some(p));
    Ok(Json(netsim::snapshot()))
}

async fn netsim_clear(_k: ServerKey, State(_s): State<AppState>) -> Result<Json<Value>, ApiError> {
    netsim::set_global(None);
    let n = netsim::clear_sessions();
    let mut v = netsim::snapshot();
    v["cleared_sessions"] = json!(n);
    Ok(Json(v))
}

async fn netsim_reset(_k: ServerKey, State(_s): State<AppState>) -> Result<Json<Value>, ApiError> {
    netsim::reset_counters();
    Ok(Json(netsim::snapshot()))
}

/// `PUT /v1/admin/dev/netsim/sessions/{id}` — conditions for one live session (QA on a single
/// client while the rest of the playtest stays clean).
async fn netsim_session_put(
    _k: ServerKey,
    State(_s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(p): Json<netsim::Params>,
) -> Result<Json<Value>, ApiError> {
    p.validate()?;
    let replaced = netsim::set_session(musubi_core::SessionId(id), Some(p));
    let mut v = netsim::snapshot();
    v["replaced"] = json!(replaced);
    Ok(Json(v))
}

async fn netsim_session_delete(
    _k: ServerKey,
    State(_s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    if !netsim::set_session(musubi_core::SessionId(id), None) {
        return Err(Error::NotFound("netsim override for this session".into()).into());
    }
    Ok(Json(netsim::snapshot()))
}

async fn loadtest_start(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(req): Json<crate::loadtest::Request>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let v = crate::loadtest::start(&s, req).await?;
    Ok((StatusCode::ACCEPTED, Json(v)))
}

#[derive(Deserialize)]
struct LoadtestListQuery {
    state: Option<String>,
    #[serde(default = "default_lt_limit")]
    limit: i64,
}

fn default_lt_limit() -> i64 {
    50
}

async fn loadtest_list(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<LoadtestListQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::loadtest::list(&s, q.state.as_deref(), q.limit.clamp(1, 500)).await?,
    ))
}

async fn loadtest_get(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::loadtest::get(&s, id).await?))
}

async fn loadtest_stop(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::loadtest::stop(&s, id).await?))
}

async fn loadtest_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    crate::loadtest::delete(&s, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct CompareQuery {
    #[serde(default = "default_threshold")]
    threshold_pct: f64,
}

fn default_threshold() -> f64 {
    20.0
}

async fn loadtest_compare(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((id, base)): Path<(Uuid, Uuid)>,
    Query(q): Query<CompareQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::loadtest::compare(&s, id, base, q.threshold_pct).await?,
    ))
}
