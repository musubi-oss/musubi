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
        let mut body = json!({ "code": self.0.code(), "message": message });
        if let Some(id) = crate::telemetry::current_trace_id() {
            body["trace_id"] = json!(id);
        }
        let mut res = (status, Json(body)).into_response();
        if let Error::ResourceExhausted(m) = &self.0 {
            if let Some(secs) = retry_after_secs(m) {
                if let Ok(v) = axum::http::HeaderValue::from_str(&secs.to_string()) {
                    res.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
                }
            }
        }
        res.into_response()
    }
}

/// `RateLimiter` phrases its errors as "... retry in 2.3s"; surface that as whole seconds (min 1).
fn retry_after_secs(message: &str) -> Option<u64> {
    let rest = message.split("retry in ").nth(1)?;
    let num: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let secs: f64 = num.parse().ok()?;
    Some(secs.ceil().max(1.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_errors_carry_retry_after() {
        let res = ApiError(Error::ResourceExhausted(
            "rate limited (http); retry in 2.3s".into(),
        ))
        .into_response();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(res.headers()[axum::http::header::RETRY_AFTER], "3");
        let res = ApiError(Error::ResourceExhausted("full".into())).into_response();
        assert!(res.headers().get(axum::http::header::RETRY_AFTER).is_none());
    }
}
