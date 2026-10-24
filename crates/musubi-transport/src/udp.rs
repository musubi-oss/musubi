//! Raw UDP transport for dedicated servers ↔ core (and constrained platforms): our own small
//! protocol over `tokio::net::UdpSocket`, in the spirit of renet/netcode.
//!
//! * Handshake with a stateless cookie: `Connect(nonce)` → `Challenge(cookie)` → `Connect(nonce,
//!   cookie)` → `Accept`. The cookie is `sha256(secret | address | nonce | 30-second slot)`, so no
//!   state is allocated for an unverified source and a spoofed address cannot get a connection.
//! * Every data packet carries `seq`, `ack` and a 32-bit ack history. Packets of reliable messages
//!   are kept until acked and retransmitted after an RTT-derived timeout; unreliable ones are
//!   fire-and-forget. Messages larger than one packet are fragmented (`frag_idx/frag_count`).
//! * Two channels: 0 = reliable ordered (reassembled and delivered in message order), 1 =
//!   unreliable sequenced (stale ones dropped; frames too large for one packet go reliable).
//! * Keepalive after 1 s of silence, disconnect after 10 s without a packet.
//!
//! There is no encryption at this layer: it is meant for private networks (fleet ↔ node) or behind
//! a proxy, as the plan says — the public internet gets QUIC/WebTransport/WebRTC.

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::future::BoxFuture;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, oneshot};

use crate::quic::QuicConnection;
use crate::{Connection, Frame, TransportError};

const MAGIC: u16 = 0x4D55; // "MU"
const VERSION: u8 = 1;
const T_CONNECT: u8 = 1;
const T_CHALLENGE: u8 = 2;
const T_ACCEPT: u8 = 3;
const T_DATA: u8 = 4;
const T_DISCONNECT: u8 = 5;
const HDR: usize = 8;
const DATA_HDR: usize = 8;
const MSG_HDR: usize = 9;
/// Payload budget per packet: safe below a 1280-byte path MTU with IPv6 + UDP headers.
pub const MTU: usize = 1200;
const FRAG: usize = MTU - HDR - DATA_HDR - MSG_HDR;
const CH_RELIABLE: u8 = 0;
const CH_UNRELIABLE: u8 = 1;
const TICK: Duration = Duration::from_millis(20);
const KEEPALIVE: Duration = Duration::from_secs(1);
const IDLE: Duration = Duration::from_secs(10);
const RTO_MIN: Duration = Duration::from_millis(100);
const RTO_MAX: Duration = Duration::from_secs(1);
const MAX_RETRIES: u32 = 30;

fn newer(a: u16, b: u16) -> bool {
    (a.wrapping_sub(b) as i16) > 0
}

fn io(e: impl std::fmt::Display) -> TransportError {
    TransportError::Io(e.to_string())
}

/// Stateless handshake cookie: HMAC-SHA256 over (address, nonce, 30 s slot), truncated to 128
/// bits. Keyed MAC rather than a bare hash so the construction does not depend on SHA-256
/// having no extension/prefix weaknesses, and compared in constant time by the server.
fn cookie(secret: &[u8; 32], addr: SocketAddr, nonce: u64, slot: u64) -> [u8; 16] {
    let mut h = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("hmac accepts any key size");
    h.update(addr.to_string().as_bytes());
    h.update(&nonce.to_be_bytes());
    h.update(&slot.to_be_bytes());
    let d = h.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&d[..16]);
    out
}

fn cookie_valid(secret: &[u8; 32], addr: SocketAddr, nonce: u64, given: &[u8; 16]) -> bool {
    let slot = slot_now();
    let now = cookie(secret, addr, nonce, slot);
    let prev = cookie(secret, addr, nonce, slot.wrapping_sub(1));
    // every candidate is evaluated: no early exit leaks which one (if any) matched
    let fresh = given.ct_eq(&now) | given.ct_eq(&prev);
    bool::from(fresh) && *given != [0; 16]
}

/// Largest reliable message the peer may make us buffer: `max_frame` plus the datagram tag,
/// rounded up to whole fragments.
fn max_frags(max_frame: usize) -> usize {
    (max_frame + 1).div_ceil(FRAG).max(1)
}

/// Fragment slots the receiver buffers for reliable messages waiting on an earlier one, as a
/// multiple of the largest message (`max_frags`). Beyond that a new message is dropped (not
/// acked) and the sender retransmits it once the gap has closed, so a peer cannot make us
/// allocate ahead of what we deliver.
const PENDING_BUDGET_FRAMES: usize = 4;
const MIN_PENDING_SLOTS: usize = 1024;
/// Connections one endpoint serves; the handshake answers with a challenge regardless, so a flood
/// of valid handshakes cannot push the table past this.
pub const MAX_CONNS: usize = 65_536;

fn slot_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() / 30)
        .unwrap_or(0)
}

fn header(t: u8, conn_id: u32, buf: &mut Vec<u8>) {
    buf.extend_from_slice(&MAGIC.to_be_bytes());
    buf.push(VERSION);
    buf.push(t);
    buf.extend_from_slice(&conn_id.to_be_bytes());
}

fn parse_header(b: &[u8]) -> Option<(u8, u32, &[u8])> {
    if b.len() < HDR || u16::from_be_bytes([b[0], b[1]]) != MAGIC || b[2] != VERSION {
        return None;
    }
    Some((
        b[3],
        u32::from_be_bytes([b[4], b[5], b[6], b[7]]),
        &b[HDR..],
    ))
}

enum Cmd {
    Send {
        id: u32,
        reliable: bool,
        bytes: Vec<u8>,
    },
    Close {
        id: u32,
    },
}

#[derive(Clone)]
struct Frag {
    ch: u8,
    mid: u16,
    idx: u16,
    count: u16,
    body: Vec<u8>,
}

/// fragment count + the fragments received so far
type Reassembly = (u16, Vec<Option<Vec<u8>>>);
/// server address, nonce, cookie once challenged, who awaits the connection
type Handshake = (
    SocketAddr,
    u64,
    Option<[u8; 16]>,
    oneshot::Sender<UdpConnection>,
);

struct Conn {
    addr: SocketAddr,
    inbound: mpsc::Sender<Frame>,
    // sending
    send_seq: u16,
    msg_id: [u16; 2],
    /// packet seq → fragment it carried (retransmitted in a fresh packet after the RTO)
    unacked: BTreeMap<u16, (Instant, Frag, u32)>,
    srtt: Option<Duration>,
    // receiving
    recv_latest: Option<u16>,
    recv_bits: u32,
    next_reliable: u16,
    reliable_pending: BTreeMap<u16, Reassembly>,
    last_unreliable: Option<u16>,
    last_recv: Instant,
    last_send: Instant,
    closing: bool,
    /// Fragments per reliable message this peer may make us buffer (`max_frags(max_frame)`).
    max_frags: usize,
    /// Fragment slots currently held in `reliable_pending`.
    pending_slots: usize,
}

impl Conn {
    fn new(addr: SocketAddr, inbound: mpsc::Sender<Frame>, max_frame: usize) -> Self {
        let now = Instant::now();
        Self {
            addr,
            inbound,
            max_frags: max_frags(max_frame),
            pending_slots: 0,
            send_seq: 0,
            msg_id: [0, 0],
            unacked: BTreeMap::new(),
            srtt: None,
            recv_latest: None,
            recv_bits: 0,
            next_reliable: 0,
            reliable_pending: BTreeMap::new(),
            last_unreliable: None,
            last_recv: now,
            last_send: now,
            closing: false,
        }
    }

    fn rto(&self) -> Duration {
        self.srtt
            .map(|s| s * 2)
            .unwrap_or(RTO_MIN * 2)
            .clamp(RTO_MIN, RTO_MAX)
    }

    fn ack_fields(&self) -> (u16, u32) {
        (self.recv_latest.unwrap_or(0), self.recv_bits)
    }

    /// Build one data packet (possibly a bare ack) and hand it to `send`.
    fn packet(&mut self, id: u32, msg: Option<Frag>, reliable: bool, retries: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(MTU);
        header(T_DATA, id, &mut buf);
        let seq = self.send_seq;
        self.send_seq = self.send_seq.wrapping_add(1);
        let (ack, bits) = self.ack_fields();
        buf.extend_from_slice(&seq.to_be_bytes());
        buf.extend_from_slice(&ack.to_be_bytes());
        buf.extend_from_slice(&bits.to_be_bytes());
        if let Some(f) = &msg {
            buf.push(f.ch);
            buf.extend_from_slice(&f.mid.to_be_bytes());
            buf.extend_from_slice(&f.idx.to_be_bytes());
            buf.extend_from_slice(&f.count.to_be_bytes());
            buf.extend_from_slice(&(f.body.len() as u16).to_be_bytes());
            buf.extend_from_slice(&f.body);
        }
        if let (true, Some(f)) = (reliable, msg) {
            self.unacked.insert(seq, (Instant::now(), f, retries));
        }
        self.last_send = Instant::now();
        buf
    }

    fn on_ack(&mut self, ack: u16, bits: u32) {
        let now = Instant::now();
        let mut acked = vec![ack];
        for i in 0..32 {
            if bits & (1 << i) != 0 {
                acked.push(ack.wrapping_sub(1 + i as u16));
            }
        }
        for s in acked {
            if let Some((sent, _, retries)) = self.unacked.remove(&s) {
                if retries == 0 {
                    let rtt = now.saturating_duration_since(sent);
                    self.srtt = Some(match self.srtt {
                        Some(p) => (p * 7 + rtt) / 8,
                        None => rtt,
                    });
                }
            }
        }
    }

    /// Record a received packet `seq` in the ack history; `false` = duplicate.
    fn note_recv(&mut self, seq: u16) -> bool {
        match self.recv_latest {
            None => {
                self.recv_latest = Some(seq);
                true
            }
            Some(latest) if newer(seq, latest) => {
                let d = seq.wrapping_sub(latest) as u32;
                self.recv_bits = if d >= 32 {
                    0
                } else {
                    (self.recv_bits << d) | (1 << (d - 1))
                };
                self.recv_latest = Some(seq);
                true
            }
            Some(latest) => {
                let d = latest.wrapping_sub(seq) as u32;
                if d == 0 || d > 32 {
                    return false;
                }
                let bit = 1 << (d - 1);
                if self.recv_bits & bit != 0 {
                    return false;
                }
                self.recv_bits |= bit;
                true
            }
        }
    }

    /// A message (or fragment) arrived; returns frames ready for the application.
    fn on_message(&mut self, ch: u8, mid: u16, idx: u16, count: u16, body: &[u8]) -> Vec<Frame> {
        let mut out = Vec::new();
        if count == 0 || idx >= count {
            return out;
        }
        match ch {
            CH_UNRELIABLE => {
                if self.last_unreliable.is_some_and(|l| !newer(mid, l)) {
                    return out;
                }
                self.last_unreliable = Some(mid);
                if let Ok(f) = QuicConnection::decode_datagram(Bytes::copy_from_slice(body)) {
                    out.push(f);
                }
            }
            CH_RELIABLE => {
                if !newer(mid, self.next_reliable) && mid != self.next_reliable {
                    return out; // already delivered
                }
                // a fragment the sender cannot legitimately produce: a count past the frame
                // limit, or a body larger than the MTU allows
                if count as usize > self.max_frags || body.len() > FRAG {
                    return out;
                }
                if !self.reliable_pending.contains_key(&mid) {
                    let budget = (self.max_frags * PENDING_BUDGET_FRAMES).max(MIN_PENDING_SLOTS);
                    if self.pending_slots + count as usize > budget {
                        return out; // ahead of what we can hold: not acked, retransmitted later
                    }
                    self.pending_slots += count as usize;
                    self.reliable_pending
                        .insert(mid, (count, vec![None; count as usize]));
                }
                let entry = self.reliable_pending.get_mut(&mid).expect("just inserted");
                if entry.0 == count && entry.1[idx as usize].is_none() {
                    entry.1[idx as usize] = Some(body.to_vec());
                }
                while let Some((c, frags)) = self.reliable_pending.get(&self.next_reliable) {
                    if frags.iter().any(Option::is_none) || *c == 0 {
                        break;
                    }
                    let (c, frags) = self.reliable_pending.remove(&self.next_reliable).unwrap();
                    self.pending_slots = self.pending_slots.saturating_sub(c as usize);
                    let mut whole = Vec::new();
                    for f in frags.into_iter().flatten() {
                        whole.extend_from_slice(&f);
                    }
                    if let Ok(f) = QuicConnection::decode_datagram(Bytes::from(whole)) {
                        out.push(f);
                    }
                    self.next_reliable = self.next_reliable.wrapping_add(1);
                }
            }
            _ => {}
        }
        out
    }
}

/// Shared by server and client: one socket, the connections on it, timers.
struct Endpoint {
    socket: Arc<UdpSocket>,
    secret: [u8; 32],
    conns: HashMap<u32, Conn>,
    by_addr: HashMap<SocketAddr, u32>,
    accept_tx: Option<mpsc::Sender<UdpConnection>>,
    cmd_tx: mpsc::Sender<Cmd>,
    /// Client handshake in flight: nonce, cookie we were given, who is waiting.
    connecting: Option<Handshake>,
    max_frame: usize,
    /// Test hook: drop every n-th outgoing data packet (0 = off).
    drop_every: Arc<AtomicU32>,
    sent_data: u32,
}

impl Endpoint {
    fn send_raw(&mut self, addr: SocketAddr, buf: &[u8], data: bool) {
        if data {
            self.sent_data = self.sent_data.wrapping_add(1);
            let n = self.drop_every.load(Ordering::Relaxed);
            if n != 0 && self.sent_data % n == 0 {
                return;
            }
        }
        let _ = self.socket.try_send_to(buf, addr);
    }

    fn send_connect(&mut self, addr: SocketAddr, nonce: u64, cookie: Option<[u8; 16]>) {
        let mut buf = Vec::with_capacity(HDR + 24);
        header(T_CONNECT, 0, &mut buf);
        buf.extend_from_slice(&nonce.to_be_bytes());
        buf.extend_from_slice(&cookie.unwrap_or([0; 16]));
        self.send_raw(addr, &buf, false);
    }

    /// A fresh, unpredictable connection id: data packets are accepted on (id, source address)
    /// alone, so a guessable counter would let an off-path sender inject into other sessions.
    fn new_conn(&mut self, addr: SocketAddr) -> (u32, UdpConnection) {
        let id = loop {
            let id: u32 = rand::random();
            if id != 0 && !self.conns.contains_key(&id) {
                break id;
            }
        };
        let (tx, rx) = mpsc::channel(1024);
        self.conns.insert(id, Conn::new(addr, tx, self.max_frame));
        self.by_addr.insert(addr, id);
        (
            id,
            UdpConnection {
                id,
                cmds: self.cmd_tx.clone(),
                inbound: rx,
                max_frame: self.max_frame,
            },
        )
    }

    fn drop_conn(&mut self, id: u32, notify: bool) {
        if let Some(c) = self.conns.remove(&id) {
            self.by_addr.remove(&c.addr);
            if notify {
                let mut buf = Vec::with_capacity(HDR);
                header(T_DISCONNECT, id, &mut buf);
                self.send_raw(c.addr, &buf, false);
            }
        }
    }

    fn on_packet(&mut self, from: SocketAddr, b: &[u8]) {
        let Some((t, id, body)) = parse_header(b) else {
            return;
        };
        match t {
            T_CONNECT if self.accept_tx.is_some() => {
                if body.len() < 24 {
                    return;
                }
                let nonce = u64::from_be_bytes(body[..8].try_into().unwrap());
                let given: [u8; 16] = body[8..24].try_into().unwrap();
                if !cookie_valid(&self.secret, from, nonce, &given) {
                    let mut buf = Vec::with_capacity(HDR + 24);
                    header(T_CHALLENGE, 0, &mut buf);
                    buf.extend_from_slice(&nonce.to_be_bytes());
                    buf.extend_from_slice(&cookie(&self.secret, from, nonce, slot_now()));
                    self.send_raw(from, &buf, false);
                    return;
                }
                // a retransmitted Connect for a live connection: just re-Accept
                let id = match self.by_addr.get(&from) {
                    Some(id) => *id,
                    None => {
                        if self.conns.len() >= MAX_CONNS {
                            return;
                        }
                        let (id, conn) = self.new_conn(from);
                        if let Some(tx) = &self.accept_tx {
                            if tx.try_send(conn).is_err() {
                                self.drop_conn(id, true);
                                return;
                            }
                        }
                        id
                    }
                };
                let mut buf = Vec::with_capacity(HDR);
                header(T_ACCEPT, id, &mut buf);
                self.send_raw(from, &buf, false);
            }
            T_CHALLENGE => {
                if body.len() < 24 {
                    return;
                }
                let nonce = u64::from_be_bytes(body[..8].try_into().unwrap());
                let given: [u8; 16] = body[8..24].try_into().unwrap();
                if let Some((addr, n, cookie_slot, _)) = &mut self.connecting {
                    if *addr == from && *n == nonce {
                        *cookie_slot = Some(given);
                        let (addr, n) = (*addr, *n);
                        self.send_connect(addr, n, Some(given));
                    }
                }
            }
            T_ACCEPT => {
                if let Some((addr, _, _, _)) = &self.connecting {
                    if *addr == from {
                        let (addr, _, _, waiter) = self.connecting.take().unwrap();
                        let (tx, rx) = mpsc::channel(1024);
                        self.conns.insert(id, Conn::new(addr, tx, self.max_frame));
                        self.by_addr.insert(addr, id);
                        let _ = waiter.send(UdpConnection {
                            id,
                            cmds: self.cmd_tx.clone(),
                            inbound: rx,
                            max_frame: self.max_frame,
                        });
                    }
                }
            }
            T_DATA => {
                if body.len() < DATA_HDR {
                    return;
                }
                let Some(c) = self.conns.get_mut(&id) else {
                    return;
                };
                if c.addr != from {
                    return;
                }
                c.last_recv = Instant::now();
                let seq = u16::from_be_bytes([body[0], body[1]]);
                let ack = u16::from_be_bytes([body[2], body[3]]);
                let bits = u32::from_be_bytes([body[4], body[5], body[6], body[7]]);
                c.on_ack(ack, bits);
                // duplicates and late retransmits still get an ack; messages dedupe themselves
                c.note_recv(seq);
                let rest = &body[DATA_HDR..];
                let mut frames = Vec::new();
                if rest.len() >= MSG_HDR {
                    let ch = rest[0];
                    let mid = u16::from_be_bytes([rest[1], rest[2]]);
                    let idx = u16::from_be_bytes([rest[3], rest[4]]);
                    let count = u16::from_be_bytes([rest[5], rest[6]]);
                    let len = u16::from_be_bytes([rest[7], rest[8]]) as usize;
                    if rest.len() >= MSG_HDR + len {
                        frames = c.on_message(ch, mid, idx, count, &rest[MSG_HDR..MSG_HDR + len]);
                    }
                    // acknowledge data promptly; bare ack packets are not themselves acked
                    let pkt = c.packet(id, None, false, 0);
                    let addr = c.addr;
                    self.send_raw(addr, &pkt, true);
                }
                let c = self.conns.get_mut(&id).unwrap();
                for f in frames {
                    let _ = c.inbound.try_send(f);
                }
            }
            T_DISCONNECT if self.conns.get(&id).is_some_and(|c| c.addr == from) => {
                self.drop_conn(id, false);
            }
            _ => {}
        }
    }

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Send {
                id,
                reliable,
                bytes,
            } => {
                let Some(c) = self.conns.get_mut(&id) else {
                    return;
                };
                let addr = c.addr;
                let reliable = reliable || bytes.len() > FRAG;
                let ch = if reliable { CH_RELIABLE } else { CH_UNRELIABLE };
                let mid = c.msg_id[ch as usize];
                c.msg_id[ch as usize] = mid.wrapping_add(1);
                let count = bytes.len().div_ceil(FRAG).max(1) as u16;
                let mut pkts = Vec::with_capacity(count as usize);
                let chunks: Vec<&[u8]> = if bytes.is_empty() {
                    vec![&[][..]]
                } else {
                    bytes.chunks(FRAG).collect()
                };
                for (i, chunk) in chunks.into_iter().enumerate() {
                    let frag = Frag {
                        ch,
                        mid,
                        idx: i as u16,
                        count,
                        body: chunk.to_vec(),
                    };
                    pkts.push(c.packet(id, Some(frag), reliable, 0));
                }
                for p in pkts {
                    self.send_raw(addr, &p, true);
                }
            }
            Cmd::Close { id } => {
                if let Some(c) = self.conns.get_mut(&id) {
                    c.closing = true;
                }
                self.drop_conn(id, true);
            }
        }
    }

    fn on_tick(&mut self) {
        let now = Instant::now();
        let mut gone = Vec::new();
        let mut resend: Vec<(SocketAddr, Vec<u8>)> = Vec::new();
        for (id, c) in self.conns.iter_mut() {
            // the session loop dropped its handle without `close()`: stop keeping the peer alive
            if now.saturating_duration_since(c.last_recv) > IDLE || c.inbound.is_closed() {
                gone.push(*id);
                continue;
            }
            let rto = c.rto();
            let expired: Vec<u16> = c
                .unacked
                .iter()
                .filter(|(_, (sent, _, retries))| {
                    now.saturating_duration_since(*sent) >= rto * (1 + retries.min(&4))
                })
                .map(|(seq, _)| *seq)
                .collect();
            let mut dead = false;
            for seq in expired {
                let Some((_, frag, retries)) = c.unacked.remove(&seq) else {
                    continue;
                };
                if retries + 1 > MAX_RETRIES {
                    dead = true;
                    break;
                }
                let pkt = c.packet(*id, Some(frag), true, retries + 1);
                resend.push((c.addr, pkt));
            }
            if dead {
                gone.push(*id);
                continue;
            }
            if now.saturating_duration_since(c.last_send) >= KEEPALIVE {
                let pkt = c.packet(*id, None, false, 0);
                resend.push((c.addr, pkt));
            }
        }
        for (addr, pkt) in resend {
            self.send_raw(addr, &pkt, true);
        }
        for id in gone {
            self.drop_conn(id, true);
        }
        if let Some((addr, nonce, cookie, _)) = &self.connecting {
            let (addr, nonce, cookie) = (*addr, *nonce, *cookie);
            self.send_connect(addr, nonce, cookie);
        }
    }
}

fn spawn_endpoint(
    socket: UdpSocket,
    secret: [u8; 32],
    accept_tx: Option<mpsc::Sender<UdpConnection>>,
    max_frame: usize,
    drop_every: Arc<AtomicU32>,
) -> (mpsc::Sender<Cmd>, Arc<UdpSocket>, EndpointHandle) {
    let (cmd_tx, cmd_rx) = mpsc::channel(4096);
    let socket = Arc::new(socket);
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<Connecting>(4);
    let ep = Endpoint {
        socket: socket.clone(),
        secret,
        conns: HashMap::new(),
        by_addr: HashMap::new(),
        accept_tx,
        cmd_tx: cmd_tx.clone(),
        connecting: None,
        max_frame,
        drop_every,
        sent_data: 0,
    };
    tokio::spawn(async move {
        let mut ep = ep;
        // the client starts its handshake through the control channel, then shares the loop
        let mut cmds = cmd_rx;
        let socket = ep.socket.clone();
        let mut buf = vec![0u8; 2048];
        let mut tick = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                r = socket.recv_from(&mut buf) => {
                    if let Ok((n, from)) = r {
                        ep.on_packet(from, &buf[..n]);
                    }
                }
                c = cmds.recv() => match c {
                    Some(c) => ep.on_cmd(c),
                    None => return,
                },
                Some(Connecting { server, waiter }) = ctl_rx.recv() => {
                    let nonce: u64 = rand::random();
                    ep.connecting = Some((server, nonce, None, waiter));
                    ep.send_connect(server, nonce, None);
                }
                _ = tick.tick() => ep.on_tick(),
            }
        }
    });
    (cmd_tx, socket, EndpointHandle { ctl: ctl_tx })
}

struct Connecting {
    server: SocketAddr,
    waiter: oneshot::Sender<UdpConnection>,
}

#[derive(Clone)]
struct EndpointHandle {
    ctl: mpsc::Sender<Connecting>,
}

/// Server side: bind, then `accept()` connections as their handshakes complete.
pub struct UdpServer {
    local: SocketAddr,
    accept_rx: tokio::sync::Mutex<mpsc::Receiver<UdpConnection>>,
    _cmds: mpsc::Sender<Cmd>,
}

impl UdpServer {
    pub async fn bind(
        listen: SocketAddr,
        secret: Option<[u8; 32]>,
        max_frame: usize,
    ) -> Result<Arc<Self>, TransportError> {
        Self::bind_with_loss(listen, secret, max_frame, 0).await
    }

    /// `drop_every` > 0 drops every n-th outgoing data packet (tests of retransmission).
    pub async fn bind_with_loss(
        listen: SocketAddr,
        secret: Option<[u8; 32]>,
        max_frame: usize,
        drop_every: u32,
    ) -> Result<Arc<Self>, TransportError> {
        let socket = UdpSocket::bind(listen).await.map_err(io)?;
        let local = socket.local_addr().map_err(io)?;
        let secret = secret.unwrap_or_else(rand::random);
        let (accept_tx, accept_rx) = mpsc::channel(1024);
        let (cmds, _, _) = spawn_endpoint(
            socket,
            secret,
            Some(accept_tx),
            max_frame,
            Arc::new(AtomicU32::new(drop_every)),
        );
        Ok(Arc::new(Self {
            local,
            accept_rx: tokio::sync::Mutex::new(accept_rx),
            _cmds: cmds,
        }))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// Next connection whose handshake completed; `None` once the endpoint is gone.
    pub async fn accept(&self) -> Option<UdpConnection> {
        self.accept_rx.lock().await.recv().await
    }
}

/// Client side (SDKs, tests): one socket per connection.
pub struct UdpClient;

impl UdpClient {
    pub async fn connect(
        server: SocketAddr,
        max_frame: usize,
    ) -> Result<UdpConnection, TransportError> {
        let bind: SocketAddr = if server.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let socket = UdpSocket::bind(bind).await.map_err(io)?;
        let (_, _, handle) = spawn_endpoint(
            socket,
            [0; 32],
            None,
            max_frame,
            Arc::new(AtomicU32::new(0)),
        );
        let (tx, rx) = oneshot::channel();
        handle
            .ctl
            .send(Connecting { server, waiter: tx })
            .await
            .map_err(|_| TransportError::Closed)?;
        tokio::time::timeout(Duration::from_secs(5), rx)
            .await
            .map_err(|_| TransportError::Timeout)?
            .map_err(|_| TransportError::Closed)
    }
}

/// One peer as seen by the session loop.
pub struct UdpConnection {
    id: u32,
    cmds: mpsc::Sender<Cmd>,
    inbound: mpsc::Receiver<Frame>,
    max_frame: usize,
}

impl UdpConnection {
    async fn push(&self, reliable: bool, frame: Frame) -> Result<(), TransportError> {
        let bytes = QuicConnection::encode_datagram(&frame).to_vec();
        if bytes.len() > self.max_frame || bytes.len() > FRAG * u16::MAX as usize {
            return Err(TransportError::TooLarge(bytes.len()));
        }
        self.cmds
            .send(Cmd::Send {
                id: self.id,
                reliable,
                bytes,
            })
            .await
            .map_err(|_| TransportError::Closed)
    }
}

impl Connection for UdpConnection {
    fn send(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(self.push(true, frame))
    }

    fn send_unreliable(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(self.push(false, frame))
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Frame, TransportError>> {
        Box::pin(async move {
            match self.inbound.recv().await {
                Some(f) if f.payload_len() > self.max_frame => {
                    Err(TransportError::TooLarge(f.payload_len()))
                }
                Some(f) => Ok(f),
                None => Err(TransportError::Closed),
            }
        })
    }

    fn close(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let _ = self.cmds.send(Cmd::Close { id: self.id }).await;
        })
    }

    fn kind(&self) -> &'static str {
        "udp"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pair(drop_every: u32) -> (UdpConnection, UdpConnection) {
        let server =
            UdpServer::bind_with_loss("127.0.0.1:0".parse().unwrap(), None, 1 << 20, drop_every)
                .await
                .unwrap();
        let client = UdpClient::connect(server.local_addr(), 1 << 20)
            .await
            .unwrap();
        let accepted = server.accept().await.unwrap();
        (client, accepted)
    }

    #[tokio::test]
    async fn handshake_fragments_and_order() {
        let (mut c, mut s) = pair(0).await;
        let big: Vec<u8> = (0..50_000u32).map(|i| (i % 251) as u8).collect();
        c.send(Frame::Binary(Bytes::from(big.clone())))
            .await
            .unwrap();
        for i in 0..100 {
            c.send(Frame::Text(format!("m{i}"))).await.unwrap();
        }
        let got = tokio::time::timeout(Duration::from_secs(5), s.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, Frame::Binary(Bytes::from(big)));
        for i in 0..100 {
            let f = tokio::time::timeout(Duration::from_secs(5), s.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(f, Frame::Text(format!("m{i}")));
        }
        // server → client, unreliable
        s.send_unreliable(Frame::Text("pos".into())).await.unwrap();
        let f = tokio::time::timeout(Duration::from_secs(5), c.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(f, Frame::Text("pos".into()));
        s.close().await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), c.recv()).await,
            Ok(Err(TransportError::Closed))
        ));
    }

    #[tokio::test]
    async fn reliable_survives_packet_loss() {
        let (mut c, mut s) = pair(3).await;
        for i in 0..200 {
            s.send(Frame::Text(format!("r{i}"))).await.unwrap();
        }
        for i in 0..200 {
            let f = tokio::time::timeout(Duration::from_secs(10), c.recv())
                .await
                .expect("delivery despite loss")
                .unwrap();
            assert_eq!(f, Frame::Text(format!("r{i}")));
        }
    }

    #[test]
    fn ack_history_tracks_out_of_order_and_duplicates() {
        let (tx, _rx) = mpsc::channel(1);
        let mut c = Conn::new("127.0.0.1:1".parse().unwrap(), tx, 64 * 1024);
        assert!(c.note_recv(5));
        assert!(c.note_recv(7));
        assert!(c.note_recv(6));
        assert!(!c.note_recv(6));
        assert!(!c.note_recv(7));
        assert_eq!(c.ack_fields(), (7, 0b11));
        assert!(c.note_recv(3));
        assert_eq!(c.ack_fields(), (7, 0b1011));
    }

    /// The handshake cookie is a keyed MAC bound to the source address and nonce; a zero or
    /// altered cookie, another secret or another source never validates.
    #[test]
    fn cookie_is_keyed_and_bound_to_source() {
        let secret = [9u8; 32];
        let addr: SocketAddr = "203.0.113.7:4000".parse().unwrap();
        let other: SocketAddr = "203.0.113.8:4000".parse().unwrap();
        let good = cookie(&secret, addr, 42, slot_now());
        assert!(cookie_valid(&secret, addr, 42, &good));
        assert!(cookie_valid(
            &secret,
            addr,
            42,
            &cookie(&secret, addr, 42, slot_now().wrapping_sub(1))
        ));
        assert!(!cookie_valid(&secret, addr, 42, &[0; 16]));
        assert!(!cookie_valid(&secret, other, 42, &good));
        assert!(!cookie_valid(&secret, addr, 43, &good));
        assert!(!cookie_valid(&[8u8; 32], addr, 42, &good));
        let mut flipped = good;
        flipped[15] ^= 1;
        assert!(!cookie_valid(&secret, addr, 42, &flipped));
        assert!(!cookie_valid(
            &secret,
            addr,
            42,
            &cookie(&secret, addr, 42, slot_now().wrapping_sub(2))
        ));
    }

    /// A peer cannot make the receiver allocate for messages it could not legitimately have in
    /// flight: fragment counts past the frame limit, fragments larger than the MTU allows, or
    /// more buffered fragments than the reassembly budget while an earlier message is missing.
    #[test]
    fn reassembly_refuses_what_the_sender_cannot_have_in_flight() {
        let (tx, _rx) = mpsc::channel(1);
        let max_frame = 64 * 1024;
        let mut c = Conn::new("127.0.0.1:1".parse().unwrap(), tx, max_frame);
        assert!(c.on_message(CH_RELIABLE, 0, 0, u16::MAX, b"x").is_empty());
        assert!(
            c.reliable_pending.is_empty(),
            "65535-fragment claim allocates nothing"
        );
        let limit = max_frags(max_frame) as u16;
        assert!(c.on_message(CH_RELIABLE, 0, 0, limit + 1, b"x").is_empty());
        assert!(c.reliable_pending.is_empty());
        assert!(c
            .on_message(CH_RELIABLE, 1, 0, 2, &vec![0u8; FRAG + 1])
            .is_empty());
        assert!(
            c.reliable_pending.is_empty(),
            "oversized fragment is not buffered"
        );
        // message 0 is missing; full-size messages 1.. are held up to the budget, then dropped
        let budget = (max_frags(max_frame) * PENDING_BUDGET_FRAMES).max(MIN_PENDING_SLOTS);
        let mut mid = 1u16;
        while (c.pending_slots + limit as usize) <= budget {
            assert!(c.on_message(CH_RELIABLE, mid, 0, limit, b"x").is_empty());
            mid += 1;
        }
        let held = c.reliable_pending.len();
        assert!(held >= PENDING_BUDGET_FRAMES && c.pending_slots <= budget);
        assert!(c.on_message(CH_RELIABLE, mid, 0, limit, b"x").is_empty());
        assert_eq!(c.reliable_pending.len(), held, "over budget: not buffered");
        // the missing message arrives: it is delivered and its slots are released
        let got = c.on_message(CH_RELIABLE, 0, 0, 1, b"\x00hi");
        assert_eq!(got, vec![Frame::Text("hi".into())]);
        assert_eq!(c.pending_slots, held * limit as usize);
        assert_eq!(c.reliable_pending.len(), held);
    }

    /// Connection ids come from the CSPRNG (an off-path sender must not be able to guess the id
    /// of a live session), and a session loop that drops its handle without `close()` still
    /// tears the peer down instead of keeping it alive with keepalives forever.
    #[tokio::test]
    async fn ids_are_unpredictable_and_dropped_handles_close() {
        let (mut c1, s1) = pair(0).await;
        let (_c2, s2) = pair(0).await;
        assert!(s1.id != 0 && s2.id != 0);
        assert!(
            !(s1.id == 1 && s2.id == 2),
            "ids are not a counter: {} {}",
            s1.id,
            s2.id
        );
        assert_eq!(s1.id, c1.id);
        drop(s1);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), c1.recv()).await,
            Ok(Err(TransportError::Closed))
        ));
    }
}
