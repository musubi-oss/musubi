//! Transport abstraction for realtime connections.
//!
//! A [`Connection`] moves opaque [`Frame`]s in both directions; the realtime layer above
//! decodes them with the negotiated [`Encoding`]. WebSocket is the first implementation,
//! QUIC (`quic`) shares it; WebTransport is next.

use std::time::Duration;

use bytes::Bytes;
use futures::future::BoxFuture;
pub use musubi_proto::Encoding;

pub mod framing;
pub mod quic;
pub mod udp;
pub mod webrtc;
pub mod webtransport;
pub mod ws;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Binary(Bytes),
    Close,
}

impl Frame {
    /// Bytes of application payload the frame carries (0 for `Close`); what `Limits::max_frame_bytes`
    /// is compared against regardless of the frame being text or binary.
    pub fn payload_len(&self) -> usize {
        match self {
            Frame::Text(s) => s.len(),
            Frame::Binary(b) => b.len(),
            Frame::Close => 0,
        }
    }
}

impl Frame {
    pub fn payload(&self) -> Option<&[u8]> {
        match self {
            Frame::Text(s) => Some(s.as_bytes()),
            Frame::Binary(b) => Some(b),
            Frame::Close => None,
        }
    }

    /// Wrap already-encoded bytes in the frame kind matching the encoding.
    pub fn encoded(enc: Encoding, bytes: Bytes) -> Frame {
        match enc {
            Encoding::Json => Frame::Text(String::from_utf8_lossy(&bytes).into_owned()),
            Encoding::Postcard => Frame::Binary(bytes),
        }
    }
}

#[derive(Debug)]
pub enum TransportError {
    Closed,
    Io(String),
    Timeout,
    TooLarge(usize),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Closed => f.write_str("connection closed"),
            TransportError::Io(e) => write!(f, "io: {e}"),
            TransportError::Timeout => f.write_str("timeout"),
            TransportError::TooLarge(n) => write!(f, "frame of {n} bytes exceeds the limit"),
        }
    }
}
impl std::error::Error for TransportError {}

pub trait Connection: Send {
    fn send(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>>;
    /// Best-effort delivery (QUIC datagrams). Transports without an unreliable channel, or frames
    /// too large for one datagram, fall back to `send`.
    fn send_unreliable(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        self.send(frame)
    }
    fn recv(&mut self) -> BoxFuture<'_, Result<Frame, TransportError>>;
    /// Several frames at once. Transports that can flush once per burst override this (one
    /// write for a queue of deltas, not one per frame); the default sends them one by one.
    fn send_many(&mut self, frames: Vec<Frame>) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            for f in frames {
                self.send(f).await?;
            }
            Ok(())
        })
    }
    fn close(&mut self) -> BoxFuture<'_, ()>;
    fn kind(&self) -> &'static str;
}

/// Per-connection limits applied by every transport.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub idle_timeout: Duration,
    pub ping_interval: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 64 * 1024,
            idle_timeout: Duration::from_secs(60),
            ping_interval: Duration::from_secs(15),
        }
    }
}
