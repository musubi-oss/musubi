//! Developer tools HTTP: request recording/replay and fixtures (admin key).

use axum::extract::{Path, Query, State};
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
