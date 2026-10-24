//! P1-23: OAuth2 clients / app registrations. Tools, partners and pipelines get their own
//! `client_id` + rotating secrets instead of sharing a server key; `POST /v1/oauth/token`
//! (RFC 6749 client credentials) exchanges them for a short-lived scoped server key that works as
//! `X-Musubi-Server-Key` or `Authorization: Bearer msk_…`; RFC 7662 introspection and RFC 7009
//! revocation complete the loop. Every client call is attributable (key name `oauth:<client>`).
use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::error::ApiError;
use super::extract::ServerKey;
use super::{AppState, ClientIp};
use crate::keys::{hash, Role, KEYS_TOPIC};

pub const KINDS: [&str; 2] = ["confidential", "public"];
const MAX_TTL: i64 = 7 * 86_400;
const DEFAULT_TTL: i64 = 3600;

#[derive(Debug, Clone, sqlx::FromRow)]
pub(super) struct ClientRow {
    pub(super) client_id: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) kind: String,
    pub(super) role: String,
    pub(super) scopes: Vec<String>,
    pub(super) redirect_uris: Vec<String>,
    pub(super) allowed_ips: Vec<String>,
    pub(super) token_ttl_secs: i64,
    pub(super) enabled: bool,
    pub(super) owner: Option<String>,
    pub(super) metadata: Value,
    pub(super) secrets: Value,
    pub(super) tokens_issued: i64,
    pub(super) failures: i64,
    pub(super) last_token_at: Option<OffsetDateTime>,
    pub(super) created_at: OffsetDateTime,
    pub(super) updated_at: OffsetDateTime,
}

const COLS: &str = "client_id, name, description, kind, role, scopes, redirect_uris, allowed_ips, token_ttl_secs, enabled, owner, metadata, secrets, tokens_issued, failures, last_token_at, created_at, updated_at";

fn secret_public_view(s: &Value) -> Value {
    json!({
        "id": s["id"], "label": s["label"], "created_at": s["created_at"],
        "expires_at": s["expires_at"], "hint": s["hint"]
    })
}

fn client_json(c: &ClientRow) -> Value {
    json!({
        "client_id": c.client_id, "name": c.name, "description": c.description, "kind": c.kind,
        "role": c.role, "scopes": c.scopes, "redirect_uris": c.redirect_uris, "allowed_ips": c.allowed_ips,
        "token_ttl_secs": c.token_ttl_secs, "enabled": c.enabled, "owner": c.owner, "metadata": c.metadata,
        "secrets": c.secrets.as_array().map(|a| a.iter().map(secret_public_view).collect::<Vec<_>>()).unwrap_or_default(),
        "tokens_issued": c.tokens_issued, "failures": c.failures,
        "last_token_at": c.last_token_at.map(|t| t.unix_timestamp()),
        "created_at": c.created_at.unix_timestamp(), "updated_at": c.updated_at.unix_timestamp()
    })
}

pub(super) async fn load(db: &PgPool, id: &str) -> Result<Option<ClientRow>> {
    sqlx::query_as(&format!(
        "select {COLS} from musubi.oauth_clients where client_id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)
}

pub fn validate_redirect_uri(u: &str) -> Result<()> {
    let bad = |m: &str| Error::InvalidArgument(format!("redirect_uri '{u}': {m}"));
    if u.len() > 2048 || u.contains('#') || u.contains(char::is_whitespace) {
        return Err(bad(
            "must be an absolute URL without fragment or whitespace",
        ));
    }
    let Some((scheme, rest)) = u.split_once("://") else {
        return Err(bad("must be an absolute URL"));
    };
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-')
        || rest.is_empty()
    {
        return Err(bad("bad scheme"));
    }
    if scheme == "http" {
        let host = rest.split(['/', ':', '?']).next().unwrap_or("");
        if host != "localhost" && host != "127.0.0.1" && host != "[::1]" {
            return Err(bad("http is only allowed for loopback; use https"));
        }
    }
    Ok(())
}

fn validate_common(
    name: &str,
    kind: &str,
    role: &str,
    scopes: &[String],
    redirect_uris: &[String],
    ttl: i64,
    allowed_ips: &[String],
) -> Result<Role> {
    if name.trim().is_empty() || name.len() > 64 {
        return Err(Error::InvalidArgument("name 1..=64 chars".into()));
    }
    if !KINDS.contains(&kind) {
        return Err(Error::InvalidArgument(
            "kind must be confidential or public".into(),
        ));
    }
    let role = Role::parse(role)
        .ok_or_else(|| Error::InvalidArgument(format!("unknown role '{role}'")))?;
    crate::scopes::validate(scopes)?;
    if redirect_uris.len() > 32 {
        return Err(Error::InvalidArgument("at most 32 redirect_uris".into()));
    }
    for u in redirect_uris {
        validate_redirect_uri(u)?;
    }
    if !(1..=MAX_TTL).contains(&ttl) {
        return Err(Error::InvalidArgument(format!(
            "token_ttl_secs must be 1..={MAX_TTL}"
        )));
    }
    if allowed_ips.len() > 64
        || allowed_ips
            .iter()
            .any(|ip| ip.parse::<std::net::IpAddr>().is_err())
    {
        return Err(Error::InvalidArgument(
            "allowed_ips must be up to 64 literal IP addresses".into(),
        ));
    }
    Ok(role)
}

fn new_secret() -> (String, Value) {
    use rand::RngCore;
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let secret = format!("mcs_{}", hex::encode(raw));
    let hint = format!("…{}", &secret[secret.len() - 4..]);
    (
        secret.clone(),
        json!({ "id": Uuid::now_v7(), "hash": hash(&secret), "hint": hint,
                "created_at": OffsetDateTime::now_utc().unix_timestamp(), "expires_at": Value::Null, "label": Value::Null }),
    )
}

/// Server-key names are capped at 64 chars; the client name alone may already be that long.
fn key_name(client_name: &str) -> String {
    let mut name = format!("oauth:{client_name}");
    if name.len() > 64 {
        let mut cut = 64;
        while !name.is_char_boundary(cut) {
            cut -= 1;
        }
        name.truncate(cut);
    }
    name
}

fn secret_matches(c: &ClientRow, secret: &str, now: i64) -> bool {
    use subtle::ConstantTimeEq;
    let h = hash(secret);
    c.secrets.as_array().is_some_and(|a| {
        a.iter().any(|s| {
            s["hash"]
                .as_str()
                .is_some_and(|stored| bool::from(stored.as_bytes().ct_eq(h.as_bytes())))
                && s["expires_at"].as_i64().is_none_or(|e| e > now)
        })
    })
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/oauth/token", post(token))
        .route("/v1/oauth/introspect", post(introspect))
        .route("/v1/oauth/revoke", post(revoke))
        .route("/v1/admin/oauth/clients", get(list).post(create))
        .route(
            "/v1/admin/oauth/clients/{id}",
            get(get_one).patch(patch).delete(delete),
        )
        .route("/v1/admin/oauth/clients/{id}/secrets", post(rotate))
        .route(
            "/v1/admin/oauth/clients/{id}/secrets/{sid}",
            axum::routing::delete(delete_secret),
        )
        .route(
            "/v1/admin/oauth/clients/{id}/tokens",
            get(tokens).delete(revoke_all),
        )
}

// ---------- RFC 6749 token endpoint ----------

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                match u8::from_str_radix(hex, 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parameters from a form-urlencoded or JSON body (both are accepted).
fn params(headers: &HeaderMap, body: &Bytes) -> HashMap<String, String> {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if ct.starts_with("application/json") {
        if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(body) {
            return m
                .into_iter()
                .filter_map(|(k, v)| match v {
                    Value::String(s) => Some((k, s)),
                    Value::Null => None,
                    other => Some((k, other.to_string())),
                })
                .collect();
        }
        return HashMap::new();
    }
    String::from_utf8_lossy(body)
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// `client_id`/`client_secret` from HTTP Basic (preferred) or the body.
fn client_auth(
    headers: &HeaderMap,
    p: &HashMap<String, String>,
) -> Option<(String, Option<String>)> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(v.trim())
            .ok()?;
        let s = String::from_utf8(decoded).ok()?;
        let (id, secret) = s.split_once(':')?;
        return Some((percent_decode(id), Some(percent_decode(secret))));
    }
    p.get("client_id")
        .map(|id| (id.clone(), p.get("client_secret").cloned()))
}

pub(super) fn oauth_error(status: StatusCode, code: &str, desc: &str) -> Response {
    let mut r = (
        status,
        Json(json!({ "error": code, "error_description": desc })),
    )
        .into_response();
    if status == StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Basic realm=\"musubi\", charset=\"UTF-8\""),
        );
    }
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}

pub(super) async fn note_failure(s: &AppState, client_id: &str, reason: &str, ip: &str) {
    let _ =
        sqlx::query("update musubi.oauth_clients set failures = failures + 1 where client_id = $1")
            .bind(client_id)
            .execute(&s.db)
            .await;
    s.events
        .emit(
            "oauth.auth_failed",
            None,
            json!({ "client_id": client_id, "reason": reason, "ip": ip }),
        )
        .await;
}

/// Authenticate the calling client (shared by token/introspect/revoke).
pub(super) async fn authenticate_client(
    s: &AppState,
    headers: &HeaderMap,
    p: &HashMap<String, String>,
    ip: &str,
) -> std::result::Result<ClientRow, Box<Response>> {
    let Some((id, secret)) = client_auth(headers, p) else {
        return Err(Box::new(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client authentication required (HTTP Basic or client_id/client_secret)",
        )));
    };
    let c = match load(&s.db, &id).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            s.events
                .emit(
                    "oauth.auth_failed",
                    None,
                    json!({ "client_id": id, "reason": "unknown_client", "ip": ip }),
                )
                .await;
            return Err(Box::new(oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "unknown client",
            )));
        }
        Err(e) => return Err(Box::new(ApiError(e).into_response())),
    };
    if !c.enabled {
        note_failure(s, &c.client_id, "disabled", ip).await;
        return Err(Box::new(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client disabled",
        )));
    }
    if c.kind == "public" {
        note_failure(s, &c.client_id, "public_client", ip).await;
        return Err(Box::new(oauth_error(
            StatusCode::BAD_REQUEST,
            "unauthorized_client",
            "public clients have no secret and cannot use client_credentials",
        )));
    }
    if !c.allowed_ips.is_empty() && !c.allowed_ips.iter().any(|a| a == ip) {
        note_failure(s, &c.client_id, "ip_not_allowed", ip).await;
        return Err(Box::new(oauth_error(
            StatusCode::UNAUTHORIZED,
            "invalid_client",
            "client not allowed from this address",
        )));
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    match secret {
        Some(sec) if secret_matches(&c, &sec, now) => Ok(c),
        _ => {
            note_failure(s, &c.client_id, "bad_secret", ip).await;
            Err(Box::new(oauth_error(
                StatusCode::UNAUTHORIZED,
                "invalid_client",
                "bad client secret",
            )))
        }
    }
}

/// `POST /v1/oauth/token` — `grant_type=client_credentials[&scope=a b]` → scoped server key.
async fn token(
    State(s): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(e) = s.limits.auth_ip(&ip.0) {
        return ApiError(e).into_response();
    }
    let p = params(&headers, &body);
    match p.get("grant_type").map(String::as_str) {
        Some("client_credentials") => {}
        Some("authorization_code") => {
            return super::oauth_authz::token_code(&s, &headers, &p, &ip.0).await
        }
        Some("refresh_token") => {
            return super::oauth_authz::token_refresh(&s, &headers, &p, &ip.0).await
        }
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "grant_type must be client_credentials, authorization_code or refresh_token",
            )
        }
    }
    let c = match authenticate_client(&s, &headers, &p, &ip.0).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let requested: Vec<String> = p
        .get("scope")
        .map(|v| v.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let scopes = if requested.is_empty() {
        c.scopes.clone()
    } else {
        if crate::scopes::validate(&requested).is_err()
            || !crate::scopes::within(&requested, &c.scopes)
        {
            note_failure(&s, &c.client_id, "invalid_scope", &ip.0).await;
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_scope",
                &format!(
                    "requested scope exceeds the client's [{}]",
                    c.scopes.join(" ")
                ),
            );
        }
        requested
    };
    let role = Role::parse(&c.role).unwrap_or(Role::Server);
    let ttl = p
        .get("ttl_secs")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(c.token_ttl_secs)
        .clamp(1, c.token_ttl_secs);
    let exp = OffsetDateTime::now_utc() + time::Duration::seconds(ttl);
    let issued = s
        .keys
        .issue_with(
            &s.db,
            &key_name(&c.name),
            role,
            Some(exp),
            Some(&c.client_id),
            &scopes,
        )
        .await;
    let (caller, secret) = match issued {
        Ok(v) => v,
        Err(e) => return ApiError(e).into_response(),
    };
    if let Some(cl) = &s.cluster {
        let _ = cl.publish(KEYS_TOPIC, &caller.id).await;
    }
    let _ = sqlx::query(
        "update musubi.oauth_clients set tokens_issued = tokens_issued + 1, last_token_at = now() where client_id = $1",
    )
    .bind(&c.client_id)
    .execute(&s.db)
    .await;
    s.events
        .emit(
            "oauth.token_issued",
            None,
            json!({ "client_id": c.client_id, "key_id": caller.id, "scopes": scopes, "expires_at": exp.unix_timestamp(), "ip": ip.0 }),
        )
        .await;
    let mut r = Json(json!({
        "access_token": secret, "token_type": "Bearer", "expires_in": ttl,
        "expires_at": exp.unix_timestamp(), "scope": scopes.join(" "),
        "client_id": c.client_id, "role": c.role, "key_id": caller.id,
        "usage": "send as `X-Musubi-Server-Key: <token>` or `Authorization: Bearer <token>`"
    }))
    .into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: Uuid,
    name: String,
    role: String,
    scopes: Vec<String>,
    subject: Option<String>,
    created_at: OffsetDateTime,
    expires_at: Option<OffsetDateTime>,
    last_used_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
}

const KEY_COLS: &str =
    "id, name, role, scopes, subject, created_at, expires_at, last_used_at, revoked_at";

fn key_json(k: &KeyRow, active: bool) -> Value {
    json!({
        "active": active, "key_id": k.id, "client_id": k.subject, "name": k.name, "role": k.role,
        "scope": k.scopes.join(" "), "scopes": k.scopes, "iat": k.created_at.unix_timestamp(),
        "exp": k.expires_at.map(|t| t.unix_timestamp()), "last_used_at": k.last_used_at.map(|t| t.unix_timestamp()),
        "revoked_at": k.revoked_at.map(|t| t.unix_timestamp()), "token_type": "Bearer"
    })
}

/// Caller for introspect/revoke: the client itself (Basic/body) or any server key.
async fn caller_or_client(
    s: &AppState,
    headers: &HeaderMap,
    p: &HashMap<String, String>,
    ip: &str,
) -> std::result::Result<Option<String>, Box<Response>> {
    if super::extract::check_server_key(s, headers).is_ok() {
        return Ok(None);
    }
    authenticate_client(s, headers, p, ip)
        .await
        .map(|c| Some(c.client_id))
}

/// `POST /v1/oauth/introspect {token}` (RFC 7662).
async fn introspect(
    State(s): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(e) = s.limits.auth_ip(&ip.0) {
        return ApiError(e).into_response();
    }
    let p = params(&headers, &body);
    let who = match caller_or_client(&s, &headers, &p, &ip.0).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let Some(tok) = p.get("token") else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "token required");
    };
    let row: Option<KeyRow> = match sqlx::query_as(&format!(
        "select {KEY_COLS} from musubi.server_keys where hash = $1"
    ))
    .bind(hash(tok))
    .fetch_optional(&s.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return ApiError(Error::internal(e)).into_response(),
    };
    let now = OffsetDateTime::now_utc();
    match row {
        // a client may only look at its own tokens
        Some(k) if who.is_none() || who.as_deref() == k.subject.as_deref() => {
            let active = k.revoked_at.is_none() && k.expires_at.is_none_or(|e| e > now);
            Json(key_json(&k, active)).into_response()
        }
        _ => super::oauth_authz::introspect_player(&s, tok, who.as_deref()).await,
    }
}

/// `POST /v1/oauth/revoke {token}` (RFC 7009) — always 200 for unknown tokens.
async fn revoke(
    State(s): State<AppState>,
    ip: ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(e) = s.limits.auth_ip(&ip.0) {
        return ApiError(e).into_response();
    }
    let p = params(&headers, &body);
    let who = match caller_or_client(&s, &headers, &p, &ip.0).await {
        Ok(w) => w,
        Err(r) => return *r,
    };
    let Some(tok) = p.get("token") else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "token required");
    };
    let row: Option<(Uuid, Option<String>)> = match sqlx::query_as(
        "select id, subject from musubi.server_keys where hash = $1 and revoked_at is null",
    )
    .bind(hash(tok))
    .fetch_optional(&s.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return ApiError(Error::internal(e)).into_response(),
    };
    if row.is_none() {
        super::oauth_authz::revoke_player_token(&s, tok, who.as_deref()).await;
    }
    if let Some((id, subject)) = row {
        if who.is_none() || who.as_deref() == subject.as_deref() {
            if let Err(e) = s.keys.revoke(&s.db, id).await {
                return ApiError(e).into_response();
            }
            if let Some(cl) = &s.cluster {
                let _ = cl.publish(KEYS_TOPIC, &id).await;
            }
            s.events
                .emit("oauth.token_revoked", None, json!({ "client_id": subject, "key_id": id, "by": who.clone().unwrap_or_else(|| "server-key".into()) }))
                .await;
        }
    }
    Json(json!({ "revoked": true })).into_response()
}

// ---------- admin ----------

#[derive(Deserialize)]
struct CreateBody {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "confidential")]
    kind: String,
    #[serde(default = "server")]
    role: String,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    redirect_uris: Vec<String>,
    #[serde(default)]
    allowed_ips: Vec<String>,
    #[serde(default = "default_ttl")]
    token_ttl_secs: i64,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    metadata: Option<Value>,
    #[serde(default)]
    secret_label: Option<String>,
}

fn confidential() -> String {
    "confidential".into()
}
fn server() -> String {
    "server".into()
}
fn default_ttl() -> i64 {
    DEFAULT_TTL
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    axum::Extension(issuer): axum::Extension<crate::keys::Caller>,
    Json(b): Json<CreateBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    super::admin::may_delegate(&issuer, &b.scopes)?;
    validate_common(
        &b.name,
        &b.kind,
        &b.role,
        &b.scopes,
        &b.redirect_uris,
        b.token_ttl_secs,
        &b.allowed_ips,
    )?;
    let client_id = format!("mc_{}", Uuid::now_v7().simple());
    let (secret, mut entry) = new_secret();
    entry["label"] = json!(b.secret_label);
    let secrets = if b.kind == "public" {
        json!([])
    } else {
        json!([entry])
    };
    sqlx::query(
        "insert into musubi.oauth_clients (client_id, name, description, kind, role, scopes, redirect_uris, allowed_ips,
             token_ttl_secs, owner, metadata, secrets)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(&client_id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(&b.kind)
    .bind(&b.role)
    .bind(&b.scopes)
    .bind(&b.redirect_uris)
    .bind(&b.allowed_ips)
    .bind(b.token_ttl_secs)
    .bind(&b.owner)
    .bind(b.metadata.unwrap_or_else(|| json!({})))
    .bind(&secrets)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let c = load(&s.db, &client_id)
        .await?
        .ok_or_else(|| Error::internal("client vanished"))?;
    s.events
        .emit("oauth.client_created", None, json!({ "client_id": client_id, "name": c.name, "kind": c.kind, "role": c.role, "scopes": c.scopes }))
        .await;
    let mut v = client_json(&c);
    if b.kind != "public" {
        v["client_secret"] = json!(secret);
    }
    Ok(Json(v))
}

async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let pat = q
        .get("q")
        .filter(|v| !v.is_empty())
        .map(|v| format!("%{}%", v.to_lowercase()));
    let enabled: Option<bool> = q.get("enabled").and_then(|v| v.parse().ok());
    let rows: Vec<ClientRow> = sqlx::query_as(&format!(
        "select {COLS} from musubi.oauth_clients
          where ($1::text is null or lower(name) like $1 or lower(client_id) like $1 or lower(coalesce(owner, '')) like $1)
            and ($2::bool is null or enabled = $2)
          order by created_at desc limit 500"
    ))
    .bind(&pat)
    .bind(enabled)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let live: Vec<(String, i64)> = sqlx::query_as(
        "select subject, count(*) from musubi.server_keys where subject like 'mc\\_%' and revoked_at is null
           and (expires_at is null or expires_at > now()) group by subject",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let live: HashMap<_, _> = live.into_iter().collect();
    Ok(Json(json!({
        "clients": rows.iter().map(|c| { let mut v = client_json(c); v["live_tokens"] = json!(live.get(&c.client_id).copied().unwrap_or(0)); v }).collect::<Vec<_>>(),
        "total": rows.len()
    })))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    let live: i64 = sqlx::query_scalar(
        "select count(*) from musubi.server_keys where subject = $1 and revoked_at is null and (expires_at is null or expires_at > now())",
    )
    .bind(&id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut v = client_json(&c);
    v["live_tokens"] = json!(live);
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PatchBody {
    name: Option<String>,
    description: Option<String>,
    role: Option<String>,
    scopes: Option<Vec<String>>,
    redirect_uris: Option<Vec<String>>,
    allowed_ips: Option<Vec<String>>,
    token_ttl_secs: Option<i64>,
    enabled: Option<bool>,
    owner: Option<String>,
    metadata: Option<Value>,
}

async fn patch(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    axum::Extension(issuer): axum::Extension<crate::keys::Caller>,
    Path(id): Path<String>,
    Json(b): Json<PatchBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    let name = b.name.unwrap_or(c.name);
    let role = b.role.unwrap_or(c.role);
    let scopes = b.scopes.unwrap_or(c.scopes);
    super::admin::may_delegate(&issuer, &scopes)?;
    let redirect_uris = b.redirect_uris.unwrap_or(c.redirect_uris);
    let allowed_ips = b.allowed_ips.unwrap_or(c.allowed_ips);
    let ttl = b.token_ttl_secs.unwrap_or(c.token_ttl_secs);
    validate_common(
        &name,
        &c.kind,
        &role,
        &scopes,
        &redirect_uris,
        ttl,
        &allowed_ips,
    )?;
    let enabled = b.enabled.unwrap_or(c.enabled);
    sqlx::query(
        "update musubi.oauth_clients set name = $2, description = $3, role = $4, scopes = $5, redirect_uris = $6,
             allowed_ips = $7, token_ttl_secs = $8, enabled = $9, owner = $10, metadata = $11, updated_at = now()
         where client_id = $1",
    )
    .bind(&id)
    .bind(name.trim())
    .bind(b.description.unwrap_or(c.description))
    .bind(&role)
    .bind(&scopes)
    .bind(&redirect_uris)
    .bind(&allowed_ips)
    .bind(ttl)
    .bind(enabled)
    .bind(b.owner.or(c.owner))
    .bind(b.metadata.unwrap_or(c.metadata))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    // tightening the client tightens its live tokens too
    if !enabled {
        revoke_client_tokens(&s, &id).await?;
    } else {
        let live: Vec<(Uuid, Vec<String>)> = sqlx::query_as(
            "select id, scopes from musubi.server_keys where subject = $1 and revoked_at is null and (expires_at is null or expires_at > now())",
        )
        .bind(&id)
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for (kid, ks) in live {
            if !crate::scopes::within(&ks, &scopes) {
                s.keys.revoke(&s.db, kid).await?;
                if let Some(cl) = &s.cluster {
                    let _ = cl.publish(KEYS_TOPIC, &kid).await;
                }
            }
        }
    }
    s.events
        .emit(
            "oauth.client_updated",
            None,
            json!({ "client_id": id, "enabled": enabled, "scopes": scopes, "role": role }),
        )
        .await;
    let c = load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    Ok(Json(client_json(&c)))
}

async fn revoke_client_tokens(s: &AppState, id: &str) -> Result<usize> {
    let live: Vec<Uuid> = sqlx::query_scalar(
        "select id from musubi.server_keys where subject = $1 and revoked_at is null",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    for kid in &live {
        s.keys.revoke(&s.db, *kid).await?;
        if let Some(cl) = &s.cluster {
            let _ = cl.publish(KEYS_TOPIC, kid).await;
        }
    }
    Ok(live.len())
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let revoked = revoke_client_tokens(&s, &id).await?;
    let n = sqlx::query("delete from musubi.oauth_clients where client_id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("oauth client".into()).into());
    }
    s.events
        .emit(
            "oauth.client_deleted",
            None,
            json!({ "client_id": id, "tokens_revoked": revoked }),
        )
        .await;
    Ok(Json(json!({ "deleted": true, "tokens_revoked": revoked })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RotateBody {
    label: Option<String>,
    /// Keep the previous secrets valid for this long (zero = retire them now).
    expire_old_in_secs: Option<i64>,
}

/// `POST /v1/admin/oauth/clients/{id}/secrets` — add a secret; old ones keep working until
/// `expire_old_in_secs` so deployments can roll.
/// Longest grace period for a retired secret (one year).
const MAX_SECRET_GRACE: i64 = 365 * 86_400;

async fn rotate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    body: Option<Json<RotateBody>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let c = load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    if c.kind == "public" {
        return Err(Error::InvalidArgument("public clients have no secrets".into()).into());
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut secrets: Vec<Value> = c.secrets.as_array().cloned().unwrap_or_default();
    if let Some(grace) = b.expire_old_in_secs {
        if !(0..=MAX_SECRET_GRACE).contains(&grace) {
            return Err(Error::InvalidArgument(format!(
                "expire_old_in_secs must be 0..={MAX_SECRET_GRACE}"
            ))
            .into());
        }
        for sec in secrets.iter_mut() {
            let cur = sec["expires_at"].as_i64();
            if cur.is_none_or(|e| e > now + grace) {
                sec["expires_at"] = json!(now + grace);
            }
        }
    }
    // retired secrets vanish once they are past expiry
    secrets.retain(|sec| sec["expires_at"].as_i64().is_none_or(|e| e > now));
    if secrets.len() >= 5 {
        return Err(
            Error::InvalidArgument("at most 5 live secrets; delete one first".into()).into(),
        );
    }
    let (secret, mut entry) = new_secret();
    entry["label"] = json!(b.label);
    secrets.push(entry.clone());
    sqlx::query(
        "update musubi.oauth_clients set secrets = $2, updated_at = now() where client_id = $1",
    )
    .bind(&id)
    .bind(Value::Array(secrets))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit("oauth.secret_rotated", None, json!({ "client_id": id, "secret_id": entry["id"], "expire_old_in_secs": b.expire_old_in_secs }))
        .await;
    Ok(Json(
        json!({ "client_id": id, "client_secret": secret, "secret": secret_public_view(&entry) }),
    ))
}

async fn delete_secret(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, sid)): Path<(String, String)>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    let mut secrets: Vec<Value> = c.secrets.as_array().cloned().unwrap_or_default();
    let before = secrets.len();
    secrets.retain(|sec| sec["id"].as_str() != Some(sid.as_str()));
    if secrets.len() == before {
        return Err(Error::NotFound("secret".into()).into());
    }
    sqlx::query(
        "update musubi.oauth_clients set secrets = $2, updated_at = now() where client_id = $1",
    )
    .bind(&id)
    .bind(Value::Array(secrets))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "oauth.secret_deleted",
            None,
            json!({ "client_id": id, "secret_id": sid }),
        )
        .await;
    Ok(Json(json!({ "deleted": true, "remaining": before - 1 })))
}

async fn tokens(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> std::result::Result<Json<Value>, ApiError> {
    load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    let all = q.get("all").is_some_and(|v| v == "true" || v == "1");
    let rows: Vec<KeyRow> = sqlx::query_as(&format!(
        "select {KEY_COLS} from musubi.server_keys where subject = $1
           and ($2 or (revoked_at is null and (expires_at is null or expires_at > now())))
         order by created_at desc limit 500"
    ))
    .bind(&id)
    .bind(all)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = OffsetDateTime::now_utc();
    Ok(Json(json!({
        "tokens": rows.iter().map(|k| key_json(k, k.revoked_at.is_none() && k.expires_at.is_none_or(|e| e > now))).collect::<Vec<_>>(),
        "total": rows.len()
    })))
}

async fn revoke_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    load(&s.db, &id)
        .await?
        .ok_or_else(|| Error::NotFound("oauth client".into()))?;
    let n = revoke_client_tokens(&s, &id).await?;
    s.events
        .emit(
            "oauth.tokens_revoked",
            None,
            json!({ "client_id": id, "count": n }),
        )
        .await;
    Ok(Json(json!({ "revoked": n })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_survives_multibyte_after_percent() {
        // `%` followed by bytes that are not a char boundary used to slice the &str and panic
        assert_eq!(percent_decode("%aé"), "%aé");
        assert_eq!(percent_decode("a%2Bb+c%zz"), "a+b c%zz");
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("é%"), "é%");
    }

    #[test]
    fn oauth_key_name_fits_the_server_key_limit() {
        assert_eq!(key_name("svc"), "oauth:svc");
        let long = "é".repeat(64);
        let n = key_name(&long);
        assert!(n.len() <= 64 && n.starts_with("oauth:"));
        assert!(n.is_char_boundary(n.len()));
    }

    #[test]
    fn client_secrets_are_random_and_stored_hashed() {
        let (a, ea) = new_secret();
        let (b, _) = new_secret();
        assert!(a.starts_with("mcs_") && a.len() == 4 + 64);
        assert_ne!(a, b);
        assert_eq!(ea["hash"], hash(&a));
        assert_eq!(
            ea["hint"].as_str().unwrap(),
            format!("…{}", &a[a.len() - 4..])
        );
    }

    #[test]
    fn redirect_uris() {
        assert!(validate_redirect_uri("https://tool.example/cb").is_ok());
        assert!(validate_redirect_uri("http://localhost:8080/cb").is_ok());
        assert!(validate_redirect_uri("http://127.0.0.1/cb").is_ok());
        assert!(validate_redirect_uri("mygame://auth/callback").is_ok());
        assert!(validate_redirect_uri("http://tool.example/cb").is_err());
        assert!(validate_redirect_uri("https://tool.example/cb#frag").is_err());
        assert!(validate_redirect_uri("not a url").is_err());
        assert!(validate_redirect_uri("tool.example/cb").is_err());
    }

    #[test]
    fn form_and_json_params() {
        let mut h = HeaderMap::new();
        let p = params(
            &h,
            &Bytes::from_static(
                b"grant_type=client_credentials&scope=storage%3Aread+fleets&x=a%2Bb",
            ),
        );
        assert_eq!(p["grant_type"], "client_credentials");
        assert_eq!(p["scope"], "storage:read fleets");
        assert_eq!(p["x"], "a+b");
        h.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        let p = params(
            &h,
            &Bytes::from_static(br#"{"grant_type":"client_credentials","ttl_secs":60}"#),
        );
        assert_eq!(p["grant_type"], "client_credentials");
        assert_eq!(p["ttl_secs"], "60");
    }

    #[test]
    fn basic_auth_wins_over_body() {
        let mut h = HeaderMap::new();
        let creds = base64::engine::general_purpose::STANDARD.encode("mc_1:s3cret");
        h.insert(
            header::AUTHORIZATION,
            format!("Basic {creds}").parse().unwrap(),
        );
        let mut p = HashMap::new();
        p.insert("client_id".to_string(), "mc_2".to_string());
        assert_eq!(
            client_auth(&h, &p),
            Some(("mc_1".into(), Some("s3cret".into())))
        );
        h.clear();
        assert_eq!(client_auth(&h, &p), Some(("mc_2".into(), None)));
    }

    #[test]
    fn validation() {
        assert!(validate_common(
            "tool",
            "confidential",
            "server",
            &["fleets:read".into()],
            &[],
            3600,
            &[]
        )
        .is_ok());
        assert!(validate_common("", "confidential", "server", &[], &[], 3600, &[]).is_err());
        assert!(validate_common("t", "secret", "server", &[], &[], 3600, &[]).is_err());
        assert!(validate_common("t", "confidential", "god", &[], &[], 3600, &[]).is_err());
        assert!(validate_common(
            "t",
            "confidential",
            "server",
            &["bad scope".into()],
            &[],
            3600,
            &[]
        )
        .is_err());
        assert!(validate_common("t", "confidential", "server", &[], &[], 0, &[]).is_err());
        assert!(validate_common(
            "t",
            "confidential",
            "server",
            &[],
            &[],
            3600,
            &["10.0.0.0/8".into()]
        )
        .is_err());
        assert!(validate_common(
            "t",
            "confidential",
            "server",
            &[],
            &[],
            3600,
            &["10.0.0.1".into()]
        )
        .is_ok());
    }
}
