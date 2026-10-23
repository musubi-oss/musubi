use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use musubi_transport::ws::WsConnection;
use musubi_transport::Limits;
use serde::Deserialize;

use crate::app::AppState;
use crate::http::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new().route("/ws", get(upgrade))
}

#[derive(Deserialize)]
struct WsQuery {
    #[serde(default)]
    token: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, deserialize_with = "lenient_bool")]
    resumable: bool,
    #[serde(default)]
    resume: Option<String>,
}

/// Query flags arrive as `true`/`false` from the Rust clients and as `1`/`0` from engines that
/// print booleans as numbers; both spell the same flag.
fn lenient_bool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let raw = String::deserialize(d)?;
    match raw.as_str() {
        "" | "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(serde::de::Error::custom(format!("not a boolean: {other}"))),
    }
}

/// tungstenite zero-fills up to `read_buffer_size` bytes before *every* read (`read_in`), and
/// the default is 128 KiB: at 1 000 players that memset was a fifth of the node's CPU while
/// client frames are a few hundred bytes. Larger frames still arrive, in more reads.
const WS_READ_BUFFER: usize = 8 * 1024;

/// `GET /ws?token=<access jwt>&format=json|binary[&status=...]`
async fn upgrade(
    State(s): State<AppState>,
    Query(q): Query<WsQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let hello = crate::realtime::connect::Hello {
        token: q.token,
        format: q.format,
        status: q.status,
        resumable: q.resumable,
        resume: q.resume,
    };
    let hub = s.hub.clone();
    if let Some(token) = hello.resume.clone() {
        let encoding = crate::realtime::connect::encoding_of(&hello)?;
        return Ok(ws
            .max_message_size(Limits::default().max_frame_bytes)
            .read_buffer_size(WS_READ_BUFFER)
            .on_upgrade(move |socket| async move {
                let conn = Box::new(WsConnection::new(socket));
                crate::realtime::connect::resume_any(&s, &token, conn, encoding).await;
            }));
    }
    let (presence, encoding) = crate::realtime::connect::authorize(&s, &hello).await?;
    Ok(ws
        .max_message_size(Limits::default().max_frame_bytes)
        .read_buffer_size(WS_READ_BUFFER)
        .on_upgrade(move |socket| async move {
            let mut params = crate::realtime::connect::params(&s, presence, encoding);
            params.resumable = hello.resumable;
            crate::realtime::serve(hub, Box::new(WsConnection::new(socket)), params).await;
        }))
}
