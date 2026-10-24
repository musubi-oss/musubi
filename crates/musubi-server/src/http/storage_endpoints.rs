//! Registered object-storage endpoints (Edgegap "endpoint storage" parity): operators register
//! S3-compatible buckets — AWS, MinIO, R2, B2, Linode, GCS interop — by name with credentials
//! sealed under the master key, list them redacted, and probe them (`POST …/test` uploads,
//! reads back and deletes a small object, recording the outcome). Named endpoints are what
//! fleets, build artifacts and exports refer to instead of pasting keys into every spec.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::config::S3Config;
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::blobstore::S3Store;
use crate::http::{ApiError, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/storage_endpoints", get(list))
        .route(
            "/v1/server/storage_endpoints/{name}",
            get(get_one).put(put).delete(delete),
        )
        .route("/v1/server/storage_endpoints/{name}/test", post(probe))
}

type Row = (
    String,
    String,
    String,
    String,
    String,
    bool,
    String,
    String,
    String,
    Vec<String>,
    Option<OffsetDateTime>,
    Option<String>,
    Option<String>,
    OffsetDateTime,
    OffsetDateTime,
);
const COLS: &str = "name, endpoint, region, bucket, prefix, path_style, access_key, secret_key, note, labels, last_test_at, last_status, last_error, created_at, updated_at";

fn rfc(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn doc(r: &Row) -> Value {
    let hint = access_key_hint(&r.6);
    json!({
        "name": r.0, "endpoint": r.1, "region": r.2, "bucket": r.3, "prefix": r.4,
        "path_style": r.5, "access_key_hint": hint, "secret_key": "***", "note": r.8,
        "labels": r.9, "last_test_at": r.10.map(rfc), "last_status": r.11, "last_error": r.12,
        "created_at": rfc(r.13), "updated_at": rfc(r.14),
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PutBody {
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    path_style: Option<bool>,
    access_key: Option<String>,
    secret_key: Option<String>,
    note: String,
    labels: Vec<String>,
}

/// Last four characters of the access key (never bytes: a non-ASCII key must not panic `list`).
fn access_key_hint(ak: &str) -> String {
    let n = ak.chars().count();
    if n > 4 {
        format!("…{}", ak.chars().skip(n - 4).collect::<String>())
    } else {
        "…".into()
    }
}

fn name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

async fn load(s: &AppState, name: &str) -> Result<Row> {
    sqlx::query_as(&format!(
        "select {COLS} from musubi.storage_endpoints where name = $1"
    ))
    .bind(name)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("storage endpoint {name}")))
}

async fn put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    if !name_ok(&name) {
        return Err(Error::InvalidArgument("name: 1..=64 of [A-Za-z0-9_.-]".into()).into());
    }
    let endpoint = b.endpoint.trim().trim_end_matches('/').to_string();
    if endpoint.len() > 512
        || !(endpoint.starts_with("https://") || endpoint.starts_with("http://"))
    {
        return Err(Error::InvalidArgument("endpoint must be an http(s) URL".into()).into());
    }
    let bucket = b.bucket.trim().to_string();
    if bucket.len() < 3
        || bucket.len() > 63
        || !bucket
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
    {
        return Err(Error::InvalidArgument("bucket: 3..=63 of [a-z0-9.-]".into()).into());
    }
    if b.region.len() > 64 || b.prefix.len() > 256 || b.note.len() > 500 {
        return Err(
            Error::InvalidArgument("region ≤ 64, prefix ≤ 256, note ≤ 500 chars".into()).into(),
        );
    }
    if b.labels.len() > 16 || b.labels.iter().any(|l| !name_ok(l)) {
        return Err(Error::InvalidArgument("labels: at most 16 of [A-Za-z0-9_.-]".into()).into());
    }
    let existing = load(&s, &name).await.ok();
    let access_key = match (b.access_key.filter(|k| !k.is_empty()), &existing) {
        (Some(k), _) => k,
        (None, Some(e)) => e.6.clone(),
        (None, None) => return Err(Error::InvalidArgument("access_key is required".into()).into()),
    };
    let secret_key = match (b.secret_key.filter(|k| !k.is_empty()), &existing) {
        (Some(k), _) => musubi_core::secrets::seal(&k),
        (None, Some(e)) => e.7.clone(),
        (None, None) => return Err(Error::InvalidArgument("secret_key is required".into()).into()),
    };
    if access_key.len() > 256 || secret_key.len() > 2048 {
        return Err(Error::InvalidArgument("credentials too long".into()).into());
    }
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.storage_endpoints (name, endpoint, region, bucket, prefix, path_style, access_key, secret_key, note, labels)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (name) do update set endpoint = $2, region = $3, bucket = $4, prefix = $5,
             path_style = $6, access_key = $7, secret_key = $8, note = $9, labels = $10,
             last_test_at = null, last_status = null, last_error = null, updated_at = now()
         returning {COLS}"
    ))
    .bind(&name)
    .bind(&endpoint)
    .bind(b.region.trim())
    .bind(&bucket)
    .bind(b.prefix.trim_matches('/'))
    .bind(b.path_style.unwrap_or(true))
    .bind(&access_key)
    .bind(&secret_key)
    .bind(&b.note)
    .bind(&b.labels)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "storage_endpoint.updated",
            None,
            json!({"name": name, "endpoint": endpoint, "bucket": bucket}),
        )
        .await;
    Ok(Json(doc(&row)))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(doc(&load(&s, &name).await?)))
}

async fn list(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.storage_endpoints order by name"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "endpoints": rows.iter().map(doc).collect::<Vec<_>>() }),
    ))
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.storage_endpoints where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("storage endpoint {name}")).into());
    }
    s.events
        .emit("storage_endpoint.deleted", None, json!({"name": name}))
        .await;
    Ok(Json(json!({"deleted": true})))
}

/// Opens a registered endpoint as a store (secret unsealed with the master key).
pub async fn store(s: &AppState, name: &str) -> Result<S3Store> {
    let r = load(s, name).await?;
    let secret = musubi_core::secrets::open(&r.7).map_err(|e| {
        Error::internal(std::io::Error::other(format!(
            "storage endpoint {name}: {e}"
        )))
    })?;
    let cfg = S3Config {
        endpoint: r.1,
        bucket: r.3,
        region: r.2,
        access_key: r.6,
        secret_key: secret,
        path_style: r.5,
        prefix: if r.4.is_empty() {
            String::new()
        } else {
            format!("{}/", r.4)
        },
        ..S3Config::default()
    };
    S3Store::new(&cfg)
        .ok_or_else(|| Error::internal(std::io::Error::other("endpoint misconfigured")))
}

/// Upload, read back and delete a probe object; the outcome is recorded on the endpoint.
async fn probe(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let st = store(&s, &name).await?;
    let key = format!("{}musubi-probe-{}.txt", st.prefix(), uuid::Uuid::now_v7());
    let body = format!(
        "musubi storage probe {}",
        OffsetDateTime::now_utc().unix_timestamp()
    );
    let started = std::time::Instant::now();
    let outcome: Result<()> = async {
        st.put(&key, body.clone().into_bytes(), "text/plain")
            .await?;
        let back = st.get(&key).await?;
        if back != body.as_bytes() {
            return Err(Error::Unavailable("probe object read back differs".into()));
        }
        st.delete(&key).await
    }
    .await;
    let ms = started.elapsed().as_millis() as i64;
    let (status, error) = match &outcome {
        Ok(()) => ("ok", None),
        Err(e) => ("error", Some(e.to_string())),
    };
    sqlx::query(
        "update musubi.storage_endpoints set last_test_at = now(), last_status = $2, last_error = $3 where name = $1",
    )
    .bind(&name)
    .bind(status)
    .bind(&error)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "storage_endpoint.tested",
            None,
            json!({"name": name, "status": status, "error": error, "ms": ms}),
        )
        .await;
    Ok(Json(
        json!({"name": name, "status": status, "error": error, "ms": ms, "key": key}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_key_hint_is_char_safe() {
        assert_eq!(access_key_hint("AKIAEXAMPLE"), "…MPLE");
        assert_eq!(access_key_hint("abcd"), "…");
        assert_eq!(access_key_hint(""), "…");
        assert_eq!(access_key_hint("ключ-доступа"), "…тупа");
        assert_eq!(access_key_hint("ab€"), "…");
    }
}
