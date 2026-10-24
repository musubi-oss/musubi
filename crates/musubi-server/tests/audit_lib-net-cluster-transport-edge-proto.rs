//! Audit regression tests (unit `lib-net-cluster-transport-edge-proto`): the QUIC node bus admits
//! only nodes whose certificate is in the registry. Run with `MUSUBI_TEST_DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

async fn db() -> Option<sqlx::PgPool> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&url)
        .await
        .unwrap();
    musubi_server::db::migrate(&db).await.unwrap();
    Some(db)
}

/// A client that accepts any server certificate (the attacker does not care who answers).
#[derive(Debug)]
struct AnyServer(quinn::rustls::crypto::CryptoProvider);

impl quinn::rustls::client::danger::ServerCertVerifier for AnyServer {
    fn verify_server_cert(
        &self,
        _: &quinn::rustls::pki_types::CertificateDer<'_>,
        _: &[quinn::rustls::pki_types::CertificateDer<'_>],
        _: &quinn::rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: quinn::rustls::pki_types::UnixTime,
    ) -> Result<quinn::rustls::client::danger::ServerCertVerified, quinn::rustls::Error> {
        Ok(quinn::rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &quinn::rustls::pki_types::CertificateDer<'_>,
        d: &quinn::rustls::DigitallySignedStruct,
    ) -> Result<quinn::rustls::client::danger::HandshakeSignatureValid, quinn::rustls::Error> {
        quinn::rustls::crypto::verify_tls12_signature(
            m,
            c,
            d,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &quinn::rustls::pki_types::CertificateDer<'_>,
        d: &quinn::rustls::DigitallySignedStruct,
    ) -> Result<quinn::rustls::client::danger::HandshakeSignatureValid, quinn::rustls::Error> {
        quinn::rustls::crypto::verify_tls13_signature(
            m,
            c,
            d,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<quinn::rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Dial a node's bus like a rogue process would: correct ALPN, any server cert accepted, and
/// either no client certificate or a self-signed one nobody registered. Returns the outcome of
/// trying to push one forged bus frame.
async fn rogue_dial(
    target: std::net::SocketAddr,
    with_cert: bool,
    frame: &[u8],
) -> Result<(), String> {
    let builder = quinn::rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyServer(
            quinn::rustls::crypto::ring::default_provider(),
        )));
    let mut tls = if with_cert {
        let c = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        builder
            .with_client_auth_cert(
                vec![quinn::rustls::pki_types::CertificateDer::from(
                    c.cert.der().to_vec(),
                )],
                quinn::rustls::pki_types::PrivateKeyDer::Pkcs8(c.key_pair.serialize_der().into()),
            )
            .unwrap()
    } else {
        builder.with_no_client_auth()
    };
    tls.alpn_protocols = vec![b"musubi-bus/1".to_vec()];
    let crypto =
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(|e| e.to_string())?;
    let mut ep =
        quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).map_err(|e| e.to_string())?;
    ep.set_default_client_config(quinn::ClientConfig::new(Arc::new(crypto)));
    let conn = ep
        .connect(target, "localhost")
        .map_err(|e| e.to_string())?
        .await
        .map_err(|e| format!("handshake: {e}"))?;
    let mut uni = conn.open_uni().await.map_err(|e| e.to_string())?;
    let len = u32::try_from(frame.len()).unwrap();
    uni.write_all(&len.to_be_bytes())
        .await
        .map_err(|e| e.to_string())?;
    uni.write_all(frame).await.map_err(|e| e.to_string())?;
    uni.finish().map_err(|e| e.to_string())?;
    // QUIC resolves the client side of the handshake before the server has judged the client
    // certificate: a refused certificate shows up as the server closing the connection (TLS
    // alert) right after. An accepted connection stays open until we close it.
    match tokio::time::timeout(Duration::from_secs(3), conn.closed()).await {
        Ok(e) => Err(format!("server closed the connection: {e}")),
        Err(_) => {
            conn.close(0u32.into(), b"");
            Ok(())
        }
    }
}

/// Without mutual TLS anyone who can reach the bus port could inject messages under any node's
/// identity; now a connection without a registered client certificate never completes, and a
/// legitimate peer still gets through.
#[tokio::test]
async fn bus_rejects_unregistered_clients_and_serves_registered_peers() {
    let Some(db) = db().await else { return };
    let group = format!("audit-{}", uuid::Uuid::now_v7());
    let bus = |name: &str| {
        musubi_cluster::Cluster::new(db.clone(), name, "local", vec![], "")
            .with_group(&group)
            .with_bus("127.0.0.1:0".parse().unwrap(), None)
            .unwrap()
    };
    let a = Arc::new(bus("audit-a"));
    let b = Arc::new(bus("audit-b"));
    a.start().await.unwrap();
    b.start().await.unwrap();
    a.refresh_peers().await.unwrap();
    b.refresh_peers().await.unwrap();
    let mut ra = a.subscribe();
    let target = a.bus_addr().unwrap();
    let forged = serde_json::to_vec(&json!({
        "origin": b.node_id().0,
        "topic": "audit.forged",
        "payload": {"evil": true},
        "group": group,
    }))
    .unwrap();
    for with_cert in [false, true] {
        let r = tokio::time::timeout(
            Duration::from_secs(10),
            rogue_dial(target, with_cert, &forged),
        )
        .await
        .expect("dial does not hang");
        assert!(
            r.is_err(),
            "rogue client (own cert: {with_cert}) must not get a bus connection"
        );
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(2), ra.recv())
            .await
            .is_err(),
        "no forged frame reached the bus"
    );
    // a registered peer: 50 KiB only travels over QUIC (far above the NOTIFY cap)
    let topic = format!("audit.big.{}", uuid::Uuid::now_v7());
    b.publish(&topic, &json!({"data": "x".repeat(50_000)}))
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let m = ra.recv().await.unwrap();
            if m.topic == topic {
                return m;
            }
        }
    })
    .await
    .expect("registered peer delivers over the mutually authenticated bus");
    assert_eq!(got.origin, b.node_id());
    a.stop().await;
    b.stop().await;
}
