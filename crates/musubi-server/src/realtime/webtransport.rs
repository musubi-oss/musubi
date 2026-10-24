//! WebTransport acceptor (browsers): one bidirectional stream per session with the QUIC framing;
//! the first frame is a JSON `Hello`. Same handshake and session loop as the QUIC acceptor.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use musubi_core::Error;
use musubi_transport::framing;
use musubi_transport::webtransport::{self, WtConnection};
use musubi_transport::Frame;

use crate::app::AppState;
use crate::realtime::connect::{self, Hello};
use musubi_transport::Limits;

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Bind and serve forever. Returns the bound address (port 0 is resolved).
pub async fn start(state: AppState, listen: SocketAddr) -> anyhow::Result<SocketAddr> {
    let cfg = match (
        &state.cfg.webtransport.cert_pem,
        &state.cfg.webtransport.key_pem,
    ) {
        (Some(c), Some(k)) => webtransport::server_config(listen, c, k).await?,
        _ => {
            tracing::warn!(
                "webtransport: no cert configured, using a self-signed localhost certificate (dev only; browsers need its hash)"
            );
            webtransport::dev_self_signed(listen)?.0
        }
    };
    start_with(state, cfg).await
}

pub async fn start_with(
    state: AppState,
    cfg: wtransport::ServerConfig,
) -> anyhow::Result<SocketAddr> {
    let endpoint = wtransport::Endpoint::server(cfg)?;
    let addr = endpoint.local_addr()?;
    // same cap as the QUIC acceptor (`quic.max_pending_handshakes`): WebTransport rides on QUIC
    // and a flood of half-open handshakes would otherwise spawn a task each, without bound
    let max_pending = state.cfg.quic.max_pending_handshakes.unwrap_or(1024);
    let pending = Arc::new(tokio::sync::Semaphore::new(max_pending));
    tracing::info!(%addr, max_pending, "webtransport listening");
    tokio::spawn(async move {
        loop {
            let incoming = endpoint.accept().await;
            let Ok(permit) = pending.clone().try_acquire_owned() else {
                metrics::counter!("musubi_webtransport_refused_total").increment(1);
                incoming.refuse();
                continue;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(req) => req.accept().await,
                    Err(e) => {
                        tracing::debug!(error = %e, "webtransport handshake failed");
                        return;
                    }
                };
                drop(permit);
                match conn {
                    Ok(conn) => handle(state, conn).await,
                    Err(e) => tracing::debug!(error = %e, "webtransport session refused"),
                }
            });
        }
    });
    Ok(addr)
}

async fn handle(state: AppState, conn: wtransport::Connection) {
    let (mut send, mut recv) = match conn.accept_bi().await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "webtransport: no stream");
            return;
        }
    };
    let max = Limits::default().max_frame_bytes;
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, framing::read_frame(&mut recv, max)).await
    {
        Ok(Ok(Frame::Text(t))) => serde_json::from_str::<Hello>(&t)
            .map_err(|e| Error::InvalidArgument(format!("hello: {e}"))),
        Ok(Ok(_)) => Err(Error::InvalidArgument("hello must be a text frame".into())),
        Ok(Err(e)) => Err(Error::Internal(e.to_string())),
        Err(_) => Err(Error::InvalidArgument("hello timeout".into())),
    };
    if let Ok(h) = &hello {
        if let Some(token) = h.resume.clone() {
            let encoding = connect::encoding_of(h).unwrap_or(musubi_proto::Encoding::Json);
            let c = Box::new(WtConnection::new(conn.clone(), send, recv, max));
            connect::resume_any(&state, &token, c, encoding).await;
            let _ = tokio::time::timeout(Duration::from_millis(500), conn.closed()).await;
            conn.close(wtransport::VarInt::from_u32(0), b"bye");
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
            let _ = framing::write_frame(
                &mut send,
                Frame::Text(serde_json::to_string(&msg).unwrap_or_default()),
            )
            .await;
            let _ = send.finish().await;
            // Let the error frame flush before tearing the connection down (close drops unsent data).
            tokio::time::sleep(Duration::from_millis(200)).await;
            conn.close(wtransport::VarInt::from_u32(1), b"unauthenticated");
            return;
        }
    };
    let mut params = connect::params(&state, presence, encoding);
    params.resumable = resumable;
    let hub: Arc<_> = state.hub.clone();
    crate::realtime::serve(
        hub,
        Box::new(WtConnection::new(conn.clone(), send, recv, max)),
        params,
    )
    .await;
    // Give the peer a moment to drain the final frames, then close.
    let _ = tokio::time::timeout(Duration::from_millis(500), conn.closed()).await;
    conn.close(wtransport::VarInt::from_u32(0), b"bye");
}
