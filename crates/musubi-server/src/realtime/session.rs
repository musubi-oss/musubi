use std::sync::Arc;
use std::time::Duration;

use musubi_core::{Error, SessionId};
use musubi_proto::{ClientMessage, Encoding, Envelope, ServerMessage, UserPresence};
use musubi_transport::{Connection, Frame, Limits};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::hub::SessionHandle;
use super::Hub;

pub struct ConnectParams {
    pub presence: UserPresence,
    pub encoding: Encoding,
    pub limits: Limits,
}

/// Drive one realtime connection until it closes. Registers with the hub on entry and
/// always unregisters on exit.
pub async fn serve(hub: Arc<Hub>, mut conn: Box<dyn Connection>, params: ConnectParams) {
    let ConnectParams {
        presence,
        encoding,
        limits,
    } = params;
    let sid = presence.session_id;
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(256);
    hub.join(SessionHandle {
        presence: presence.clone(),
        tx: tx.clone(),
    });
    info!(session = %sid, account = %presence.account_id, transport = conn.kind(), "realtime connected");

    let result = run_loop(&hub, conn.as_mut(), sid, encoding, limits, tx, &mut rx).await;
    hub.leave(sid);
    conn.close().await;
    match result {
        Ok(()) => info!(session = %sid, "realtime disconnected"),
        Err(e) => debug!(session = %sid, error = %e, "realtime closed with error"),
    }
}

async fn run_loop(
    hub: &Hub,
    conn: &mut dyn Connection,
    sid: SessionId,
    encoding: Encoding,
    limits: Limits,
    tx: mpsc::Sender<ServerMessage>,
    rx: &mut mpsc::Receiver<ServerMessage>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut idle = tokio::time::interval(limits.idle_timeout);
    idle.reset();
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(msg) = out else { return Ok(()) };
                let bytes = encoding.encode(&Envelope { cid: None, msg })?;
                conn.send(Frame::encoded(encoding, bytes)).await?;
            }
            inc = conn.recv() => {
                let frame = inc?;
                idle.reset();
                let Some(payload) = frame.payload() else { return Ok(()) };
                if payload.len() > limits.max_frame_bytes {
                    send(conn, encoding, None, ServerMessage::error(&Error::InvalidArgument("frame too large".into()))).await?;
                    continue;
                }
                let env: Envelope<ClientMessage> = match encoding.decode(payload) {
                    Ok(e) => e,
                    Err(e) => {
                        send(conn, encoding, None, ServerMessage::error(&Error::InvalidArgument(format!("bad frame: {e}")))).await?;
                        continue;
                    }
                };
                if let Some(reply) = handle(hub, sid, env.msg) {
                    let bytes = encoding.encode(&Envelope { cid: env.cid, msg: reply })?;
                    conn.send(Frame::encoded(encoding, bytes)).await?;
                }
            }
            _ = idle.tick() => {
                warn!(session = %sid, "idle timeout");
                let _ = tx.try_send(ServerMessage::error(&Error::Unavailable("idle timeout".into())));
                tokio::time::sleep(Duration::from_millis(50)).await;
                return Ok(());
            }
        }
    }
}

async fn send(
    conn: &mut dyn Connection,
    enc: Encoding,
    cid: Option<String>,
    msg: ServerMessage,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let bytes = enc.encode(&Envelope { cid, msg })?;
    conn.send(Frame::encoded(enc, bytes)).await?;
    Ok(())
}

fn handle(hub: &Hub, sid: SessionId, msg: ClientMessage) -> Option<ServerMessage> {
    match msg {
        ClientMessage::Ping => Some(ServerMessage::Pong),
        ClientMessage::StatusUpdate { status } => {
            if status.as_ref().is_some_and(|s| s.chars().count() > 128) {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "status must be ≤ 128 chars".into(),
                )));
            }
            hub.set_status(sid, status);
            None
        }
        ClientMessage::StatusFollow { account_ids } => {
            if account_ids.len() > 1000 {
                return Some(ServerMessage::error(&Error::InvalidArgument(
                    "follow at most 1000 users".into(),
                )));
            }
            Some(ServerMessage::Status {
                presences: hub.follow(sid, &account_ids),
            })
        }
        ClientMessage::StatusUnfollow { account_ids } => {
            hub.unfollow(sid, &account_ids);
            None
        }
        ClientMessage::Rpc { id, .. } => Some(ServerMessage::error(&Error::NotFound(format!(
            "rpc {id}: runtime not loaded"
        )))),
    }
}
