//! WebRTC signalling: one HTTP round trip carries the browser's SDP offer and our answer; the
//! session itself then runs over the data channel (see `realtime::webrtc`).

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use musubi_core::Error;
use musubi_transport::Limits;
use serde_json::Value;

use crate::app::AppState;
use crate::http::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/rtc/offer", post(offer))
}

/// Body: the browser's `RTCSessionDescription` (`{type: "offer", sdp}`), or `{offer: {...}}`.
/// Reply: the answer in the same form. Unauthenticated like opening a socket; the `Hello` on the
/// data channel carries the token.
async fn offer(
    State(s): State<AppState>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let rtc = s
        .rtc
        .as_ref()
        .ok_or_else(|| Error::NotFound("webrtc not configured".into()))?;
    let offer = body.get("offer").cloned().unwrap_or(body);
    let (answer, conn) = rtc
        .accept_offer(offer, Limits::default().max_frame_bytes)
        .await
        .map_err(|e| Error::InvalidArgument(format!("offer: {e}")))?;
    tokio::spawn(crate::realtime::webrtc::handle(s.clone(), conn));
    Ok(Json(answer))
}
