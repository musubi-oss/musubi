//! Stream framing shared by the QUIC and WebTransport transports: `type:u8 | len:u32be | payload`,
//! type 0 = text (JSON), 1 = binary (postcard), 2 = close.

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{Frame, TransportError};

pub async fn read_frame<R: AsyncRead + Unpin>(
    recv: &mut R,
    max_frame: usize,
) -> Result<Frame, TransportError> {
    let mut head = [0u8; 5];
    recv.read_exact(&mut head)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::UnexpectedEof => TransportError::Closed,
            _ => TransportError::Io(e.to_string()),
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

pub async fn write_frame<W: AsyncWrite + Unpin>(
    send: &mut W,
    frame: Frame,
) -> Result<(), TransportError> {
    let (t, body): (u8, Vec<u8>) = match frame {
        Frame::Text(s) => (0, s.into_bytes()),
        Frame::Binary(b) => (1, b.to_vec()),
        Frame::Close => (2, Vec::new()),
    };
    let len = u32::try_from(body.len()).map_err(|_| TransportError::TooLarge(body.len()))?;
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(t);
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&body);
    send.write_all(&buf)
        .await
        .map_err(|e| TransportError::Io(e.to_string()))
}
