//! Per-player files: saves, screenshots, replays, custom maps — binary blobs keyed per account
//! with revisions (`player_keep_revisions`, promote any kept revision back to current), a
//! `purpose` tag, content tags and a public flag so other players can fetch them. Blobs live in
//! `[ugc.s3]` when configured, else in Postgres.
//!
//! Client: `GET /v1/account/files?purpose&tag`, `PUT|GET|PATCH|DELETE /v1/account/files/{key}`
//! (`GET ?meta=1` = document, `?revisions=1` = revision list, `?revision=n` = that revision).
//! Others' public files: `GET /v1/accounts/{id}/files`, `GET /v1/accounts/{id}/files/{key}`.
//! Server: the same under `/v1/server/accounts/{id}/files…`, private files included.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/account/files", get(list_mine))
        .route(
            "/v1/account/files/{*key}",
            get(get_mine)
                .put(put_mine)
                .patch(patch_mine)
                .delete(delete_mine),
        )
        .route("/v1/accounts/{id}/files", get(list_public))
        .route("/v1/accounts/{id}/files/{*key}", get(get_public))
        .route("/v1/server/accounts/{id}/files", get(list_server))
        .route(
            "/v1/server/accounts/{id}/files/{*key}",
            get(get_server)
                .put(put_server)
                .patch(patch_server)
                .delete(delete_server),
        )
}

const COLS: &str =
    "f.id, f.account_id, f.key, f.purpose, f.public, f.current_revision, f.latest_revision, \
    r.size, r.sha256, r.mime, f.metadata, f.tags, f.created_at, f.updated_at";
const FROM: &str = "from musubi.player_files f \
    join musubi.player_file_revisions r on r.file_id = f.id and r.revision = f.current_revision";

type FileRow = (
    Uuid,
    Uuid,
    String,
    String,
    bool,
    i32,
    i32,
    i64,
    String,
    String,
    Value,
    Vec<String>,
    OffsetDateTime,
    OffsetDateTime,
);

fn doc(s: &AppState, r: &FileRow) -> Value {
    let url = if r.4 {
        Some(format!("/v1/accounts/{}/files/{}", r.1, r.2))
    } else {
        None
    };
    let cdn = (r.4 && s.blobs.is_some() && !s.cfg.files.public_base_url.is_empty()).then(|| {
        format!(
            "{}/{}",
            s.cfg.files.public_base_url.trim_end_matches('/'),
            blob_key(r.1, r.0, r.5)
        )
    });
    json!({
        "id": r.0, "account_id": r.1, "key": r.2, "purpose": r.3, "public": r.4,
        "revision": r.5, "latest_revision": r.6, "size": r.7, "sha256": r.8, "mime": r.9,
        "metadata": r.10, "tags": r.11, "created_at": r.12, "updated_at": r.13,
        "url": url, "cdn_url": cdn, "storage": if s.blobs.is_some() { "s3" } else { "db" },
    })
}

fn blob_key(account: Uuid, file: Uuid, revision: i32) -> String {
    format!("player_files/{account}/{file}/{revision}")
}

fn check_key(key: &str) -> Result<()> {
    let ok = !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-/".contains(&b))
        && !key.starts_with('/')
        && !key.ends_with('/')
        && key
            .split('/')
            .all(|seg| !seg.is_empty() && seg != "." && seg != "..");
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "file key: 1..=128 of [A-Za-z0-9_.-/], segments separated by `/`, no `.`/`..`".into(),
        ))
    }
}

fn check_purpose(p: &str) -> Result<()> {
    if p.len() <= 32
        && p.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "purpose: up to 32 of [a-z0-9_-]".into(),
        ))
    }
}

fn parse_tags(raw: &[String]) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim().to_ascii_lowercase();
        if t.is_empty() {
            continue;
        }
        if t.len() > 32
            || !t
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.:-".contains(&b))
        {
            return Err(Error::InvalidArgument(format!(
                "tag `{t}`: 1..=32 of [a-z0-9_.:-]"
            )));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    if out.len() > 16 {
        return Err(Error::InvalidArgument("at most 16 tags".into()));
    }
    Ok(out)
}

fn account_path(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("account id".into()))
}

async fn fetch(s: &AppState, account: Uuid, key: &str) -> Result<Option<FileRow>> {
    sqlx::query_as(&format!(
        "select {COLS} {FROM} where f.account_id = $1 and f.key = $2"
    ))
    .bind(account)
    .bind(key)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    purpose: Option<String>,
    tag: Option<String>,
}

async fn list(s: &AppState, account: Uuid, q: &ListQ, public_only: bool) -> Result<Value> {
    let rows: Vec<FileRow> = sqlx::query_as(&format!(
        "select {COLS} {FROM} where f.account_id = $1
           and ($2::text is null or f.purpose = $2)
           and ($3::text is null or $3 = any(f.tags))
           and (not $4 or f.public)
         order by f.key limit 1000"
    ))
    .bind(account)
    .bind(q.purpose.as_deref().filter(|p| !p.is_empty()))
    .bind(q.tag.as_deref().map(str::to_ascii_lowercase))
    .bind(public_only)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({ "files": rows.iter().map(|r| doc(s, r)).collect::<Vec<_>>() }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UploadQ {
    purpose: Option<String>,
    public: Option<bool>,
    /// JSON object merged into the file's metadata.
    metadata: Option<String>,
    /// Comma-separated content tags; omitted on re-upload = keep the current set.
    tags: Option<String>,
}

async fn upload(
    s: &AppState,
    account: Uuid,
    key: &str,
    q: UploadQ,
    headers: &HeaderMap,
    body: Body,
    by: &str,
) -> Result<Value> {
    check_key(key)?;
    if let Some(p) = &q.purpose {
        check_purpose(p)?;
    }
    let metadata: Value = match &q.metadata {
        Some(m) => {
            let v: Value = serde_json::from_str(m)
                .map_err(|e| Error::InvalidArgument(format!("metadata: {e}")))?;
            if !v.is_object() {
                return Err(Error::InvalidArgument("metadata must be an object".into()));
            }
            if m.len() > 4096 {
                return Err(Error::InvalidArgument("metadata ≤ 4 KiB".into()));
            }
            v
        }
        None => json!({}),
    };
    let tags = match &q.tags {
        Some(t) => Some(parse_tags(
            &t.split(',').map(str::to_string).collect::<Vec<_>>(),
        )?),
        None => None,
    };
    let max = s.cfg.files.player_max_bytes.max(1) as usize;
    let bytes = axum::body::to_bytes(body, max)
        .await
        .map_err(|_| Error::InvalidArgument(format!("file larger than {max} bytes")))?;
    if bytes.is_empty() {
        return Err(Error::InvalidArgument("empty body".into()));
    }
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|m| !m.is_empty() && m.len() <= 128)
        .unwrap_or("application/octet-stream")
        .to_string();
    let sha = hex::encode(sha2::Sha256::digest(&bytes));
    let size = bytes.len() as i64;

    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let existing: Option<(Uuid, i32)> = sqlx::query_as(
        "select id, latest_revision from musubi.player_files where account_id = $1 and key = $2 for update",
    )
    .bind(account)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let (file_id, revision) = match existing {
        Some((id, latest)) => {
            let rev = latest + 1;
            sqlx::query(
                "update musubi.player_files set current_revision = $2, latest_revision = $2,
                    purpose = coalesce($3, purpose), public = coalesce($4, public),
                    metadata = metadata || $5, tags = coalesce($6, tags), updated_at = now()
                 where id = $1",
            )
            .bind(id)
            .bind(rev)
            .bind(&q.purpose)
            .bind(q.public)
            .bind(&metadata)
            .bind(&tags)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            (id, rev)
        }
        None => {
            let (n,): (i64,) =
                sqlx::query_as("select count(*) from musubi.player_files where account_id = $1")
                    .bind(account)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
            if n >= s.cfg.files.player_max_files as i64 {
                return Err(Error::ResourceExhausted(format!(
                    "file quota: {} files per player",
                    s.cfg.files.player_max_files
                )));
            }
            let id = Uuid::now_v7();
            sqlx::query(
                "insert into musubi.player_files (id, account_id, key, purpose, public, metadata, tags)
                 values ($1, $2, $3, coalesce($4, ''), coalesce($5, false), $6, coalesce($7, '{}'::text[]))",
            )
            .bind(id)
            .bind(account)
            .bind(key)
            .bind(&q.purpose)
            .bind(q.public)
            .bind(&metadata)
            .bind(&tags)
            .execute(&mut *tx)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
                    Error::NotFound(format!("account {account}"))
                }
                e => Error::internal(e),
            })?;
            (id, 1)
        }
    };
    let (blob, bkey): (Option<Vec<u8>>, Option<String>) = match &s.blobs {
        Some(store) => {
            let k = blob_key(account, file_id, revision);
            store.put(&k, bytes.to_vec(), &mime).await?;
            (None, Some(k))
        }
        None => (Some(bytes.to_vec()), None),
    };
    sqlx::query(
        "insert into musubi.player_file_revisions (file_id, revision, size, sha256, mime, blob, blob_key, created_by)
         values ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(file_id)
    .bind(revision)
    .bind(size)
    .bind(&sha)
    .bind(&mime)
    .bind(&blob)
    .bind(&bkey)
    .bind(by)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    // keep the newest N revisions (the current one is among them: it is the newest)
    let keep = s.cfg.files.player_keep_revisions.max(1) as i32;
    let pruned: Vec<(Option<String>,)> = sqlx::query_as(
        "delete from musubi.player_file_revisions where file_id = $1 and revision <= $2 returning blob_key",
    )
    .bind(file_id)
    .bind(revision - keep)
    .fetch_all(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    if let Some(store) = &s.blobs {
        for (k,) in pruned.into_iter() {
            if let Some(k) = k {
                let _ = store.delete(&k).await;
            }
        }
    }
    let row = fetch(s, account, key)
        .await?
        .ok_or_else(|| Error::Internal("file vanished".into()))?;
    s.events
        .emit(
            "player_file.updated",
            Some(AccountId(account)),
            json!({ "key": key, "revision": revision, "size": size, "sha256": sha, "mime": mime,
                "public": row.4, "purpose": row.3, "by": by }),
        )
        .await;
    Ok(doc(s, &row))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GetQ {
    meta: Option<String>,
    revisions: Option<String>,
    revision: Option<i32>,
}

fn flag(v: &Option<String>) -> bool {
    v.as_deref()
        .is_some_and(|v| v.is_empty() || v == "1" || v == "true")
}

async fn revisions(s: &AppState, row: &FileRow) -> Result<Value> {
    type Rev = (i32, i64, String, String, String, OffsetDateTime);
    let rows: Vec<Rev> = sqlx::query_as(
        "select revision, size, sha256, mime, created_by, created_at from musubi.player_file_revisions
         where file_id = $1 order by revision desc",
    )
    .bind(row.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({
        "key": row.2, "current_revision": row.5, "latest_revision": row.6,
        "revisions": rows.iter().map(|r| json!({
            "revision": r.0, "size": r.1, "sha256": r.2, "mime": r.3, "by": r.4,
            "created_at": r.5, "current": r.0 == row.5,
        })).collect::<Vec<_>>(),
    }))
}

async fn download(
    s: &AppState,
    row: &FileRow,
    revision: Option<i32>,
    headers: &HeaderMap,
) -> Result<Response> {
    let rev = revision.unwrap_or(row.5);
    type Blob = (i64, String, String, Option<Vec<u8>>, Option<String>);
    let r: Option<Blob> = sqlx::query_as(
        "select size, sha256, mime, blob, blob_key from musubi.player_file_revisions where file_id = $1 and revision = $2",
    )
    .bind(row.0)
    .bind(rev)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((size, sha, mime, blob, bkey)) = r else {
        return Err(Error::NotFound(format!("revision {rev} of `{}`", row.2)));
    };
    let etag = format!("\"{sha}\"");
    let cache = if row.4 {
        "public, max-age=60"
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
    let body = match (blob, bkey, &s.blobs) {
        (Some(b), _, _) => Body::from(b),
        (None, Some(k), Some(store)) => {
            let (_, stream) = store.get_stream(&k).await?;
            Body::from_stream(stream)
        }
        (None, Some(_), None) => {
            return Err(Error::Unavailable(
                "file blob is in object storage but [ugc.s3] is not configured on this node".into(),
            ))
        }
        (None, None, _) => return Err(Error::NotFound("file has no blob".into())),
    };
    let filename = row.2.rsplit('/').next().unwrap_or("file").replace('"', "");
    Ok((
        [
            (header::CONTENT_TYPE, mime),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache.to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{filename}\""),
            ),
            // the mime is whatever the uploader declared: never let a browser sniff it into HTML
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
            (
                header::HeaderName::from_static("x-musubi-file-revision"),
                rev.to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

async fn get_any(
    s: &AppState,
    account: Uuid,
    key: &str,
    q: &GetQ,
    headers: &HeaderMap,
    public_only: bool,
) -> Result<Response> {
    check_key(key)?;
    let row = fetch(s, account, key).await?;
    let row = match row {
        Some(r) if !public_only || r.4 => r,
        _ => return Err(Error::NotFound(format!("file `{key}`"))),
    };
    if flag(&q.meta) {
        return Ok(Json(doc(s, &row)).into_response());
    }
    if flag(&q.revisions) {
        return Ok(Json(revisions(s, &row).await?).into_response());
    }
    download(s, &row, q.revision, headers).await
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PatchBody {
    public: Option<bool>,
    purpose: Option<String>,
    /// Merged into the stored metadata (`null` values remove keys).
    metadata: Option<Value>,
    /// Replaces the tag set (`[]` clears it).
    tags: Option<Vec<String>>,
    /// Promote a kept revision back to current.
    current_revision: Option<i32>,
}

async fn patch(s: &AppState, account: Uuid, key: &str, b: PatchBody, by: &str) -> Result<Value> {
    check_key(key)?;
    if let Some(p) = &b.purpose {
        check_purpose(p)?;
    }
    let tags = match &b.tags {
        Some(t) => Some(parse_tags(t)?),
        None => None,
    };
    if let Some(m) = &b.metadata {
        if !m.is_object() {
            return Err(Error::InvalidArgument("metadata must be an object".into()));
        }
    }
    let before = fetch(s, account, key)
        .await?
        .ok_or_else(|| Error::NotFound(format!("file `{key}`")))?;
    if let Some(rev) = b.current_revision {
        let exists: Option<(i32,)> = sqlx::query_as(
            "select revision from musubi.player_file_revisions where file_id = $1 and revision = $2",
        )
        .bind(before.0)
        .bind(rev)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
        if exists.is_none() {
            return Err(Error::NotFound(format!(
                "revision {rev} of `{key}` (kept: see ?revisions=1)"
            )));
        }
    }
    let metadata = b.metadata.map(|m| {
        let mut merged = before.10.as_object().cloned().unwrap_or_default();
        for (k, v) in m.as_object().cloned().unwrap_or_default() {
            if v.is_null() {
                merged.remove(&k);
            } else {
                merged.insert(k, v);
            }
        }
        Value::Object(merged)
    });
    sqlx::query(
        "update musubi.player_files set public = coalesce($2, public), purpose = coalesce($3, purpose),
            metadata = coalesce($4, metadata), tags = coalesce($5, tags),
            current_revision = coalesce($6, current_revision), updated_at = now()
         where id = $1",
    )
    .bind(before.0)
    .bind(b.public)
    .bind(&b.purpose)
    .bind(&metadata)
    .bind(&tags)
    .bind(b.current_revision)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let row = fetch(s, account, key)
        .await?
        .ok_or_else(|| Error::Internal("file vanished".into()))?;
    let kind = if b.current_revision.is_some_and(|r| r != before.5) {
        "player_file.promoted"
    } else {
        "player_file.updated"
    };
    s.events
        .emit(
            kind,
            Some(AccountId(account)),
            json!({ "key": key, "revision": row.5, "from_revision": before.5, "public": row.4,
                "purpose": row.3, "by": by }),
        )
        .await;
    Ok(doc(s, &row))
}

async fn remove(s: &AppState, account: Uuid, key: &str, by: &str) -> Result<Value> {
    check_key(key)?;
    let row = fetch(s, account, key)
        .await?
        .ok_or_else(|| Error::NotFound(format!("file `{key}`")))?;
    let keys: Vec<(Option<String>,)> = sqlx::query_as(
        "delete from musubi.player_file_revisions where file_id = $1 returning blob_key",
    )
    .bind(row.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    sqlx::query("delete from musubi.player_files where id = $1")
        .bind(row.0)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    if let Some(store) = &s.blobs {
        for (k,) in keys.into_iter() {
            if let Some(k) = k {
                let _ = store.delete(&k).await;
            }
        }
    }
    s.events
        .emit(
            "player_file.deleted",
            Some(AccountId(account)),
            json!({ "key": key, "revisions": row.6, "by": by }),
        )
        .await;
    Ok(json!({ "ok": true, "key": key }))
}

// ---- client -----------------------------------------------------------------------------------

async fn list_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(list(&s, me.account_id.0, &q, false).await?))
}

async fn get_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
    Query(q): Query<GetQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    Ok(get_any(&s, me.account_id.0, &key, &q, &headers, false).await?)
}

async fn put_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
    Query(q): Query<UploadQ>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    s.limits.account(me.account_id)?;
    Ok(Json(
        upload(&s, me.account_id.0, &key, q, &headers, body, "client").await?,
    ))
}

async fn patch_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
    Json(b): Json<PatchBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(patch(&s, me.account_id.0, &key, b, "client").await?))
}

async fn delete_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(remove(&s, me.account_id.0, &key, "client").await?))
}

// ---- other players' public files ---------------------------------------------------------------

async fn list_public(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<String>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(list(&s, account_path(&id)?, &q, true).await?))
}

async fn get_public(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<GetQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    Ok(get_any(&s, account_path(&id)?, &key, &q, &headers, true).await?)
}

// ---- server -----------------------------------------------------------------------------------

async fn list_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(list(&s, account_path(&id)?, &q, false).await?))
}

async fn get_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<GetQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    Ok(get_any(&s, account_path(&id)?, &key, &q, &headers, false).await?)
}

async fn put_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, key)): Path<(String, String)>,
    Query(q): Query<UploadQ>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        upload(&s, account_path(&id)?, &key, q, &headers, body, "server").await?,
    ))
}

async fn patch_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, key)): Path<(String, String)>,
    Json(b): Json<PatchBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        patch(&s, account_path(&id)?, &key, b, "server").await?,
    ))
}

async fn delete_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, key)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(remove(&s, account_path(&id)?, &key, "server").await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_purposes_are_strict() {
        assert!(check_key("saves/slot1.bin").is_ok());
        assert!(check_key("a").is_ok());
        assert!(check_key("").is_err());
        assert!(check_key("/abs").is_err());
        assert!(check_key("a//b").is_err());
        assert!(check_key("a/../b").is_err());
        assert!(check_key("sp ace").is_err());
        assert!(check_key(&"x".repeat(129)).is_err());
        assert!(check_purpose("save-game_1").is_ok());
        assert!(check_purpose("").is_ok());
        assert!(check_purpose("Save").is_err());
        assert_eq!(
            parse_tags(&["HD".into(), "hd".into(), " s3 ".into()]).unwrap(),
            vec!["hd", "s3"]
        );
        assert!(parse_tags(&["bad tag".into()]).is_err());
    }
}
