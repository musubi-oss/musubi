use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use musubi_auth::Authenticated;
use musubi_core::Error;

use crate::app::AppState;
use crate::http::ApiError;

/// Extracts and verifies `Authorization: Bearer <access token>`.
pub struct Bearer(pub Authenticated);

impl FromRequestParts<AppState> for Bearer {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Unauthenticated("missing bearer token".into()))?;
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or_else(|| Error::Unauthenticated("malformed authorization header".into()))?;
        let auth = state.auth.verify_access(token)?;
        if !crate::scopes::allows(&auth.scopes, parts.method.as_str(), parts.uri.path()) {
            return Err(Error::PermissionDenied(format!(
                "token scopes [{}] do not cover {} {}",
                auth.scopes.join(", "),
                parts.method,
                parts.uri.path()
            ))
            .into());
        }
        state.banned.check(auth.account_id)?;
        if !crate::legal::exempt(parts.uri.path()) {
            state.legal.check(&state.db, auth.account_id).await?;
        }
        state.limits.account(auth.account_id)?;
        Ok(Bearer(auth))
    }
}

/// Server-to-server caller authenticated by the shared `http.server_key`.
pub struct ServerKey;

impl FromRequestParts<AppState> for ServerKey {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let caller = check_server_key(state, &parts.headers)?;
        state.limits.key(&caller)?;
        if !caller.role.allows(parts.method.as_str(), parts.uri.path()) {
            return Err(Error::PermissionDenied(format!(
                "key '{}' has role {}: {} {} is outside its scope",
                caller.name,
                caller.role.as_str(),
                parts.method,
                parts.uri.path()
            ))
            .into());
        }
        if !crate::scopes::allows(&caller.scopes, parts.method.as_str(), parts.uri.path()) {
            return Err(Error::PermissionDenied(format!(
                "key '{}' scopes [{}] do not cover {} {}",
                caller.name,
                caller.scopes.join(", "),
                parts.method,
                parts.uri.path()
            ))
            .into());
        }
        parts.extensions.insert(caller);
        Ok(ServerKey)
    }
}

/// A game server process: `X-Musubi-Server-Key` carries the per-server token the fleet put in
/// its `MUSUBI_SERVER_KEY` (`gs_<server id>_…`), which only identifies that one server — a leaked
/// or rogue server cannot speak for another or reach `/v1/server/*`. With
/// `[fleet].legacy_server_key = true` the operator key plus `X-Musubi-Server-Id` is still accepted.
pub struct GameServerAuth(pub uuid::Uuid);

impl FromRequestParts<AppState> for GameServerAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let key = parts
            .headers
            .get("x-musubi-server-key")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Key".into()))?;
        if key.starts_with("gs_") {
            let id = state.fleets.authenticate_server_token(key).await?;
            state.limits.game_server(id)?;
            return Ok(GameServerAuth(id));
        }
        if !state.cfg.fleet.legacy_server_key {
            return Err(Error::Unauthenticated(
                "game server endpoints take the per-server token from MUSUBI_SERVER_KEY (gs_…); set [fleet].legacy_server_key = true to accept the operator key with X-Musubi-Server-Id"
                    .into(),
            )
            .into());
        }
        let caller = check_server_key(state, &parts.headers)?;
        state.limits.key(&caller)?;
        let id = parts
            .headers
            .get("x-musubi-server-id")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Id".into()))?;
        let id = uuid::Uuid::parse_str(id)
            .map_err(|_| Error::InvalidArgument("bad X-Musubi-Server-Id".into()))?;
        Ok(GameServerAuth(id))
    }
}

/// `X-Musubi-Server-Key`: the config key (constant-time compare, bootstrap admin) or a key issued
/// through `/v1/admin/keys` with its role.
/// Constant-time equality for keys/tokens (length leaks, bytes do not).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

pub fn check_server_key(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> musubi_core::Result<crate::keys::Caller> {
    // OAuth2 client-credentials tokens (P1-23) are server keys and may travel as a bearer token
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.starts_with("msk_"));
    let given = headers
        .get("x-musubi-server-key")
        .and_then(|v| v.to_str().ok())
        .or(bearer)
        .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Key".into()))?;
    let expected = state.cfg.http.server_key.as_bytes();
    if ct_eq(given.as_bytes(), expected) {
        return Ok(crate::keys::Caller::bootstrap());
    }
    if let Some(c) = state.keys.lookup(given) {
        state.keys.touch(Some(state.db.clone()), c.id);
        return Ok(c);
    }
    Err(Error::Unauthenticated("bad server key".into()))
}

/// Best-effort client address: first `X-Forwarded-For` hop, then `X-Real-IP`, then the peer
/// address (when served with connect info), else `"unknown"`. Infallible.
pub struct ClientIp(pub String);

impl<S: Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let header = |n: &str| {
            parts
                .headers
                .get(n)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let ip = header("x-forwarded-for")
            .or_else(|| header("x-real-ip"))
            .or_else(|| {
                parts
                    .extensions
                    .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                    .map(|c| c.0.ip().to_string())
            })
            .unwrap_or_else(|| "unknown".into());
        Ok(ClientIp(ip))
    }
}
