//! Raw UDP acceptor (dedicated servers ↔ core): the transport's handshake admits a peer, then the
//! first frame on the reliable channel is the JSON `Hello` — the same session loop as everywhere.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use musubi_core::Error;
use musubi_transport::udp::UdpServer;
use musubi_transport::{Connection, Frame, Limits};

use crate::app::AppState;
use crate::realtime::connect::{self, Hello};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Bind and serve forever. Returns the bound address (port 0 is resolved).
pub async fn start(
    state: AppState,
    listen: SocketAddr,
    secret: Option<[u8; 32]>,
) -> anyhow::Result<SocketAddr> {
    let server = UdpServer::bind(listen, secret, Limits::default().max_frame_bytes)
        .await
        .map_err(|e| anyhow::anyhow!("udp: {e}"))?;
    let addr = server.local_addr();
    tracing::info!(%addr, "udp listening");
    tokio::spawn(async move {
        while let Some(conn) = server.accept().await {
            tokio::spawn(handle(state.clone(), Box::new(conn)));
        }
    });
    Ok(addr)
}

pub(crate) async fn handle(state: AppState, conn: Box<dyn Connection>) {
    handle_with_timeout(state, conn, HELLO_TIMEOUT).await;
}

/// Shared by the UDP and WebRTC acceptors: `Hello` as the first frame, then resume or serve.
pub(crate) async fn handle_with_timeout(
    state: AppState,
    mut conn: Box<dyn Connection>,
    hello_timeout: Duration,
) {
    let hello = match tokio::time::timeout(hello_timeout, conn.recv()).await {
        Ok(Ok(Frame::Text(t))) => serde_json::from_str::<Hello>(&t)
            .map_err(|e| Error::InvalidArgument(format!("hello: {e}"))),
        Ok(Ok(_)) => Err(Error::InvalidArgument("hello must be a text frame".into())),
        Ok(Err(e)) => Err(Error::Internal(e.to_string())),
        Err(_) => Err(Error::InvalidArgument("hello timeout".into())),
    };
    if let Ok(h) = &hello {
        if let Some(token) = h.resume.clone() {
            let encoding = connect::encoding_of(h).unwrap_or(musubi_proto::Encoding::Json);
            connect::resume_any(&state, &token, conn, encoding).await;
            return;
        }
    }
    let resumable = hello.as_ref().map(|h| h.resumable).unwrap_or(false);
    let (presence, encoding) = match async { connect::authorize(&state, &hello?).await }.await {
        Ok(v) => v,
        Err(e) => {
            let msg = musubi_proto::Envelope {
                cid: None,
                msg: musubi_proto::ServerMessage::error(&e),
            };
            let _ = conn
                .send(Frame::Text(serde_json::to_string(&msg).unwrap_or_default()))
                .await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            conn.close().await;
            return;
        }
    };
    let mut params = connect::params(&state, presence, encoding);
    params.resumable = resumable;
    let hub: Arc<_> = state.hub.clone();
    crate::realtime::serve(hub, conn, params).await;
}
