use axum::extract::ws::{Message, WebSocket};
use futures::future::BoxFuture;
use futures::StreamExt;

use crate::{Connection, Frame, TransportError};

pub struct WsConnection {
    inner: WebSocket,
}

impl WsConnection {
    pub fn new(inner: WebSocket) -> Self {
        Self { inner }
    }
}

impl Connection for WsConnection {
    fn send(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(async move {
            let msg = match frame {
                Frame::Text(s) => Message::Text(s.into()),
                Frame::Binary(b) => Message::Binary(b),
                Frame::Close => Message::Close(None),
            };
            self.inner
                .send(msg)
                .await
                .map_err(|e| TransportError::Io(e.to_string()))
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Frame, TransportError>> {
        Box::pin(async move {
            loop {
                match self.inner.next().await {
                    None => return Err(TransportError::Closed),
                    Some(Err(e)) => return Err(TransportError::Io(e.to_string())),
                    Some(Ok(Message::Text(s))) => return Ok(Frame::Text(s.to_string())),
                    Some(Ok(Message::Binary(b))) => return Ok(Frame::Binary(b)),
                    Some(Ok(Message::Close(_))) => return Ok(Frame::Close),
                    // axum answers pings itself; pongs are only liveness evidence.
                    Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                }
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.inner.send(Message::Close(None)).await;
        })
    }

    fn kind(&self) -> &'static str {
        "ws"
    }
}
