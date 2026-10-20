//! Build artifacts (GameLift "Build resources"): versioned server binaries/zips stored next to the
//! fleet definitions instead of a hand-managed bucket. A fleet or build references one as
//! `spec.artifact = "name@version"`; on save it is resolved into `MUSUBI_ARTIFACT_URL` (capability
//! URL with the artifact's token), `MUSUBI_ARTIFACT_SHA256`, `MUSUBI_ARTIFACT_FILENAME` and
//! `MUSUBI_ARTIFACT_SIZE`, so any provider's launcher can fetch and verify it. Also the fleet cost
//! attribute (`spec.cost`) with a live spend estimate.

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{Error, Result};
use musubi_fleet::FleetSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

const MAX_DB_BYTES: usize = 256 << 20;
const MAX_BYTES: usize = 4 << 30;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/builds", get(list))
        .route(
            "/v1/server/builds/{name}/{version}",
            get(get_one).put(upload).delete(delete),
        )
        .route("/v1/server/builds/{name}/{version}/download", get(download))
        .route("/v1/server/fleets/{id}/cost", get(fleet_cost))
        .layer(DefaultBodyLimit::max(MAX_BYTES))
}

type Row = (
    String,
    String,
    String,
    i64,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    Value,
    OffsetDateTime,
);
const COLS: &str =
    "name, version, filename, size, sha256, mime, blob_key, token, notes, metadata, created_at";

fn doc(s: &AppState, r: &Row) -> Value {
    json!({
        "name": r.0, "version": r.1, "filename": r.2, "size": r.3, "sha256": r.4, "mime": r.5,
        "storage": if r.6.is_some() { "s3" } else { "db" }, "token": r.7, "notes": r.8,
        "metadata": r.9, "created_at": r.10.unix_timestamp(),
        "download_url": download_url(s, &r.0, &r.1, &r.7),
    })
}

fn download_url(s: &AppState, name: &str, version: &str, token: &str) -> String {
    format!(
        "{}/v1/server/builds/{name}/{version}/download?token={token}",
        s.fleets.api_url().trim_end_matches('/')
    )
}

fn check_ident(kind: &str, v: &str) -> Result<()> {
    if v.is_empty()
        || v.len() > 64
        || !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        || v.starts_with('.')
    {
        return Err(Error::InvalidArgument(format!(
            "{kind} must be [A-Za-z0-9_.-]{{1,64}} and not start with `.`"
        )));
    }
    Ok(())
}

async fn load(s: &AppState, name: &str, version: &str) -> Result<Row> {
    sqlx::query_as(&format!(
        "select {COLS} from musubi.build_artifacts where name = $1 and version = $2"
    ))
    .bind(name)
    .bind(version)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("build artifact {name}@{version}")))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    name: Option<String>,
}

/// `GET /v1/server/builds?name` — artifacts, newest first.
async fn list(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.build_artifacts where ($1::text is null or name = $1)
         order by created_at desc limit 500"
    ))
    .bind(&q.name)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let artifacts: Vec<Value> = rows.iter().map(|r| doc(&s, r)).collect();
    Ok(Json(json!({ "artifacts": artifacts })))
}

async fn get_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(doc(&s, &load(&s, &name, &version).await?)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UploadQ {
    filename: Option<String>,
    notes: Option<String>,
    metadata: Option<String>,
}

/// `PUT /v1/server/builds/{name}/{version}?filename&notes&metadata` — raw body is the artifact
/// (zip, tar.gz, binary). Goes to `[ugc.s3]` when configured, else into Postgres (≤ 256 MiB).
/// Re-uploading a version replaces the bytes and rotates the download token.
async fn upload(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version)): Path<(String, String)>,
    Query(q): Query<UploadQ>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    check_ident("build name", &name)?;
    check_ident("build version", &version)?;
    if body.is_empty() {
        return Err(Error::InvalidArgument("artifact body is empty".into()).into());
    }
    let filename = q
        .filename
        .filter(|f| !f.is_empty() && f.len() <= 128 && !f.contains('/') && !f.contains('\\'))
        .unwrap_or_else(|| format!("{name}-{version}.bin"));
    if q.notes.as_ref().is_some_and(|n| n.len() > 500) {
        return Err(Error::InvalidArgument("notes exceed 500 chars".into()).into());
    }
    let metadata: Value = match &q.metadata {
        Some(m) => serde_json::from_str(m)
            .ok()
            .filter(Value::is_object)
            .ok_or_else(|| Error::InvalidArgument("metadata must be a JSON object".into()))?,
        None => json!({}),
    };
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|m| !m.is_empty() && m.len() <= 128)
        .unwrap_or("application/octet-stream")
        .to_string();
    let sha = hex::encode(sha2::Sha256::digest(&body));
    let size = body.len() as i64;
    let (blob, key): (Option<Vec<u8>>, Option<String>) = match &s.blobs {
        Some(store) => {
            let key = format!("builds/{name}/{version}/{filename}");
            store.put(&key, body.to_vec(), &mime).await?;
            (None, Some(key))
        }
        None => {
            if body.len() > MAX_DB_BYTES {
                return Err(Error::InvalidArgument(
                    "artifacts over 256 MiB need [ugc.s3] object storage".into(),
                )
                .into());
            }
            (Some(body.to_vec()), None)
        }
    };
    let token = hex::encode(rand::random::<[u8; 24]>());
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.build_artifacts (name, version, filename, size, sha256, mime, blob, blob_key, token, notes, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (name, version) do update set filename = excluded.filename, size = excluded.size,
             sha256 = excluded.sha256, mime = excluded.mime, blob = excluded.blob, blob_key = excluded.blob_key,
             token = excluded.token, notes = coalesce(excluded.notes, build_artifacts.notes),
             metadata = build_artifacts.metadata || excluded.metadata, created_at = now()
         returning {COLS}"
    ))
    .bind(&name)
    .bind(&version)
    .bind(&filename)
    .bind(size)
    .bind(&sha)
    .bind(&mime)
    .bind(&blob)
    .bind(&key)
    .bind(&token)
    .bind(&q.notes)
    .bind(&metadata)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "build.artifact_uploaded",
            None,
            json!({ "name": name, "version": version, "size": size, "sha256": sha }),
        )
        .await;
    Ok(Json(doc(&s, &row)))
}

async fn delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s, &name, &version).await?;
    if let (Some(key), Some(store)) = (&r.6, &s.blobs) {
        let _ = store.delete(key).await;
    }
    sqlx::query("delete from musubi.build_artifacts where name = $1 and version = $2")
        .bind(&name)
        .bind(&version)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.events
        .emit(
            "build.artifact_deleted",
            None,
            json!({ "name": name, "version": version }),
        )
        .await;
    Ok(Json(
        json!({ "name": name, "version": version, "removed": true }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TokenQ {
    token: Option<String>,
}

/// `GET /v1/server/builds/{name}/{version}/download?token=` — capability URL for launchers: the
/// token from the artifact record replaces the server key (hosts never hold it).
async fn download(
    State(s): State<AppState>,
    Path((name, version)): Path<(String, String)>,
    Query(q): Query<TokenQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let r = load(&s, &name, &version).await?;
    let by_token = q.token.as_deref() == Some(r.7.as_str());
    let by_key = headers
        .get("x-musubi-server-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|k| s.fleets.server_key_matches(k));
    if !by_token && !by_key {
        return Err(Error::Unauthenticated("artifact token required".into()).into());
    }
    let blob: Option<Vec<u8>> = sqlx::query_scalar(
        "select blob from musubi.build_artifacts where name = $1 and version = $2",
    )
    .bind(&name)
    .bind(&version)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let body = match (blob, &r.6, &s.blobs) {
        (Some(b), _, _) => Body::from(b),
        (None, Some(key), Some(store)) => {
            let (_, stream) = store.get_stream(key).await?;
            Body::from_stream(stream)
        }
        _ => {
            return Err(Error::Unavailable(
                "artifact is in object storage but [ugc.s3] is not configured on this node".into(),
            )
            .into())
        }
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, &r.5)
        .header(header::CONTENT_LENGTH, r.3)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", r.2),
        )
        .header("x-artifact-sha256", &r.4)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .body(body)
        .unwrap())
}

/// Resolve `spec.artifact` into launcher env and validate `spec.cost`; called when a fleet or a
/// build is created.
pub async fn prepare_spec(s: &AppState, spec: &mut FleetSpec) -> Result<()> {
    if let Some(c) = &spec.cost {
        if !c.hourly_per_server.is_finite() || c.hourly_per_server < 0.0 {
            return Err(Error::InvalidArgument(
                "spec.cost.hourly_per_server must be a non-negative number".into(),
            ));
        }
        if c.currency.len() != 3 || !c.currency.chars().all(|ch| ch.is_ascii_alphabetic()) {
            return Err(Error::InvalidArgument(
                "spec.cost.currency must be a 3-letter code".into(),
            ));
        }
    }
    if spec.verify_image {
        let Some(image) = spec.image.clone() else {
            return Err(Error::InvalidArgument(
                "spec.verify_image needs spec.image".into(),
            ));
        };
        let auth = spec.secret_env.get("REGISTRY_AUTH").cloned();
        let digest =
            crate::registry::verify_image(s.logins.http(), &image, auth.as_deref()).await?;
        spec.image_digest = Some(digest);
    } else {
        spec.image_digest = None;
    }
    let Some(a) = spec.artifact.clone() else {
        return Ok(());
    };
    let Some((name, version)) = a.split_once('@') else {
        return Err(Error::InvalidArgument(
            "spec.artifact must be `name@version`".into(),
        ));
    };
    check_ident("artifact name", name)?;
    check_ident("artifact version", version)?;
    let r = load(s, name, version).await?;
    spec.env.insert(
        "MUSUBI_ARTIFACT_URL".into(),
        download_url(s, &r.0, &r.1, &r.7),
    );
    spec.env
        .insert("MUSUBI_ARTIFACT_SHA256".into(), r.4.clone());
    spec.env
        .insert("MUSUBI_ARTIFACT_FILENAME".into(), r.2.clone());
    spec.env
        .insert("MUSUBI_ARTIFACT_SIZE".into(), r.3.to_string());
    Ok(())
}

/// `GET /v1/server/fleets/{id}/cost` — spend estimate from `spec.cost` and the live server count.
async fn fleet_cost(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id = uuid::Uuid::parse_str(&id).map_err(|_| Error::InvalidArgument("fleet id".into()))?;
    let f = s.fleets.get(id).await?;
    let servers = s.fleets.servers(id, None).await?;
    let live = servers.iter().filter(|x| x.state != "terminated").count();
    let allocated = servers.iter().filter(|x| x.state == "allocated").count();
    let ready = servers.iter().filter(|x| x.state == "ready").count();
    let Some(c) = &f.spec.cost else {
        return Ok(Json(
            json!({ "fleet_id": id, "configured": false, "live": live }),
        ));
    };
    let hourly = c.hourly_per_server * live as f64;
    let idle_hourly = c.hourly_per_server * ready as f64;
    Ok(Json(json!({
        "fleet_id": id, "configured": true, "currency": c.currency,
        "hourly_per_server": c.hourly_per_server,
        "live": live, "allocated": allocated, "ready": ready,
        "hourly_now": hourly, "daily_now": hourly * 24.0, "monthly_now": hourly * 730.0,
        "idle_hourly_now": idle_hourly,
        "max_hourly": c.hourly_per_server * f.max_servers as f64,
        "max_monthly": c.hourly_per_server * f.max_servers as f64 * 730.0,
    })))
}
