//! Admin API: the backend of the operator console. Server-key only.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use musubi_storage::{Caller, DeleteOp, Object, WriteOp};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;

pub const KICK_TOPIC: &str = "admin.kick";
/// Payload: node uuid. The addressed node drains and exits as on SIGTERM.
pub const DRAIN_TOPIC: &str = "admin.drain";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/runtime/modules", get(modules_list))
        .route(
            "/v1/admin/runtime/modules/{name}",
            put(module_put)
                .delete(module_delete)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::modules::MAX_MODULE_BYTES,
                )),
        )
        .route("/v1/admin/cluster/matches", get(cluster_matches))
        .route("/v1/admin/cluster/worlds", get(cluster_worlds))
        .route(
            "/v1/admin/worlds/{w}/cells/{x}/{y}/host",
            post(world_cell_host),
        )
        .route("/v1/admin/worlds/{w}/entities", get(world_entities))
        .route("/v1/admin/cluster/nodes/{id}/drain", post(drain_node))
        .route("/v1/admin/retention/run", post(retention_run))
        .route("/v1/admin/runtime/schedules", get(schedules_status))
        .route(
            "/v1/admin/runtime/schedules/{name}/run",
            post(schedules_run),
        )
        .route(
            "/v1/admin/auth/locks/{email}",
            get(login_lock).delete(clear_login_lock),
        )
        .route("/v1/admin/jwt/keys", get(jwt_keys).post(jwt_rotate))
        .route("/v1/admin/jwt/rotate", post(jwt_rotate))
        .route("/v1/admin/jwt/keys/{kid}", delete(jwt_retire))
        .route("/v1/admin/keys", get(keys_list).post(keys_issue))
        .route("/v1/admin/keys/{id}", delete(keys_revoke))
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
pub(crate) async fn kick_everywhere(s: &AppState, id: AccountId) -> usize {
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
    s.banned.set(id, true, s.cluster.as_ref()).await;
    s.events.emit("account.banned", Some(id), json!({})).await;
    let kicked = kick_everywhere(&s, id).await;
    Ok(Json(json!({ "banned": true, "kicked_local": kicked })))
}

async fn unban(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.accounts.set_disabled(AccountId(id), false).await?;
    s.banned.set(AccountId(id), false, s.cluster.as_ref()).await;
    s.events
        .emit("account.unbanned", Some(AccountId(id)), json!({}))
        .await;
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
    s.events
        .emit("account.purged", Some(id), json!({ "rows": removed }))
        .await;
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
                ttl_secs: None,
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

async fn modules_list(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::modules::list(&s).await?))
}

/// Upload a module (`.wasm` binary, WAT text or a component) under `name`; it goes live on every
/// node without a restart. Re-uploading replaces the previous version.
async fn module_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::modules::put(&s, &name, &body).await?))
}

async fn module_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !crate::modules::delete(&s, &name).await? {
        return Err(Error::NotFound(format!("module {name}")).into());
    }
    Ok(Json(json!({ "deleted": name })))
}

/// Every match of this cluster with its current home (from the snapshot store), newest first.
async fn cluster_matches(
    _k: ServerKey,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(
        Uuid,
        String,
        String,
        i64,
        Option<Uuid>,
        time::OffsetDateTime,
    )> = sqlx::query_as(
        "select match_id, module, label, tick, node_id, updated_at from musubi.match_snapshots
         where cluster_group = $1 order by updated_at desc limit 1000",
    )
    .bind(&s.cfg.node.cluster_group)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let matches: Vec<Value> = rows
        .into_iter()
        .map(|(id, module, label, tick, node, at)| {
            json!({ "match_id": id, "module": module, "label": label, "tick": tick, "node_id": node, "updated_at": at.unix_timestamp() })
        })
        .collect();
    Ok(Json(
        json!({ "matches": matches, "local": s.matches.count() }),
    ))
}

/// Every world of this cluster with its home node.
async fn cluster_worlds(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(String, Option<Uuid>, time::OffsetDateTime)> = sqlx::query_as(
        "select name, node_id, created_at from musubi.worlds where cluster_group = $1 order by name",
    )
    .bind(&s.cfg.node.cluster_group)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let worlds: Vec<Value> = rows
        .into_iter()
        .map(|(name, node, at)| json!({ "name": name, "node_id": node, "created_at": at.unix_timestamp() }))
        .collect();
    Ok(Json(json!({ "worlds": worlds, "local": s.worlds.list() })))
}

/// Drain a node through the API: the local node starts its SIGTERM path, another node is told
/// over the bus. The process exits when the drain completes; the orchestrator restarts it.
async fn drain_node(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let me = s.cluster.as_ref().map(|c| c.node_id().0);
    if me == Some(id) || (me.is_none() && id.is_nil()) {
        s.shutdown.notify_one();
        return Ok(Json(json!({ "draining": id, "local": true })));
    }
    let Some(c) = &s.cluster else {
        return Err(Error::NotFound("single node: use the nil uuid for self".into()).into());
    };
    let known = c.nodes().await?.iter().any(|n| n.id.0 == id && n.alive);
    if !known {
        return Err(Error::NotFound(format!("node {id} is not alive")).into());
    }
    c.publish(DRAIN_TOPIC, &id).await?;
    Ok(Json(json!({ "draining": id, "local": false })))
}

/// React to `admin.drain` addressed to this node.
pub fn start_drain_listener(s: AppState) {
    let Some(c) = s.cluster.clone() else {
        return;
    };
    let mut rx = c.subscribe();
    let me = c.node_id().0;
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) if m.topic == DRAIN_TOPIC => {
                    if serde_json::from_value::<Uuid>(m.payload).ok() == Some(me) {
                        tracing::warn!("drain requested through the admin api");
                        s.shutdown.notify_one();
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}

#[derive(Deserialize)]
struct IssueKey {
    name: String,
    role: crate::keys::Role,
}

async fn keys_list(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "keys": crate::keys::ServerKeys::list(&s.db).await? }),
    ))
}

/// The secret is returned only here; store it on the caller's side.
async fn keys_issue(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(b): Json<IssueKey>,
) -> Result<Json<Value>, ApiError> {
    let (caller, secret) = s.keys.issue(&s.db, &b.name, b.role).await?;
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::keys::KEYS_TOPIC, &caller.id).await;
    }
    Ok(Json(
        json!({ "id": caller.id, "name": caller.name, "role": caller.role, "secret": secret }),
    ))
}

async fn keys_revoke(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    if !s.keys.revoke(&s.db, id).await? {
        return Err(Error::NotFound("key".into()).into());
    }
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::keys::KEYS_TOPIC, &id).await;
    }
    Ok(Json(json!({ "revoked": id })))
}

/// Force a retention pass now (the leader also runs one every `retention.interval_secs`).
async fn retention_run(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "removed": crate::retention::run(&s.db, &s.cfg.retention).await? }),
    ))
}

async fn jwt_keys(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::jwt::list(&s.db, &s.auth).await?))
}

/// Generate a new ES256 key; it signs from now on everywhere, older keys keep verifying.
async fn jwt_rotate(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let kid = crate::jwt::rotate(&s.db, &s.auth).await?;
    if let Some(c) = &s.cluster {
        let _ = c
            .publish(crate::jwt::JWT_TOPIC, &json!({ "kid": kid }))
            .await;
    }
    Ok(Json(json!({ "kid": kid })))
}

/// Stop signing with a key; tokens it signed stay valid until they expire, then it is pruned.
async fn jwt_retire(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(kid): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !crate::jwt::retire(&s.db, &s.auth, &kid).await? {
        return Err(musubi_core::Error::NotFound("jwt key".into()).into());
    }
    if let Some(c) = &s.cluster {
        let _ = c
            .publish(
                crate::jwt::JWT_TOPIC,
                &json!({ "kid": kid, "retired": true }),
            )
            .await;
    }
    Ok(Json(json!({ "ok": true })))
}

/// Current email-login lockout for an address (404 when none).
async fn login_lock(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(email): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match s.auth.login_lock(&email).await? {
        Some(lock) => Ok(Json(serde_json::to_value(lock).unwrap_or_default())),
        None => Err(musubi_core::Error::NotFound("no active lock".into()).into()),
    }
}

/// Lift an email-login lockout and forget its failure count.
async fn clear_login_lock(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(email): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let cleared = s.auth.clear_login_lock(&email).await?;
    Ok(Json(serde_json::json!({ "cleared": cleared })))
}

/// Configured cron schedules with next/last run info.
async fn schedules_status(
    _k: ServerKey,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.schedules.status()))
}

/// Run `schedule_<name>` now on this node, outside its cron.
async fn schedules_run(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let out = s.schedules.run_one(&s.runtime, &name, None).await?;
    Ok(Json(json!({ "name": name, "output": out })))
}

#[derive(Deserialize)]
struct CellHostReq {
    node_id: Uuid,
}

/// Assign a cell of a world to a node: the cell's entities and simulators move there, the rest of
/// the world stays where it is. The directory and the bus carry the assignment to every node.
/// This node's shard of the world: every entity with cell, position and transit state.
async fn world_entities(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(w): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.worlds.local_entities(&w)?))
}

async fn world_cell_host(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((w, x, y)): Path<(String, i32, i32)>,
    Json(r): Json<CellHostReq>,
) -> Result<Json<Value>, ApiError> {
    let cell = musubi_mesh::CellId { x, y };
    if s.worlds.home_of(&w).await?.is_none() {
        return Err(Error::NotFound(format!("world {w}")).into());
    }
    s.worlds
        .publish_cell_host(&w, cell, Some(r.node_id))
        .await?;
    s.fanout.cell_assign(&w, cell, r.node_id);
    let hosted = if r.node_id == s.worlds.node_id() {
        s.worlds.host_cell(&w, cell).await?
    } else {
        vec![]
    };
    Ok(Json(
        json!({ "world": w, "cell": cell, "node_id": r.node_id, "hosted_here": hosted }),
    ))
}
