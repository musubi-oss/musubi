//! P1-62: simulated network conditions for development — latency, jitter and packet loss injected
//! around a realtime connection, globally or for one session, switchable at runtime through
//! `/v1/admin/dev/netsim`. Frames keep their order; dropped frames are counted, never errors.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

use musubi_core::{Error, Result, SessionId};
use musubi_transport::{Connection, Frame, TransportError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Params {
    /// Added one-way delay per frame.
    pub latency_ms: u64,
    /// Uniform ± jitter around `latency_ms`.
    pub jitter_ms: u64,
    /// Fraction of frames dropped, 0..=100.
    pub loss_pct: f64,
    /// `both` | `in` (client → server) | `out` (server → client).
    pub direction: String,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            latency_ms: 0,
            jitter_ms: 0,
            loss_pct: 0.0,
            direction: "both".into(),
        }
    }
}

impl Params {
    pub fn validate(&self) -> Result<()> {
        if self.latency_ms > 10_000 {
            return Err(Error::InvalidArgument("latency_ms must be <= 10000".into()));
        }
        if self.jitter_ms > self.latency_ms {
            return Err(Error::InvalidArgument(
                "jitter_ms must not exceed latency_ms".into(),
            ));
        }
        if !(0.0..=100.0).contains(&self.loss_pct) || self.loss_pct.is_nan() {
            return Err(Error::InvalidArgument("loss_pct must be 0..=100".into()));
        }
        if !matches!(self.direction.as_str(), "both" | "in" | "out") {
            return Err(Error::InvalidArgument(
                "direction must be both, in or out".into(),
            ));
        }
        Ok(())
    }

    fn applies(&self, outbound: bool) -> bool {
        match self.direction.as_str() {
            "in" => !outbound,
            "out" => outbound,
            _ => true,
        }
    }

    fn delay(&self) -> Option<Duration> {
        if self.latency_ms == 0 {
            return None;
        }
        let j = if self.jitter_ms == 0 {
            0.0
        } else {
            (rand::random::<f64>() * 2.0 - 1.0) * self.jitter_ms as f64
        };
        let ms = (self.latency_ms as f64 + j).max(0.0);
        Some(Duration::from_micros((ms * 1000.0) as u64))
    }

    fn drops(&self) -> bool {
        self.loss_pct > 0.0 && rand::random::<f64>() * 100.0 < self.loss_pct
    }
}

#[derive(Default)]
struct State {
    global: Option<Params>,
    sessions: HashMap<SessionId, Params>,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);
static DELAYED_OUT: AtomicU64 = AtomicU64::new(0);
static DROPPED_OUT: AtomicU64 = AtomicU64::new(0);
static DELAYED_IN: AtomicU64 = AtomicU64::new(0);
static DROPPED_IN: AtomicU64 = AtomicU64::new(0);

fn state() -> &'static RwLock<State> {
    static S: OnceLock<RwLock<State>> = OnceLock::new();
    S.get_or_init(|| RwLock::new(State::default()))
}

fn refresh_active() {
    let s = state().read().unwrap();
    ACTIVE.store(
        s.global.is_some() || !s.sessions.is_empty(),
        Ordering::Relaxed,
    );
}

/// Apply `[dev] netsim` from the config at startup.
pub fn configure(cfg: &musubi_core::config::NetsimConfig) {
    ENABLED.store(cfg.enabled, Ordering::Relaxed);
    if !cfg.enabled {
        return;
    }
    let p = Params {
        latency_ms: cfg.latency_ms,
        jitter_ms: cfg.jitter_ms,
        loss_pct: cfg.loss_pct,
        direction: cfg.direction.clone(),
    };
    if p.validate().is_ok() {
        set_global(Some(p));
    }
}

pub fn set_global(p: Option<Params>) {
    state().write().unwrap().global = p;
    refresh_active();
}

pub fn set_session(sid: SessionId, p: Option<Params>) -> bool {
    let mut s = state().write().unwrap();
    let existed = match p {
        Some(p) => s.sessions.insert(sid, p).is_some(),
        None => s.sessions.remove(&sid).is_some(),
    };
    drop(s);
    refresh_active();
    existed
}

pub fn clear_sessions() -> usize {
    let n = {
        let mut s = state().write().unwrap();
        std::mem::take(&mut s.sessions).len()
    };
    refresh_active();
    n
}

pub fn reset_counters() {
    for c in [&DELAYED_OUT, &DROPPED_OUT, &DELAYED_IN, &DROPPED_IN] {
        c.store(0, Ordering::Relaxed);
    }
}

/// Effective parameters for a session (session override wins over the global profile).
pub fn effective(sid: SessionId) -> Option<Params> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    let s = state().read().unwrap();
    s.sessions.get(&sid).cloned().or_else(|| s.global.clone())
}

pub fn snapshot() -> Value {
    let s = state().read().unwrap();
    json!({
        "active": ACTIVE.load(Ordering::Relaxed),
        "global": s.global,
        "sessions": s.sessions.iter().map(|(k, v)| json!({ "session_id": k, "params": v })).collect::<Vec<_>>(),
        "counters": {
            "delayed_out": DELAYED_OUT.load(Ordering::Relaxed),
            "dropped_out": DROPPED_OUT.load(Ordering::Relaxed),
            "delayed_in": DELAYED_IN.load(Ordering::Relaxed),
            "dropped_in": DROPPED_IN.load(Ordering::Relaxed),
        }
    })
}

/// A connection with the simulated conditions applied around the real transport.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

enum Cmd {
    Send(tokio::time::Instant, Frame, bool),
    Many(tokio::time::Instant, Vec<Frame>),
    Close,
}

type Inbound = std::result::Result<(tokio::time::Instant, Frame), TransportError>;

/// A delay line in front of a connection: every frame gets a release time (`max(previous, now + delay)`) and a
/// pump task owning the real connection sends/receives on time, so order is kept and a 60 Hz stream is never
/// serialised behind one frame's delay (the old inline `sleep` did exactly that and starved the relay).
pub struct NetsimConn {
    sid: SessionId,
    kind: &'static str,
    out_tx: tokio::sync::mpsc::UnboundedSender<Cmd>,
    in_rx: tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    out_release: tokio::time::Instant,
}

impl NetsimConn {
    /// The wrapper only when the dev profile is configured or a profile is active; production sessions keep the
    /// bare connection.
    pub fn wrap(inner: Box<dyn Connection>, sid: SessionId) -> Box<dyn Connection> {
        if ENABLED.load(Ordering::Relaxed) || is_active() {
            Box::new(Self::new(inner, sid))
        } else {
            inner
        }
    }

    pub fn new(mut inner: Box<dyn Connection>, sid: SessionId) -> Self {
        let kind = inner.kind();
        let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Cmd>();
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        tokio::spawn(async move {
            let mut queue: std::collections::VecDeque<Cmd> = std::collections::VecDeque::new();
            let mut in_release = tokio::time::Instant::now();
            loop {
                let next = match queue.front() {
                    Some(Cmd::Send(d, ..)) | Some(Cmd::Many(d, _)) => Some(*d),
                    Some(Cmd::Close) => Some(tokio::time::Instant::now()),
                    None => None,
                };
                tokio::select! {
                    _ = async { match next { Some(d) => tokio::time::sleep_until(d).await, None => std::future::pending::<()>().await } } => {
                        let r = match queue.pop_front() {
                            Some(Cmd::Send(_, f, false)) => inner.send(f).await,
                            Some(Cmd::Send(_, f, true)) => inner.send_unreliable(f).await,
                            Some(Cmd::Many(_, fs)) => inner.send_many(fs).await,
                            Some(Cmd::Close) => { inner.close().await; break; }
                            None => Ok(()),
                        };
                        if r.is_err() { break; }
                    }
                    cmd = out_rx.recv() => match cmd {
                        Some(c) => queue.push_back(c),
                        None => { inner.close().await; break; }
                    },
                    r = inner.recv() => match r {
                        Ok(f) => {
                            let (drop_it, delay) = decide(sid, false, false);
                            if drop_it { continue; }
                            let now = tokio::time::Instant::now();
                            in_release = (now + delay.unwrap_or_default()).max(in_release);
                            if in_tx.send(Ok((in_release, f))).is_err() { break; }
                        }
                        Err(e) => { let _ = in_tx.send(Err(e)); break; }
                    },
                }
            }
        });
        Self {
            sid,
            kind,
            out_tx,
            in_rx,
            out_release: tokio::time::Instant::now(),
        }
    }

    fn schedule(&mut self, delay: Option<Duration>) -> tokio::time::Instant {
        let now = tokio::time::Instant::now();
        self.out_release = (now + delay.unwrap_or_default()).max(self.out_release);
        self.out_release
    }
}

/// `lossy`: the frame may be dropped — only unreliable outbound frames. The sim sits above the reliable
/// transport, so a dropped control frame would never be retransmitted, which no real network does.
fn decide(sid: SessionId, outbound: bool, lossy: bool) -> (bool, Option<Duration>) {
    let Some(p) = effective(sid) else {
        return (false, None);
    };
    if !p.applies(outbound) {
        return (false, None);
    }
    if lossy && p.drops() {
        if outbound {
            DROPPED_OUT.fetch_add(1, Ordering::Relaxed);
        } else {
            DROPPED_IN.fetch_add(1, Ordering::Relaxed);
        }
        return (true, None);
    }
    let d = p.delay();
    if d.is_some() {
        if outbound {
            DELAYED_OUT.fetch_add(1, Ordering::Relaxed);
        } else {
            DELAYED_IN.fetch_add(1, Ordering::Relaxed);
        }
    }
    (false, d)
}

type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

impl Connection for NetsimConn {
    fn send(&mut self, frame: Frame) -> BoxFut<'_, std::result::Result<(), TransportError>> {
        let (drop_it, delay) = decide(self.sid, true, false);
        if !drop_it {
            let at = self.schedule(delay);
            let _ = self.out_tx.send(Cmd::Send(at, frame, false));
        }
        Box::pin(async { Ok(()) })
    }

    fn send_unreliable(
        &mut self,
        frame: Frame,
    ) -> BoxFut<'_, std::result::Result<(), TransportError>> {
        let (drop_it, delay) = decide(self.sid, true, true);
        if !drop_it {
            let at = self.schedule(delay);
            let _ = self.out_tx.send(Cmd::Send(at, frame, true));
        }
        Box::pin(async { Ok(()) })
    }

    fn send_many(
        &mut self,
        frames: Vec<Frame>,
    ) -> BoxFut<'_, std::result::Result<(), TransportError>> {
        // one decision per frame, one delay per burst (frames of a burst share the wire)
        let mut kept = Vec::with_capacity(frames.len());
        let mut delay = None;
        for f in frames {
            let (drop_it, d) = decide(self.sid, true, false);
            if !drop_it {
                kept.push(f);
                if delay.is_none() {
                    delay = d;
                }
            }
        }
        if !kept.is_empty() {
            let at = self.schedule(delay);
            let _ = self.out_tx.send(Cmd::Many(at, kept));
        }
        Box::pin(async { Ok(()) })
    }

    fn recv(&mut self) -> BoxFut<'_, std::result::Result<Frame, TransportError>> {
        Box::pin(async move {
            match self.in_rx.recv().await {
                Some(Ok((at, f))) => {
                    tokio::time::sleep_until(at).await;
                    Ok(f)
                }
                Some(Err(e)) => Err(e),
                None => Err(TransportError::Closed),
            }
        })
    }

    fn close(&mut self) -> BoxFut<'_, ()> {
        let _ = self.out_tx.send(Cmd::Close);
        Box::pin(async {})
    }

    fn kind(&self) -> &'static str {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_validate_and_direction() {
        let mut p = Params::default();
        assert!(p.validate().is_ok());
        p.latency_ms = 200;
        p.jitter_ms = 300;
        assert!(p.validate().is_err());
        p.jitter_ms = 50;
        p.loss_pct = 101.0;
        assert!(p.validate().is_err());
        p.loss_pct = 100.0;
        p.direction = "sideways".into();
        assert!(p.validate().is_err());
        p.direction = "in".into();
        assert!(p.validate().is_ok());
        assert!(p.applies(false) && !p.applies(true));
        assert!(p.drops(), "100% loss always drops");
        let d = p.delay().unwrap();
        assert!(
            d >= Duration::from_millis(150) && d <= Duration::from_millis(250),
            "{d:?}"
        );
        p.latency_ms = 0;
        p.jitter_ms = 0;
        assert!(p.delay().is_none());
    }

    #[test]
    fn session_override_and_active_flag() {
        let sid = SessionId(uuid::Uuid::now_v7());
        let other = SessionId(uuid::Uuid::now_v7());
        set_global(None);
        clear_sessions();
        assert!(effective(sid).is_none());
        set_session(
            sid,
            Some(Params {
                latency_ms: 10,
                ..Default::default()
            }),
        );
        assert_eq!(effective(sid).unwrap().latency_ms, 10);
        assert!(effective(other).is_none());
        set_global(Some(Params {
            latency_ms: 5,
            ..Default::default()
        }));
        assert_eq!(effective(other).unwrap().latency_ms, 5);
        assert_eq!(
            effective(sid).unwrap().latency_ms,
            10,
            "session override wins"
        );
        assert!(set_session(sid, None));
        assert_eq!(effective(sid).unwrap().latency_ms, 5);
        set_global(None);
        assert!(effective(sid).is_none());
        assert_eq!(snapshot()["active"], false);
    }
}
