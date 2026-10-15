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
        state.banned.check(auth.account_id)?;
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
        if parts.uri.path().starts_with("/v1/admin/") && caller.role != crate::keys::Role::Admin {
            return Err(Error::PermissionDenied(format!(
                "key '{}' has role {}, admin required",
                caller.name,
                caller.role.as_str()
            ))
            .into());
        }
        parts.extensions.insert(caller);
        Ok(ServerKey)
    }
}

/// `X-Musubi-Server-Key`: the config key (constant-time compare, bootstrap admin) or a key issued
/// through `/v1/admin/keys` with its role.
pub fn check_server_key(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> musubi_core::Result<crate::keys::Caller> {
    let given = headers
        .get("x-musubi-server-key")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Key".into()))?;
    let expected = state.cfg.http.server_key.as_bytes();
    let ok = given.len() == expected.len()
        && given
            .bytes()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if ok {
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
