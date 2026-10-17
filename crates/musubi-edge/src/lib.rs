//! musubi-edge: UDP ingress proxy for dedicated servers (plan 05, the "Quilkin analog"). Clients
//! never learn game-server addresses: every datagram sent to the proxy starts with a routing header
//! `0xED 0x6E | len:u8 | token`, the proxy resolves the token to an upstream through a [`Router`]
//! and forwards the rest from a per-client socket, so upstream replies find their way back without
//! any header. Malformed packets and unknown tokens are dropped before any upstream traffic (no
//! amplification); a client's session expires after `idle` without packets.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::net::UdpSocket;

pub const MAGIC: [u8; 2] = [0xED, 0x6E];
pub const MAX_TOKEN: usize = 128;
const MAX_DATAGRAM: usize = 65_535;

/// Maps a routing token to the game server it belongs to. `None` = drop the packet.
#[async_trait::async_trait]
pub trait Router: Send + Sync + 'static {
    async fn resolve(&self, token: &[u8]) -> Option<SocketAddr>;
}

/// Split a client datagram into (token, payload); `None` when the header is missing or malformed.
pub fn split_header(pkt: &[u8]) -> Option<(&[u8], &[u8])> {
    if pkt.len() < 3 || pkt[..2] != MAGIC {
        return None;
    }
    let n = pkt[2] as usize;
    if n == 0 || n > MAX_TOKEN || pkt.len() < 3 + n {
        return None;
    }
    Some((&pkt[3..3 + n], &pkt[3 + n..]))
}

/// Client side: prepend the routing header. Panics if the token is empty or longer than `MAX_TOKEN`.
pub fn with_header(token: &[u8], payload: &[u8]) -> Vec<u8> {
    assert!(
        !token.is_empty() && token.len() <= MAX_TOKEN,
        "token 1..={MAX_TOKEN} bytes"
    );
    let mut v = Vec::with_capacity(3 + token.len() + payload.len());
    v.extend_from_slice(&MAGIC);
    v.push(token.len() as u8);
    v.extend_from_slice(token);
    v.extend_from_slice(payload);
    v
}

struct Session {
    token: Vec<u8>,
    upstream: Arc<UdpSocket>,
    peer: SocketAddr,
    /// Millis since proxy start of the last client packet.
    last: AtomicU64,
    replies: tokio::task::JoinHandle<()>,
}

pub struct UdpProxy {
    sock: Arc<UdpSocket>,
    sessions: Arc<DashMap<SocketAddr, Arc<Session>>>,
    router: Arc<dyn Router>,
    idle: Duration,
    epoch: Instant,
    forwarded: AtomicU64,
    dropped: AtomicU64,
}

impl UdpProxy {
    pub async fn start(
        listen: SocketAddr,
        router: Arc<dyn Router>,
        idle: Duration,
    ) -> std::io::Result<Arc<Self>> {
        let sock = Arc::new(UdpSocket::bind(listen).await?);
        let proxy = Arc::new(Self {
            sock,
            sessions: Arc::new(DashMap::new()),
            router,
            idle,
            epoch: Instant::now(),
            forwarded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        });
        tokio::spawn(proxy.clone().ingress());
        tokio::spawn(proxy.clone().sweeper());
        Ok(proxy)
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.sock.local_addr().expect("bound socket")
    }

    pub fn sessions(&self) -> usize {
        self.sessions.len()
    }

    /// (datagrams forwarded upstream, datagrams dropped at the edge).
    pub fn counters(&self) -> (u64, u64) {
        (
            self.forwarded.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
        )
    }

    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    async fn ingress(self: Arc<Self>) {
        let mut buf = vec![0u8; MAX_DATAGRAM];
        loop {
            let (n, from) = match self.sock.recv_from(&mut buf).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::warn!(error = %e, "edge: recv failed");
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    continue;
                }
            };
            let Some((token, payload)) = split_header(&buf[..n]) else {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let session = match self.sessions.get(&from) {
                Some(s) if s.token == token => Some(s.clone()),
                _ => None,
            };
            let session = match session {
                Some(s) => s,
                None => match self.open(from, token).await {
                    Some(s) => s,
                    None => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                },
            };
            session.last.store(self.now_ms(), Ordering::Relaxed);
            match session.upstream.send(payload).await {
                Ok(_) => {
                    self.forwarded.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::debug!(client = %from, upstream = %session.peer, error = %e, "edge: upstream send failed");
                    self.close(from);
                }
            }
        }
    }

    /// Resolve the token and open a per-client upstream socket (replacing any session the client
    /// had with a different token, e.g. after a new allocation).
    async fn open(self: &Arc<Self>, client: SocketAddr, token: &[u8]) -> Option<Arc<Session>> {
        let peer = self.router.resolve(token).await?;
        let bind: SocketAddr = if peer.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let upstream = match UdpSocket::bind(bind).await {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!(error = %e, "edge: cannot bind upstream socket");
                return None;
            }
        };
        if let Err(e) = upstream.connect(peer).await {
            tracing::warn!(upstream = %peer, error = %e, "edge: cannot connect upstream socket");
            return None;
        }
        let replies = tokio::spawn({
            let upstream = upstream.clone();
            let down = self.sock.clone();
            async move {
                let mut buf = vec![0u8; MAX_DATAGRAM];
                loop {
                    match upstream.recv(&mut buf).await {
                        Ok(n) => {
                            if down.send_to(&buf[..n], client).await.is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            tracing::debug!(client = %client, error = %e, "edge: upstream recv failed");
                            return;
                        }
                    }
                }
            }
        });
        let session = Arc::new(Session {
            token: token.to_vec(),
            upstream,
            peer,
            last: AtomicU64::new(self.now_ms()),
            replies,
        });
        if let Some((_, old)) = self.sessions.remove(&client) {
            old.replies.abort();
        }
        self.sessions.insert(client, session.clone());
        tracing::debug!(client = %client, upstream = %peer, "edge: session opened");
        Some(session)
    }

    fn close(&self, client: SocketAddr) {
        if let Some((_, s)) = self.sessions.remove(&client) {
            s.replies.abort();
        }
    }

    async fn sweeper(self: Arc<Self>) {
        let every = (self.idle / 2).max(Duration::from_millis(10));
        loop {
            tokio::time::sleep(every).await;
            let cutoff = self.now_ms().saturating_sub(self.idle.as_millis() as u64);
            let stale: Vec<SocketAddr> = self
                .sessions
                .iter()
                .filter(|e| e.value().last.load(Ordering::Relaxed) < cutoff)
                .map(|e| *e.key())
                .collect();
            for c in stale {
                self.close(c);
            }
        }
    }
}

impl Drop for UdpProxy {
    fn drop(&mut self) {
        for e in self.sessions.iter() {
            e.value().replies.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Table(HashMap<Vec<u8>, SocketAddr>);

    #[async_trait::async_trait]
    impl Router for Table {
        async fn resolve(&self, token: &[u8]) -> Option<SocketAddr> {
            self.0.get(token).copied()
        }
    }

    async fn echo(prefix: &'static str) -> SocketAddr {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            loop {
                let (n, from) = s.recv_from(&mut buf).await.unwrap();
                let mut out = prefix.as_bytes().to_vec();
                out.extend_from_slice(&buf[..n]);
                s.send_to(&out, from).await.unwrap();
            }
        });
        addr
    }

    async fn recv_or_timeout(s: &UdpSocket, ms: u64) -> Option<Vec<u8>> {
        let mut buf = [0u8; 2048];
        tokio::time::timeout(Duration::from_millis(ms), s.recv(&mut buf))
            .await
            .ok()
            .map(|r| buf[..r.unwrap()].to_vec())
    }

    #[test]
    fn header_roundtrip_and_rejects() {
        let p = with_header(b"tok", b"hello");
        assert_eq!(split_header(&p), Some((&b"tok"[..], &b"hello"[..])));
        assert_eq!(split_header(b"hello"), None);
        assert_eq!(split_header(&[0xED, 0x6E, 0]), None);
        assert_eq!(split_header(&[0xED, 0x6E, 5, b'a']), None);
        assert_eq!(
            split_header(&with_header(b"t", b"")),
            Some((&b"t"[..], &b""[..]))
        );
    }

    #[tokio::test]
    async fn routes_by_token_hides_upstream_and_drops_strangers() {
        let a = echo("A:").await;
        let b = echo("B:").await;
        let table = Table(HashMap::from([(b"ta".to_vec(), a), (b"tb".to_vec(), b)]));
        let proxy = UdpProxy::start(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(table),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let p = proxy.local_addr();

        let c1 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        c1.connect(p).await.unwrap();
        c1.send(&with_header(b"ta", b"hi")).await.unwrap();
        assert_eq!(recv_or_timeout(&c1, 1000).await.unwrap(), b"A:hi");
        // replies come from the proxy, not the game server
        let c2 = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        c2.send_to(&with_header(b"tb", b"yo"), p).await.unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = tokio::time::timeout(Duration::from_secs(1), c2.recv_from(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"B:yo");
        assert_eq!(from, p);
        assert_eq!(proxy.sessions(), 2);

        // unknown token and raw payload: silence
        c1.send(&with_header(b"nope", b"x")).await.unwrap();
        c1.send(b"raw").await.unwrap();
        assert!(recv_or_timeout(&c1, 200).await.is_none());
        let (fwd, dropped) = proxy.counters();
        assert_eq!((fwd, dropped), (2, 2));

        // same client, new token (new allocation) → re-routed
        c1.send(&with_header(b"tb", b"again")).await.unwrap();
        assert_eq!(recv_or_timeout(&c1, 1000).await.unwrap(), b"B:again");
        assert_eq!(proxy.sessions(), 2);
    }

    #[tokio::test]
    async fn idle_sessions_expire() {
        let a = echo("").await;
        let table = Table(HashMap::from([(b"t".to_vec(), a)]));
        let proxy = UdpProxy::start(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(table),
            Duration::from_millis(100),
        )
        .await
        .unwrap();
        let c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        c.connect(proxy.local_addr()).await.unwrap();
        c.send(&with_header(b"t", b"1")).await.unwrap();
        assert_eq!(recv_or_timeout(&c, 1000).await.unwrap(), b"1");
        assert_eq!(proxy.sessions(), 1);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(proxy.sessions(), 0);
        // a new packet simply opens a new session
        c.send(&with_header(b"t", b"2")).await.unwrap();
        assert_eq!(recv_or_timeout(&c, 1000).await.unwrap(), b"2");
    }
}
