//! WebRTC DataChannel transport (`str0m`, sans-IO) for browsers without WebTransport. The browser
//! posts an SDP offer over HTTP, gets the answer, and opens two data channels: `reliable`
//! (ordered, retransmitted) and optionally `unreliable` (unordered, no retransmits). Each channel
//! message is one frame with the QUIC transport's datagram encoding (`type:u8 | payload`); the
//! first frame on the reliable channel is the JSON `Hello`, like QUIC and WebTransport.
//!
//! One UDP socket serves every peer (ICE-lite, a single host candidate at the advertised
//! address); one driver task owns all `Rtc` state machines and multiplexes the socket, timers and
//! outbound frames over them, the way str0m's own examples do. Sessions talk to it over channels.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::future::BoxFuture;
use str0m::change::SdpOffer;
use str0m::channel::ChannelId;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::quic::QuicConnection;
use crate::{Connection, Frame, TransportError};

pub const RELIABLE_LABEL: &str = "reliable";
pub const UNRELIABLE_LABEL: &str = "unreliable";
/// A peer whose reliable channel is not open this long after its offer was accepted is dropped:
/// an offer is posted anonymously over HTTP, so an abandoned one must not keep ICE/DTLS state.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Peers one `RtcServer` holds at once (each is an ICE agent + DTLS + SCTP state machine).
pub const MAX_PEERS: usize = 4096;
/// Frames a session may queue for a peer before its channel opens.
const MAX_PENDING: usize = 256;

enum Cmd {
    Peer {
        id: u64,
        rtc: Box<Rtc>,
        frames: mpsc::Sender<Frame>,
    },
    Send {
        id: u64,
        reliable: bool,
        bytes: Vec<u8>,
    },
    Close {
        id: u64,
    },
}

struct Peer {
    rtc: Rtc,
    frames: mpsc::Sender<Frame>,
    reliable: Option<ChannelId>,
    unreliable: Option<ChannelId>,
    /// Frames queued before the browser opened its channels.
    pending: Vec<(bool, Vec<u8>)>,
    deadline: Instant,
    /// When the offer was accepted (handshake timeout runs from here).
    since: Instant,
}

impl Peer {
    fn write(&mut self, reliable: bool, bytes: Vec<u8>) {
        let cid = if reliable {
            self.reliable
        } else {
            self.unreliable.or(self.reliable)
        };
        match cid.and_then(|c| self.rtc.channel(c)) {
            Some(mut ch) => {
                if ch.write(true, &bytes).is_err() {
                    self.rtc.disconnect();
                }
            }
            None if self.pending.len() >= MAX_PENDING => self.rtc.disconnect(),
            None => self.pending.push((reliable, bytes)),
        }
    }

    /// Drain `poll_output` to the next timeout: transmit, dispatch events. `Err` = peer is gone.
    fn drain(&mut self, socket: &UdpSocket) -> Result<Instant, ()> {
        loop {
            if !self.rtc.is_alive() {
                return Err(());
            }
            match self.rtc.poll_output() {
                Ok(Output::Timeout(t)) => {
                    self.deadline = t;
                    return Ok(t);
                }
                Ok(Output::Transmit(t)) => {
                    let _ = socket.try_send_to(&t.contents, t.destination);
                }
                Ok(Output::Event(e)) => match e {
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => {
                        self.rtc.disconnect();
                    }
                    Event::ChannelOpen(cid, label) => {
                        if label == UNRELIABLE_LABEL {
                            self.unreliable = Some(cid);
                        } else if self.reliable.is_none() {
                            self.reliable = Some(cid);
                        }
                        for (reliable, bytes) in std::mem::take(&mut self.pending) {
                            self.write(reliable, bytes);
                        }
                    }
                    Event::ChannelData(d) => {
                        if let Ok(f) = QuicConnection::decode_datagram(Bytes::from(d.data)) {
                            // a slow session loop drops inbound frames rather than stalling every peer
                            let _ = self.frames.try_send(f);
                        }
                    }
                    Event::ChannelClose(cid) if Some(cid) == self.reliable => {
                        self.rtc.disconnect();
                    }
                    _ => {}
                },
                Err(_) => return Err(()),
            }
        }
    }
}

pub struct RtcServer {
    cmds: mpsc::Sender<Cmd>,
    local: SocketAddr,
    public: SocketAddr,
    next_id: AtomicU64,
    /// Peers the driver currently holds (handshaking or connected).
    live: Arc<AtomicUsize>,
}

fn io(e: impl std::fmt::Display) -> TransportError {
    TransportError::Io(e.to_string())
}

impl RtcServer {
    /// Bind the shared UDP socket; `public` is the address advertised to browsers as the host
    /// candidate (defaults to the bound address, or loopback when bound to an unspecified IP).
    pub async fn bind(
        listen: SocketAddr,
        public: Option<SocketAddr>,
    ) -> Result<Arc<Self>, TransportError> {
        let socket = UdpSocket::bind(listen).await.map_err(io)?;
        let local = socket.local_addr().map_err(io)?;
        let public = public.unwrap_or_else(|| {
            if local.ip().is_unspecified() {
                SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), local.port())
            } else {
                local
            }
        });
        let (tx, rx) = mpsc::channel(4096);
        let live = Arc::new(AtomicUsize::new(0));
        tokio::spawn(drive(socket, rx, live.clone()));
        Ok(Arc::new(Self {
            cmds: tx,
            local,
            public,
            next_id: AtomicU64::new(1),
            live,
        }))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn public_addr(&self) -> SocketAddr {
        self.public
    }

    /// Peers currently held by the driver (handshaking or connected).
    pub fn peers(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    /// A browser's offer (`RTCSessionDescription` JSON: `{type, sdp}`) → the answer in the same
    /// form, plus the connection the session loop serves once the data channel is up.
    pub async fn accept_offer(
        &self,
        offer: serde_json::Value,
        max_frame: usize,
    ) -> Result<(serde_json::Value, RtcConnection), TransportError> {
        if self.live.load(Ordering::Relaxed) >= MAX_PEERS {
            return Err(TransportError::Io("too many webrtc peers".into()));
        }
        let offer: SdpOffer =
            serde_json::from_value(offer).map_err(|e| io(format!("offer: {e}")))?;
        let mut rtc = Rtc::builder().set_ice_lite(true).build(Instant::now());
        let cand = Candidate::host(self.public, "udp").map_err(io)?;
        rtc.add_local_candidate(cand);
        let answer = rtc.sdp_api().accept_offer(offer).map_err(io)?;
        let answer = serde_json::to_value(&answer).map_err(io)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (ftx, frx) = mpsc::channel(1024);
        self.cmds
            .send(Cmd::Peer {
                id,
                rtc: Box::new(rtc),
                frames: ftx,
            })
            .await
            .map_err(|_| TransportError::Closed)?;
        Ok((
            answer,
            RtcConnection {
                id,
                cmds: self.cmds.clone(),
                frames: frx,
                max_frame,
            },
        ))
    }
}

async fn drive(socket: UdpSocket, mut cmds: mpsc::Receiver<Cmd>, live: Arc<AtomicUsize>) {
    let Ok(local) = socket.local_addr() else {
        return;
    };
    let mut peers: HashMap<u64, Peer> = HashMap::new();
    let mut by_addr: HashMap<SocketAddr, u64> = HashMap::new();
    let mut buf = vec![0u8; 2000];
    loop {
        let now = Instant::now();
        let mut next = now + Duration::from_secs(1);
        let mut gone = Vec::new();
        for (id, p) in peers.iter_mut() {
            if p.reliable.is_none() && now.saturating_duration_since(p.since) > HANDSHAKE_TIMEOUT {
                p.rtc.disconnect();
            }
            match p.drain(&socket) {
                Ok(t) => next = next.min(t),
                Err(()) => gone.push(*id),
            }
        }
        for id in gone {
            peers.remove(&id);
            by_addr.retain(|_, v| *v != id);
        }
        live.store(peers.len(), Ordering::Relaxed);
        let wake = tokio::time::Instant::from_std(next.max(now + Duration::from_millis(1)));
        tokio::select! {
            r = socket.recv_from(&mut buf) => {
                let Ok((n, source)) = r else { continue };
                let Ok(contents) = buf[..n].try_into() else { continue };
                let input = Input::Receive(
                    Instant::now(),
                    Receive { proto: Protocol::Udp, source, destination: local, contents },
                );
                let target = by_addr
                    .get(&source)
                    .copied()
                    .filter(|id| peers.contains_key(id))
                    .or_else(|| peers.iter().find(|(_, p)| p.rtc.accepts(&input)).map(|(id, _)| *id));
                if let Some(id) = target {
                    by_addr.insert(source, id);
                    if let Some(p) = peers.get_mut(&id) {
                        if p.rtc.handle_input(input).is_err() {
                            p.rtc.disconnect();
                        }
                    }
                }
            }
            c = cmds.recv() => match c {
                None => return,
                Some(Cmd::Peer { id, rtc, frames }) => {
                    peers.insert(id, Peer {
                        rtc: *rtc,
                        frames,
                        reliable: None,
                        unreliable: None,
                        pending: Vec::new(),
                        deadline: Instant::now(),
                        since: Instant::now(),
                    });
                    live.store(peers.len(), Ordering::Relaxed);
                }
                Some(Cmd::Send { id, reliable, bytes }) => {
                    if let Some(p) = peers.get_mut(&id) {
                        p.write(reliable, bytes);
                    }
                }
                Some(Cmd::Close { id }) => {
                    if let Some(p) = peers.get_mut(&id) {
                        p.rtc.disconnect();
                    }
                }
            },
            _ = tokio::time::sleep_until(wake) => {
                let now = Instant::now();
                for p in peers.values_mut() {
                    if now >= p.deadline && p.rtc.handle_input(Input::Timeout(now)).is_err() {
                        p.rtc.disconnect();
                    }
                }
            }
        }
    }
}

/// One browser peer as seen by the session loop.
pub struct RtcConnection {
    id: u64,
    cmds: mpsc::Sender<Cmd>,
    frames: mpsc::Receiver<Frame>,
    max_frame: usize,
}

impl RtcConnection {
    async fn push(&self, reliable: bool, frame: Frame) -> Result<(), TransportError> {
        let bytes = QuicConnection::encode_datagram(&frame).to_vec();
        if bytes.len() > self.max_frame {
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

impl Connection for RtcConnection {
    fn send(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(self.push(true, frame))
    }

    fn send_unreliable(&mut self, frame: Frame) -> BoxFuture<'_, Result<(), TransportError>> {
        Box::pin(self.push(false, frame))
    }

    fn recv(&mut self) -> BoxFuture<'_, Result<Frame, TransportError>> {
        Box::pin(async move {
            match self.frames.recv().await {
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
        "webrtc"
    }
}
