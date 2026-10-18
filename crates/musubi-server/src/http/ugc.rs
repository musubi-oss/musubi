//! User-generated content: maps, mods, skins… as versioned items carrying a blob (or an external
//! URL), tags, search, ratings, subscriptions and a moderation queue
//! (draft → pending → approved | rejected | hidden).

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::extract::Bearer;
use crate::http::{ApiError, ServerKey};

const KINDS: &[&str] = &[
    "map",
    "mod",
    "skin",
    "blueprint",
    "replay",
    "level",
    "other",
];
const VISIBILITIES: &[&str] = &["public", "unlisted", "private"];
const MOD_STATUSES: &[&str] = &["approved", "rejected", "hidden", "pending"];

const COLS: &str = "id, owner_id, kind, title, description, tags, visibility, status, version, url, mime, size, sha256, downloads, rating_sum, rating_count, moderation, metadata, created_at, updated_at";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/ugc", get(browse).post(create))
        .route("/v1/ugc/mine", get(mine))
        .route("/v1/ugc/subscriptions", get(subscriptions))
        .route("/v1/ugc/{id}", get(get_one).put(update).delete(delete_one))
        .route(
            "/v1/ugc/{id}/blob",
            get(blob_get)
                .put(blob_put)
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route("/v1/ugc/{id}/submit", post(submit))
        .route("/v1/ugc/{id}/rate", post(rate))
        .route(
            "/v1/ugc/{id}/subscribe",
            post(subscribe).delete(unsubscribe),
        )
        .route("/v1/admin/ugc", get(queue))
        .route("/v1/admin/ugc/{id}", post(moderate))
}

fn rfc3339(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn item_json(r: &sqlx::postgres::PgRow) -> Value {
    let sum: i64 = r.get("rating_sum");
    let count: i64 = r.get("rating_count");
    json!({
        "id": r.get::<Uuid, _>("id"),
        "owner_id": r.get::<Uuid, _>("owner_id"),
        "kind": r.get::<String, _>("kind"),
        "title": r.get::<String, _>("title"),
        "description": r.get::<String, _>("description"),
        "tags": r.get::<Vec<String>, _>("tags"),
        "visibility": r.get::<String, _>("visibility"),
        "status": r.get::<String, _>("status"),
        "version": r.get::<i32, _>("version"),
        "url": r.get::<Option<String>, _>("url"),
        "mime": r.get::<String, _>("mime"),
        "size": r.get::<i64, _>("size"),
        "sha256": r.get::<String, _>("sha256"),
        "downloads": r.get::<i64, _>("downloads"),
        "rating": { "count": count, "average": if count > 0 { sum as f64 / count as f64 } else { 0.0 } },
        "moderation": r.get::<Value, _>("moderation"),
        "metadata": r.get::<Value, _>("metadata"),
        "created_at": rfc3339(r.get("created_at")),
        "updated_at": rfc3339(r.get("updated_at")),
    })
}

fn normalize_tags(tags: &[String]) -> Result<Vec<String>, Error> {
    if tags.len() > 10 {
        return Err(Error::InvalidArgument("at most 10 tags".into()));
    }
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_lowercase();
        if t.is_empty() || t.chars().count() > 32 {
            return Err(Error::InvalidArgument("tags must be 1..=32 chars".into()));
        }
        if !out.contains(&t) {
            out.push(t);
        }
    }
    Ok(out)
}

fn check_text(title: &str, description: &str) -> Result<(), Error> {
    let tl = title.trim().chars().count();
    if tl == 0 || tl > 120 {
        return Err(Error::InvalidArgument("title must be 1..=120 chars".into()));
    }
    if description.chars().count() > 4000 {
        return Err(Error::InvalidArgument(
            "description must be ≤ 4000 chars".into(),
        ));
    }
    Ok(())
}

fn check_url(url: &Option<String>) -> Result<(), Error> {
    if let Some(u) = url {
        if !(u.starts_with("https://") || u.starts_with("http://")) || u.len() > 2048 {
            return Err(Error::InvalidArgument("url must be http(s)".into()));
        }
    }
    Ok(())
}

async fn fetch(s: &AppState, id: Uuid) -> Result<Option<sqlx::postgres::PgRow>, Error> {
    sqlx::query(&format!(
        "select {COLS} from musubi.ugc_items where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

/// Visible to a viewer: the owner always, anyone else only when approved and not private.
fn visible(row: &sqlx::postgres::PgRow, viewer: AccountId) -> bool {
    let owner: Uuid = row.get("owner_id");
    let status: String = row.get("status");
    let vis: String = row.get("visibility");
    owner == viewer.0 || (status == "approved" && vis != "private")
}

async fn owned(s: &AppState, id: Uuid, me: AccountId) -> Result<sqlx::postgres::PgRow, Error> {
    let Some(row) = fetch(s, id).await? else {
        return Err(Error::NotFound(format!("ugc item {id}")));
    };
    if row.get::<Uuid, _>("owner_id") != me.0 {
        return Err(Error::PermissionDenied("not your item".into()));
    }
    Ok(row)
}

#[derive(Deserialize)]
struct CreateReq {
    kind: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default = "public")]
    visibility: String,
    #[serde(default)]
    metadata: Value,
}

fn public() -> String {
    "public".into()
}

/// Register a draft. Upload the blob (or set `url`), then `submit` for moderation.
async fn create(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<CreateReq>,
) -> Result<Json<Value>, ApiError> {
    s.age.check(me.account_id, "ugc")?;
    if !KINDS.contains(&r.kind.as_str()) {
        return Err(
            Error::InvalidArgument(format!("kind must be one of {}", KINDS.join("|"))).into(),
        );
    }
    if !VISIBILITIES.contains(&r.visibility.as_str()) {
        return Err(
            Error::InvalidArgument("visibility must be public|unlisted|private".into()).into(),
        );
    }
    check_text(&r.title, &r.description)?;
    check_url(&r.url)?;
    let tags = normalize_tags(&r.tags)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.ugc_items (id, owner_id, kind, title, description, tags, visibility, url, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(me.account_id.0)
    .bind(&r.kind)
    .bind(r.title.trim())
    .bind(&r.description)
    .bind(&tags)
    .bind(&r.visibility)
    .bind(&r.url)
    .bind(if r.metadata.is_null() { json!({}) } else { r.metadata })
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "ugc.created",
            Some(me.account_id),
            json!({ "id": id, "kind": r.kind }),
        )
        .await;
    let row = fetch(&s, id)
        .await?
        .ok_or_else(|| Error::internal("ugc row vanished"))?;
    Ok(Json(item_json(&row)))
}

#[derive(Deserialize)]
struct BrowseQuery {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    /// Case-insensitive substring of the title.
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    owner_id: Option<Uuid>,
    /// `new` (default), `top` (rating), `downloads`.
    #[serde(default)]
    sort: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    20
}

fn like(q: &str) -> String {
    format!(
        "%{}%",
        q.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

/// Approved public items, filtered and sorted.
async fn browse(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Query(q): Query<BrowseQuery>,
) -> Result<Json<Value>, ApiError> {
    let order = match q.sort.as_deref().unwrap_or("new") {
        "new" => "created_at desc",
        "top" => "(rating_sum::float8 / greatest(rating_count, 1)) desc, rating_count desc, created_at desc",
        "downloads" => "downloads desc, created_at desc",
        other => {
            return Err(Error::InvalidArgument(format!("sort {other:?}: expected new|top|downloads")).into())
        }
    };
    let rows = sqlx::query(&format!(
        "select {COLS} from musubi.ugc_items
         where status = 'approved' and visibility = 'public'
           and ($1::text is null or kind = $1)
           and ($2::text is null or $2 = any(tags))
           and ($3::text is null or title ilike $3)
           and ($4::uuid is null or owner_id = $4)
         order by {order} limit $5 offset $6"
    ))
    .bind(&q.kind)
    .bind(q.tag.as_deref().map(|t| t.trim().to_lowercase()))
    .bind(q.q.as_deref().map(like))
    .bind(q.owner_id)
    .bind(q.limit.clamp(1, 100))
    .bind(q.offset.max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "items": rows.iter().map(item_json).collect::<Vec<_>>() }),
    ))
}

/// The caller's items in every status.
async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "select {COLS} from musubi.ugc_items where owner_id = $1 order by updated_at desc limit 200"
    ))
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "items": rows.iter().map(item_json).collect::<Vec<_>>() }),
    ))
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let Some(row) = fetch(&s, id).await? else {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    };
    if !visible(&row, me.account_id) {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    }
    let mut v = item_json(&row);
    let my_rating: Option<i16> = sqlx::query_scalar(
        "select stars from musubi.ugc_ratings where item_id = $1 and account_id = $2",
    )
    .bind(id)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let subscribed: Option<i32> = sqlx::query_scalar(
        "select 1 from musubi.ugc_subscriptions where item_id = $1 and account_id = $2",
    )
    .bind(id)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    v["my_rating"] = json!(my_rating);
    v["subscribed"] = json!(subscribed.is_some());
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
struct UpdateReq {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    visibility: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
}

/// Edit metadata (owner). Text edits do not change the moderation status; a new blob does.
async fn update(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateReq>,
) -> Result<Json<Value>, ApiError> {
    let row = owned(&s, id, me.account_id).await?;
    let title = r.title.unwrap_or_else(|| row.get("title"));
    let description = r.description.unwrap_or_else(|| row.get("description"));
    check_text(&title, &description)?;
    let tags = match &r.tags {
        Some(t) => normalize_tags(t)?,
        None => row.get("tags"),
    };
    let url = r.url.or_else(|| row.get("url"));
    check_url(&url)?;
    let visibility = r.visibility.unwrap_or_else(|| row.get("visibility"));
    if !VISIBILITIES.contains(&visibility.as_str()) {
        return Err(
            Error::InvalidArgument("visibility must be public|unlisted|private".into()).into(),
        );
    }
    let metadata = r.metadata.unwrap_or_else(|| row.get("metadata"));
    sqlx::query(
        "update musubi.ugc_items set title = $2, description = $3, tags = $4, url = $5, visibility = $6,
           metadata = $7, updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(title.trim())
    .bind(&description)
    .bind(&tags)
    .bind(&url)
    .bind(&visibility)
    .bind(&metadata)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let row = fetch(&s, id)
        .await?
        .ok_or_else(|| Error::internal("ugc row vanished"))?;
    Ok(Json(item_json(&row)))
}

async fn delete_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    owned(&s, id, me.account_id).await?;
    let gone: Option<String> =
        sqlx::query_scalar("delete from musubi.ugc_items where id = $1 returning blob_key")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if let (Some(store), Some(key)) = (&s.blobs, gone) {
        if let Err(e) = store.delete(&key).await {
            tracing::warn!(error = %e, key, "ugc blob not deleted from object storage");
        }
    }
    s.events
        .emit("ugc.deleted", Some(me.account_id), json!({ "id": id }))
        .await;
    Ok(Json(json!({ "deleted": true })))
}

/// Upload the content (raw body, ≤ 16 MiB). Bumps `version`; an approved item goes back to
/// `pending` so the new revision is moderated before subscribers see it.
async fn blob_put(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<Value>, ApiError> {
    let row = owned(&s, id, me.account_id).await?;
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|m| !m.is_empty() && m.len() <= 128)
        .unwrap_or("application/octet-stream")
        .to_string();
    let max = s.cfg.ugc.max_blob_bytes.max(1);
    if let Some(len) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
    {
        if len > max {
            return Err(too_large(len, max).into());
        }
    }
    let key = s
        .blobs
        .as_ref()
        .map(|_| format!("{}ugc/{id}/{}", s.cfg.ugc.s3.prefix, Uuid::now_v7()));
    // The body is consumed as it arrives: hashed on the fly and, with object storage, shipped in
    // `part_bytes` pieces (one plain PUT when it fits in a single piece), so the node never holds
    // more than one part of a large mod.
    let streamed = match stream_blob(&s, body, key.as_deref(), &mime, max).await {
        Ok(v) => v,
        Err(e) => {
            if let (Some(store), Some(key), Some(upload)) = (&s.blobs, &key, e.upload.as_deref()) {
                if let Err(e) = store.abort_multipart(key, upload).await {
                    tracing::warn!(error = %e, key, "ugc multipart upload not aborted");
                }
            }
            return Err(e.error.into());
        }
    };
    if streamed.size == 0 {
        if let (Some(store), Some(key)) = (&s.blobs, &key) {
            let _ = store.delete(key).await;
        }
        return Err(Error::InvalidArgument("empty body".into()).into());
    }
    let sha = streamed.sha256;
    let size = streamed.size;
    let status: String = row.get("status");
    let next = if status == "approved" {
        "pending"
    } else {
        status.as_str()
    };
    let (version, old_key): (i32, Option<String>) = sqlx::query_as(
        "with old as (select blob_key from musubi.ugc_items where id = $1)
         update musubi.ugc_items set blob = $2, blob_key = $7, mime = $3, size = $4, sha256 = $5, version = version + 1,
           status = $6, updated_at = now() where id = $1 returning version, (select blob_key from old)",
    )
    .bind(id)
    .bind(streamed.inline.as_deref())
    .bind(&mime)
    .bind(size as i64)
    .bind(&sha)
    .bind(next)
    .bind(&key)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if let (Some(store), Some(old)) = (&s.blobs, old_key.filter(|o| Some(o) != key.as_ref())) {
        if let Err(e) = store.delete(&old).await {
            tracing::warn!(error = %e, key = old, "previous ugc blob not deleted");
        }
    }
    s.events
        .emit(
            "ugc.updated",
            Some(me.account_id),
            json!({ "id": id, "version": version, "size": size, "sha256": sha, "status": next }),
        )
        .await;
    Ok(Json(
        json!({ "id": id, "version": version, "size": size, "sha256": sha, "mime": mime, "status": next }),
    ))
}

fn too_large(len: u64, max: u64) -> Error {
    Error::InvalidArgument(format!(
        "blob is {len} bytes, [ugc] max_blob_bytes is {max}"
    ))
}

pub(crate) struct Streamed {
    pub(crate) sha256: String,
    pub(crate) size: u64,
    /// The bytes themselves when they stay in Postgres (no object storage configured).
    pub(crate) inline: Option<Vec<u8>>,
}

pub(crate) struct StreamFailed {
    pub(crate) error: Error,
    /// A multipart upload that was begun and must be aborted.
    pub(crate) upload: Option<String>,
}

pub(crate) async fn stream_blob(
    s: &AppState,
    body: axum::body::Body,
    key: Option<&str>,
    mime: &str,
    max: u64,
) -> Result<Streamed, StreamFailed> {
    use futures::StreamExt;
    use sha2::Digest;
    let mut stream = body.into_data_stream();
    let mut hasher = sha2::Sha256::new();
    let mut size: u64 = 0;
    let mut buf: Vec<u8> = Vec::new();
    let part_bytes = s.cfg.ugc.s3.part_bytes.max(1) as usize;
    let mut upload: Option<String> = None;
    let mut parts: Vec<(u32, String)> = Vec::new();
    let fail = |error: Error, upload: &Option<String>| StreamFailed {
        error,
        upload: upload.clone(),
    };
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| {
            fail(
                Error::InvalidArgument(format!("body read failed: {e}")),
                &upload,
            )
        })?;
        size += chunk.len() as u64;
        if size > max {
            return Err(fail(too_large(size, max), &upload));
        }
        hasher.update(&chunk);
        buf.extend_from_slice(&chunk);
        if let (Some(store), Some(key)) = (&s.blobs, key) {
            while buf.len() >= part_bytes {
                let id = match &upload {
                    Some(u) => u.clone(),
                    None => {
                        let u = store
                            .begin_multipart(key, mime)
                            .await
                            .map_err(|e| fail(e, &None))?;
                        upload = Some(u.clone());
                        u
                    }
                };
                let rest = buf.split_off(part_bytes);
                let part = std::mem::replace(&mut buf, rest);
                let n = parts.len() as u32 + 1;
                let etag = store
                    .upload_part(key, &id, n, part)
                    .await
                    .map_err(|e| fail(e, &upload))?;
                parts.push((n, etag));
            }
        }
    }
    let sha256: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let inline = match (&s.blobs, key) {
        (Some(store), Some(key)) => {
            match &upload {
                Some(id) => {
                    if !buf.is_empty() {
                        let n = parts.len() as u32 + 1;
                        let etag = store
                            .upload_part(key, id, n, std::mem::take(&mut buf))
                            .await
                            .map_err(|e| fail(e, &upload))?;
                        parts.push((n, etag));
                    }
                    store
                        .complete_multipart(key, id, &parts)
                        .await
                        .map_err(|e| fail(e, &upload))?;
                }
                None if size > 0 => store
                    .put(key, std::mem::take(&mut buf), mime)
                    .await
                    .map_err(|e| fail(e, &None))?,
                None => {}
            }
            None
        }
        _ => Some(buf),
    };
    Ok(Streamed {
        sha256,
        size,
        inline,
    })
}

/// Download the content; counts a download for anyone but the owner.
async fn blob_get(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    let Some(row) = fetch(&s, id).await? else {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    };
    if !visible(&row, me.account_id) {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    }
    let (blob, key): (Option<Vec<u8>>, Option<String>) =
        sqlx::query_as("select blob, blob_key from musubi.ugc_items where id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    let (len, body) = match (blob, key, &s.blobs) {
        (Some(b), _, _) => (Some(b.len() as u64), axum::body::Body::from(b)),
        (None, Some(key), Some(store)) => {
            let (len, stream) = store.get_stream(&key).await?;
            (len, axum::body::Body::from_stream(stream))
        }
        (None, Some(_), None) => {
            return Err(Error::Unavailable(
                "ugc blob is in object storage but [ugc.s3] is not configured on this node".into(),
            )
            .into())
        }
        (None, None, _) => {
            return Err(Error::NotFound(format!("ugc item {id} has no blob (see url)")).into())
        }
    };
    if row.get::<Uuid, _>("owner_id") != me.account_id.0 {
        sqlx::query("update musubi.ugc_items set downloads = downloads + 1 where id = $1")
            .bind(id)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
    }
    let mime: String = row.get("mime");
    let version: i32 = row.get("version");
    let sha: String = row.get("sha256");
    let mut res = (
        [
            (header::CONTENT_TYPE, mime),
            (header::ETAG, format!("\"{sha}\"")),
            (
                header::HeaderName::from_static("x-ugc-version"),
                version.to_string(),
            ),
        ],
        body,
    )
        .into_response();
    if let Some(len) = len {
        res.headers_mut()
            .insert(header::CONTENT_LENGTH, len.to_string().parse().unwrap());
    }
    Ok(res)
}

/// Hand a draft (or a rejected item) to moderation; it needs a blob or a URL.
async fn submit(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = owned(&s, id, me.account_id).await?;
    let status: String = row.get("status");
    if status != "draft" && status != "rejected" {
        return Err(Error::Conflict(format!("item is {status}")).into());
    }
    if row.get::<i64, _>("size") == 0 && row.get::<Option<String>, _>("url").is_none() {
        return Err(
            Error::InvalidArgument("upload a blob or set url before submitting".into()).into(),
        );
    }
    sqlx::query("update musubi.ugc_items set status = 'pending', updated_at = now() where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.events
        .emit("ugc.submitted", Some(me.account_id), json!({ "id": id }))
        .await;
    Ok(Json(json!({ "id": id, "status": "pending" })))
}

#[derive(Deserialize)]
struct RateReq {
    stars: i16,
}

async fn rate(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(r): Json<RateReq>,
) -> Result<Json<Value>, ApiError> {
    if !(1..=5).contains(&r.stars) {
        return Err(Error::InvalidArgument("stars must be 1..=5".into()).into());
    }
    let Some(row) = fetch(&s, id).await? else {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    };
    if row.get::<String, _>("status") != "approved" {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    }
    sqlx::query(
        "insert into musubi.ugc_ratings (item_id, account_id, stars) values ($1, $2, $3)
         on conflict (item_id, account_id) do update set stars = excluded.stars, created_at = now()",
    )
    .bind(id)
    .bind(me.account_id.0)
    .bind(r.stars)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let (sum, count): (i64, i64) = sqlx::query_as(
        "update musubi.ugc_items set
           rating_sum = (select coalesce(sum(stars), 0)::bigint from musubi.ugc_ratings where item_id = $1),
           rating_count = (select count(*) from musubi.ugc_ratings where item_id = $1)
         where id = $1 returning rating_sum, rating_count",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "ugc.rated",
            Some(me.account_id),
            json!({ "id": id, "stars": r.stars }),
        )
        .await;
    Ok(Json(
        json!({ "id": id, "rating": { "count": count, "average": sum as f64 / count.max(1) as f64 } }),
    ))
}

async fn subscribe(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let Some(row) = fetch(&s, id).await? else {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    };
    if !visible(&row, me.account_id) {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    }
    sqlx::query(
        "insert into musubi.ugc_subscriptions (account_id, item_id) values ($1, $2) on conflict do nothing",
    )
    .bind(me.account_id.0)
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit("ugc.subscribed", Some(me.account_id), json!({ "id": id }))
        .await;
    Ok(Json(json!({ "subscribed": true })))
}

async fn unsubscribe(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n =
        sqlx::query("delete from musubi.ugc_subscriptions where account_id = $1 and item_id = $2")
            .bind(me.account_id.0)
            .bind(id)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
    Ok(Json(json!({ "subscribed": false, "removed": n > 0 })))
}

/// Approved items the caller follows, with their current `version` so the client knows what to
/// re-download.
async fn subscriptions(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    let cols = COLS
        .split(", ")
        .map(|c| format!("i.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = sqlx::query(&format!(
        "select {cols} from musubi.ugc_items i
         join musubi.ugc_subscriptions su on su.item_id = i.id
         where su.account_id = $1 and i.status = 'approved' order by su.created_at desc limit 500"
    ))
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "items": rows.iter().map(item_json).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
struct QueueQuery {
    #[serde(default = "pending")]
    status: String,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn pending() -> String {
    "pending".into()
}

/// Moderation queue (`status=pending` by default; any status for review).
async fn queue(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<QueueQuery>,
) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "select {COLS} from musubi.ugc_items where status = $1 order by updated_at asc limit $2 offset $3"
    ))
    .bind(&q.status)
    .bind(q.limit.clamp(1, 100))
    .bind(q.offset.max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "items": rows.iter().map(item_json).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
struct ModerateReq {
    status: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    by: String,
}

/// Decide: `approved` makes the current version visible, `rejected` sends it back to the owner
/// (who may fix and resubmit), `hidden` pulls a live item, `pending` re-queues it.
async fn moderate(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(r): Json<ModerateReq>,
) -> Result<Json<Value>, ApiError> {
    if !MOD_STATUSES.contains(&r.status.as_str()) {
        return Err(Error::InvalidArgument(
            "status must be approved|rejected|hidden|pending".into(),
        )
        .into());
    }
    let Some(row) = fetch(&s, id).await? else {
        return Err(Error::NotFound(format!("ugc item {id}")).into());
    };
    let version: i32 = row.get("version");
    let decision = json!({ "status": r.status, "reason": r.reason, "by": r.by, "version": version,
        "at": rfc3339(time::OffsetDateTime::now_utc()) });
    sqlx::query(
        "update musubi.ugc_items set status = $2, moderation = $3, updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(&r.status)
    .bind(&decision)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let owner: Uuid = row.get("owner_id");
    s.events
        .emit(
            "ugc.moderated",
            Some(AccountId(owner)),
            json!({ "id": id, "status": r.status, "reason": r.reason, "version": version }),
        )
        .await;
    let row = fetch(&s, id)
        .await?
        .ok_or_else(|| Error::internal("ugc row vanished"))?;
    Ok(Json(item_json(&row)))
}
