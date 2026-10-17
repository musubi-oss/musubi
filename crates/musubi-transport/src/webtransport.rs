//! WebTransport (HTTP/3 over QUIC, `wtransport`) for browsers: one bidirectional stream per
//! realtime session with the same framing as the QUIC transport (`framing`), unreliable frames as
//! datagrams with the QUIC transport's 1-byte type prefix (oversized ones fall back to the stream).

use futures::future::BoxFuture;
use wtransport::{RecvStream, SendStream};

use crate::quic::QuicConnection;
use crate::{framing, Connection, Frame, TransportError};

pub struct WtConnection {
    conn: wtransport::Connection,
    send: SendStream,
    /// See `QuicConnection::frames`: stream reads must not be cancelled mid-frame.
    frames: tokio::sync::mpsc::Receiver<Result<Frame, TransportError>>,
}

impl WtConnection {
    pub fn new(
        conn: wtransport::Connection,
        send: SendStream,
        mut recv: RecvStream,
        max_frame: usize,
    ) -> Self {
        let (tx, frames) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let r = framing::read_frame(&mut recv, max_frame).await;
                let stop = r.is_err();
                if tx.send(r).await.is_err() || stop {
                    return;
                }
            }
        });
        Self { conn, send, frames }
    }
}

impl Connection for WtConnection {
    fn send(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(framing::write_frame(&mut self.send, frame))
    }

    fn send_unreliable(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            let d = QuicConnection::encode_datagram(&frame);
            let fits = self.conn.max_datagram_size().is_some_and(|m| d.len() <= m);
            if fits && self.conn.send_datagram(d).is_ok() {
                return Ok(());
            }
            framing::write_frame(&mut self.send, frame).await
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Frame, TransportError>> {
        Box::pin(async move {
            tokio::select! {
                f = self.frames.recv() => f.unwrap_or(Err(TransportError::Closed)),
                d = self.conn.receive_datagram() => match d {
                    Ok(d) => QuicConnection::decode_datagram(bytes::Bytes::copy_from_slice(d.payload().as_ref())),
                    Err(e) => Err(TransportError::Io(e.to_string())),
                },
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.send.finish().await;
            self.conn.close(wtransport::VarInt::from_u32(0), b"bye");
        })
    }

    fn kind(&self) -> &'static str {
        "webtransport"
    }
}

/// Server config from a PEM chain + key; `dev_self_signed` for development (`localhost`).
pub async fn server_config(
    listen: std::net::SocketAddr,
    cert_pem: &std::path::Path,
    key_pem: &std::path::Path,
) -> Result<wtransport::ServerConfig, TransportError> {
    let id = wtransport::Identity::load_pemfiles(cert_pem, key_pem)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))?;
    Ok(wtransport::ServerConfig::builder()
        .with_bind_address(listen)
        .with_identity(id)
        .build())
}

/// Self-signed `localhost` identity; the certificate's hash (`serverCertificateHashes`) is what a
/// browser or a test client pins.
pub fn dev_self_signed(
    listen: std::net::SocketAddr,
) -> Result<(wtransport::ServerConfig, wtransport::tls::Sha256Digest), TransportError> {
    let id = wtransport::Identity::self_signed(["localhost", "127.0.0.1", "::1"])
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let hash = id
        .certificate_chain()
        .as_slice()
        .first()
        .map(|c| c.hash())
        .ok_or_else(|| TransportError::Io("empty certificate chain".into()))?;
    Ok((
        wtransport::ServerConfig::builder()
            .with_bind_address(listen)
            .with_identity(id)
            .build(),
        hash,
    ))
}
