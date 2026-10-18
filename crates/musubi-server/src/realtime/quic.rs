//! QUIC acceptor: one bidirectional stream per session; the first frame is a JSON `Hello`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use musubi_core::Error;
use musubi_transport::quic::{self, QuicConnection};
use musubi_transport::Frame;

use crate::app::AppState;
use crate::realtime::connect::{self, Hello};
use musubi_transport::Limits;

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// Initials answered with a Retry token because the source was not yet validated.
pub static RETRIES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Connections refused because `quic.max_pending_handshakes` were already in flight.
pub static REFUSED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Bind and serve forever. Returns the bound address (port 0 is resolved).
pub async fn start(state: AppState, listen: SocketAddr) -> anyhow::Result<SocketAddr> {
    let cfg = match (&state.cfg.quic.cert_pem, &state.cfg.quic.key_pem) {
        (Some(c), Some(k)) => quic::server_config(&std::fs::read(c)?, &std::fs::read(k)?)?,
        _ => {
            tracing::warn!(
                "quic: no cert configured, using a self-signed localhost certificate (dev only)"
            );
            quic::dev_self_signed()?.0
        }
    };
    start_with(state, listen, cfg).await
}

pub async fn start_with(
    state: AppState,
    listen: SocketAddr,
    cfg: quinn::ServerConfig,
) -> anyhow::Result<SocketAddr> {
    let endpoint = quinn::Endpoint::server(cfg, listen)?;
    let addr = endpoint.local_addr()?;
    let retry = state.cfg.quic.retry.unwrap_or(true);
    let max_pending = state.cfg.quic.max_pending_handshakes.unwrap_or(1024);
    let pending = Arc::new(tokio::sync::Semaphore::new(max_pending));
    tracing::info!(%addr, retry, max_pending, "quic listening");
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            // plan 3.7: address validation before any handshake state, then a cap on handshakes
            // in flight so a flood of Initials cannot exhaust the node
            if retry && !incoming.remote_address_validated() {
                RETRIES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                incoming.retry().ok();
                continue;
            }
            let Ok(permit) = pending.clone().try_acquire_owned() else {
                REFUSED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                incoming.refuse();
                continue;
            };
            let state = state.clone();
            tokio::spawn(async move {
                let conn = incoming.await;
                drop(permit);
                match conn {
                    Ok(conn) => handle(state, conn).await,
                    Err(e) => tracing::debug!(error = %e, "quic handshake failed"),
                }
            });
        }
    });
    Ok(addr)
}

async fn handle(state: AppState, conn: quinn::Connection) {
    let (mut send, mut recv) = match conn.accept_bi().await {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "quic: no stream");
            return;
        }
    };
    let max = Limits::default().max_frame_bytes;
    let hello =
        match tokio::time::timeout(HELLO_TIMEOUT, QuicConnection::read_frame(&mut recv, max)).await
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
            let c = Box::new(QuicConnection::new(conn.clone(), send, recv, max));
            connect::resume_any(&state, &token, c, encoding).await;
            let _ = tokio::time::timeout(Duration::from_millis(500), conn.closed()).await;
            conn.close(0u32.into(), b"bye");
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
            let _ = QuicConnection::write_frame(
                &mut send,
                Frame::Text(serde_json::to_string(&msg).unwrap_or_default()),
            )
            .await;
            let _ = send.finish();
            // Let the error frame flush before tearing the connection down (close drops unsent data).
            let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
            conn.close(1u32.into(), b"unauthenticated");
            return;
        }
    };
    let mut params = connect::params(&state, presence, encoding);
    params.resumable = resumable;
    let hub: Arc<_> = state.hub.clone();
    crate::realtime::serve(
        hub,
        Box::new(QuicConnection::new(conn.clone(), send, recv, max)),
        params,
    )
    .await;
    // Give the peer a moment to drain the final frames, then close.
    let _ = tokio::time::timeout(Duration::from_millis(500), conn.closed()).await;
    conn.close(0u32.into(), b"bye");
}
