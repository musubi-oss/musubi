use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use musubi_core::{Error, SessionId};
use musubi_proto::{Encoding, UserPresence};
use musubi_transport::ws::WsConnection;
use musubi_transport::Limits;
use serde::Deserialize;

use crate::app::AppState;
use crate::http::ApiError;
use crate::realtime::ConnectParams;

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
    let me = s.auth.verify_access(&q.token)?;
    let encoding = match q.format.as_deref() {
        None => Encoding::Json,
        Some(f) => Encoding::parse(f)
            .ok_or_else(|| Error::InvalidArgument("format must be json or binary".into()))?,
    };
    let username: String = sqlx::query_scalar(
        "select username from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::Unauthenticated("account gone".into()))?;
    // Each socket is its own realtime session, distinct from the auth session that issued the token.
    let presence = UserPresence {
        account_id: me.account_id,
        session_id: SessionId::new(),
        username,
        status: q.status,
    };
    let hub = s.hub.clone();
    Ok(ws
        .max_message_size(Limits::default().max_frame_bytes)
        .on_upgrade(move |socket| async move {
            let params = ConnectParams {
                presence,
                encoding,
                limits: Limits::default(),
                chat: s.chat.clone(),
            };
            crate::realtime::serve(hub, Box::new(WsConnection::new(socket)), params).await;
        }))
}
