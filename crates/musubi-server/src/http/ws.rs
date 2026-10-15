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
    token: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    status: Option<String>,
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
    };
    let (presence, encoding) = crate::realtime::connect::authorize(&s, &hello).await?;
    let hub = s.hub.clone();
    Ok(ws
        .max_message_size(Limits::default().max_frame_bytes)
        .on_upgrade(move |socket| async move {
            let params = crate::realtime::connect::params(&s, presence, encoding);
            crate::realtime::serve(hub, Box::new(WsConnection::new(socket)), params).await;
        }))
}
