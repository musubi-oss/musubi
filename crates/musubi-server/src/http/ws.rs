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
    #[serde(default)]
    resumable: bool,
    #[serde(default)]
    resume: Option<String>,
}

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
            .on_upgrade(move |socket| async move {
                let conn = Box::new(WsConnection::new(socket));
                crate::realtime::connect::resume_any(&s, &token, conn, encoding).await;
            }));
    }
    let (presence, encoding) = crate::realtime::connect::authorize(&s, &hello).await?;
    Ok(ws
        .max_message_size(Limits::default().max_frame_bytes)
        .on_upgrade(move |socket| async move {
            let mut params = crate::realtime::connect::params(&s, presence, encoding);
            params.resumable = hello.resumable;
            crate::realtime::serve(hub, Box::new(WsConnection::new(socket)), params).await;
        }))
}
