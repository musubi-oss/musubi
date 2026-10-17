//! WebRTC DataChannel acceptor: the browser's offer arrives over `POST /v1/rtc/offer`, the
//! answer goes back in the response, and once the data channel is up the first frame is the
//! JSON `Hello` — the same handshake and session loop as QUIC and WebTransport.

use musubi_transport::webrtc::RtcConnection;

use crate::app::AppState;

pub async fn handle(state: AppState, conn: RtcConnection) {
    // ICE + DTLS + SCTP before the first frame can take a while on a bad network.
    crate::realtime::udp::handle_with_timeout(
        state,
        Box::new(conn),
        std::time::Duration::from_secs(15),
    )
    .await;
}
