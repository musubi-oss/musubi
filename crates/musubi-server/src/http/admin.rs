//! Admin API: the backend of the operator console. Server-key only.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_storage::{Caller, DeleteOp, Object, WriteOp};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;

pub const KICK_TOPIC: &str = "admin.kick";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/accounts", get(accounts))
        .route(
            "/v1/admin/accounts/{id}",
            get(account).delete(purge_account),
        )
        .route("/v1/admin/accounts/{id}/ban", post(ban))
        .route("/v1/admin/accounts/{id}/unban", post(unban))
        .route("/v1/admin/accounts/{id}/kick", post(kick))
        .route("/v1/admin/accounts/{id}/export", get(export))
        .route("/v1/admin/accounts/{id}/sessions", get(sessions))
        .route(
            "/v1/admin/accounts/{id}/identities/{provider}/{subject}",
            delete(unlink),
        )
        .route("/v1/admin/storage/{collection}", get(storage_list))
        .route(
            "/v1/admin/storage/{collection}/{key}",
            get(storage_get).put(storage_put).delete(storage_delete),
        )
        .route("/v1/admin/config", get(config))
        .route("/v1/admin/audit", get(crate::audit::list))
}

#[derive(Deserialize)]
struct SearchQ {
    q: Option<String>,
    #[serde(default)]
    include_deleted: bool,
    limit: Option<i64>,
    cursor: Option<Uuid>,
}

async fn accounts(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<SearchQ>,
) -> Result<Json<Value>, ApiError> {
    let items = s
        .accounts
        .search(
            q.q.as_deref(),
            q.include_deleted,
            q.limit.unwrap_or(50),
            q.cursor,
        )
        .await?;
    let cursor = items.last().map(|a| a.id.0);
    Ok(Json(json!({ "accounts": items, "cursor": cursor })))
}

async fn account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let id = AccountId(id);
    let status = s.accounts.status(id).await?;
    let account = s.accounts.get(id).await.ok();
    let presences = s.hub.presences_of(id);
    Ok(Json(json!({
        "status": status,
        "account": account,
        "online": presences,
    })))
}

/// Kick the account's live realtime sessions on every node.
async fn kick_everywhere(s: &AppState, id: AccountId) -> usize {
    let local = s.hub.kick_account(id);
    if let Some(c) = &s.cluster {
        if let Err(e) = c.publish(KICK_TOPIC, &json!({ "account_id": id })).await {
            tracing::warn!(error = %e, "admin kick fan-out failed");
        }
    }
    local
}

async fn ban(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let id = AccountId(id);
    s.accounts.set_disabled(id, true).await?;
    let kicked = kick_everywhere(&s, id).await;
    Ok(Json(json!({ "banned": true, "kicked_local": kicked })))
}

async fn unban(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.accounts.set_disabled(AccountId(id), false).await?;
    Ok(Json(json!({ "banned": false })))
}

async fn kick(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let kicked = kick_everywhere(&s, AccountId(id)).await;
    Ok(Json(json!({ "kicked_local": kicked })))
}

async fn export(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.accounts.export(AccountId(id)).await?))
}

async fn purge_account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let id = AccountId(id);
    kick_everywhere(&s, id).await;
    let removed = s.accounts.purge(id).await?;
    Ok(Json(json!({ "purged": true, "rows": removed })))
}

async fn sessions(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(t) - 'refresh_hash' order by t.created_at desc), '[]'::jsonb)
         from (select * from musubi.sessions where account_id = $1 order by created_at desc limit 200) t",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "sessions": rows, "online": s.hub.presences_of(AccountId(id)) }),
    ))
}

async fn unlink(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((id, provider, subject)): Path<(Uuid, String, String)>,
) -> Result<Json<Value>, ApiError> {
    s.auth.unlink(AccountId(id), &provider, &subject).await?;
    Ok(Json(json!({ "unlinked": true })))
}

#[derive(Deserialize)]
struct StorageListQ {
    owner_id: Option<Uuid>,
    limit: Option<i64>,
    cursor: Option<String>,
}

async fn storage_list(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(collection): Path<String>,
    Query(q): Query<StorageListQ>,
) -> Result<Json<Value>, ApiError> {
    let (objects, cursor) = s
        .storage
        .list(
            Caller::Server,
            &collection,
            q.owner_id,
            q.limit.unwrap_or(50),
            q.cursor.as_deref(),
        )
        .await?;
    Ok(Json(json!({ "objects": objects, "cursor": cursor })))
}

#[derive(Deserialize)]
struct OwnerQ {
    owner_id: Option<Uuid>,
}

async fn storage_get(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((collection, key)): Path<(String, String)>,
    Query(q): Query<OwnerQ>,
) -> Result<Json<Object>, ApiError> {
    let owner = q.owner_id.unwrap_or(Uuid::nil());
    let mut objs = s
        .storage
        .read(
            Caller::Server,
            &[musubi_storage::ReadRef {
                collection,
                key,
                owner_id: Some(owner),
            }],
        )
        .await?;
    objs.pop()
        .map(Json)
        .ok_or_else(|| Error::NotFound("object".into()).into())
}

#[derive(Deserialize)]
struct PutBody {
    owner_id: Option<Uuid>,
    value: Value,
    version: Option<String>,
    #[serde(default = "default_read")]
    read_permission: i16,
    #[serde(default)]
    write_permission: i16,
}

fn default_read() -> i16 {
    1
}

async fn storage_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((collection, key)): Path<(String, String)>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    let acks = s
        .storage
        .write(
            Caller::Server,
            Some(b.owner_id.unwrap_or(Uuid::nil())),
            &[WriteOp {
                collection,
                key,
                value: b.value,
                version: b.version,
                read_permission: b.read_permission,
                write_permission: b.write_permission,
            }],
        )
        .await?;
    Ok(Json(json!({ "acks": acks })))
}

async fn storage_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((collection, key)): Path<(String, String)>,
    Query(q): Query<OwnerQ>,
) -> Result<Json<Value>, ApiError> {
    s.storage
        .delete(
            Caller::Server,
            Some(q.owner_id.unwrap_or(Uuid::nil())),
            &[DeleteOp {
                collection,
                key,
                version: None,
            }],
        )
        .await?;
    Ok(Json(json!({ "deleted": true })))
}

/// Effective config with secrets redacted (any key mentioning key/secret/password/pem/token).
async fn config(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut v = serde_json::to_value(&*s.cfg).map_err(Error::internal)?;
    redact(&mut v, "");
    Ok(Json(v))
}

fn redact(v: &mut Value, key: &str) {
    match v {
        Value::Object(m) => {
            for (k, child) in m.iter_mut() {
                redact(child, k);
            }
        }
        Value::String(s) if is_secret_key(key) && !s.is_empty() => *s = "***".into(),
        _ => {}
    }
}

fn is_secret_key(k: &str) -> bool {
    let k = k.to_ascii_lowercase();
    ["key", "secret", "password", "pem", "token"]
        .iter()
        .any(|w| k.contains(w))
        && !k.ends_with("_path")
        && !k.ends_with("_url")
        && k != "key_id"
}
