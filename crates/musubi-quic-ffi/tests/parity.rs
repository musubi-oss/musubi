//! The FFI crate re-implements the node's QUIC framing (it cannot depend on musubi-transport:
//! that pulls axum/wtransport/str0m into a client static lib). Lock the two together byte-for-byte.

use bytes::Bytes;
use musubi_quic_ffi::{encode_datagram, encode_stream_frame, FrameType, ALPN};
use musubi_transport::quic::QuicConnection;
use musubi_transport::Frame;

#[test]
fn alpn_matches_node() {
    assert_eq!(ALPN, musubi_transport::quic::ALPN);
}

#[test]
fn datagram_layout_matches_node() {
    for (ours, theirs) in [
        (
            encode_datagram(FrameType::Text, b"{\"a\":1}"),
            QuicConnection::encode_datagram(&Frame::Text("{\"a\":1}".into())),
        ),
        (
            encode_datagram(FrameType::Binary, &[1, 2, 3]),
            QuicConnection::encode_datagram(&Frame::Binary(Bytes::from_static(&[1, 2, 3]))),
        ),
    ] {
        assert_eq!(Bytes::from(ours), theirs);
    }
}

#[tokio::test]
async fn stream_frame_layout_matches_node() {
    // write_frame needs a real quinn SendStream; drive a loopback connection with the node's own dev config
    let (server_cfg, cert) = musubi_transport::quic::dev_self_signed().unwrap();
    let server = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = server.local_addr().unwrap();
    let client_cfg = musubi_transport::quic::client_config(&[cert]).unwrap();
    let mut client = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client.set_default_client_config(client_cfg);
    let (c, s) = tokio::join!(
        async { client.connect(addr, "localhost").unwrap().await.unwrap() },
        async { server.accept().await.unwrap().await.unwrap() }
    );
    let (mut send, _recv) = c.open_bi().await.unwrap();
    QuicConnection::write_frame(&mut send, Frame::Text("hi".into()))
        .await
        .unwrap();
    QuicConnection::write_frame(&mut send, Frame::Binary(Bytes::from_static(&[9, 8])))
        .await
        .unwrap();
    send.finish().unwrap();
    let (_ss, mut sr) = s.accept_bi().await.unwrap();
    let got = sr.read_to_end(1 << 16).await.unwrap();
    let mut want = encode_stream_frame(FrameType::Text, b"hi");
    want.extend(encode_stream_frame(FrameType::Binary, &[9, 8]));
    assert_eq!(got, want);
}
