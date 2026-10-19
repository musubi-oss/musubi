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
        .route(
            "/v1/admin/runtime/modules/{name}/canary",
            post(canary_put)
                .delete(canary_delete)
                .layer(axum::extract::DefaultBodyLimit::max(
                    crate::modules::MAX_MODULE_BYTES,
                )),
        )
        .route(
            "/v1/admin/runtime/modules/{name}/canary/promote",
            post(canary_promote),
        )
        .route(
            "/v1/admin/runtime/modules/{name}/env",
            get(module_env_get)
                .put(module_env_put)
                .delete(module_env_clear),
        )
        .route(
            "/v1/admin/runtime/modules/{name}/env/{key}",
            delete(module_env_delete),
        )
        .route(
            "/v1/admin/runtime/modules/{name}/versions",
            get(module_versions),
        )
        .route(
            "/v1/admin/runtime/modules/{name}/versions/{version}/rollback",
            post(module_rollback),
        )
        .route("/v1/admin/cluster/matches", get(cluster_matches))
        .route("/v1/admin/cluster/worlds", get(cluster_worlds))
        .route(
            "/v1/admin/worlds/{w}/cells/{x}/{y}/host",
            post(world_cell_host),
        )
        .route("/v1/admin/worlds/{w}/entities", get(world_entities))
        .route("/v1/admin/worlds/{w}/at", get(world_at))
        .route("/v1/admin/worlds/{w}/history", get(world_history))
        .route("/v1/admin/worlds/{w}/verify", post(world_verify))
        .route("/v1/admin/worlds/{w}/snapshot", post(world_snapshot))
        .route("/v1/admin/cluster/nodes/{id}/drain", post(drain_node))
        .route("/v1/admin/retention/run", post(retention_run))
        .route("/v1/admin/runtime/schedules", get(schedules_status))
        .route(
            "/v1/admin/runtime/schedules/{name}/run",
            post(schedules_run),
        )
        .route("/v1/admin/auth/queue", get(queue_stats).delete(queue_flush))
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
            get(account).patch(patch_account).delete(purge_account),
        )
        .route("/v1/admin/accounts/{id}/ban", post(ban).get(get_ban))
        .route("/v1/admin/accounts/{id}/unban", post(unban))
        .route("/v1/admin/accounts/{id}/kick", post(kick))
        .route("/v1/admin/accounts/{id}/export", get(export))
        .route("/v1/admin/accounts/{id}/restore", post(restore))
        .route(
            "/v1/admin/accounts/{id}/limits",
            get(account_limits_get)
                .put(account_limits_put)
                .delete(account_limits_delete),
        )
        .route("/v1/admin/keys/{id}/limits", put(key_limits_put))
        .route("/v1/admin/accounts/{id}/sessions", get(sessions))
        .route(
            "/v1/admin/accounts/{id}/identities/{provider}/{subject}",
            delete(unlink),
        )
        .route("/v1/admin/storage_schemas", get(schemas_list))
        .route(
            "/v1/admin/storage_schemas/{collection}",
            get(schema_get).put(schema_put).delete(schema_delete),
        )
        .route(
            "/v1/admin/storage_schemas/{collection}/migrate",
            post(schema_migrate),
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
    /// `provider:subject` of a linked identity (device, email, steam, google, apple, oidc:<name>, passkey).
    identity: Option<String>,
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
        .search_by(
            q.q.as_deref(),
            q.identity.as_deref(),
            q.include_deleted,
            q.limit.unwrap_or(50),
            q.cursor,
        )
        .await?;
    let cursor = items.last().map(|a| a.id.0);
    Ok(Json(json!({ "accounts": items, "cursor": cursor })))
}

/// Console edit of a player's profile: any subset of username / display_name / avatar_url / lang /
/// timezone / location / metadata; the same validation as the player's own PATCH /v1/account.
async fn patch_account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(u): Json<musubi_account::UpdateAccount>,
) -> Result<Json<Value>, ApiError> {
    let id = AccountId(id);
    s.accounts.update(id, u).await?;
    let account = s.accounts.get(id).await?;
    s.events
        .emit("account.updated", Some(id), json!({ "by": "admin" }))
        .await;
    Ok(Json(
        serde_json::to_value(account).map_err(Error::internal)?,
    ))
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

/// Mirror a ban into Aurix, best effort: the account is disabled here regardless, this only
/// drops its live voice sessions and keeps it out of channels until the unban follows.
fn voice_ban(s: &AppState, id: AccountId, banned: bool) {
    let Some(v) = s.voice.clone() else { return };
    if !s.cfg.aurix.moderation_sync {
        return;
    }
    tokio::spawn(async move {
        let r = if banned {
            v.ban_user(&id.to_string(), "account banned").await
        } else {
            v.unban_user(&id.to_string()).await
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, account = %id, banned, "aurix ban sync failed");
        }
    });
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

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct BanBody {
    /// Internal note for moderators.
    pub reason: Option<String>,
    /// Shown to the player in the rejection.
    pub message: Option<String>,
    /// Lifted automatically after this many seconds; absent = permanent.
    pub until_secs: Option<i64>,
    /// Keep the account working but make chat and matchmaking silently ignore it.
    pub shadow: bool,
    /// Feature ban: the account keeps working except for these features
    /// (`chat`, `matchmaking`, `groups`, `ugc`, `trading`, `leaderboards`, `voice`, `friends`).
    #[serde(default)]
    pub features: Vec<String>,
    /// Code from the reason catalog (`PUT /v1/admin/bans/reasons/{code}`): fills in reason,
    /// message, duration and features that the body leaves out.
    #[serde(default)]
    pub reason_code: Option<String>,
    /// Also ban every device / address in the account's login registry.
    #[serde(default)]
    pub ban_devices: bool,
    #[serde(default)]
    pub ban_ips: bool,
}

/// `POST /v1/admin/accounts/{id}/ban` — body optional (legacy: permanent hard ban).
async fn ban(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    body: Option<Json<BanBody>>,
) -> Result<Json<Value>, ApiError> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    Ok(Json(apply_ban(&s, AccountId(id), b).await?))
}

/// Apply (or replace) a ban: the row, the account flag, kicks, voice and the cluster-wide cache.
/// Shared with player-report decisions.
pub(crate) async fn apply_ban(s: &AppState, id: AccountId, b: BanBody) -> Result<Value, ApiError> {
    let mut b = b;
    if let Some(code) = b.reason_code.as_deref() {
        let (code, title, message, dur, _sev, features, _) = super::bans::load_reason(s, code)
            .await?
            .ok_or_else(|| Error::NotFound(format!("ban reason {code}")))?;
        if b.reason.is_none() {
            b.reason = Some(title);
        }
        if b.message.is_none() && !message.is_empty() {
            b.message = Some(message);
        }
        if b.until_secs.is_none() {
            b.until_secs = dur;
        }
        if b.features.is_empty() {
            b.features = features;
        }
        b.reason_code = Some(code);
    }
    super::bans::validate_features(&b.features)?;
    if b.shadow && !b.features.is_empty() {
        return Err(
            Error::InvalidArgument("a ban is either shadow or feature-scoped".into()).into(),
        );
    }
    if b.until_secs.is_some_and(|u| u <= 0) {
        return Err(Error::InvalidArgument("until_secs must be positive".into()).into());
    }
    if b.message.as_ref().is_some_and(|m| m.chars().count() > 512) {
        return Err(Error::InvalidArgument("message over 512 chars".into()).into());
    }
    let until = b
        .until_secs
        .map(|u| time::OffsetDateTime::now_utc().unix_timestamp() + u);
    let info = crate::bans::BanInfo {
        reason: b.reason.clone(),
        message: b.message.clone(),
        until,
        shadow: b.shadow,
        features: b.features.clone(),
    };
    sqlx::query(
        "insert into musubi.account_bans (account_id, reason, message, until, shadow, actor, features, reason_code)
         values ($1, $2, $3, to_timestamp($4), $5, 'server', $6, $7)
         on conflict (account_id) do update set reason = excluded.reason,
            message = excluded.message, until = excluded.until, shadow = excluded.shadow,
            actor = excluded.actor, features = excluded.features,
            reason_code = excluded.reason_code, created_at = now()",
    )
    .bind(id.0)
    .bind(&b.reason)
    .bind(&b.message)
    .bind(until.map(|u| u as f64))
    .bind(b.shadow)
    .bind(&b.features)
    .bind(&b.reason_code)
    .execute(&s.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => {
            Error::NotFound("account".into())
        }
        e => Error::internal(e),
    })?;
    let mut kicked = 0;
    if b.shadow {
        // a previous hard ban on the same account becomes a shadow one
        s.accounts.set_disabled(id, false).await?;
        voice_ban(s, id, false);
    } else if !b.features.is_empty() {
        // feature ban: the account stays usable, only the listed features deny
        s.accounts.set_disabled(id, false).await?;
        voice_ban(s, id, b.features.iter().any(|f| f == "voice"));
    } else {
        s.accounts.set_disabled(id, true).await?;
        kicked = kick_everywhere(s, id).await;
        voice_ban(s, id, true);
    }
    s.banned.set(id, Some(info), s.cluster.as_ref()).await;
    let (devices_banned, ips_banned) = if b.ban_devices || b.ban_ips {
        super::bans::ban_registry(
            s,
            id.0,
            b.ban_devices,
            b.ban_ips,
            b.reason.as_deref(),
            b.message.as_deref(),
            until,
        )
        .await?
    } else {
        (0, 0)
    };
    s.events
        .emit(
            "account.banned",
            Some(id),
            json!({
                "reason": b.reason, "reason_code": b.reason_code, "until": until,
                "shadow": b.shadow, "features": b.features,
            }),
        )
        .await;
    Ok(json!({
        "banned": true, "shadow": b.shadow, "features": b.features, "reason_code": b.reason_code,
        "until": until, "kicked_local": kicked,
        "devices_banned": devices_banned, "ips_banned": ips_banned,
    }))
}

/// Lift a ban: used by `unban` and by the leader sweep for expired ones.
pub(crate) async fn lift_ban(s: &AppState, id: AccountId, auto: bool) -> Result<(), Error> {
    sqlx::query("delete from musubi.account_bans where account_id = $1")
        .bind(id.0)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.accounts.set_disabled(id, false).await?;
    s.banned.set(id, None, s.cluster.as_ref()).await;
    voice_ban(s, id, false);
    s.events
        .emit("account.unbanned", Some(id), json!({ "auto": auto }))
        .await;
    Ok(())
}

async fn unban(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    lift_ban(&s, AccountId(id), false).await?;
    Ok(Json(json!({ "banned": false })))
}

/// `GET /v1/admin/accounts/{id}/ban`
async fn get_ban(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    type Row = (
        Option<String>,
        Option<String>,
        Option<i64>,
        bool,
        String,
        Vec<String>,
        Option<String>,
    );
    let row: Option<Row> = sqlx::query_as(
        "select reason, message, (extract(epoch from until))::bigint, shadow,
                to_char(created_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),
                features, reason_code
         from musubi.account_bans where account_id = $1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((reason, message, until, shadow, created_at, features, reason_code)) = row else {
        return Err(Error::NotFound("ban".into()).into());
    };
    Ok(Json(json!({
        "account_id": id, "reason": reason, "reason_code": reason_code, "message": message,
        "until": until, "shadow": shadow, "features": features, "created_at": created_at,
        "active": until.is_none_or(|u| u > time::OffsetDateTime::now_utc().unix_timestamp()),
    })))
}

/// Expired bans → lifted (leader sweep, once a minute).
pub(crate) async fn lift_expired_bans(s: &AppState) -> Result<usize, Error> {
    let ids: Vec<(Uuid,)> =
        sqlx::query_as("select account_id from musubi.account_bans where until <= now()")
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    for (id,) in &ids {
        lift_ban(s, AccountId(*id), true).await?;
    }
    Ok(ids.len())
}

/// Soft-deleted accounts older than `[retention] deleted_account_days` are purged for good.
pub(crate) async fn purge_deleted_accounts(s: &AppState) -> Result<usize, Error> {
    let days = s.cfg.retention.deleted_account_days;
    if days == 0 {
        return Ok(0);
    }
    let ids: Vec<(Uuid,)> = sqlx::query_as(
        "select id from musubi.accounts
         where deleted_at is not null and deleted_at < now() - make_interval(days => $1::int)
         limit 200",
    )
    .bind(days as i32)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    for (id,) in &ids {
        let id = AccountId(*id);
        s.accounts.purge(id).await?;
        s.events
            .emit("account.purged", Some(id), json!({ "after_days": days }))
            .await;
    }
    Ok(ids.len())
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

#[derive(Deserialize)]
struct RestoreBody {
    export: Value,
    /// Restore only these tables of the bundle (default: every known table it contains and the
    /// account row; list `account` to include the row explicitly).
    tables: Option<Vec<String>>,
    /// `replace` (default) clears the account's rows of each restored table first; `merge`
    /// keeps live rows and adds the missing ones.
    #[serde(default = "default_restore_mode")]
    mode: String,
    /// Recreate the account row from the bundle when it was purged.
    #[serde(default)]
    create_missing: bool,
}

fn default_restore_mode() -> String {
    "replace".into()
}

/// Point-in-time restore from an export bundle (see `AccountService::restore`).
async fn restore(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(b): Json<RestoreBody>,
) -> Result<Json<Value>, ApiError> {
    let replace = match b.mode.as_str() {
        "replace" => true,
        "merge" => false,
        other => {
            return Err(Error::InvalidArgument(format!(
                "mode must be replace or merge, not {other}"
            ))
            .into())
        }
    };
    let out = s
        .accounts
        .restore(
            AccountId(id),
            &b.export,
            b.tables.as_deref(),
            replace,
            b.create_missing,
        )
        .await?;
    s.events
        .emit(
            "account.restored",
            Some(AccountId(id)),
            json!({ "mode": b.mode, "tables": out["tables"], "account": out["account"] }),
        )
        .await;
    Ok(Json(out))
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
                merge: None,
                base: None,
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
#[derive(Deserialize, Default)]
struct ForceQ {
    /// Publish even when required module env is unbound (P1-63).
    #[serde(default)]
    force: bool,
}

async fn module_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<ForceQ>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    crate::module_env::check_publish(&s, &name, q.force).await?;
    Ok(Json(crate::modules::put(&s, &name, &body).await?))
}

#[derive(Deserialize)]
struct ModuleEnvBody {
    values: serde_json::Map<String, Value>,
    /// Keys to seal even though the IDL does not mark them secret (or undeclared ones).
    #[serde(default)]
    secrets: Vec<String>,
}

async fn module_env_get(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::module_env::describe(&s, &name).await?))
}

async fn module_env_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
    Json(b): Json<ModuleEnvBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        crate::module_env::bind(&s, &name, &b.values, &b.secrets, "server_key").await?,
    ))
}

async fn module_env_clear(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = crate::module_env::unbind(&s, &name, None, "server_key").await?;
    Ok(Json(json!({ "module": name, "removed": n })))
}

async fn module_env_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((name, key)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    if crate::module_env::unbind(&s, &name, Some(&key), "server_key").await? == 0 {
        return Err(Error::NotFound(format!("env `{name}/{key}`")).into());
    }
    Ok(Json(json!({ "module": name, "key": key, "removed": true })))
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
    /// Resource scopes (`<area>`, `<area>:read`, `<area>:write`); empty = whole role.
    #[serde(default)]
    scopes: Vec<String>,
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
    let (caller, secret) = s
        .keys
        .issue_with(&s.db, &b.name, b.role, None, None, &b.scopes)
        .await?;
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::keys::KEYS_TOPIC, &caller.id).await;
    }
    Ok(Json(
        json!({ "id": caller.id, "name": caller.name, "role": caller.role, "secret": secret }),
    ))
}

#[derive(Deserialize)]
struct AccountLimitBody {
    #[serde(flatten)]
    limit: crate::limits::AccountLimit,
    #[serde(default)]
    note: String,
}

/// The account's rate-limit override (404 when it runs on the `[limits]` defaults).
async fn account_limits_get(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(Option<i32>, Option<i32>, Option<i32>, Option<i32>, Option<i64>, String, time::OffsetDateTime)> = sqlx::query_as(
        "select http_per_second, http_burst, rpc_per_second, rpc_burst, daily, note, updated_at from musubi.account_limits where account_id = $1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((hp, hb, rp, rb, d, note, at)) = row else {
        return Err(Error::NotFound("no limit override for this account".into()).into());
    };
    Ok(Json(json!({
        "account_id": id, "http_per_second": hp, "http_burst": hb, "rpc_per_second": rp,
        "rpc_burst": rb, "daily": d, "note": note, "updated_at": at.unix_timestamp(),
        "defaults": {
            "http_per_second": s.cfg.limits.http_per_second_per_account, "http_burst": s.cfg.limits.http_burst,
            "rpc_per_second": s.cfg.limits.rpc_per_second_per_account, "rpc_burst": s.cfg.limits.rpc_burst,
        },
    })))
}

/// Override the account's budgets (`http_per_second`, `http_burst`, `rpc_per_second`,
/// `rpc_burst`, `daily`; omitted = default). Takes effect on every node at once.
async fn account_limits_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(b): Json<AccountLimitBody>,
) -> Result<Json<Value>, ApiError> {
    if b.limit.is_empty() {
        return Err(Error::InvalidArgument(
            "set at least one of http_per_second, http_burst, rpc_per_second, rpc_burst, daily (or DELETE to clear)".into(),
        )
        .into());
    }
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from musubi.accounts where id = $1)")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if !exists {
        return Err(Error::NotFound("account".into()).into());
    }
    s.limits.set_override(&s.db, id, b.limit, &b.note).await?;
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::limits::LIMITS_TOPIC, &id).await;
    }
    s.events
        .emit(
            "account.limits",
            Some(AccountId(id)),
            json!({ "limit": b.limit, "note": b.note }),
        )
        .await;
    Ok(Json(
        json!({ "account_id": id, "limit": b.limit, "note": b.note }),
    ))
}

async fn account_limits_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let cleared = s.limits.clear_override(&s.db, id).await?;
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::limits::LIMITS_TOPIC, &id).await;
    }
    Ok(Json(json!({ "account_id": id, "cleared": cleared })))
}

/// Budget of one issued server key: `per_second`, `burst`, `daily` (omitted = the
/// `[limits] key_*` defaults; an empty body clears the override).
async fn key_limits_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(l): Json<crate::keys::KeyLimit>,
) -> Result<Json<Value>, ApiError> {
    let limit = (l != crate::keys::KeyLimit::default()).then_some(l);
    if !s.keys.set_limit(&s.db, id, limit).await? {
        return Err(Error::NotFound("key".into()).into());
    }
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::keys::KEYS_TOPIC, &id).await;
    }
    Ok(Json(json!({ "id": id, "limit": limit })))
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

#[derive(Deserialize)]
struct AtQuery {
    tick: u64,
}

/// Time machine: the world's persistent entities as of `tick` (snapshot at or before it plus
/// journal up to it); 404 with the oldest retained tick when history does not reach that far.
async fn world_at(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(w): Path<String>,
    Query(q): Query<AtQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.worlds.entities_at(&w, q.tick).await?))
}

#[derive(Deserialize)]
struct HistoryQuery {
    entity: Option<Uuid>,
    #[serde(default)]
    from_tick: u64,
    to_tick: Option<u64>,
    #[serde(default = "default_history_limit")]
    limit: usize,
}

fn default_history_limit() -> usize {
    500
}

/// Journal ops with provenance (who/what wrote each persistent mutation and when).
async fn world_history(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(w): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        s.worlds
            .history(
                &w,
                q.entity,
                q.from_tick,
                q.to_tick.unwrap_or(u64::MAX),
                q.limit,
            )
            .await?,
    ))
}

/// Rebuild the world from snapshots + journal and diff it against the live state.
async fn world_verify(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(w): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(s.worlds.verify(&w).await?))
}

/// Snapshot this node's shard of the world now (also a history row for the time machine).
async fn world_snapshot(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(w): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let tick = s.worlds.snapshot_now(&w).await?;
    Ok(Json(json!({"world": w, "tick": tick})))
}

/// `?level=` picks a quadtree cell below the base grid (default 0).
#[derive(Deserialize, Default)]
pub struct CellLevelQuery {
    #[serde(default)]
    pub level: u8,
    /// Vertical index for `three_d` worlds (P1-57); 0 for flat worlds.
    #[serde(default)]
    pub z: i32,
}

async fn world_cell_host(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((w, x, y)): Path<(String, i32, i32)>,
    Query(q): Query<CellLevelQuery>,
    Json(r): Json<CellHostReq>,
) -> Result<Json<Value>, ApiError> {
    let cell = musubi_mesh::CellId {
        x,
        y,
        level: q.level,
        z: q.z,
    };
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

/// Login queue health for the console: capacity, online, waiting, recently admitted.
async fn queue_stats(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let st = s.auth.queue_stats().await?;
    Ok(Json(serde_json::to_value(st).map_err(Error::internal)?))
}

/// Drop every queue ticket (waiting clients are told to log in again).
async fn queue_flush(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let dropped = s.auth.queue_flush().await?;
    Ok(Json(json!({ "dropped": dropped })))
}

#[derive(Deserialize)]
struct SchemaBody {
    #[serde(default)]
    version: i32,
    #[serde(default)]
    schema: Value,
    #[serde(default)]
    migrations: Vec<musubi_storage::schema::Migration>,
    #[serde(default)]
    indexes: Vec<musubi_storage::schema::IndexDef>,
    #[serde(default)]
    policy: Option<musubi_storage::policy::Policy>,
}

fn schema_json(sc: &musubi_storage::CollectionSchema) -> Result<Json<Value>, ApiError> {
    Ok(Json(serde_json::to_value(sc).map_err(Error::internal)?))
}

/// All collection contracts (schema, version, migrations, indexes).
async fn schemas_list(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "schemas": s.storage.schemas() })))
}

async fn schema_get(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(collection): Path<String>,
) -> Result<Json<Value>, ApiError> {
    match s.storage.schema_of(&collection) {
        Some(sc) => schema_json(&sc),
        None => Err(Error::NotFound(format!("no schema for {collection}")).into()),
    }
}

/// Set the contract: `schema` (JSON Schema subset) is enforced on every write/patch, `version` +
/// `migrations` move older objects forward on read, `indexes` become expression indexes used by
/// `$where` range filters.
async fn schema_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(collection): Path<String>,
    Json(b): Json<SchemaBody>,
) -> Result<Json<Value>, ApiError> {
    let sc = s
        .storage
        .put_schema(musubi_storage::CollectionSchema {
            collection,
            version: b.version,
            schema: b.schema,
            migrations: b.migrations,
            indexes: b.indexes,
            policy: b.policy,
        })
        .await?;
    schema_json(&sc)
}

async fn schema_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(collection): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let deleted = s.storage.delete_schema(&collection).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

#[derive(Deserialize)]
struct MigrateQuery {
    #[serde(default = "default_migrate_batch")]
    batch: i64,
}

fn default_migrate_batch() -> i64 {
    500
}

/// Rewrite one batch of objects still at an older schema version (call until `remaining` is 0).
async fn schema_migrate(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(collection): Path<String>,
    Query(q): Query<MigrateQuery>,
) -> Result<Json<Value>, ApiError> {
    let (migrated, remaining) = s.storage.migrate_collection(&collection, q.batch).await?;
    Ok(Json(
        json!({ "migrated": migrated, "remaining": remaining }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CanaryQuery {
    percent: i32,
    /// Publish even when required module env is unbound (P1-63).
    force: bool,
    /// Comma-separated account ids always routed to the canary.
    accounts: String,
    note: String,
}

/// Upload a canary build of a module (`?percent=10&accounts=a,b`): those accounts' RPCs run on it
/// cluster-wide while everyone else stays on the active build.
async fn canary_put(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<CanaryQuery>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, ApiError> {
    let mut accounts = Vec::new();
    for a in q
        .accounts
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        accounts.push(
            uuid::Uuid::parse_str(a)
                .map_err(|_| Error::InvalidArgument(format!("bad account id {a}")))?,
        );
    }
    crate::module_env::check_publish(&s, &name, q.force).await?;
    Ok(Json(
        crate::modules::put_canary(&s, &name, &body, q.percent, accounts, &q.note).await?,
    ))
}

async fn canary_delete(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !crate::modules::drop_canary(&s, &name).await? {
        return Err(Error::NotFound(format!("module {name} has no canary")).into());
    }
    Ok(Json(json!({ "name": name, "canary": false })))
}

async fn canary_promote(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::modules::promote_canary(&s, &name).await?))
}

async fn module_versions(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::modules::versions(&s, &name).await?))
}

async fn module_rollback(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((name, version)): Path<(String, i32)>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(crate::modules::rollback(&s, &name, version).await?))
}
