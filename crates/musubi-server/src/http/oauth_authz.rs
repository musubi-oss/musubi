//! OAuth2 authorization-code flow (RFC 6749 §4.1, PKCE RFC 7636, refresh rotation per OAuth 2.1):
//! "Sign in with <game>" for companion apps, community sites, Discord bots and tournament tools
//! that act *for a player* rather than as a server. A signed-in player previews the request with
//! `GET /v1/oauth/authorize` (client, scopes, whether already granted) and approves or denies it
//! with `POST /v1/oauth/authorize`; the backend hands back the single-use code and the full
//! `redirect_to` URL (the UI performs the redirect). The app exchanges the code at
//! `POST /v1/oauth/token grant_type=authorization_code` — public clients must prove PKCE `S256`,
//! confidential ones authenticate with their secret — and receives a down-scoped player access
//! token (the same scoped JWT `POST /v1/auth/token/scope` mints) plus a rotating refresh token;
//! reuse of a rotated refresh token revokes the whole family. Grants are visible and revocable by
//! the player (`/v1/account/oauth/grants`) and by operators (`/v1/admin/oauth/clients/{id}/grants`).

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use base64::Engine;
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use time::OffsetDateTime;
use uuid::Uuid;

use super::error::ApiError;
use super::extract::{Bearer, ServerKey};
use super::oauth::{authenticate_client, load, note_failure, oauth_error, ClientRow};
use super::AppState;
use crate::keys::hash;

pub const CODE_TTL_SECS: i64 = 600;
pub const REFRESH_TTL_SECS: i64 = 30 * 86_400;
pub const MAX_ACCESS_TTL_SECS: i64 = 86_400;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/oauth/authorize",
            get(authorize_preview).post(authorize),
        )
        .route("/v1/account/oauth/grants", get(my_grants))
        .route(
            "/v1/account/oauth/grants/{client_id}",
            delete(revoke_my_grant),
        )
        .route(
            "/v1/admin/oauth/clients/{id}/grants",
            get(admin_grants).delete(admin_revoke_grants),
        )
}

fn random_token(prefix: &str) -> String {
    use rand::RngCore;
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    format!("{prefix}{}", hex::encode(b))
}

fn pkce_s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct AuthorizeReq {
    client_id: String,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    response_type: Option<String>,
    /// `POST` only: `false` denies the request.
    approve: Option<bool>,
}

struct Checked {
    client: ClientRow,
    redirect_uri: String,
    scopes: Vec<String>,
    challenge: Option<String>,
}

fn scopes_of(raw: Option<&str>) -> Vec<String> {
    let mut v: Vec<String> = raw
        .unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect();
    v.sort();
    v.dedup();
    v
}

async fn check(s: &AppState, r: &AuthorizeReq) -> Result<Checked> {
    if r.response_type.as_deref().is_some_and(|t| t != "code") {
        return Err(Error::InvalidArgument("response_type must be code".into()));
    }
    if r.client_id.is_empty() {
        return Err(Error::InvalidArgument("client_id is required".into()));
    }
    let Some(client) = load(&s.db, &r.client_id).await? else {
        return Err(Error::NotFound(format!("oauth client {}", r.client_id)));
    };
    if !client.enabled {
        return Err(Error::PermissionDenied("client disabled".into()));
    }
    if client.redirect_uris.is_empty() {
        return Err(Error::InvalidArgument(
            "client has no redirect_uris; the authorization-code flow needs one".into(),
        ));
    }
    let redirect_uri = match &r.redirect_uri {
        Some(u) => {
            if !client.redirect_uris.iter().any(|x| x == u) {
                return Err(Error::InvalidArgument(
                    "redirect_uri is not registered for this client".into(),
                ));
            }
            u.clone()
        }
        None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
        None => {
            return Err(Error::InvalidArgument(
                "redirect_uri is required (client has several)".into(),
            ))
        }
    };
    let scopes = scopes_of(r.scope.as_deref());
    if scopes.is_empty() {
        return Err(Error::InvalidArgument(
            "scope is required (space-separated resource scopes, e.g. `account:read friends`)"
                .into(),
        ));
    }
    if scopes.iter().any(|x| x == "*") {
        return Err(Error::InvalidArgument(
            "scope `*` cannot be delegated to a third-party app".into(),
        ));
    }
    crate::scopes::validate(&scopes)?;
    if !crate::scopes::within(&scopes, &client.scopes) {
        return Err(Error::InvalidArgument(format!(
            "requested scope exceeds the client's [{}]",
            client.scopes.join(" ")
        )));
    }
    if r.state.as_ref().is_some_and(|st| st.len() > 512) {
        return Err(Error::InvalidArgument("state: at most 512 chars".into()));
    }
    let challenge = match (&r.code_challenge, r.code_challenge_method.as_deref()) {
        (None, _) if client.kind == "public" => {
            return Err(Error::InvalidArgument(
                "public clients must send a PKCE code_challenge (S256)".into(),
            ))
        }
        (None, _) => None,
        (Some(_), Some(m)) if m != "S256" => {
            return Err(Error::InvalidArgument(
                "code_challenge_method must be S256 (plain is not accepted)".into(),
            ))
        }
        (Some(c), _) => {
            if !(43..=128).contains(&c.len())
                || !c
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
            {
                return Err(Error::InvalidArgument(
                    "code_challenge: 43..=128 base64url chars".into(),
                ));
            }
            Some(c.clone())
        }
    };
    Ok(Checked {
        client,
        redirect_uri,
        scopes,
        challenge,
    })
}

fn client_public(c: &ClientRow) -> Value {
    json!({
        "client_id": c.client_id, "name": c.name, "description": c.description,
        "kind": c.kind, "owner": c.owner,
    })
}

async fn existing_grant(
    s: &AppState,
    client_id: &str,
    account: AccountId,
) -> Result<Option<Vec<String>>> {
    let r: Option<(Vec<String>,)> = sqlx::query_as(
        "select scopes from musubi.oauth_grants where client_id = $1 and account_id = $2 and revoked_at is null",
    )
    .bind(client_id)
    .bind(account.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(r.map(|x| x.0))
}

/// Consent-screen data for a signed-in player.
async fn authorize_preview(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Query(r): Query<AuthorizeReq>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = check(&s, &r).await?;
    let granted = existing_grant(&s, &c.client.client_id, me.account_id).await?;
    let already = granted
        .as_ref()
        .is_some_and(|g| crate::scopes::within(&c.scopes, g));
    Ok(Json(json!({
        "client": client_public(&c.client),
        "scopes": c.scopes,
        "redirect_uri": c.redirect_uri,
        "state": r.state,
        "pkce": c.challenge.is_some(),
        "already_granted": already,
        "granted_scopes": granted,
    })))
}

fn append_query(uri: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = String::from(uri);
    let mut sep = if uri.contains('?') { '&' } else { '?' };
    for (k, v) in pairs {
        out.push(sep);
        out.push_str(k);
        out.push('=');
        out.push_str(&urlencode(v));
        sep = '&';
    }
    out
}

fn urlencode(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

/// Player decision: `approve: true` mints a single-use code bound to client, redirect URI, scopes
/// and PKCE challenge; `false` reports `access_denied` the way the redirect would.
async fn authorize(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(r): Json<AuthorizeReq>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = check(&s, &r).await?;
    let state = r.state.clone().unwrap_or_default();
    let state_pairs: Vec<(&str, &str)> = if state.is_empty() {
        vec![]
    } else {
        vec![("state", state.as_str())]
    };
    if !r.approve.unwrap_or(true) {
        let mut pairs = vec![("error", "access_denied")];
        pairs.extend(state_pairs.iter().copied());
        s.events
            .emit(
                "oauth.authorization_denied",
                Some(me.account_id),
                json!({ "client_id": c.client.client_id, "scopes": c.scopes }),
            )
            .await;
        return Ok(Json(json!({
            "error": "access_denied",
            "state": r.state,
            "redirect_to": append_query(&c.redirect_uri, &pairs),
        })));
    }
    let code = random_token("mac_");
    sqlx::query(
        "insert into musubi.oauth_codes (code_hash, client_id, account_id, scopes, redirect_uri, code_challenge, \
         code_challenge_method, state, expires_at) \
         values ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(secs => $9))",
    )
    .bind(hash(&code))
    .bind(&c.client.client_id)
    .bind(me.account_id.0)
    .bind(&c.scopes)
    .bind(&c.redirect_uri)
    .bind(&c.challenge)
    .bind(c.challenge.as_ref().map(|_| "S256"))
    .bind(&r.state)
    .bind(CODE_TTL_SECS as f64)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    // remember the consent (union of scopes) so the next preview can skip the prompt
    sqlx::query(
        "insert into musubi.oauth_grants (client_id, account_id, scopes) values ($1, $2, $3) \
         on conflict (client_id, account_id) do update set \
           scopes = (select array_agg(distinct x) from unnest(musubi.oauth_grants.scopes || excluded.scopes) as t(x)), \
           updated_at = now(), revoked_at = null",
    )
    .bind(&c.client.client_id)
    .bind(me.account_id.0)
    .bind(&c.scopes)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "oauth.authorized",
            Some(me.account_id),
            json!({ "client_id": c.client.client_id, "scopes": c.scopes, "pkce": c.challenge.is_some() }),
        )
        .await;
    let mut pairs = vec![("code", code.as_str())];
    pairs.extend(state_pairs.iter().copied());
    Ok(Json(json!({
        "code": code,
        "state": r.state,
        "expires_in": CODE_TTL_SECS,
        "redirect_to": append_query(&c.redirect_uri, &pairs),
    })))
}

/// Client identity for the token endpoint: confidential clients authenticate with their secret,
/// public clients only name themselves (PKCE proves possession of the session).
async fn token_client(
    s: &AppState,
    headers: &HeaderMap,
    p: &HashMap<String, String>,
    ip: &str,
) -> std::result::Result<ClientRow, Box<Response>> {
    let id = p.get("client_id").cloned();
    if let Some(id) = &id {
        if let Ok(Some(c)) = load(&s.db, id).await {
            if c.kind == "public" {
                if !c.enabled {
                    note_failure(s, &c.client_id, "disabled", ip).await;
                    return Err(Box::new(oauth_error(
                        StatusCode::UNAUTHORIZED,
                        "invalid_client",
                        "client disabled",
                    )));
                }
                return Ok(c);
            }
        }
    }
    authenticate_client(s, headers, p, ip).await
}

async fn username_of(s: &AppState, account: AccountId) -> Result<Option<String>> {
    let r: Option<(Option<String>,)> =
        sqlx::query_as("select username from musubi.accounts where id = $1 and deleted_at is null")
            .bind(account.0)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    match r {
        Some((u,)) => Ok(u),
        None => Err(Error::NotFound("account".into())),
    }
}

async fn issue(
    s: &AppState,
    c: &ClientRow,
    account: AccountId,
    scopes: &[String],
    family: Uuid,
    ip: &str,
    grant_type: &str,
) -> std::result::Result<Response, Box<Response>> {
    let username = match username_of(s, account).await {
        Ok(u) => u,
        Err(Error::NotFound(_)) => {
            return Err(Box::new(oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "account no longer exists",
            )))
        }
        Err(e) => return Err(Box::new(ApiError(e).into_response())),
    };
    let ttl = c.token_ttl_secs.clamp(60, MAX_ACCESS_TTL_SECS);
    let (access, exp) = match s.auth.sign_scoped(
        account,
        musubi_core::SessionId(Uuid::now_v7()),
        username,
        scopes.to_vec(),
        ttl,
    ) {
        Ok(v) => v,
        Err(e) => return Err(Box::new(ApiError(e).into_response())),
    };
    let refresh = random_token("mrt_");
    if let Err(e) = sqlx::query(
        "insert into musubi.oauth_refresh_tokens (token_hash, family, client_id, account_id, scopes, expires_at) \
         values ($1, $2, $3, $4, $5, now() + make_interval(secs => $6))",
    )
    .bind(hash(&refresh))
    .bind(family)
    .bind(&c.client_id)
    .bind(account.0)
    .bind(scopes)
    .bind(REFRESH_TTL_SECS as f64)
    .execute(&s.db)
    .await
    {
        return Err(Box::new(ApiError(Error::internal(e)).into_response()));
    }
    let _ = sqlx::query(
        "update musubi.oauth_grants set last_used_at = now() where client_id = $1 and account_id = $2",
    )
    .bind(&c.client_id)
    .bind(account.0)
    .execute(&s.db)
    .await;
    let _ = sqlx::query(
        "update musubi.oauth_clients set tokens_issued = tokens_issued + 1, last_token_at = now() where client_id = $1",
    )
    .bind(&c.client_id)
    .execute(&s.db)
    .await;
    s.events
        .emit(
            "oauth.player_token_issued",
            Some(account),
            json!({ "client_id": c.client_id, "scopes": scopes, "grant_type": grant_type, "expires_at": exp, "ip": ip }),
        )
        .await;
    let mut r = Json(json!({
        "access_token": access, "token_type": "Bearer", "expires_in": ttl, "expires_at": exp,
        "refresh_token": refresh, "refresh_expires_in": REFRESH_TTL_SECS,
        "scope": scopes.join(" "), "account_id": account, "client_id": c.client_id,
    }))
    .into_response();
    r.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(r)
}

async fn grant_fail(s: &AppState, client_id: &str, ip: &str, reason: &str, desc: &str) -> Response {
    note_failure(s, client_id, reason, ip).await;
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", desc)
}

#[derive(sqlx::FromRow)]
struct CodeRow {
    client_id: String,
    account_id: Uuid,
    scopes: Vec<String>,
    redirect_uri: String,
    code_challenge: Option<String>,
    expires_at: OffsetDateTime,
    consumed_at: Option<OffsetDateTime>,
}

/// `grant_type=authorization_code` (called from `POST /v1/oauth/token`).
pub async fn token_code(
    s: &AppState,
    headers: &HeaderMap,
    p: &HashMap<String, String>,
    ip: &str,
) -> Response {
    let c = match token_client(s, headers, p, ip).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let Some(code) = p.get("code") else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "code required");
    };
    let mut tx = match s.db.begin().await {
        Ok(t) => t,
        Err(e) => return ApiError(Error::internal(e)).into_response(),
    };
    let row: Option<CodeRow> = match sqlx::query_as(
        "select client_id, account_id, scopes, redirect_uri, code_challenge, expires_at, consumed_at \
         from musubi.oauth_codes where code_hash = $1 for update",
    )
    .bind(hash(code))
    .fetch_optional(&mut *tx)
    .await
    {
        Ok(r) => r,
        Err(e) => return ApiError(Error::internal(e)).into_response(),
    };
    let Some(row) = row else {
        note_failure(s, &c.client_id, "unknown_code", ip).await;
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "unknown code");
    };
    // single use: whatever happens next, this code is spent
    let _ = sqlx::query("update musubi.oauth_codes set consumed_at = coalesce(consumed_at, now()) where code_hash = $1")
        .bind(hash(code))
        .execute(&mut *tx)
        .await;
    let _ = tx.commit().await;
    let now = OffsetDateTime::now_utc();
    if row.consumed_at.is_some() {
        // replay: a leaked code — revoke every token minted from this grant
        let _ = sqlx::query(
            "update musubi.oauth_refresh_tokens set revoked_at = now() where client_id = $1 and account_id = $2 and revoked_at is null",
        )
        .bind(&row.client_id)
        .bind(row.account_id)
        .execute(&s.db)
        .await;
        return grant_fail(
            s,
            &c.client_id,
            ip,
            "code_replayed",
            "code already used; tokens of this grant were revoked",
        )
        .await;
    }
    if row.expires_at <= now {
        return grant_fail(s, &c.client_id, ip, "code_expired", "code expired").await;
    }
    if row.client_id != c.client_id {
        return grant_fail(
            s,
            &c.client_id,
            ip,
            "code_client_mismatch",
            "code was issued to another client",
        )
        .await;
    }
    if p.get("redirect_uri")
        .is_some_and(|u| u != &row.redirect_uri)
    {
        return grant_fail(
            s,
            &c.client_id,
            ip,
            "redirect_mismatch",
            "redirect_uri does not match the authorization request",
        )
        .await;
    }
    match (&row.code_challenge, p.get("code_verifier")) {
        (Some(ch), Some(v)) => {
            if !(43..=128).contains(&v.len())
                || !bool::from(pkce_s256(v).as_bytes().ct_eq(ch.as_bytes()))
            {
                return grant_fail(
                    s,
                    &c.client_id,
                    ip,
                    "pkce_failed",
                    "code_verifier does not match code_challenge",
                )
                .await;
            }
        }
        (Some(_), None) => {
            return grant_fail(
                s,
                &c.client_id,
                ip,
                "pkce_missing",
                "code_verifier required",
            )
            .await
        }
        (None, _) if c.kind == "public" => {
            return grant_fail(
                s,
                &c.client_id,
                ip,
                "pkce_missing",
                "public clients must use PKCE",
            )
            .await
        }
        (None, _) => {}
    }
    match issue(
        s,
        &c,
        AccountId(row.account_id),
        &row.scopes,
        Uuid::now_v7(),
        ip,
        "authorization_code",
    )
    .await
    {
        Ok(r) => r,
        Err(r) => *r,
    }
}

#[derive(sqlx::FromRow)]
struct RefreshRow {
    family: Uuid,
    client_id: String,
    account_id: Uuid,
    scopes: Vec<String>,
    expires_at: OffsetDateTime,
    rotated_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
}

/// `grant_type=refresh_token`: rotates the token; presenting an already-rotated token is treated
/// as theft and kills the whole family.
pub async fn token_refresh(
    s: &AppState,
    headers: &HeaderMap,
    p: &HashMap<String, String>,
    ip: &str,
) -> Response {
    let c = match token_client(s, headers, p, ip).await {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let Some(tok) = p.get("refresh_token") else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "refresh_token required",
        );
    };
    let row: Option<RefreshRow> = match sqlx::query_as(
        "select family, client_id, account_id, scopes, expires_at, rotated_at, revoked_at \
         from musubi.oauth_refresh_tokens where token_hash = $1",
    )
    .bind(hash(tok))
    .fetch_optional(&s.db)
    .await
    {
        Ok(r) => r,
        Err(e) => return ApiError(Error::internal(e)).into_response(),
    };
    let Some(row) = row else {
        note_failure(s, &c.client_id, "unknown_refresh", ip).await;
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "unknown refresh token",
        );
    };
    if row.client_id != c.client_id {
        note_failure(s, &c.client_id, "refresh_client_mismatch", ip).await;
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "token belongs to another client",
        );
    }
    let now = OffsetDateTime::now_utc();
    if row.rotated_at.is_some() {
        let _ = sqlx::query(
            "update musubi.oauth_refresh_tokens set revoked_at = coalesce(revoked_at, now()) where family = $1",
        )
        .bind(row.family)
        .execute(&s.db)
        .await;
        s.events
            .emit(
                "oauth.refresh_reuse_detected",
                Some(AccountId(row.account_id)),
                json!({ "client_id": c.client_id, "family": row.family, "ip": ip }),
            )
            .await;
        note_failure(s, &c.client_id, "refresh_reused", ip).await;
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "refresh token already rotated; the token family was revoked",
        );
    }
    if row.revoked_at.is_some() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "refresh token revoked",
        );
    }
    if row.expires_at <= now {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "refresh token expired",
        );
    }
    let grant_alive: Option<(i64,)> = sqlx::query_as(
        "select 1::bigint from musubi.oauth_grants where client_id = $1 and account_id = $2 and revoked_at is null",
    )
    .bind(&row.client_id)
    .bind(row.account_id)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if grant_alive.is_none() {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "grant revoked by the player",
        );
    }
    // optional down-scoping on refresh
    let scopes = match p.get("scope") {
        Some(raw) => {
            let want = scopes_of(Some(raw));
            if want.is_empty() || !crate::scopes::within(&want, &row.scopes) {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_scope",
                    "scope exceeds the original grant",
                );
            }
            want
        }
        None => row.scopes.clone(),
    };
    // Atomic claim: two concurrent presentations of the same token must yield exactly one
    // rotation; the loser is treated as reuse (RFC 6749 §10.4 / OAuth 2.1 §6.1).
    let claimed = sqlx::query(
        "update musubi.oauth_refresh_tokens set rotated_at = now(), last_used_at = now() \
         where token_hash = $1 and rotated_at is null and revoked_at is null",
    )
    .bind(hash(tok))
    .execute(&s.db)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if claimed == 0 {
        let _ = sqlx::query(
            "update musubi.oauth_refresh_tokens set revoked_at = coalesce(revoked_at, now()) where family = $1",
        )
        .bind(row.family)
        .execute(&s.db)
        .await;
        s.events
            .emit(
                "oauth.refresh_reuse_detected",
                Some(AccountId(row.account_id)),
                json!({ "client_id": c.client_id, "family": row.family, "ip": ip, "concurrent": true }),
            )
            .await;
        note_failure(s, &c.client_id, "refresh_reused", ip).await;
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_grant",
            "refresh token already rotated; the token family was revoked",
        );
    }
    match issue(
        s,
        &c,
        AccountId(row.account_id),
        &scopes,
        row.family,
        ip,
        "refresh_token",
    )
    .await
    {
        Ok(r) => r,
        Err(r) => *r,
    }
}

/// RFC 7662 answer for tokens the server-key table does not know: player access tokens and
/// refresh tokens from this flow.
pub async fn introspect_player(s: &AppState, tok: &str, who: Option<&str>) -> Response {
    if tok.starts_with("mrt_") {
        let row: Option<RefreshRow> = sqlx::query_as(
            "select family, client_id, account_id, scopes, expires_at, rotated_at, revoked_at \
             from musubi.oauth_refresh_tokens where token_hash = $1",
        )
        .bind(hash(tok))
        .fetch_optional(&s.db)
        .await
        .unwrap_or(None);
        return match row {
            Some(r) if who.is_none_or(|w| w == r.client_id) => {
                let active = r.rotated_at.is_none()
                    && r.revoked_at.is_none()
                    && r.expires_at > OffsetDateTime::now_utc();
                Json(json!({
                    "active": active, "token_type": "refresh_token", "client_id": r.client_id,
                    "sub": AccountId(r.account_id), "scope": r.scopes.join(" "),
                    "exp": r.expires_at.unix_timestamp(), "family": r.family,
                }))
                .into_response()
            }
            _ => Json(json!({ "active": false })).into_response(),
        };
    }
    match s.auth.verify_access(tok) {
        Ok(a) => Json(json!({
            "active": true, "token_type": "player", "sub": a.account_id, "username": a.username,
            "scope": a.scopes.join(" "), "exp": a.exp, "session_id": a.session_id,
        }))
        .into_response(),
        Err(_) => Json(json!({ "active": false })).into_response(),
    }
}

/// RFC 7009 for refresh tokens of this flow (player access JWTs are short-lived and stateless).
pub async fn revoke_player_token(s: &AppState, tok: &str, who: Option<&str>) {
    if !tok.starts_with("mrt_") {
        return;
    }
    let row: Option<(Uuid, String, Uuid)> = sqlx::query_as(
        "select family, client_id, account_id from musubi.oauth_refresh_tokens where token_hash = $1",
    )
    .bind(hash(tok))
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);
    if let Some((family, client_id, account)) = row {
        if who.is_none_or(|w| w == client_id) {
            let _ = sqlx::query(
                "update musubi.oauth_refresh_tokens set revoked_at = coalesce(revoked_at, now()) where family = $1",
            )
            .bind(family)
            .execute(&s.db)
            .await;
            s.events
                .emit(
                    "oauth.player_token_revoked",
                    Some(AccountId(account)),
                    json!({ "client_id": client_id, "family": family, "by": who.unwrap_or("server-key") }),
                )
                .await;
        }
    }
}

#[derive(sqlx::FromRow)]
struct GrantRow {
    client_id: String,
    account_id: Uuid,
    scopes: Vec<String>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    last_used_at: Option<OffsetDateTime>,
    revoked_at: Option<OffsetDateTime>,
    name: String,
    kind: String,
    owner: Option<String>,
    live_tokens: i64,
}

const GRANT_COLS: &str = "g.client_id, g.account_id, g.scopes, g.created_at, g.updated_at, g.last_used_at, g.revoked_at, \
    c.name, c.kind, c.owner, \
    (select count(*) from musubi.oauth_refresh_tokens t where t.client_id = g.client_id and t.account_id = g.account_id \
       and t.revoked_at is null and t.rotated_at is null and t.expires_at > now()) as live_tokens";

fn grant_json(g: &GrantRow) -> Value {
    json!({
        "client_id": g.client_id, "client": { "name": g.name, "kind": g.kind, "owner": g.owner },
        "account_id": AccountId(g.account_id), "scopes": g.scopes,
        "created_at": g.created_at.unix_timestamp(), "updated_at": g.updated_at.unix_timestamp(),
        "last_used_at": g.last_used_at.map(|t| t.unix_timestamp()),
        "revoked_at": g.revoked_at.map(|t| t.unix_timestamp()),
        "live_refresh_tokens": g.live_tokens,
    })
}

/// Apps the player has authorised.
async fn my_grants(
    Bearer(me): Bearer,
    State(s): State<AppState>,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<GrantRow> = sqlx::query_as(&format!(
        "select {GRANT_COLS} from musubi.oauth_grants g join musubi.oauth_clients c using (client_id) \
         where g.account_id = $1 and g.revoked_at is null order by g.updated_at desc"
    ))
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "grants": rows.iter().map(grant_json).collect::<Vec<_>>() }),
    ))
}

async fn revoke_grant(s: &AppState, client_id: &str, account: AccountId, by: &str) -> Result<bool> {
    let n = sqlx::query(
        "update musubi.oauth_grants set revoked_at = now(), updated_at = now() \
         where client_id = $1 and account_id = $2 and revoked_at is null",
    )
    .bind(client_id)
    .bind(account.0)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Ok(false);
    }
    sqlx::query(
        "update musubi.oauth_refresh_tokens set revoked_at = coalesce(revoked_at, now()) \
         where client_id = $1 and account_id = $2",
    )
    .bind(client_id)
    .bind(account.0)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "oauth.grant_revoked",
            Some(account),
            json!({ "client_id": client_id, "by": by }),
        )
        .await;
    Ok(true)
}

/// Disconnect an app: the grant and all its refresh tokens die; outstanding access tokens
/// expire on their own (≤ client `token_ttl_secs`).
async fn revoke_my_grant(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(client_id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    if !revoke_grant(&s, &client_id, me.account_id, "player").await? {
        return Err(Error::NotFound(format!("no grant for client {client_id}")).into());
    }
    Ok(Json(json!({ "revoked": true })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AdminGrantsQuery {
    account_id: Option<Uuid>,
    include_revoked: bool,
    limit: Option<i64>,
}

async fn admin_grants(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AdminGrantsQuery>,
) -> std::result::Result<Json<Value>, ApiError> {
    if load(&s.db, &id).await?.is_none() {
        return Err(Error::NotFound(format!("oauth client {id}")).into());
    }
    let rows: Vec<GrantRow> = sqlx::query_as(&format!(
        "select {GRANT_COLS} from musubi.oauth_grants g join musubi.oauth_clients c using (client_id) \
         where g.client_id = $1 and ($2::uuid is null or g.account_id = $2) and ($3 or g.revoked_at is null) \
         order by g.updated_at desc limit $4"
    ))
    .bind(&id)
    .bind(q.account_id)
    .bind(q.include_revoked)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let total: (i64,) = sqlx::query_as(
        "select count(*) from musubi.oauth_grants where client_id = $1 and revoked_at is null",
    )
    .bind(&id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "client_id": id, "active_grants": total.0,
        "grants": rows.iter().map(grant_json).collect::<Vec<_>>(),
    })))
}

/// Operator kill-switch: revoke every player grant (or one account's) of a client.
async fn admin_revoke_grants(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<AdminGrantsQuery>,
) -> std::result::Result<Json<Value>, ApiError> {
    if load(&s.db, &id).await?.is_none() {
        return Err(Error::NotFound(format!("oauth client {id}")).into());
    }
    let accounts: Vec<(Uuid,)> = sqlx::query_as(
        "select account_id from musubi.oauth_grants where client_id = $1 and revoked_at is null \
         and ($2::uuid is null or account_id = $2)",
    )
    .bind(&id)
    .bind(q.account_id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut n = 0;
    for (a,) in accounts {
        if revoke_grant(&s, &id, AccountId(a), "admin").await? {
            n += 1;
        }
    }
    Ok(Json(json!({ "revoked": n })))
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn random_token_is_prefixed_256_bit_hex() {
        let a = random_token("mrt_");
        let b = random_token("mrt_");
        assert_eq!(a.len(), 4 + 64);
        assert!(a.starts_with("mrt_"));
        assert!(a[4..].bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn pkce_s256_matches_rfc_7636_appendix_b() {
        assert_eq!(
            pkce_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    /// The rotation claim is a conditional update: of two concurrent presentations of one
    /// refresh token exactly one wins, the other sees zero rows and is handled as reuse.
    #[tokio::test]
    async fn refresh_rotation_claim_is_atomic() {
        let Ok(url) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
            return;
        };
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        crate::db::migrate(&db).await.unwrap();
        let client_id = format!("mc_{}", Uuid::now_v7().simple());
        sqlx::query(
            "insert into musubi.oauth_clients (client_id, name, kind, role, scopes, redirect_uris, allowed_ips, token_ttl_secs, secrets)
             values ($1, 'audit', 'public', 'server', '{}', '{}', '{}', 3600, '[]')",
        )
        .bind(&client_id)
        .execute(&db)
        .await
        .unwrap();
        let tok = random_token("mrt_");
        sqlx::query(
            "insert into musubi.oauth_refresh_tokens (token_hash, family, client_id, account_id, scopes, expires_at)
             values ($1, $2, $3, $4, '{}', now() + interval '1 hour')",
        )
        .bind(hash(&tok))
        .bind(Uuid::now_v7())
        .bind(&client_id)
        .bind(Uuid::now_v7())
        .execute(&db)
        .await
        .unwrap();
        let claim = || async {
            sqlx::query(
                "update musubi.oauth_refresh_tokens set rotated_at = now(), last_used_at = now() \
                 where token_hash = $1 and rotated_at is null and revoked_at is null",
            )
            .bind(hash(&tok))
            .execute(&db)
            .await
            .unwrap()
            .rows_affected()
        };
        let (a, b) = tokio::join!(claim(), claim());
        assert_eq!(a + b, 1, "exactly one presentation rotates the token");
        sqlx::query("delete from musubi.oauth_clients where client_id = $1")
            .bind(&client_id)
            .execute(&db)
            .await
            .unwrap();
    }
}
