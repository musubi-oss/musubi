//! Generic streams (P1-59) for game servers and tools: join/leave/send/count on arbitrary
//! stream ids over the realtime hub. Module code gets the same primitive as `stream.*` host ops.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{Error, SessionId};
use musubi_proto::ServerMessage;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub const MAX_STREAM_ID: usize = 128;
pub const MAX_STREAM_DATA: usize = 16 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/streams", get(inventory))
        .route("/v1/server/streams/{id}", get(members))
        .route("/v1/server/streams/{id}/join", post(join))
        .route("/v1/server/streams/{id}/leave", post(leave))
        .route("/v1/server/streams/{id}/send", post(send))
}

pub fn check_stream_id(id: &str) -> musubi_core::Result<()> {
    if id.is_empty() || id.len() > MAX_STREAM_ID {
        return Err(Error::InvalidArgument(format!(
            "stream id must be 1..={MAX_STREAM_ID} bytes"
        )));
    }
    if id.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(Error::InvalidArgument(
            "stream id must not contain whitespace or control characters".into(),
        ));
    }
    Ok(())
}

pub fn check_stream_data(data: &Value) -> musubi_core::Result<()> {
    let n = serde_json::to_vec(data).map(|b| b.len()).unwrap_or(0);
    if n > MAX_STREAM_DATA {
        return Err(Error::InvalidArgument(format!(
            "stream data too large ({n} > {MAX_STREAM_DATA} bytes)"
        )));
    }
    Ok(())
}

pub fn parse_session(s: &str) -> musubi_core::Result<SessionId> {
    uuid::Uuid::parse_str(s)
        .map(SessionId::from)
        .map_err(|_| Error::InvalidArgument("bad session_id".into()))
}

pub fn members_json(s: &AppState, stream: &str) -> Value {
    let members: Vec<Value> = s
        .hub
        .stream_members(stream)
        .into_iter()
        .map(|p| {
            json!({
                "account_id": p.account_id,
                "session_id": p.session_id,
                "username": p.username,
                "status": p.status,
            })
        })
        .collect();
    json!({ "stream": stream, "count": members.len(), "members": members })
}

#[derive(Deserialize)]
struct SessionReq {
    session_id: String,
}

#[derive(Deserialize)]
struct SendReq {
    data: Value,
}

async fn inventory(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let mut inv = s.hub.stream_inventory();
    inv.sort();
    let streams: Vec<Value> = inv
        .into_iter()
        .map(|(id, n)| json!({ "stream": id, "count": n }))
        .collect();
    Ok(Json(json!({ "streams": streams })))
}

async fn members(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    check_stream_id(&id)?;
    Ok(Json(members_json(&s, &id)))
}

async fn join(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<SessionReq>,
) -> Result<Json<Value>, ApiError> {
    check_stream_id(&id)?;
    let sid = parse_session(&r.session_id)?;
    if !s.hub.join_stream(&id, sid) {
        return Err(Error::NotFound("session is not connected to this node".into()).into());
    }
    Ok(Json(
        json!({ "stream": id, "joined": true, "count": s.hub.stream_count(&id) }),
    ))
}

async fn leave(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<SessionReq>,
) -> Result<Json<Value>, ApiError> {
    check_stream_id(&id)?;
    let sid = parse_session(&r.session_id)?;
    let left = s.hub.leave_stream(&id, sid);
    Ok(Json(
        json!({ "stream": id, "left": left, "count": s.hub.stream_count(&id) }),
    ))
}

async fn send(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<SendReq>,
) -> Result<Json<Value>, ApiError> {
    check_stream_id(&id)?;
    check_stream_data(&r.data)?;
    let delivered = s.hub.send_stream(
        &id,
        ServerMessage::StreamData {
            stream: id.clone(),
            data: r.data,
            sender: None,
        },
    );
    Ok(Json(json!({ "stream": id, "delivered": delivered })))
}
