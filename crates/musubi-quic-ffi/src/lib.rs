//! C ABI QUIC client for the Musubi realtime protocol, linked into the Unreal plugin
//! (`MusubiQuic` module). Same stack as the node (quinn + rustls), same wire as
//! `musubi_transport::quic`: ALPN `musubi-rt/1`, stream frames `[type u8][len u32 BE][body]`
//! (0 = text/JSON, 1 = binary/postcard, 2 = close), datagrams `[type u8][body]` for the
//! unreliable channel. The first bidirectional stream carries the `hello` and then the
//! envelope stream; the caller (FMusubiSocket) owns the protocol — this crate only moves frames.
//!
//! Threading: one tokio runtime per `mq_connect`, all callbacks are *not* used; instead the UE side
//! polls `mq_poll` from the game thread (no cross-thread UObject access). Everything behind the
//! handle is `Send` and the handle is single-owner.

use bytes::Bytes;
use rustls_pki_types::pem::PemObject;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::net::ToSocketAddrs;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

pub const ALPN: &[u8] = b"musubi-rt/1";
pub const MAX_FRAME: usize = 64 * 1024;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const IDLE: Duration = Duration::from_secs(60);

#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameType {
    Text = 0,
    Binary = 1,
    Close = 2,
}

/// Frame layout shared with the node (`musubi_transport::quic::QuicConnection::write_frame`).
pub fn encode_stream_frame(ty: FrameType, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + body.len());
    v.push(ty as u8);
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    v
}

/// Datagram layout shared with the node (`QuicConnection::encode_datagram`).
pub fn encode_datagram(ty: FrameType, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(1 + body.len());
    v.push(ty as u8);
    v.extend_from_slice(body);
    v
}

fn frame_type(b: u8) -> Option<FrameType> {
    match b {
        0 => Some(FrameType::Text),
        1 => Some(FrameType::Binary),
        2 => Some(FrameType::Close),
        _ => None,
    }
}

/// Events surfaced to the caller through `mq_poll`.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MqEventKind {
    None = 0,
    Connected = 1,
    /// `data` = frame body, `frame_type` = 0 text / 1 binary; `unreliable` = arrived as a datagram.
    Frame = 2,
    /// `data` = UTF-8 reason.
    Closed = 3,
    /// `data` = UTF-8 message; the connection is dead afterwards.
    Error = 4,
}

enum Event {
    Connected,
    Frame {
        ty: FrameType,
        body: Bytes,
        unreliable: bool,
    },
    Closed(String),
    Error(String),
}

enum Cmd {
    Send {
        ty: FrameType,
        body: Bytes,
        unreliable: bool,
    },
    Close,
}

struct Inner {
    rt: tokio::runtime::Runtime,
    cmd: mpsc::UnboundedSender<Cmd>,
    events: Arc<Mutex<std::collections::VecDeque<Event>>>,
    /// Body of the last event handed out by `mq_poll`; kept alive until the next poll.
    last: Vec<u8>,
    datagram_max: Arc<Mutex<usize>>,
}

pub struct MqConn(Inner);

/// Event record filled by `mq_poll`. `data` stays valid until the next `mq_poll`/`mq_close`.
#[repr(C)]
pub struct MqEvent {
    pub kind: MqEventKind,
    pub frame_type: u8,
    pub unreliable: u8,
    pub len: u32,
    pub data: *const u8,
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn client_config(
    insecure: bool,
    extra_root_pem: Option<&str>,
) -> Result<quinn::ClientConfig, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(pem) = extra_root_pem {
        for c in rustls_pki_types::CertificateDer::pem_slice_iter(pem.as_bytes()) {
            let c = c.map_err(|e| format!("root pem: {e:?}"))?;
            roots.add(c).map_err(|e| format!("root: {e}"))?;
        }
    }
    let mut tls = if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(
                rustls::crypto::ring::default_provider(),
            )))
            .with_no_client_auth()
    } else {
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth()
    };
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let crypto =
        quinn::crypto::rustls::QuicClientConfig::try_from(tls).map_err(|e| e.to_string())?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(IDLE.try_into().map_err(|_| "idle".to_string())?));
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

/// Dev-only verifier for self-signed node certificates (`insecure = 1`); logged by the UE side.
#[derive(Debug)]
struct NoVerify(rustls::crypto::CryptoProvider);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &rustls_pki_types::CertificateDer<'_>,
        _: &[rustls_pki_types::CertificateDer<'_>],
        _: &rustls_pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &rustls_pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &rustls_pki_types::CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

async fn read_frame(recv: &mut quinn::RecvStream) -> Result<Option<(FrameType, Bytes)>, String> {
    let mut head = [0u8; 5];
    match recv.read_exact(&mut head).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(_)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    }
    let ty = frame_type(head[0]).ok_or_else(|| format!("unknown frame type {}", head[0]))?;
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_FRAME {
        return Err(format!("frame too large: {len}"));
    }
    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some((ty, Bytes::from(body))))
}

async fn run(
    cfg: quinn::ClientConfig,
    addr: std::net::SocketAddr,
    sni: String,
    mut cmd: mpsc::UnboundedReceiver<Cmd>,
    events: Arc<Mutex<std::collections::VecDeque<Event>>>,
    datagram_max: Arc<Mutex<usize>>,
) {
    let push = |e: Event| events.lock().unwrap().push_back(e);
    let bind: std::net::SocketAddr = if addr.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    }
    .parse()
    .unwrap();
    let mut ep = match quinn::Endpoint::client(bind) {
        Ok(e) => e,
        Err(e) => return push(Event::Error(format!("endpoint: {e}"))),
    };
    ep.set_default_client_config(cfg);
    let conn = match ep.connect(addr, &sni) {
        Ok(c) => match c.await {
            Ok(c) => c,
            Err(e) => return push(Event::Error(format!("connect: {e}"))),
        },
        Err(e) => return push(Event::Error(format!("connect: {e}"))),
    };
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => return push(Event::Error(format!("open_bi: {e}"))),
    };
    *datagram_max.lock().unwrap() = conn.max_datagram_size().unwrap_or(0);
    push(Event::Connected);
    loop {
        tokio::select! {
            c = cmd.recv() => match c {
                Some(Cmd::Send { ty, body, unreliable }) => {
                    if unreliable {
                        // a datagram that does not fit (or is unsupported) degrades to the stream, like the node
                        let d = encode_datagram(ty, &body);
                        let fits = conn.max_datagram_size().map(|m| d.len() <= m).unwrap_or(false);
                        if fits && conn.send_datagram(Bytes::from(d)).is_ok() { continue; }
                    }
                    if let Err(e) = send.write_all(&encode_stream_frame(ty, &body)).await {
                        push(Event::Error(format!("send: {e}")));
                        break;
                    }
                }
                Some(Cmd::Close) | None => {
                    let _ = send.write_all(&encode_stream_frame(FrameType::Close, b"")).await;
                    let _ = send.finish();
                    conn.close(0u32.into(), b"bye");
                    push(Event::Closed("local".into()));
                    break;
                }
            },
            f = read_frame(&mut recv) => match f {
                Ok(Some((FrameType::Close, body))) => {
                    push(Event::Closed(String::from_utf8_lossy(&body).into_owned()));
                    break;
                }
                Ok(Some((ty, body))) => push(Event::Frame { ty, body, unreliable: false }),
                Ok(None) => { push(Event::Closed("stream finished".into())); break; }
                Err(e) => { push(Event::Error(e)); break; }
            },
            d = conn.read_datagram() => match d {
                Ok(d) if !d.is_empty() => match frame_type(d[0]) {
                    Some(ty) if ty != FrameType::Close => push(Event::Frame { ty, body: d.slice(1..), unreliable: true }),
                    _ => {}
                },
                Ok(_) => {}
                Err(e) => { push(Event::Closed(format!("connection lost: {e}"))); break; }
            },
        }
    }
    ep.wait_idle().await;
}

/// Connect to `host:port` (DNS allowed), TLS SNI `sni` (empty = host). `insecure != 0` accepts any
/// certificate (dev nodes with self-signed certs). `root_pem` may add a private CA (nullable).
/// Returns a handle or null when the arguments are invalid; connection errors arrive as events.
#[no_mangle]
pub extern "C" fn mq_connect(
    host: *const c_char,
    port: u16,
    sni: *const c_char,
    insecure: c_int,
    root_pem: *const c_char,
) -> *mut MqConn {
    let host = cstr(host);
    let sni_s = cstr(sni);
    let sni = if sni_s.is_empty() {
        host.clone()
    } else {
        sni_s
    };
    let root = if root_pem.is_null() {
        None
    } else {
        Some(cstr(root_pem))
    };
    let addr = match (host.as_str(), port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut a| a.next())
    {
        Some(a) => a,
        None => return std::ptr::null_mut(),
    };
    let cfg = match client_config(insecure != 0, root.as_deref()) {
        Ok(c) => c,
        Err(_) => return std::ptr::null_mut(),
    };
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .thread_name("musubi-quic")
        .build()
    {
        Ok(r) => r,
        Err(_) => return std::ptr::null_mut(),
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let events = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let datagram_max = Arc::new(Mutex::new(0usize));
    rt.spawn(run(
        cfg,
        addr,
        sni,
        rx,
        events.clone(),
        datagram_max.clone(),
    ));
    Box::into_raw(Box::new(MqConn(Inner {
        rt,
        cmd: tx,
        events,
        last: Vec::new(),
        datagram_max,
    })))
}

/// Queue a frame. `frame_type` 0 = text, 1 = binary. `unreliable != 0` → datagram when it fits.
/// # Safety
/// `conn` must be null or a live handle from `mq_connect`; pointers/lengths must describe valid memory.
#[no_mangle]
pub unsafe extern "C" fn mq_send(
    conn: *mut MqConn,
    ty_byte: u8,
    data: *const u8,
    len: u32,
    unreliable: c_int,
) -> c_int {
    if conn.is_null() || (data.is_null() && len > 0) || ty_byte > 1 {
        return -1;
    }
    let c = unsafe { &*conn };
    let body = if len == 0 {
        Bytes::new()
    } else {
        Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(data, len as usize) })
    };
    let ty = frame_type(ty_byte).unwrap_or(FrameType::Binary);
    match c.0.cmd.send(Cmd::Send {
        ty,
        body,
        unreliable: unreliable != 0,
    }) {
        Ok(()) => 0,
        Err(_) => -2,
    }
}

/// Pop the next event; returns 0 and `kind = None` when the queue is empty.
/// # Safety
/// `conn` must be null or a live handle from `mq_connect`; pointers/lengths must describe valid memory.
#[no_mangle]
pub unsafe extern "C" fn mq_poll(conn: *mut MqConn, out: *mut MqEvent) -> c_int {
    if conn.is_null() || out.is_null() {
        return -1;
    }
    let c = unsafe { &mut *conn };
    let ev = c.0.events.lock().unwrap().pop_front();
    let o = unsafe { &mut *out };
    *o = MqEvent {
        kind: MqEventKind::None,
        frame_type: 0,
        unreliable: 0,
        len: 0,
        data: std::ptr::null(),
    };
    let Some(ev) = ev else { return 0 };
    match ev {
        Event::Connected => o.kind = MqEventKind::Connected,
        Event::Frame {
            ty,
            body,
            unreliable,
        } => {
            o.kind = MqEventKind::Frame;
            o.frame_type = ty as u8;
            o.unreliable = unreliable as u8;
            c.0.last = body.to_vec();
        }
        Event::Closed(s) => {
            o.kind = MqEventKind::Closed;
            c.0.last = s.into_bytes();
        }
        Event::Error(s) => {
            o.kind = MqEventKind::Error;
            c.0.last = s.into_bytes();
        }
    }
    o.len = c.0.last.len() as u32;
    o.data = c.0.last.as_ptr();
    1
}

/// Largest datagram the path accepts (0 = datagrams unsupported / not connected yet).
/// # Safety
/// `conn` must be null or a live handle from `mq_connect`; pointers/lengths must describe valid memory.
#[no_mangle]
pub unsafe extern "C" fn mq_max_datagram(conn: *mut MqConn) -> u32 {
    if conn.is_null() {
        return 0;
    }
    *unsafe { &*conn }.0.datagram_max.lock().unwrap() as u32
}

/// Close and free. Safe to call with null.
/// # Safety
/// `conn` must be null or a live handle from `mq_connect`; pointers/lengths must describe valid memory.
#[no_mangle]
pub unsafe extern "C" fn mq_close(conn: *mut MqConn) {
    if conn.is_null() {
        return;
    }
    let c = unsafe { Box::from_raw(conn) };
    let _ = c.0.cmd.send(Cmd::Close);
    c.0.rt.shutdown_timeout(Duration::from_millis(500));
}

/// Keep the exported symbol table stable for the UE loader.
#[no_mangle]
pub extern "C" fn mq_abi_version() -> u32 {
    1
}

#[allow(dead_code)]
fn _assert_send() {
    fn f<T: Send>() {}
    f::<Inner>();
    let _ = std::ptr::null::<c_void>();
}
