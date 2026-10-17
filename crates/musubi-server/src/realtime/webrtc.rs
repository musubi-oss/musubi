//! WebRTC DataChannel acceptor: the browser's offer arrives over `POST /v1/rtc/offer`, the
//! answer goes back in the response, and once the data channel is up the first frame is the
//! JSON `Hello` — the same handshake and session loop as QUIC and WebTransport.

use std::sync::Arc;
use std::time::Duration;

use musubi_core::Error;
use musubi_transport::webrtc::RtcConnection;
use musubi_transport::{Connection, Frame};

use crate::app::AppState;
use crate::realtime::connect::{self, Hello};

/// ICE + DTLS + SCTP before the first frame can take a while on a bad network.
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);

pub async fn handle(state: AppState, mut conn: RtcConnection) {
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, conn.recv()).await {
        Ok(Ok(Frame::Text(t))) => serde_json::from_str::<Hello>(&t)
            .map_err(|e| Error::InvalidArgument(format!("hello: {e}"))),
        Ok(Ok(_)) => Err(Error::InvalidArgument("hello must be a text frame".into())),
        Ok(Err(e)) => Err(Error::Internal(e.to_string())),
        Err(_) => Err(Error::InvalidArgument("hello timeout".into())),
    };
    if let Ok(h) = &hello {
        if let Some(token) = h.resume.clone() {
            let encoding = connect::encoding_of(h).unwrap_or(musubi_proto::Encoding::Json);
            connect::resume_any(&state, &token, Box::new(conn), encoding).await;
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
    crate::realtime::serve(hub, Box::new(conn), params).await;
}
