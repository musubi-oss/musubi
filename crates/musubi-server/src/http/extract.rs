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
