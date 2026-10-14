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
        Ok(Bearer(state.auth.verify_access(token)?))
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
        let given = parts
            .headers
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
        if !ok {
            return Err(Error::Unauthenticated("bad server key".into()).into());
        }
        Ok(ServerKey)
    }
}
