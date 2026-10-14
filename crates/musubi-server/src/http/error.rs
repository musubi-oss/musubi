use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use musubi_core::Error;
use serde_json::json;

/// Wire form of `musubi_core::Error`: `{ "code": "...", "message": "..." }`.
pub struct ApiError(pub Error);

impl From<Error> for ApiError {
    fn from(e: Error) -> Self {
        ApiError(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status =
            StatusCode::from_u16(self.0.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        if status.is_server_error() {
            tracing::error!(error = %self.0, "request failed");
        }
        let message = match &self.0 {
            Error::Internal(_) => "internal error".to_string(),
            other => other.to_string(),
        };
        (
            status,
            Json(json!({ "code": self.0.code(), "message": message })),
        )
            .into_response()
    }
}
