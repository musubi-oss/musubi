//! P1-25: limited access mode — login allow-list, maintenance and scheduled playtest windows.
//!
//! Every login path runs [`gate`] right after the account is resolved: the effective mode (base
//! policy or the active scheduled window) decides whether the fresh session survives. `limited`
//! lets allow-listed players (by account, identity, email domain or audience) and optionally
//! existing accounts in; `maintenance` lets only the bypass providers/audiences in. Denials are
//! recorded for the operator and published as `access.denied`.
use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_auth::Session;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::error::ApiError;
use super::extract::ServerKey;
use super::AppState;

pub const MODES: [&str; 3] = ["open", "limited", "maintenance"];
pub const KINDS: [&str; 4] = ["account", "identity", "email_domain", "audience"];
const CACHE_TTL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub name: Option<String>,
    /// Unix seconds, inclusive.
    pub start: i64,
    /// Unix seconds, exclusive.
    pub end: i64,
    pub mode: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    /// Base mode outside scheduled windows.
    #[serde(default = "open")]
    pub mode: String,
    /// Shown to denied players (and through `GET /v1/access`).
    #[serde(default)]
    pub message: Option<String>,
    /// Login providers the policy applies to; empty = all (e.g. a Steam-only closed beta).
    #[serde(default)]
    pub providers: Vec<String>,
    /// `limited`: accounts that already existed keep logging in (soft launch — no new signups).
    #[serde(default)]
    pub allow_existing_accounts: bool,
    /// Revoke the sessions of players who would be denied now when the policy changes.
    #[serde(default = "yes")]
    pub kick_on_change: bool,
    /// Providers that always pass (internal tooling, `custom` tokens for staff builds).
    #[serde(default)]
    pub bypass_providers: Vec<String>,
    /// Audiences that always pass (QA, press, employees).
    #[serde(default)]
    pub bypass_audiences: Vec<String>,
    /// Scheduled overrides (playtest weekends, maintenance slots); the latest-starting active
    /// window wins.
    #[serde(default)]
    pub windows: Vec<Window>,
    /// Hint for clients: when to try again when nothing is scheduled.
    #[serde(default)]
    pub retry_after_secs: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

fn open() -> String {
    "open".into()
}
fn yes() -> bool {
    true
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            mode: open(),
            message: None,
            providers: vec![],
            allow_existing_accounts: false,
            kick_on_change: true,
            bypass_providers: vec![],
            bypass_audiences: vec![],
            windows: vec![],
            retry_after_secs: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Effective {
    pub mode: String,
    pub message: Option<String>,
    pub window: Option<String>,
    /// When the effective mode changes next (a window opens or closes), unix seconds.
    pub next_change_at: Option<i64>,
}

impl Policy {
    pub fn validate(&self) -> Result<()> {
        if !MODES.contains(&self.mode.as_str()) {
            return Err(Error::InvalidArgument(
                "mode must be open, limited or maintenance".into(),
            ));
        }
        if self.windows.len() > 200 {
            return Err(Error::InvalidArgument("too many windows (max 200)".into()));
        }
        for w in &self.windows {
            if !MODES.contains(&w.mode.as_str()) {
                return Err(Error::InvalidArgument(format!(
                    "window mode '{}' must be open, limited or maintenance",
                    w.mode
                )));
            }
            if w.end <= w.start {
                return Err(Error::InvalidArgument(
                    "window end must be after start".into(),
                ));
            }
        }
        if self.retry_after_secs.is_some_and(|r| r < 0) {
            return Err(Error::InvalidArgument(
                "retry_after_secs must be >= 0".into(),
            ));
        }
        if let Some(m) = &self.message {
            if m.len() > 2000 {
                return Err(Error::InvalidArgument("message too long".into()));
            }
        }
        Ok(())
    }

    pub fn effective(&self, now: i64) -> Effective {
        let mut cur: Option<&Window> = None;
        let mut next: Option<i64> = None;
        for w in &self.windows {
            if w.start <= now && now < w.end {
                if cur.is_none_or(|c| w.start > c.start) {
                    cur = Some(w);
                }
            } else if w.start > now {
                next = Some(next.map_or(w.start, |n| n.min(w.start)));
            }
        }
        match cur {
            Some(w) => Effective {
                mode: w.mode.clone(),
                message: w.message.clone().or_else(|| self.message.clone()),
                window: w.name.clone(),
                next_change_at: Some(next.map_or(w.end, |n| n.min(w.end))),
            },
            None => Effective {
                mode: self.mode.clone(),
                message: self.message.clone(),
                window: None,
                next_change_at: next,
            },
        }
    }

    fn applies_to(&self, provider: &str) -> bool {
        self.providers.is_empty()
            || provider.is_empty()
            || self.providers.iter().any(|p| p == provider)
    }
}

fn cache() -> &'static RwLock<Option<(Instant, Policy)>> {
    static C: OnceLock<RwLock<Option<(Instant, Policy)>>> = OnceLock::new();
    C.get_or_init(|| RwLock::new(None))
}

pub fn invalidate() {
    *cache().write().unwrap() = None;
}

pub async fn load(db: &PgPool) -> Result<Policy> {
    if let Some((at, p)) = cache().read().unwrap().as_ref() {
        if at.elapsed() < CACHE_TTL {
            return Ok(p.clone());
        }
    }
    let row: Option<Value> =
        sqlx::query_scalar("select policy from musubi.access_policy where id = 1")
            .fetch_optional(db)
            .await
            .map_err(Error::internal)?;
    let p = match row {
        Some(v) => serde_json::from_value(v).unwrap_or_default(),
        None => Policy::default(),
    };
    *cache().write().unwrap() = Some((Instant::now(), p.clone()));
    Ok(p)
}

#[derive(Debug, Clone, Serialize)]
pub struct Decision {
    pub allowed: bool,
    /// `open`, `provider_bypass`, `audience_bypass`, `allowlist`, `existing_account`,
    /// `not_allowlisted`, `signups_closed`, `maintenance`, `not_applicable`.
    pub reason: String,
    /// The allow-list key or audience that let the player in.
    pub via: Option<String>,
}

fn ok(reason: &str, via: Option<String>) -> Decision {
    Decision {
        allowed: true,
        reason: reason.into(),
        via,
    }
}
fn deny(reason: &str) -> Decision {
    Decision {
        allowed: false,
        reason: reason.into(),
        via: None,
    }
}

/// Keys the account can be allow-listed under: `account:<id>`, `identity:<provider>:<subject>`,
/// `email_domain:<domain>`.
async fn account_keys(db: &PgPool, account: AccountId) -> Result<Vec<String>> {
    let ids: Vec<(String, String)> =
        sqlx::query_as("select provider, subject from musubi.identities where account_id = $1")
            .bind(account.0)
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
    let mut keys = vec![format!("account:{}", account.0)];
    for (p, s) in ids {
        keys.push(format!("identity:{p}:{s}"));
        if p == "email" {
            if let Some((_, dom)) = s.rsplit_once('@') {
                keys.push(format!("email_domain:{}", dom.to_lowercase()));
            }
        }
    }
    Ok(keys)
}

async fn audiences_of(s: &AppState, account: AccountId) -> Vec<String> {
    match s.audiences.of(&s.db, &s.economy, account).await {
        Ok(a) => a.as_ref().clone(),
        Err(e) => {
            tracing::warn!(error = %e, "audience evaluation for access gate failed");
            vec![]
        }
    }
}

/// Would `account` pass right now? `provider` is the login provider (`""` when unknown, e.g.
/// when re-checking live sessions), `created` whether the account was created by this login.
pub async fn decide(
    s: &AppState,
    account: AccountId,
    provider: &str,
    created: bool,
    pol: &Policy,
    eff: &Effective,
) -> Result<Decision> {
    if eff.mode == "open" {
        return Ok(ok("open", None));
    }
    if !pol.applies_to(provider) {
        return Ok(ok("not_applicable", None));
    }
    if !provider.is_empty() && pol.bypass_providers.iter().any(|p| p == provider) {
        return Ok(ok("provider_bypass", Some(provider.into())));
    }
    let needs_audiences = !pol.bypass_audiences.is_empty()
        || sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from musubi.access_allowlist where kind = 'audience'
                 and (expires_at is null or expires_at > now()))",
        )
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    let auds = if needs_audiences {
        audiences_of(s, account).await
    } else {
        vec![]
    };
    if let Some(a) = auds.iter().find(|a| pol.bypass_audiences.contains(a)) {
        return Ok(ok("audience_bypass", Some(a.clone())));
    }
    if eff.mode == "maintenance" {
        return Ok(deny("maintenance"));
    }
    let mut keys = account_keys(&s.db, account).await?;
    keys.extend(auds.iter().map(|a| format!("audience:{a}")));
    let hit: Option<String> = sqlx::query_scalar(
        "select key from musubi.access_allowlist where key = any($1)
           and (expires_at is null or expires_at > now()) order by key limit 1",
    )
    .bind(&keys)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if let Some(k) = hit {
        return Ok(ok("allowlist", Some(k)));
    }
    if !created && pol.allow_existing_accounts {
        return Ok(ok("existing_account", None));
    }
    Ok(deny(if created {
        "signups_closed"
    } else {
        "not_allowlisted"
    }))
}

/// Login gate: revokes the just-issued session and answers 403 when the player may not enter.
pub async fn gate(
    s: &AppState,
    sess: &Session,
    provider: &str,
    ip: &str,
) -> std::result::Result<(), ApiError> {
    let pol = load(&s.db).await?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let eff = pol.effective(now);
    if eff.mode == "open" {
        return Ok(());
    }
    let d = decide(s, sess.account_id, provider, sess.created, &pol, &eff).await?;
    if d.allowed {
        return Ok(());
    }
    if let Err(e) = s.auth.logout(sess.session_id, false).await {
        tracing::warn!(error = %e, "revoking denied session failed");
    }
    let _ = sqlx::query(
        "insert into musubi.access_denials (id, account_id, provider, ip, mode, reason, created)
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::now_v7())
    .bind(sess.account_id.0)
    .bind(provider)
    .bind(ip)
    .bind(&eff.mode)
    .bind(&d.reason)
    .bind(sess.created)
    .execute(&s.db)
    .await;
    s.events
        .emit(
            "access.denied",
            Some(sess.account_id),
            json!({ "provider": provider, "mode": eff.mode, "reason": d.reason,
                    "window": eff.window, "created": sess.created }),
        )
        .await;
    let mut msg = match eff.mode.as_str() {
        "maintenance" => "maintenance: ".to_string(),
        _ => format!("limited_access ({}): ", d.reason),
    };
    msg.push_str(eff.message.as_deref().unwrap_or(match eff.mode.as_str() {
        "maintenance" => "the game is down for maintenance",
        _ => "this account is not on the access list",
    }));
    if let Some(t) = eff.next_change_at {
        msg.push_str(&format!(" (next change at {t})"));
    } else if let Some(r) = pol.retry_after_secs {
        msg.push_str(&format!(" (retry after {r}s)"));
    }
    Err(Error::PermissionDenied(msg).into())
}

/// Revoke the sessions of everyone online who would be denied by the current policy.
pub async fn enforce(s: &AppState) -> Result<Value> {
    let pol = load(&s.db).await?;
    let eff = pol.effective(OffsetDateTime::now_utc().unix_timestamp());
    if eff.mode == "open" {
        return Ok(json!({ "mode": "open", "checked": 0, "kicked": 0, "accounts": [] }));
    }
    let online: Vec<Uuid> = sqlx::query_scalar(
        "select distinct account_id from musubi.sessions
          where revoked_at is null and expires_at > now() limit 20000",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut kicked = Vec::new();
    for id in &online {
        let d = decide(s, AccountId(*id), "", false, &pol, &eff).await?;
        if d.allowed {
            continue;
        }
        sqlx::query(
            "update musubi.sessions set revoked_at = now() where account_id = $1 and revoked_at is null",
        )
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        super::admin::kick_everywhere(s, AccountId(*id)).await;
        s.events
            .emit(
                "access.kicked",
                Some(AccountId(*id)),
                json!({ "mode": eff.mode, "reason": d.reason, "window": eff.window }),
            )
            .await;
        kicked.push(*id);
    }
    Ok(json!({
        "mode": eff.mode, "checked": online.len(), "kicked": kicked.len(),
        "accounts": kicked.iter().take(200).collect::<Vec<_>>()
    }))
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/access", get(status))
        .route("/v1/admin/access/policy", get(get_policy).put(put_policy))
        .route("/v1/admin/access/enforce", post(post_enforce))
        .route("/v1/admin/access/check", get(check))
        .route("/v1/admin/access/denials", get(denials))
        .route("/v1/admin/access/allowlist", get(list_allowlist))
        .route("/v1/admin/access/allowlist/import", post(import_allowlist))
        .route(
            "/v1/admin/access/allowlist/{kind}/{value}",
            axum::routing::put(put_entry).delete(delete_entry),
        )
}

fn status_json(pol: &Policy, now: i64) -> Value {
    let eff = pol.effective(now);
    json!({
        "mode": eff.mode, "message": eff.message, "window": eff.window,
        "next_change_at": eff.next_change_at, "retry_after_secs": pol.retry_after_secs,
        "providers": pol.providers, "allow_existing_accounts": pol.allow_existing_accounts,
        "server_time": now,
        "upcoming": pol.windows.iter().filter(|w| w.end > now).map(|w| json!({
            "name": w.name, "start": w.start, "end": w.end, "mode": w.mode, "message": w.message
        })).collect::<Vec<_>>()
    })
}

/// `GET /v1/access` — public: what a client should show before/after a failed login.
async fn status(State(s): State<AppState>) -> std::result::Result<Json<Value>, ApiError> {
    let pol = load(&s.db).await?;
    Ok(Json(status_json(
        &pol,
        OffsetDateTime::now_utc().unix_timestamp(),
    )))
}

async fn get_policy(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let pol = load(&s.db).await?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    Ok(Json(
        json!({ "policy": pol, "effective": pol.effective(now) }),
    ))
}

async fn put_policy(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(mut pol): Json<Policy>,
) -> std::result::Result<Json<Value>, ApiError> {
    pol.validate()?;
    for p in pol
        .providers
        .iter_mut()
        .chain(pol.bypass_providers.iter_mut())
    {
        *p = p.trim().to_lowercase();
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    pol.updated_at = Some(now);
    let v = serde_json::to_value(&pol).map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.access_policy (id, policy, updated_at) values (1, $1, now())
         on conflict (id) do update set policy = excluded.policy, updated_at = now()",
    )
    .bind(&v)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    invalidate();
    let eff = pol.effective(now);
    s.events
        .emit(
            "access.policy_changed",
            None,
            json!({ "mode": pol.mode, "effective": eff.mode, "windows": pol.windows.len() }),
        )
        .await;
    let kicked = if pol.kick_on_change && eff.mode != "open" {
        Some(enforce(&s).await?)
    } else {
        None
    };
    Ok(Json(
        json!({ "policy": pol, "effective": eff, "enforced": kicked }),
    ))
}

async fn post_enforce(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(enforce(&s).await?))
}

async fn check(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let id = q
        .get("account_id")
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| Error::InvalidArgument("account_id required".into()))?;
    let provider = q.get("provider").cloned().unwrap_or_default();
    let pol = load(&s.db).await?;
    let eff = pol.effective(OffsetDateTime::now_utc().unix_timestamp());
    let d = decide(&s, AccountId(id), &provider, false, &pol, &eff).await?;
    let keys = account_keys(&s.db, AccountId(id)).await?;
    Ok(Json(
        json!({ "decision": d, "effective": eff, "keys": keys }),
    ))
}

fn parse_kind_value(kind: &str, value: &str) -> Result<(String, String)> {
    if !KINDS.contains(&kind) {
        return Err(Error::InvalidArgument(format!(
            "kind must be one of {}",
            KINDS.join(", ")
        )));
    }
    let value = value.trim();
    if value.is_empty() || value.len() > 512 {
        return Err(Error::InvalidArgument("value must be 1..=512 chars".into()));
    }
    let value = match kind {
        "account" => Uuid::parse_str(value)
            .map_err(|_| Error::InvalidArgument("account value must be a uuid".into()))?
            .to_string(),
        "identity" => {
            let (p, sub) = value.split_once(':').ok_or_else(|| {
                Error::InvalidArgument("identity value must be provider:subject".into())
            })?;
            if p.is_empty() || sub.is_empty() {
                return Err(Error::InvalidArgument(
                    "identity value must be provider:subject".into(),
                ));
            }
            format!("{}:{sub}", p.to_lowercase())
        }
        "email_domain" => {
            let d = value.trim_start_matches('@').to_lowercase();
            if !d.contains('.') || d.contains('@') {
                return Err(Error::InvalidArgument(
                    "email_domain value must be a domain like studio.example".into(),
                ));
            }
            d
        }
        _ => value.to_string(),
    };
    Ok((kind.to_string(), value))
}

#[derive(Debug, Default, Deserialize)]
struct EntryBody {
    #[serde(default)]
    note: Option<String>,
    /// Unix seconds; the entry stops matching afterwards (playtest invite).
    #[serde(default)]
    expires_at: Option<i64>,
    #[serde(default)]
    added_by: Option<String>,
}

async fn upsert_entry(db: &PgPool, kind: &str, value: &str, b: &EntryBody) -> Result<Value> {
    let (kind, value) = parse_kind_value(kind, value)?;
    let key = format!("{kind}:{value}");
    let exp = b
        .expires_at
        .map(|t| {
            OffsetDateTime::from_unix_timestamp(t)
                .map_err(|_| Error::InvalidArgument("bad expires_at".into()))
        })
        .transpose()?;
    let row: (OffsetDateTime,) = sqlx::query_as(
        "insert into musubi.access_allowlist (key, kind, value, note, added_by, expires_at)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (key) do update set note = coalesce(excluded.note, musubi.access_allowlist.note),
             added_by = coalesce(excluded.added_by, musubi.access_allowlist.added_by),
             expires_at = excluded.expires_at
         returning created_at",
    )
    .bind(&key)
    .bind(&kind)
    .bind(&value)
    .bind(&b.note)
    .bind(&b.added_by)
    .bind(exp)
    .fetch_one(db)
    .await
    .map_err(Error::internal)?;
    Ok(
        json!({ "key": key, "kind": kind, "value": value, "note": b.note, "added_by": b.added_by,
               "expires_at": b.expires_at, "created_at": row.0.unix_timestamp() }),
    )
}

async fn put_entry(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((kind, value)): Path<(String, String)>,
    body: Option<Json<EntryBody>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    Ok(Json(upsert_entry(&s.db, &kind, &value, &b).await?))
}

async fn delete_entry(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((kind, value)): Path<(String, String)>,
) -> std::result::Result<Json<Value>, ApiError> {
    let (kind, value) = parse_kind_value(&kind, &value)?;
    let n = sqlx::query("delete from musubi.access_allowlist where key = $1")
        .bind(format!("{kind}:{value}"))
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("allow-list entry".into()).into());
    }
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Debug, Deserialize)]
struct ImportEntry {
    kind: String,
    value: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ImportBody {
    entries: Vec<ImportEntry>,
    #[serde(default)]
    added_by: Option<String>,
}

/// `POST /v1/admin/access/allowlist/import` — bulk upsert (CSV of a playtest signup form).
async fn import_allowlist(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<ImportBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    if b.entries.len() > 10_000 {
        return Err(Error::InvalidArgument("max 10000 entries per import".into()).into());
    }
    let mut imported = 0usize;
    let mut rejected = Vec::new();
    for e in &b.entries {
        let body = EntryBody {
            note: e.note.clone(),
            expires_at: e.expires_at,
            added_by: b.added_by.clone(),
        };
        match upsert_entry(&s.db, &e.kind, &e.value, &body).await {
            Ok(_) => imported += 1,
            Err(Error::InvalidArgument(m)) => {
                if rejected.len() < 100 {
                    rejected.push(json!({ "kind": e.kind, "value": e.value, "error": m }));
                }
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(Json(
        json!({ "imported": imported, "rejected": rejected.len(), "errors": rejected }),
    ))
}

#[derive(sqlx::FromRow)]
struct EntryRow {
    key: String,
    kind: String,
    value: String,
    note: Option<String>,
    added_by: Option<String>,
    expires_at: Option<OffsetDateTime>,
    created_at: OffsetDateTime,
}

async fn list_allowlist(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let kind = q.get("kind").filter(|k| !k.is_empty()).cloned();
    if let Some(k) = &kind {
        if !KINDS.contains(&k.as_str()) {
            return Err(Error::InvalidArgument("unknown kind".into()).into());
        }
    }
    let pat = q
        .get("q")
        .filter(|v| !v.is_empty())
        .map(|v| format!("%{}%", v.to_lowercase()));
    let include_expired = q
        .get("include_expired")
        .is_some_and(|v| v == "true" || v == "1");
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let offset: i64 = q
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
        .max(0);
    let rows: Vec<EntryRow> = sqlx::query_as(
        "select key, kind, value, note, added_by, expires_at, created_at from musubi.access_allowlist
          where ($1::text is null or kind = $1) and ($2::text is null or lower(value) like $2 or lower(coalesce(note, '')) like $2)
            and ($3 or expires_at is null or expires_at > now())
          order by created_at desc, key limit $4 offset $5",
    )
    .bind(&kind)
    .bind(&pat)
    .bind(include_expired)
    .bind(limit)
    .bind(offset)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let total: i64 = sqlx::query_scalar(
        "select count(*) from musubi.access_allowlist
          where ($1::text is null or kind = $1) and ($2::text is null or lower(value) like $2 or lower(coalesce(note, '')) like $2)
            and ($3 or expires_at is null or expires_at > now())",
    )
    .bind(&kind)
    .bind(&pat)
    .bind(include_expired)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let by_kind: Vec<(String, i64)> = sqlx::query_as(
        "select kind, count(*) from musubi.access_allowlist
          where expires_at is null or expires_at > now() group by kind",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "entries": rows.iter().map(|r| json!({
            "key": r.key, "kind": r.kind, "value": r.value, "note": r.note, "added_by": r.added_by,
            "expires_at": r.expires_at.map(|t| t.unix_timestamp()), "created_at": r.created_at.unix_timestamp()
        })).collect::<Vec<_>>(),
        "total": total,
        "by_kind": by_kind.into_iter().collect::<HashMap<_, _>>()
    })))
}

#[derive(sqlx::FromRow)]
struct DenialRow {
    id: Uuid,
    account_id: Option<Uuid>,
    provider: String,
    ip: String,
    mode: String,
    reason: String,
    created: bool,
    created_at: OffsetDateTime,
}

async fn denials(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let since: i64 = q
        .get("since_secs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(86_400)
        .max(1);
    let limit: i64 = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100)
        .clamp(1, 1000);
    let account = q.get("account_id").and_then(|v| Uuid::parse_str(v).ok());
    let rows: Vec<DenialRow> = sqlx::query_as(
        "select id, account_id, provider, ip, mode, reason, created, created_at from musubi.access_denials
          where created_at > now() - make_interval(secs => $1) and ($2::uuid is null or account_id = $2)
          order by created_at desc limit $3",
    )
    .bind(since as f64)
    .bind(account)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let total: i64 = sqlx::query_scalar(
        "select count(*) from musubi.access_denials
          where created_at > now() - make_interval(secs => $1) and ($2::uuid is null or account_id = $2)",
    )
    .bind(since as f64)
    .bind(account)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let by_reason: Vec<(String, i64)> = sqlx::query_as(
        "select reason, count(*) from musubi.access_denials
          where created_at > now() - make_interval(secs => $1) group by reason",
    )
    .bind(since as f64)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let by_provider: Vec<(String, i64)> = sqlx::query_as(
        "select provider, count(*) from musubi.access_denials
          where created_at > now() - make_interval(secs => $1) group by provider",
    )
    .bind(since as f64)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let unique: i64 = sqlx::query_scalar(
        "select count(distinct account_id) from musubi.access_denials
          where created_at > now() - make_interval(secs => $1)",
    )
    .bind(since as f64)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "denials": rows.iter().map(|r| json!({
            "id": r.id, "account_id": r.account_id, "provider": r.provider, "ip": r.ip, "mode": r.mode,
            "reason": r.reason, "created": r.created, "at": r.created_at.unix_timestamp()
        })).collect::<Vec<_>>(),
        "total": total, "unique_accounts": unique,
        "by_reason": by_reason.into_iter().collect::<HashMap<_, _>>(),
        "by_provider": by_provider.into_iter().collect::<HashMap<_, _>>()
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(start: i64, end: i64, mode: &str) -> Window {
        Window {
            name: Some(format!("{mode}-{start}")),
            start,
            end,
            mode: mode.into(),
            message: None,
        }
    }

    #[test]
    fn windows_override_base_mode_and_report_next_change() {
        let mut p = Policy {
            mode: "limited".into(),
            windows: vec![
                w(100, 200, "open"),
                w(150, 400, "maintenance"),
                w(500, 600, "open"),
            ],
            ..Default::default()
        };
        assert_eq!(p.effective(50).mode, "limited");
        assert_eq!(p.effective(50).next_change_at, Some(100));
        assert_eq!(p.effective(120).mode, "open");
        assert_eq!(p.effective(120).next_change_at, Some(150));
        // overlapping: the latest-starting window wins
        assert_eq!(p.effective(170).mode, "maintenance");
        assert_eq!(p.effective(170).next_change_at, Some(400));
        assert_eq!(p.effective(450).mode, "limited");
        assert_eq!(p.effective(450).next_change_at, Some(500));
        assert_eq!(p.effective(700).next_change_at, None);
        p.windows.clear();
        assert_eq!(p.effective(1).window, None);
    }

    #[test]
    fn validation_and_scope() {
        let mut p = Policy::default();
        assert!(p.validate().is_ok());
        p.mode = "closed".into();
        assert!(p.validate().is_err());
        p.mode = "limited".into();
        p.windows.push(w(10, 10, "open"));
        assert!(p.validate().is_err());
        p.windows[0].end = 11;
        p.windows[0].mode = "nope".into();
        assert!(p.validate().is_err());
        p.windows[0].mode = "open".into();
        p.retry_after_secs = Some(-1);
        assert!(p.validate().is_err());
        p.retry_after_secs = Some(60);
        assert!(p.validate().is_ok());
        p.providers = vec!["steam".into()];
        assert!(p.applies_to("steam") && !p.applies_to("device") && p.applies_to(""));
    }

    #[test]
    fn allowlist_values_are_normalised() {
        assert_eq!(
            parse_kind_value("identity", "Steam:7656").unwrap().1,
            "steam:7656"
        );
        assert_eq!(
            parse_kind_value("email_domain", "@Studio.Example")
                .unwrap()
                .1,
            "studio.example"
        );
        assert!(parse_kind_value("email_domain", "nodot").is_err());
        assert!(parse_kind_value("account", "not-a-uuid").is_err());
        assert!(parse_kind_value("identity", "nosubject").is_err());
        assert!(parse_kind_value("group", "x").is_err());
    }
}
