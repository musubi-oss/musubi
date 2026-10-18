//! QUIC transport (quinn). One bidirectional stream per realtime session carries the same
//! envelopes as WebSocket, framed as `type:u8 | len:u32be | payload` where type is
//! 0 = text (JSON), 1 = binary (postcard), 2 = close. Unreliable frames travel as QUIC datagrams
//! with the same 1-byte type prefix (no length); oversized ones fall back to the stream.

use std::sync::Arc;

use bytes::Bytes;
use futures::future::BoxFuture;
use quinn::{RecvStream, SendStream};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

use crate::{Connection, Frame, TransportError};

pub const ALPN: &[u8] = b"musubi-rt/1";

pub struct QuicConnection {
    conn: quinn::Connection,
    send: SendStream,
    /// Frames read off the stream by a dedicated task: `recv` selects between this and
    /// datagrams, and dropping a half-done `read_frame` future would lose stream bytes.
    frames: tokio::sync::mpsc::Receiver<Result<Frame, TransportError>>,
}

impl QuicConnection {
    pub fn new(
        conn: quinn::Connection,
        send: SendStream,
        mut recv: RecvStream,
        max_frame: usize,
    ) -> Self {
        let (tx, frames) = tokio::sync::mpsc::channel(256);
        tokio::spawn(async move {
            loop {
                let r = Self::read_frame(&mut recv, max_frame).await;
                let stop = r.is_err();
                if tx.send(r).await.is_err() || stop {
                    return;
                }
            }
        });
        Self { conn, send, frames }
    }

    pub fn encode_datagram(frame: &Frame) -> Bytes {
        let (t, body): (u8, &[u8]) = match frame {
            Frame::Text(s) => (0, s.as_bytes()),
            Frame::Binary(b) => (1, b),
            Frame::Close => (2, &[]),
        };
        let mut v = Vec::with_capacity(1 + body.len());
        v.push(t);
        v.extend_from_slice(body);
        Bytes::from(v)
    }

    pub fn decode_datagram(d: Bytes) -> Result<Frame, TransportError> {
        let Some((&t, body)) = d.split_first() else {
            return Err(TransportError::Io("empty datagram".into()));
        };
        match t {
            0 => String::from_utf8(body.to_vec())
                .map(Frame::Text)
                .map_err(|_| TransportError::Io("invalid utf-8 datagram".into())),
            1 => Ok(Frame::Binary(Bytes::copy_from_slice(body))),
            2 => Ok(Frame::Close),
            t => Err(TransportError::Io(format!("unknown datagram type {t}"))),
        }
    }

    /// Read one frame off a stream (shared with the handshake, which runs before the session loop).
    pub async fn read_frame(
        recv: &mut RecvStream,
        max_frame: usize,
    ) -> Result<Frame, TransportError> {
        let mut head = [0u8; 5];
        recv.read_exact(&mut head).await.map_err(|e| match e {
            quinn::ReadExactError::FinishedEarly(_) => TransportError::Closed,
            other => TransportError::Io(other.to_string()),
        })?;
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        if len > max_frame {
            return Err(TransportError::TooLarge(len));
        }
        let mut body = vec![0u8; len];
        recv.read_exact(&mut body)
            .await
            .map_err(|e| TransportError::Io(e.to_string()))?;
        match head[0] {
            0 => String::from_utf8(body)
                .map(Frame::Text)
                .map_err(|_| TransportError::Io("invalid utf-8 text frame".into())),
            1 => Ok(Frame::Binary(Bytes::from(body))),
            2 => Ok(Frame::Close),
            t => Err(TransportError::Io(format!("unknown frame type {t}"))),
        }
    }

    pub async fn write_frame(send: &mut SendStream, frame: Frame) -> Result<(), TransportError> {
        let (t, body): (u8, Vec<u8>) = match frame {
            Frame::Text(s) => (0, s.into_bytes()),
            Frame::Binary(b) => (1, b.to_vec()),
            Frame::Close => (2, Vec::new()),
        };
        let mut buf = Vec::with_capacity(5 + body.len());
        buf.push(t);
        buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
        buf.extend_from_slice(&body);
        send.write_all(&buf)
            .await
            .map_err(|e| TransportError::Io(e.to_string()))
    }
}

impl Connection for QuicConnection {
    fn send(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move { Self::write_frame(&mut self.send, frame).await })
    }

    fn send_unreliable(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            let d = Self::encode_datagram(&frame);
            let fits = self.conn.max_datagram_size().is_some_and(|m| d.len() <= m);
            if fits {
                match self.conn.send_datagram(d) {
                    Ok(()) => return Ok(()),
                    Err(
                        quinn::SendDatagramError::UnsupportedByPeer
                        | quinn::SendDatagramError::Disabled,
                    ) => {}
                    Err(quinn::SendDatagramError::TooLarge) => {}
                    Err(e) => return Err(TransportError::Io(e.to_string())),
                }
            }
            Self::write_frame(&mut self.send, frame).await
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Frame, TransportError>> {
        Box::pin(async move {
            tokio::select! {
                f = self.frames.recv() => f.unwrap_or(Err(TransportError::Closed)),
                d = self.conn.read_datagram() => match d {
                    Ok(d) => Self::decode_datagram(d),
                    Err(e) => Err(TransportError::Io(e.to_string())),
                },
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = Self::write_frame(&mut self.send, Frame::Close).await;
            let _ = self.send.finish();
        })
    }

    fn kind(&self) -> &'static str {
        "quic"
    }
}

/// Server config from PEM cert chain + PKCS#8/SEC1 key.
pub fn server_config(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<quinn::ServerConfig, TransportError> {
    use rustls_pki_types::pem::PemObject;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| TransportError::Io(format!("cert pem: {e}")))?;
    let key = PrivateKeyDer::from_pem_slice(key_pem)
        .map_err(|e| TransportError::Io(format!("key pem: {e}")))?;
    build(certs, key)
}

/// Self-signed `localhost` cert for development; returns the DER so a test client can trust it.
pub fn dev_self_signed() -> Result<(quinn::ServerConfig, CertificateDer<'static>), TransportError> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let der = CertificateDer::from(cert.cert.der().to_vec());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    Ok((build(vec![der.clone()], key)?, der))
}

fn build(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<quinn::ServerConfig, TransportError> {
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TransportError::Io(e.to_string()))?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|e| TransportError::Io(e.to_string()))?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_secs(60))
            .map_err(|e| TransportError::Io(e.to_string()))?,
    ));
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
    // one session = one bidirectional stream (+ datagrams); a client gets a few spare streams
    // for reconnect races and no unidirectional ones
    transport.max_concurrent_bidi_streams(8u32.into());
    transport.max_concurrent_uni_streams(0u32.into());
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

/// Client config trusting exactly `roots` (tests, dev) with our ALPN.
pub fn client_config(
    roots: &[CertificateDer<'static>],
) -> Result<quinn::ClientConfig, TransportError> {
    let mut store = rustls::RootCertStore::empty();
    for r in roots {
        store
            .add(r.clone())
            .map_err(|e| TransportError::Io(e.to_string()))?;
    }
    let mut tls = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|e| TransportError::Io(e.to_string()))?;
    Ok(quinn::ClientConfig::new(Arc::new(crypto)))
}

/// Client config that accepts ANY server certificate (benchmarks and local experiments only —
/// never for a real client).
pub fn insecure_client_config() -> Result<quinn::ClientConfig, TransportError> {
    #[derive(Debug)]
    struct NoVerify(rustls::crypto::CryptoProvider);
    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &rustls_pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls_pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
    let provider = rustls::crypto::ring::default_provider();
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(provider.clone()))
        .with_safe_default_protocol_versions()
        .map_err(|e| TransportError::Io(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
        .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)
        .map_err(|e| TransportError::Io(e.to_string()))?;
    Ok(quinn::ClientConfig::new(Arc::new(crypto)))
}
