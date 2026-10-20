//! Build artifacts (GameLift "Build resources", UGS differential updates): versioned server
//! binaries/zips stored next to the fleet definitions as content-defined chunks shared across
//! versions. A fleet or build references one as `spec.artifact = "name@version"`; on save it is
//! resolved into `MUSUBI_ARTIFACT_URL` (capability URL with the artifact's token),
//! `MUSUBI_ARTIFACT_MANIFEST_URL`, `MUSUBI_ARTIFACT_SHA256`, `MUSUBI_ARTIFACT_FILENAME` and
//! `MUSUBI_ARTIFACT_SIZE`, so any provider's launcher can fetch the whole file — or only the
//! chunks its local cache lacks — and verify it. Uploaders do the same in reverse: send the
//! manifest, learn which chunks the cluster already holds, transfer the rest, commit. Also the
//! fleet cost attribute (`spec.cost`) with a live spend estimate.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use musubi_core::chunk::{ChunkParams, Manifest};
use musubi_core::{Error, Result};
use musubi_fleet::FleetSpec;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Digest;
use time::OffsetDateTime;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

const MAX_BYTES: usize = 4 << 30;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/builds", get(list))
        .route("/v1/server/builds/chunking", get(chunking))
        .route("/v1/server/builds/gc", post(gc))
        .route(
            "/v1/server/builds/{name}/{version}",
            get(get_one).put(upload).delete(delete),
        )
        .route("/v1/server/builds/{name}/{version}/download", get(download))
        .route("/v1/server/builds/{name}/{version}/manifest", get(manifest))
        .route("/v1/server/builds/{name}/{version}/delta", post(delta))
        .route(
            "/v1/server/builds/{name}/{version}/upload",
            post(begin_upload),
        )
        .route("/v1/server/builds/{name}/{version}/commit", post(commit))
        .route(
            "/v1/server/builds/{name}/{version}/chunks/{hash}",
            get(get_chunk).put(put_chunk),
        )
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
    bool,
    Option<Value>,
);
const COLS: &str =
    "name, version, filename, size, sha256, mime, blob_key, token, notes, metadata, created_at, \
     (manifest is not null) as chunked, delta";

fn doc(s: &AppState, r: &Row) -> Value {
    let mut v = json!({
        "name": r.0, "version": r.1, "filename": r.2, "size": r.3, "sha256": r.4, "mime": r.5,
        "storage": if r.11 { if s.blobs.is_some() { "chunks+s3" } else { "chunks" } } else if r.6.is_some() { "s3" } else { "db" },
        "chunked": r.11, "token": r.7, "notes": r.8,
        "metadata": r.9, "created_at": r.10.unix_timestamp(),
        "download_url": download_url(s, &r.0, &r.1, &r.7),
        "delta": r.12,
    });
    if r.11 {
        v["manifest_url"] = json!(manifest_url(s, &r.0, &r.1, &r.7));
    }
    v
}

fn api_base(s: &AppState) -> String {
    s.fleets.api_url().trim_end_matches('/').to_string()
}

fn download_url(s: &AppState, name: &str, version: &str, token: &str) -> String {
    format!(
        "{}/v1/server/builds/{name}/{version}/download?token={token}",
        api_base(s)
    )
}

fn manifest_url(s: &AppState, name: &str, version: &str, token: &str) -> String {
    format!(
        "{}/v1/server/builds/{name}/{version}/manifest?token={token}",
        api_base(s)
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

fn check_hash(h: &str) -> Result<()> {
    if h.len() != 64
        || !h
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::InvalidArgument(
            "chunk hash must be 64 lowercase hex chars (sha256)".into(),
        ));
    }
    Ok(())
}

fn params(s: &AppState) -> Result<ChunkParams> {
    let b = &s.cfg.builds;
    let p = ChunkParams {
        min: b.chunk_min_kb << 10,
        avg: b.chunk_avg_kb << 10,
        max: b.chunk_max_kb << 10,
    };
    p.validate()
        .map_err(|e| Error::InvalidArgument(format!("[builds] chunking: {e}")))?;
    Ok(p)
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

async fn load_manifest(s: &AppState, name: &str, version: &str) -> Result<Option<Manifest>> {
    let m: Option<Option<Value>> = sqlx::query_scalar(
        "select manifest from musubi.build_artifacts where name = $1 and version = $2",
    )
    .bind(name)
    .bind(version)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match m {
        None => Err(Error::NotFound(format!("build artifact {name}@{version}"))),
        Some(None) => Ok(None),
        Some(Some(v)) => Ok(Some(serde_json::from_value(v).map_err(Error::internal)?)),
    }
}

/// Newest other version of the same build that has a manifest — the baseline a delta is
/// measured against.
async fn previous(s: &AppState, name: &str, version: &str) -> Result<Option<(String, Manifest)>> {
    let r: Option<(String, Value)> = sqlx::query_as(
        "select version, manifest from musubi.build_artifacts
         where name = $1 and version <> $2 and manifest is not null
         order by created_at desc limit 1",
    )
    .bind(name)
    .bind(version)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match r {
        None => Ok(None),
        Some((v, m)) => Ok(Some((
            v,
            serde_json::from_value(m).map_err(Error::internal)?,
        ))),
    }
}

// ---- chunk store ------------------------------------------------------------------------------

async fn present(s: &AppState, hashes: &[String]) -> Result<HashSet<String>> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<String> =
        sqlx::query_scalar("select hash from musubi.build_chunks where hash = any($1)")
            .bind(hashes)
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    Ok(rows.into_iter().collect())
}

/// Store one chunk if the cluster does not hold it yet; `Ok(false)` when it was already there.
async fn store_chunk(s: &AppState, hash: &str, bytes: &[u8]) -> Result<bool> {
    let exists: Option<i32> =
        sqlx::query_scalar("select 1 from musubi.build_chunks where hash = $1")
            .bind(hash)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    if exists.is_some() {
        return Ok(false);
    }
    let (blob, key): (Option<&[u8]>, Option<String>) = match &s.blobs {
        Some(store) => {
            let key = format!("builds/chunks/{hash}");
            store
                .put(&key, bytes.to_vec(), "application/octet-stream")
                .await?;
            (None, Some(key))
        }
        None => (Some(bytes), None),
    };
    let n = sqlx::query(
        "insert into musubi.build_chunks (hash, size, blob, blob_key) values ($1, $2, $3, $4)
         on conflict (hash) do nothing",
    )
    .bind(hash)
    .bind(bytes.len() as i32)
    .bind(blob)
    .bind(&key)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    cache_put(s, hash, bytes).await;
    Ok(n > 0)
}

async fn read_chunk(s: &AppState, hash: &str) -> Result<Vec<u8>> {
    if let Some(b) = cache_get(s, hash).await {
        return Ok(b);
    }
    let row: Option<(Option<Vec<u8>>, Option<String>)> =
        sqlx::query_as("select blob, blob_key from musubi.build_chunks where hash = $1")
            .bind(hash)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((blob, key)) = row else {
        return Err(Error::NotFound(format!("chunk {hash}")));
    };
    let bytes = match (blob, key, &s.blobs) {
        (Some(b), _, _) => b,
        (None, Some(k), Some(store)) => store.get(&k).await?,
        _ => {
            return Err(Error::Unavailable(
                "chunk is in object storage but [ugc.s3] is not configured on this node".into(),
            ))
        }
    };
    cache_put(s, hash, &bytes).await;
    Ok(bytes)
}

async fn add_refs(s: &AppState, m: &Manifest, by: i32) -> Result<()> {
    let uniq: Vec<String> = m
        .chunks
        .iter()
        .map(|c| c.hash.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    sqlx::query("update musubi.build_chunks set refs = refs + $2 where hash = any($1)")
        .bind(&uniq)
        .bind(by)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(())
}

/// Remove chunks no committed artifact references and no pending upload still needs.
async fn gc_orphans(s: &AppState) -> Result<(u64, u64)> {
    let rows: Vec<(String, Option<String>, i32)> = sqlx::query_as(
        "select c.hash, c.blob_key, c.size from musubi.build_chunks c
         where c.refs <= 0 and not exists (
             select 1 from musubi.build_uploads u
             where u.manifest->'chunks' @> jsonb_build_array(jsonb_build_object('hash', c.hash)))",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    if rows.is_empty() {
        return Ok((0, 0));
    }
    let mut bytes = 0u64;
    let mut hashes = Vec::with_capacity(rows.len());
    for (h, key, size) in &rows {
        if let (Some(k), Some(store)) = (key, &s.blobs) {
            let _ = store.delete(k).await;
        }
        if let Some(dir) = cache_dir(s) {
            let _ = tokio::fs::remove_file(dir.join(h)).await;
        }
        bytes += *size as u64;
        hashes.push(h.clone());
    }
    sqlx::query("delete from musubi.build_chunks where hash = any($1)")
        .bind(&hashes)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok((hashes.len() as u64, bytes))
}

// ---- per-node disk cache ----------------------------------------------------------------------

static CACHE_WRITES: AtomicU64 = AtomicU64::new(0);

fn cache_dir(s: &AppState) -> Option<PathBuf> {
    if !s.cfg.builds.cache || s.cfg.node.data_dir.trim().is_empty() {
        return None;
    }
    Some(PathBuf::from(&s.cfg.node.data_dir).join("build_chunks"))
}

async fn cache_get(s: &AppState, hash: &str) -> Option<Vec<u8>> {
    let p = cache_dir(s)?.join(hash);
    let b = tokio::fs::read(&p).await.ok()?;
    if hex::encode(sha2::Sha256::digest(&b)) != hash {
        let _ = tokio::fs::remove_file(&p).await;
        return None;
    }
    Some(b)
}

async fn cache_put(s: &AppState, hash: &str, bytes: &[u8]) {
    let Some(dir) = cache_dir(s) else { return };
    if tokio::fs::create_dir_all(&dir).await.is_err() {
        return;
    }
    let final_path = dir.join(hash);
    if tokio::fs::metadata(&final_path).await.is_ok() {
        return;
    }
    let tmp = dir.join(format!(".{hash}.{}", rand::random::<u32>()));
    if tokio::fs::write(&tmp, bytes).await.is_ok()
        && tokio::fs::rename(&tmp, &final_path).await.is_err()
    {
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    if CACHE_WRITES.fetch_add(1, Ordering::Relaxed) % 32 == 0 {
        let _ = cache_evict(&dir, s.cfg.builds.cache_max_mb << 20).await;
    }
}

async fn cache_scan(dir: &PathBuf) -> std::io::Result<Vec<(PathBuf, u64, std::time::SystemTime)>> {
    let mut out = Vec::new();
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(e) = rd.next_entry().await? {
        if e.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let md = e.metadata().await?;
        if md.is_file() {
            out.push((
                e.path(),
                md.len(),
                md.modified().unwrap_or(std::time::UNIX_EPOCH),
            ));
        }
    }
    Ok(out)
}

/// Oldest-written chunks go first once the cache exceeds `max_bytes`; returns (files, bytes) evicted.
async fn cache_evict(dir: &PathBuf, max_bytes: u64) -> std::io::Result<(u64, u64)> {
    let mut files = cache_scan(dir).await?;
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    if total <= max_bytes {
        return Ok((0, 0));
    }
    files.sort_by_key(|f| f.2);
    let (mut n, mut bytes) = (0u64, 0u64);
    for (p, len, _) in files {
        if total <= max_bytes {
            break;
        }
        if tokio::fs::remove_file(&p).await.is_ok() {
            total -= len;
            n += 1;
            bytes += len;
        }
    }
    Ok((n, bytes))
}

// ---- finalize an artifact from a manifest whose chunks are all stored -------------------------

struct Meta {
    filename: String,
    mime: String,
    notes: Option<String>,
    metadata: Value,
}

fn meta_from(
    name: &str,
    version: &str,
    filename: Option<String>,
    mime: Option<String>,
    notes: Option<String>,
    metadata: Option<Value>,
) -> Result<Meta> {
    let filename = filename
        .filter(|f| !f.is_empty() && f.len() <= 128 && !f.contains('/') && !f.contains('\\'))
        .unwrap_or_else(|| format!("{name}-{version}.bin"));
    if notes.as_ref().is_some_and(|n| n.len() > 500) {
        return Err(Error::InvalidArgument("notes exceed 500 chars".into()));
    }
    let metadata = match metadata {
        Some(m) if m.is_object() => m,
        Some(_) => {
            return Err(Error::InvalidArgument(
                "metadata must be a JSON object".into(),
            ))
        }
        None => json!({}),
    };
    let mime = mime
        .filter(|m| !m.is_empty() && m.len() <= 128)
        .unwrap_or_else(|| "application/octet-stream".into());
    Ok(Meta {
        filename,
        mime,
        notes,
        metadata,
    })
}

/// Verify every chunk is stored with the declared size and that the assembled bytes hash to the
/// manifest's sha256, then write the artifact row (replacing an older upload of the same version),
/// bump chunk references and drop whatever became orphaned.
async fn finalize(
    s: &AppState,
    name: &str,
    version: &str,
    meta: Meta,
    m: &Manifest,
) -> Result<Row> {
    let have = present(
        s,
        &m.chunks.iter().map(|c| c.hash.clone()).collect::<Vec<_>>(),
    )
    .await?;
    let have: HashSet<&str> = have.iter().map(String::as_str).collect();
    let missing = m.missing(&have);
    if !missing.is_empty() {
        let shown: Vec<&str> = missing.iter().take(5).map(|c| c.hash.as_str()).collect();
        return Err(Error::Conflict(format!(
            "{} chunks not uploaded yet ({} bytes), e.g. {}",
            missing.len(),
            missing.iter().map(|c| c.size as u64).sum::<u64>(),
            shown.join(", ")
        )));
    }
    let mut hasher = sha2::Sha256::new();
    for c in &m.chunks {
        let b = read_chunk(s, &c.hash).await?;
        if b.len() != c.size as usize {
            return Err(Error::InvalidArgument(format!(
                "chunk {} is {} bytes in the store, manifest says {}",
                c.hash,
                b.len(),
                c.size
            )));
        }
        hasher.update(&b);
    }
    let sha = hex::encode(hasher.finalize());
    if sha != m.sha256 {
        return Err(Error::InvalidArgument(format!(
            "assembled artifact sha256 {sha} does not match the manifest's {}",
            m.sha256
        )));
    }
    let delta = match previous(s, name, version).await? {
        Some((from, prev)) => {
            let mut d = serde_json::to_value(m.delta_from(&prev)).map_err(Error::internal)?;
            d["from"] = json!(from);
            Some(d)
        }
        None => None,
    };
    // an older upload of this exact version gives its chunks back (and its legacy blob)
    if let Ok(old) = load(s, name, version).await {
        if let Some(prev) = load_manifest(s, name, version).await? {
            add_refs(s, &prev, -1).await?;
        }
        if let (Some(key), Some(store)) = (&old.6, &s.blobs) {
            let _ = store.delete(key).await;
        }
    }
    let token = hex::encode(rand::random::<[u8; 24]>());
    let manifest = serde_json::to_value(m).map_err(Error::internal)?;
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.build_artifacts (name, version, filename, size, sha256, mime, blob, blob_key, token, notes, metadata, manifest, delta)
         values ($1, $2, $3, $4, $5, $6, null, null, $7, $8, $9, $10, $11)
         on conflict (name, version) do update set filename = excluded.filename, size = excluded.size,
             sha256 = excluded.sha256, mime = excluded.mime, blob = null, blob_key = null,
             token = excluded.token, notes = coalesce(excluded.notes, build_artifacts.notes),
             metadata = build_artifacts.metadata || excluded.metadata, manifest = excluded.manifest,
             delta = excluded.delta, created_at = now()
         returning {COLS}"
    ))
    .bind(name)
    .bind(version)
    .bind(&meta.filename)
    .bind(m.size as i64)
    .bind(&sha)
    .bind(&meta.mime)
    .bind(&token)
    .bind(&meta.notes)
    .bind(&meta.metadata)
    .bind(&manifest)
    .bind(&delta)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    add_refs(s, m, 1).await?;
    sqlx::query("delete from musubi.build_uploads where name = $1 and version = $2")
        .bind(name)
        .bind(version)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    let _ = gc_orphans(s).await;
    s.events
        .emit(
            "build.artifact_uploaded",
            None,
            json!({ "name": name, "version": version, "size": m.size, "sha256": sha,
                    "chunks": m.chunks.len(), "delta": delta }),
        )
        .await;
    Ok(row)
}

// ---- handlers ---------------------------------------------------------------------------------

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
    let pending: Vec<(String, String, i64, OffsetDateTime)> = sqlx::query_as(
        "select name, version, (manifest->>'size')::bigint, created_at from musubi.build_uploads
         where ($1::text is null or name = $1) order by created_at desc limit 100",
    )
    .bind(&q.name)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let pending: Vec<Value> = pending
        .iter()
        .map(|p| json!({ "name": p.0, "version": p.1, "size": p.2, "started_at": p.3.unix_timestamp() }))
        .collect();
    Ok(Json(
        json!({ "artifacts": artifacts, "pending_uploads": pending }),
    ))
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
/// (zip, tar.gz, binary). The server chunks it with `[builds]` parameters and stores only the
/// chunks it does not hold yet (chunks go to `[ugc.s3]` when configured, else into Postgres).
/// Re-uploading a version replaces the bytes and rotates the download token. For large builds
/// prefer the delta flow (`POST …/upload` → `PUT …/chunks/{hash}` → `POST …/commit`), which
/// never sends what the cluster already has.
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
    let metadata: Option<Value> = match &q.metadata {
        Some(m) => Some(
            serde_json::from_str(m)
                .map_err(|_| Error::InvalidArgument("metadata must be a JSON object".into()))?,
        ),
        None => None,
    };
    let mime = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let meta = meta_from(
        &name,
        &version,
        q.filename.clone(),
        mime,
        q.notes.clone(),
        metadata,
    )?;
    let p = params(&s)?;
    let m = Manifest::of(&body, &p);
    // registered as a pending upload first: chunks stored below carry no references until
    // `finalize`, and a concurrent delete / gc must not sweep them away in between
    sqlx::query(
        "insert into musubi.build_uploads (name, version, filename, mime, notes, metadata, manifest)
         values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (name, version) do update set filename = excluded.filename, mime = excluded.mime,
             notes = excluded.notes, metadata = excluded.metadata, manifest = excluded.manifest,
             max_new_pct = null, created_at = now()",
    )
    .bind(&name)
    .bind(&version)
    .bind(&meta.filename)
    .bind(&meta.mime)
    .bind(&meta.notes)
    .bind(&meta.metadata)
    .bind(serde_json::to_value(&m).map_err(Error::internal)?)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let have = present(
        &s,
        &m.chunks.iter().map(|c| c.hash.clone()).collect::<Vec<_>>(),
    )
    .await?;
    let have: HashSet<&str> = have.iter().map(String::as_str).collect();
    let mut stored = 0u64;
    for c in m.missing(&have) {
        let off = c.offset as usize;
        if store_chunk(&s, &c.hash, &body[off..off + c.size as usize]).await? {
            stored += 1;
        }
    }
    let row = finalize(&s, &name, &version, meta, &m).await?;
    let mut v = doc(&s, &row);
    v["chunks"] = json!(m.chunks.len());
    v["chunks_stored"] = json!(stored);
    v["chunks_reused"] = json!(m.chunks.len() as u64 - stored);
    Ok(Json(v))
}

#[derive(Deserialize)]
struct BeginBody {
    manifest: Manifest,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    mime: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
    /// Reject the upload when more than this share of the artifact (percent) is new compared
    /// to the previous version — the "difference analysis" gate.
    #[serde(default)]
    max_new_pct: Option<f64>,
}

/// `POST /v1/server/builds/{name}/{version}/upload {manifest, filename, mime, notes, metadata,
/// max_new_pct}` — open a delta upload: the manifest (chunk list from `musubi build push` or
/// any client using the same chunking) is checked, compared with the previous version and
/// answered with the chunks the cluster lacks. Nothing is visible to fleets until `…/commit`.
async fn begin_upload(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version)): Path<(String, String)>,
    Json(b): Json<BeginBody>,
) -> Result<Json<Value>, ApiError> {
    check_ident("build name", &name)?;
    check_ident("build version", &version)?;
    b.manifest
        .validate()
        .map_err(|e| Error::InvalidArgument(format!("manifest: {e}")))?;
    if b.max_new_pct.is_some_and(|p| !(0.0..=100.0).contains(&p)) {
        return Err(Error::InvalidArgument("max_new_pct must be 0..=100".into()).into());
    }
    let meta = meta_from(&name, &version, b.filename, b.mime, b.notes, b.metadata)?;
    let p = params(&s)?;
    let prev = previous(&s, &name, &version).await?;
    let delta = prev
        .as_ref()
        .map(|(from, pm)| (from.clone(), b.manifest.delta_from(pm)));
    if let (Some(max), Some((from, d))) = (b.max_new_pct, &delta) {
        if d.new_pct > max {
            s.events
                .emit(
                    "build.upload_rejected",
                    None,
                    json!({ "name": name, "version": version, "from": from, "new_pct": d.new_pct, "max_new_pct": max }),
                )
                .await;
            return Err(Error::Conflict(format!(
                "delta rejected: {}% of {name}@{version} is new relative to {from}, max_new_pct is {max}",
                d.new_pct
            ))
            .into());
        }
    }
    let have = present(
        &s,
        &b.manifest
            .chunks
            .iter()
            .map(|c| c.hash.clone())
            .collect::<Vec<_>>(),
    )
    .await?;
    let have: HashSet<&str> = have.iter().map(String::as_str).collect();
    let missing = b.manifest.missing(&have);
    let manifest = serde_json::to_value(&b.manifest).map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.build_uploads (name, version, filename, mime, notes, metadata, manifest, max_new_pct)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (name, version) do update set filename = excluded.filename, mime = excluded.mime,
             notes = excluded.notes, metadata = excluded.metadata, manifest = excluded.manifest,
             max_new_pct = excluded.max_new_pct, created_at = now()",
    )
    .bind(&name)
    .bind(&version)
    .bind(&meta.filename)
    .bind(&meta.mime)
    .bind(&meta.notes)
    .bind(&meta.metadata)
    .bind(&manifest)
    .bind(b.max_new_pct)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let missing_bytes: u64 = missing.iter().map(|c| c.size as u64).sum();
    Ok(Json(json!({
        "name": name, "version": version, "state": "pending",
        "size": b.manifest.size, "sha256": b.manifest.sha256, "chunks": b.manifest.chunks.len(),
        "params_match": b.manifest.params == p, "server_params": p,
        "missing": missing.iter().map(|c| json!({ "hash": c.hash, "size": c.size, "offset": c.offset })).collect::<Vec<_>>(),
        "missing_chunks": missing.len(), "missing_bytes": missing_bytes,
        "previous": delta.as_ref().map(|(f, _)| f.clone()),
        "delta": delta.as_ref().map(|(_, d)| d),
        "chunk_url": format!("{}/v1/server/builds/{name}/{version}/chunks/{{hash}}", api_base(&s)),
    })))
}

/// `PUT /v1/server/builds/{name}/{version}/chunks/{hash}` — one chunk of a pending upload; the
/// body must hash to `{hash}`. Already-stored chunks are acknowledged without being rewritten.
async fn put_chunk(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version, hash)): Path<(String, String, String)>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    check_ident("build name", &name)?;
    check_ident("build version", &version)?;
    check_hash(&hash)?;
    let p = params(&s)?;
    if body.is_empty() || body.len() > p.max.max(64 << 20) {
        return Err(Error::InvalidArgument(format!(
            "chunk must be 1..={} bytes",
            p.max.max(64 << 20)
        ))
        .into());
    }
    let declared: Option<i64> = sqlx::query_scalar(
        "select (c->>'size')::bigint from musubi.build_uploads u,
             jsonb_array_elements(u.manifest->'chunks') c
         where u.name = $1 and u.version = $2 and c->>'hash' = $3 limit 1",
    )
    .bind(&name)
    .bind(&version)
    .bind(&hash)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(declared) = declared else {
        return Err(Error::NotFound(format!(
            "no pending upload of {name}@{version} lists chunk {hash}"
        ))
        .into());
    };
    if declared != body.len() as i64 {
        return Err(Error::InvalidArgument(format!(
            "chunk {hash}: manifest declares {declared} bytes, body has {}",
            body.len()
        ))
        .into());
    }
    if hex::encode(sha2::Sha256::digest(&body)) != hash {
        return Err(Error::InvalidArgument(
            "chunk bytes do not hash to {hash}".replace("{hash}", &hash),
        )
        .into());
    }
    let stored = store_chunk(&s, &hash, &body).await?;
    Ok(Json(
        json!({ "hash": hash, "size": body.len(), "stored": stored }),
    ))
}

/// `POST /v1/server/builds/{name}/{version}/commit` — every chunk of the pending manifest must
/// be stored; the assembled bytes are re-hashed and the version becomes visible to fleets.
async fn commit(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    check_ident("build name", &name)?;
    check_ident("build version", &version)?;
    let pending: Option<(String, String, Option<String>, Value, Value)> = sqlx::query_as(
        "select filename, mime, notes, metadata, manifest from musubi.build_uploads
         where name = $1 and version = $2",
    )
    .bind(&name)
    .bind(&version)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((filename, mime, notes, metadata, manifest)) = pending else {
        return Err(Error::NotFound(format!("no pending upload of {name}@{version}")).into());
    };
    let m: Manifest = serde_json::from_value(manifest).map_err(Error::internal)?;
    let meta = Meta {
        filename,
        mime,
        notes,
        metadata,
    };
    let row = finalize(&s, &name, &version, meta, &m).await?;
    let mut v = doc(&s, &row);
    v["chunks"] = json!(m.chunks.len());
    Ok(Json(v))
}

async fn delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, version)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let pending = sqlx::query("delete from musubi.build_uploads where name = $1 and version = $2")
        .bind(&name)
        .bind(&version)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    let r = match load(&s, &name, &version).await {
        Ok(r) => r,
        Err(Error::NotFound(_)) if pending > 0 => {
            let (chunks, bytes) = gc_orphans(&s).await?;
            return Ok(Json(json!({
                "name": name, "version": version, "removed": false, "pending_cancelled": true,
                "chunks_freed": chunks, "bytes_freed": bytes,
            })));
        }
        Err(e) => return Err(e.into()),
    };
    if let Some(m) = load_manifest(&s, &name, &version).await? {
        add_refs(&s, &m, -1).await?;
    }
    if let (Some(key), Some(store)) = (&r.6, &s.blobs) {
        let _ = store.delete(key).await;
    }
    sqlx::query("delete from musubi.build_artifacts where name = $1 and version = $2")
        .bind(&name)
        .bind(&version)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    let (chunks, bytes) = gc_orphans(&s).await?;
    s.events
        .emit(
            "build.artifact_deleted",
            None,
            json!({ "name": name, "version": version, "chunks_freed": chunks, "bytes_freed": bytes }),
        )
        .await;
    Ok(Json(json!({
        "name": name, "version": version, "removed": true, "pending_cancelled": pending > 0,
        "chunks_freed": chunks, "bytes_freed": bytes,
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct TokenQ {
    token: Option<String>,
}

/// Launchers hold the artifact's token (capability URL), operators the server key.
fn authorize(s: &AppState, r: &Row, q: &TokenQ, headers: &HeaderMap) -> Result<()> {
    let by_token = q.token.as_deref() == Some(r.7.as_str());
    let by_key = headers
        .get("x-musubi-server-key")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|k| s.fleets.server_key_matches(k));
    if !by_token && !by_key {
        return Err(Error::Unauthenticated("artifact token required".into()));
    }
    Ok(())
}

/// `GET /v1/server/builds/{name}/{version}/download?token=` — the whole artifact (chunked
/// versions are streamed chunk by chunk from cache / store).
async fn download(
    State(s): State<AppState>,
    Path((name, version)): Path<(String, String)>,
    Query(q): Query<TokenQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let r = load(&s, &name, &version).await?;
    authorize(&s, &r, &q, &headers)?;
    let body = if let Some(m) = load_manifest(&s, &name, &version).await? {
        let s2 = s.clone();
        let stream = futures::stream::iter(m.chunks).then(move |c| {
            let s = s2.clone();
            async move {
                read_chunk(&s, &c.hash)
                    .await
                    .map(axum::body::Bytes::from)
                    .map_err(|e| std::io::Error::other(e.to_string()))
            }
        });
        Body::from_stream(stream)
    } else {
        let blob: Option<Vec<u8>> = sqlx::query_scalar(
            "select blob from musubi.build_artifacts where name = $1 and version = $2",
        )
        .bind(&name)
        .bind(&version)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
        match (blob, &r.6, &s.blobs) {
            (Some(b), _, _) => Body::from(b),
            (None, Some(key), Some(store)) => {
                let (_, stream) = store.get_stream(key).await?;
                Body::from_stream(stream)
            }
            _ => {
                return Err(Error::Unavailable(
                    "artifact is in object storage but [ugc.s3] is not configured on this node"
                        .into(),
                )
                .into())
            }
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

/// `GET /v1/server/builds/{name}/{version}/manifest?token=` — chunk list for launchers that
/// keep a local chunk cache: fetch only `chunks` they lack from `chunk_url`, assemble, verify.
async fn manifest(
    State(s): State<AppState>,
    Path((name, version)): Path<(String, String)>,
    Query(q): Query<TokenQ>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s, &name, &version).await?;
    authorize(&s, &r, &q, &headers)?;
    let Some(m) = load_manifest(&s, &name, &version).await? else {
        return Err(Error::Conflict(format!(
            "{name}@{version} predates chunked storage; re-upload it to get a manifest"
        ))
        .into());
    };
    let tok = q.token.as_deref().unwrap_or(&r.7);
    Ok(Json(json!({
        "name": r.0, "version": r.1, "filename": r.2, "size": r.3, "sha256": r.4, "mime": r.5,
        "params": m.params, "chunks": m.chunks, "delta": r.12,
        "chunk_url": format!("{}/v1/server/builds/{name}/{version}/chunks/{{hash}}?token={tok}", api_base(&s)),
        "download_url": download_url(&s, &r.0, &r.1, &r.7),
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct DeltaBody {
    /// Chunk hashes the caller already holds.
    have: Vec<String>,
    /// A version of the same build the caller has in full (its chunks count as held).
    from: Option<String>,
}

/// `POST /v1/server/builds/{name}/{version}/delta?token= {have, from}` — which chunks a holder of
/// `have` (and/or of version `from`) must fetch to assemble this version.
async fn delta(
    State(s): State<AppState>,
    Path((name, version)): Path<(String, String)>,
    Query(q): Query<TokenQ>,
    headers: HeaderMap,
    Json(b): Json<DeltaBody>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s, &name, &version).await?;
    authorize(&s, &r, &q, &headers)?;
    let Some(m) = load_manifest(&s, &name, &version).await? else {
        return Err(Error::Conflict(format!(
            "{name}@{version} predates chunked storage; re-upload it to get a manifest"
        ))
        .into());
    };
    if b.have.len() > 1_000_000 {
        return Err(Error::InvalidArgument("have: too many hashes".into()).into());
    }
    let mut have: HashSet<&str> = b.have.iter().map(|h| h.as_str()).collect();
    let from_manifest = match &b.from {
        Some(f) => {
            check_ident("from", f)?;
            load_manifest(&s, &name, f).await.ok().flatten()
        }
        None => None,
    };
    if let Some(fm) = &from_manifest {
        have.extend(fm.chunks.iter().map(|c| c.hash.as_str()));
    }
    let missing = m.missing(&have);
    let missing_bytes: u64 = missing.iter().map(|c| c.size as u64).sum();
    let tok = q.token.as_deref().unwrap_or(&r.7);
    Ok(Json(json!({
        "name": r.0, "version": r.1, "size": r.3, "sha256": r.4,
        "from": b.from, "from_known": from_manifest.is_some(),
        "total_chunks": m.chunks.len(),
        "missing": missing.iter().map(|c| json!({ "hash": c.hash, "size": c.size, "offset": c.offset })).collect::<Vec<_>>(),
        "missing_chunks": missing.len(), "missing_bytes": missing_bytes,
        "reused_bytes": m.size - missing_bytes.min(m.size),
        "chunk_url": format!("{}/v1/server/builds/{name}/{version}/chunks/{{hash}}?token={tok}", api_base(&s)),
    })))
}

/// `GET /v1/server/builds/{name}/{version}/chunks/{hash}?token=` — one chunk; only hashes the
/// artifact's manifest lists are served under its token.
async fn get_chunk(
    State(s): State<AppState>,
    Path((name, version, hash)): Path<(String, String, String)>,
    Query(q): Query<TokenQ>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let r = load(&s, &name, &version).await?;
    authorize(&s, &r, &q, &headers)?;
    check_hash(&hash)?;
    let listed: Option<i32> = sqlx::query_scalar(
        "select 1 from musubi.build_artifacts where name = $1 and version = $2
           and manifest->'chunks' @> jsonb_build_array(jsonb_build_object('hash', $3::text))",
    )
    .bind(&name)
    .bind(&version)
    .bind(&hash)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if listed.is_none() {
        return Err(Error::NotFound(format!("{name}@{version} has no chunk {hash}")).into());
    }
    let bytes = read_chunk(&s, &hash).await?;
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, bytes.len())
        .header("x-chunk-sha256", &hash)
        .header(
            header::CACHE_CONTROL,
            "private, max-age=31536000, immutable",
        )
        .body(Body::from(bytes))
        .unwrap())
}

/// `GET /v1/server/builds/chunking` — chunking parameters, store and cache statistics.
async fn chunking(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let p = params(&s)?;
    let (chunks, bytes, orphans): (i64, i64, i64) = sqlx::query_as(
        "select count(*), coalesce(sum(size), 0)::bigint,
                count(*) filter (where refs <= 0 and not exists (
                    select 1 from musubi.build_uploads u
                    where u.manifest->'chunks' @> jsonb_build_array(jsonb_build_object('hash', c.hash))))
         from musubi.build_chunks c",
    )
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let (artifacts, chunked, logical): (i64, i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where manifest is not null),
                coalesce(sum(size) filter (where manifest is not null), 0)::bigint
         from musubi.build_artifacts",
    )
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let pending: i64 = sqlx::query_scalar("select count(*) from musubi.build_uploads")
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    let cache = match cache_dir(&s) {
        Some(dir) => {
            let files = cache_scan(&dir).await.unwrap_or_default();
            json!({ "enabled": true, "dir": dir, "files": files.len(),
                    "bytes": files.iter().map(|f| f.1).sum::<u64>(),
                    "max_bytes": s.cfg.builds.cache_max_mb << 20 })
        }
        None => json!({ "enabled": false }),
    };
    Ok(Json(json!({
        "params": p,
        "store": { "backend": if s.blobs.is_some() { "s3" } else { "db" }, "chunks": chunks,
                   "bytes": bytes, "orphans": orphans },
        "artifacts": { "total": artifacts, "chunked": chunked, "logical_bytes": logical,
                       "dedup_ratio": if bytes > 0 { (logical as f64 / bytes as f64 * 100.0).round() / 100.0 } else { 1.0 } },
        "pending_uploads": pending,
        "cache": cache,
    })))
}

/// `POST /v1/server/builds/gc` — drop pending uploads older than `[builds].upload_ttl_hours`,
/// then chunks nothing references, then trim this node's cache to its cap.
async fn gc(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let ttl = s.cfg.builds.upload_ttl_hours.max(1) as f64;
    let expired = sqlx::query(
        "delete from musubi.build_uploads where created_at < now() - $1 * interval '1 hour'",
    )
    .bind(ttl)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    let (chunks, bytes) = gc_orphans(&s).await?;
    let evicted = match cache_dir(&s) {
        Some(dir) => cache_evict(&dir, s.cfg.builds.cache_max_mb << 20)
            .await
            .unwrap_or((0, 0)),
        None => (0, 0),
    };
    Ok(Json(json!({
        "expired_uploads": expired, "chunks_freed": chunks, "bytes_freed": bytes,
        "cache_evicted_files": evicted.0, "cache_evicted_bytes": evicted.1,
    })))
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
    if r.11 {
        spec.env.insert(
            "MUSUBI_ARTIFACT_MANIFEST_URL".into(),
            manifest_url(s, &r.0, &r.1, &r.7),
        );
    } else {
        spec.env.remove("MUSUBI_ARTIFACT_MANIFEST_URL");
    }
    spec.env
        .insert("MUSUBI_ARTIFACT_SHA256".into(), r.4.clone());
    spec.env
        .insert("MUSUBI_ARTIFACT_FILENAME".into(), r.2.clone());
    spec.env
        .insert("MUSUBI_ARTIFACT_SIZE".into(), r.3.to_string());
    Ok(())
}

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
