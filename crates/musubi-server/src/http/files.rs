//! Title / global files (P1-65): versioned binaries (patches, localization tables, asset
//! bundles, config blobs) under `/v1/files/{path}` with sha256 ETags, a checksum manifest
//! filtered by platform/version selector, public (CDN-able) vs private (token) files, blobs in
//! `[ugc.s3]` object storage or Postgres.

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;

use crate::app::AppState;
use crate::http::remote_config::{ctx_from, etag_json, manifest_checksum, selector_matches, Ctx};
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/files/manifest", get(manifest))
        .route("/v1/files/{*path}", get(download))
        .route("/v1/server/files", get(list_server))
        .route(
            "/v1/server/files/{*path}",
            put(upload).patch(patch_meta).delete(delete),
        )
        .layer(DefaultBodyLimit::disable())
}

fn check_path(p: &str) -> Result<(), ApiError> {
    let bad = |m: &str| -> Result<(), ApiError> {
        Err(Error::InvalidArgument(format!("file path: {m}")).into())
    };
    if p.is_empty() || p.len() > 512 {
        return bad("1..512 chars");
    }
    let segs: Vec<&str> = p.split('/').collect();
    if segs.len() > 32 {
        return bad("at most 32 segments");
    }
    for s in &segs {
        if s.is_empty() || *s == "." || *s == ".." {
            return bad("empty, `.` or `..` segment");
        }
        if !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '@' | '+'))
        {
            return bad("segments are [A-Za-z0-9_.@+-]");
        }
    }
    if p == "manifest" {
        return bad("`manifest` is reserved");
    }
    Ok(())
}

type FileRow = (
    String,
    i64,
    String,
    String,
    bool,
    Value,
    Value,
    i32,
    i64,
    Option<String>,
);

const FILE_COLS: &str = "path, size, sha256, mime, public, selector, metadata, version, \
                         extract(epoch from updated_at)::bigint, blob_key";

fn url_for(s: &AppState, path: &str, public: bool) -> String {
    let base = s.cfg.files.public_base_url.trim_end_matches('/');
    if public && !base.is_empty() {
        format!("{base}/{path}")
    } else {
        format!("/v1/files/{path}")
    }
}

fn file_doc(s: &AppState, r: &FileRow) -> Value {
    json!({ "path": r.0, "size": r.1, "sha256": r.2, "mime": r.3, "public": r.4, "selector": r.5,
        "metadata": r.6, "version": r.7, "updated_at": r.8, "url": url_for(s, &r.0, r.4),
        "storage": if r.9.is_some() { "s3" } else { "db" } })
}

#[derive(Deserialize)]
struct UploadQ {
    #[serde(default = "yes")]
    public: bool,
    platforms: Option<String>,
    min_version: Option<String>,
    max_version: Option<String>,
    environment: Option<String>,
    countries: Option<String>,
    metadata: Option<String>,
}

fn yes() -> bool {
    true
}

fn csv_list(s: &Option<String>) -> Option<Value> {
    s.as_ref().map(|v| {
        json!(v
            .split(',')
            .map(|x| x.trim().to_ascii_lowercase())
            .filter(|x| !x.is_empty())
            .collect::<Vec<_>>())
    })
}

fn selector_from(q: &UploadQ) -> Value {
    let mut sel = serde_json::Map::new();
    if let Some(v) = csv_list(&q.platforms) {
        sel.insert("platforms".into(), v);
    }
    if let Some(v) = csv_list(&q.environment) {
        sel.insert("environment".into(), v);
    }
    if let Some(v) = csv_list(&q.countries) {
        sel.insert("countries".into(), v);
    }
    if let Some(v) = &q.min_version {
        sel.insert("min_version".into(), json!(v));
    }
    if let Some(v) = &q.max_version {
        sel.insert("max_version".into(), json!(v));
    }
    Value::Object(sel)
}

/// `PUT /v1/server/files/{path}` with the raw body; `content-type` is kept and served back;
/// `?public=false&platforms=ios,android&min_version=1.2&max_version=1.9&environment=prod`.
async fn upload(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(path): Path<String>,
    Query(q): Query<UploadQ>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    check_path(&path)?;
    let max = s.cfg.files.max_bytes.max(1) as usize;
    let bytes = axum::body::to_bytes(body, max)
        .await
        .map_err(|_| Error::InvalidArgument(format!("file larger than {max} bytes")))?;
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|m| !m.is_empty() && m.len() <= 128)
        .unwrap_or("application/octet-stream")
        .to_string();
    let metadata: Value = match &q.metadata {
        Some(m) => {
            serde_json::from_str(m).map_err(|e| Error::InvalidArgument(format!("metadata: {e}")))?
        }
        None => json!({}),
    };
    let selector = selector_from(&q);
    let sha = hex::encode(sha2::Sha256::digest(&bytes));
    let size = bytes.len() as i64;
    let (blob, key): (Option<Vec<u8>>, Option<String>) = match &s.blobs {
        Some(store) => {
            let key = format!("files/{path}");
            store.put(&key, bytes.to_vec(), &mime).await?;
            (None, Some(key))
        }
        None => (Some(bytes.to_vec()), None),
    };
    let row: FileRow = sqlx::query_as(&format!(
        "insert into musubi.files (path, size, sha256, mime, blob, blob_key, public, selector, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (path) do update set size = excluded.size, sha256 = excluded.sha256,
             mime = excluded.mime, blob = excluded.blob, blob_key = excluded.blob_key,
             public = excluded.public, selector = excluded.selector,
             metadata = musubi.files.metadata || excluded.metadata,
             version = musubi.files.version + 1, updated_at = now()
         returning {FILE_COLS}"
    ))
    .bind(&path)
    .bind(size)
    .bind(&sha)
    .bind(&mime)
    .bind(&blob)
    .bind(&key)
    .bind(q.public)
    .bind(&selector)
    .bind(&metadata)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "file.updated",
            None,
            json!({ "path": path, "sha256": sha, "size": size, "version": row.7, "public": q.public }),
        )
        .await;
    Ok(Json(file_doc(&s, &row)))
}

#[derive(Deserialize)]
struct MetaBody {
    public: Option<bool>,
    selector: Option<Value>,
    metadata: Option<Value>,
}

async fn patch_meta(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(path): Path<String>,
    Json(b): Json<MetaBody>,
) -> Result<Json<Value>, ApiError> {
    if let Some(sel) = &b.selector {
        if !sel.is_object() {
            return Err(Error::InvalidArgument("selector must be an object".into()).into());
        }
    }
    let row: Option<FileRow> = sqlx::query_as(&format!(
        "update musubi.files set public = coalesce($2, public), selector = coalesce($3, selector),
             metadata = coalesce($4, metadata), updated_at = now()
         where path = $1 returning {FILE_COLS}"
    ))
    .bind(&path)
    .bind(b.public)
    .bind(&b.selector)
    .bind(&b.metadata)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match row {
        Some(r) => Ok(Json(file_doc(&s, &r))),
        None => Err(Error::NotFound(format!("file `{path}`")).into()),
    }
}

async fn delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(path): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("delete from musubi.files where path = $1 returning blob_key")
            .bind(&path)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    if let Some((Some(key),)) = &row {
        if let Some(store) = &s.blobs {
            let _ = store.delete(key).await;
        }
    }
    if row.is_some() {
        s.events
            .emit("file.deleted", None, json!({ "path": path }))
            .await;
    }
    Ok(Json(json!({ "deleted": row.is_some() })))
}

#[derive(Deserialize)]
struct ListQ {
    prefix: Option<String>,
}

async fn list_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<FileRow> = sqlx::query_as(&format!(
        "select {FILE_COLS} from musubi.files where path like $1 || '%' order by path"
    ))
    .bind(q.prefix.unwrap_or_default())
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "files": rows.iter().map(|r| file_doc(&s, r)).collect::<Vec<_>>() }),
    ))
}

/// A bearer token when present (verified), none when the request is anonymous.
fn maybe_account(s: &AppState, headers: &HeaderMap) -> Result<bool, ApiError> {
    let Some(h) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return Ok(false);
    };
    let token = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
        .ok_or_else(|| Error::Unauthenticated("malformed authorization header".into()))?;
    s.auth.verify_access(token)?;
    Ok(true)
}

/// `GET /v1/files/manifest?prefix&platform&version` — public files for anyone, private too
/// with a token; entries filtered by their selector; `ETag` = checksum of (path, sha256) pairs.
async fn manifest(
    State(s): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Response, ApiError> {
    let authed = maybe_account(&s, &headers)?;
    let prefix = q.prefix.clone().unwrap_or_default();
    let ctx = ctx_from(q, &headers);
    let rows: Vec<FileRow> = sqlx::query_as(&format!(
        "select {FILE_COLS} from musubi.files where path like $1 || '%' and (public or $2) order by path"
    ))
    .bind(&prefix)
    .bind(authed)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let env = s.cfg.node.environment.as_str();
    let rows: Vec<&FileRow> = rows
        .iter()
        .filter(|r| selector_matches(&r.5, env, &ctx, None))
        .collect();
    let items: Vec<(String, String)> = rows.iter().map(|r| (r.0.clone(), r.2.clone())).collect();
    let sum = manifest_checksum(&items);
    let files: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({ "path": r.0, "size": r.1, "sha256": r.2, "mime": r.3, "version": r.7,
                "updated_at": r.8, "public": r.4, "url": url_for(&s, &r.0, r.4), "metadata": r.6 })
        })
        .collect();
    Ok(etag_json(
        &headers,
        &sum,
        json!({ "files": files, "checksum": sum, "total_bytes": rows.iter().map(|r| r.1).sum::<i64>() }),
    ))
}

type DlRow = (
    i64,
    String,
    String,
    bool,
    Option<Vec<u8>>,
    Option<String>,
    i32,
);

/// Streams a file; `If-None-Match` against the sha256 ETag answers 304. Private files need a
/// token (401 without one, 404 never leaks existence to anonymous callers).
async fn download(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Result<Response, ApiError> {
    let row: Option<DlRow> =
        sqlx::query_as(
            "select size, sha256, mime, public, blob, blob_key, version from musubi.files where path = $1",
        )
        .bind(&path)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    let authed = maybe_account(&s, &headers)?;
    let Some((size, sha, mime, public, blob, key, version)) = row else {
        return Err(Error::NotFound(format!("file `{path}`")).into());
    };
    if !public && !authed {
        return Err(Error::Unauthenticated("private file: bearer token required".into()).into());
    }
    let etag = format!("\"{sha}\"");
    let cache = if public {
        "public, max-age=300"
    } else {
        "private, no-store"
    };
    let inm = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
        .unwrap_or(false);
    if inm {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag),
                (header::CACHE_CONTROL, cache.to_string()),
            ],
        )
            .into_response());
    }
    let body = match (blob, key, &s.blobs) {
        (Some(b), _, _) => Body::from(b),
        (None, Some(key), Some(store)) => {
            let (_, stream) = store.get_stream(&key).await?;
            Body::from_stream(stream)
        }
        (None, Some(_), None) => {
            return Err(Error::Unavailable(
                "file blob is in object storage but [ugc.s3] is not configured on this node".into(),
            )
            .into())
        }
        (None, None, _) => return Err(Error::NotFound(format!("file `{path}` has no blob")).into()),
    };
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache.to_string()),
            (
                header::HeaderName::from_static("x-musubi-file-version"),
                version.to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_strict() {
        assert!(check_path("patches/1.2/win64.bin").is_ok());
        assert!(check_path("loc/en-US.csv").is_ok());
        assert!(check_path("../etc/passwd").is_err());
        assert!(check_path("a//b").is_err());
        assert!(check_path("manifest").is_err());
        assert!(check_path("sp ace").is_err());
        assert!(check_path("").is_err());
    }
}
