//! First line of volumetric defence for the UDP edge (Hathora "basic DDoS protection"): every
//! datagram is judged by source address before any parsing — deny/allow lists, a reactive
//! blocklist fed by strikes (malformed headers, unknown tokens), a per-source packet/byte budget,
//! a per-source budget for session-opening packets (token lookups are the expensive path) and a
//! global packet ceiling. Real floods still need the provider's scrubbing; this keeps one abusive
//! source or a reflection burst from starving the proxy and the game servers behind it.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dashmap::DashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct GuardConfig {
    pub enabled: bool,
    /// Sustained packets per second one source address may send (burst = 2 s worth).
    pub max_pps_per_source: u32,
    /// Sustained bytes per second per source address.
    pub max_bytes_per_source: u32,
    /// Session-opening packets (unknown source or new token) per source per second.
    pub max_opens_per_source: u32,
    /// Packets per second across all sources (0 = unlimited).
    pub global_max_pps: u32,
    /// Datagrams shorter than this (header included) are dropped unparsed.
    pub min_packet_bytes: usize,
    /// Strikes (malformed / unknown token / over budget) within `strike_window_secs` that block.
    pub strikes_to_block: u32,
    pub strike_window_secs: u64,
    pub block_secs: u64,
    /// CIDRs never limited or blocked (own game servers, QA), and CIDRs always dropped.
    pub allow: Vec<(IpAddr, u8)>,
    pub deny: Vec<(IpAddr, u8)>,
}

impl Default for GuardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_pps_per_source: 2000,
            max_bytes_per_source: 2_000_000,
            max_opens_per_source: 10,
            global_max_pps: 0,
            min_packet_bytes: 4,
            strikes_to_block: 20,
            strike_window_secs: 10,
            block_secs: 300,
            allow: Vec::new(),
            deny: Vec::new(),
        }
    }
}

/// `a.b.c.d/p` or `::/p` (no prefix = host).
pub fn parse_cidr(text: &str) -> Option<(IpAddr, u8)> {
    let (ip, len) = match text.trim().split_once('/') {
        Some((ip, len)) => (ip, Some(len)),
        None => (text.trim(), None),
    };
    let ip: IpAddr = ip.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    let len: u8 = match len {
        Some(l) => l.parse().ok()?,
        None => max,
    };
    (len <= max).then_some((ip, len))
}

fn cidr_contains((net, len): &(IpAddr, u8), ip: IpAddr) -> bool {
    let to_bits = |ip: IpAddr| -> (u128, bool) {
        match ip {
            IpAddr::V4(v4) => (u32::from(v4) as u128, true),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => (u32::from(v4) as u128, true),
                None => (u128::from(v6), false),
            },
        }
    };
    let (a, a4) = to_bits(*net);
    let (b, b4) = to_bits(ip);
    if a4 != b4 {
        return false;
    }
    let max = if a4 { 32 } else { 128 };
    if *len == 0 {
        return true;
    }
    let mask = (!0u128) << (max - *len as u32);
    (a & mask) == (b & mask)
}

/// Why a datagram was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Denied,
    Blocked,
    TooSmall,
    GlobalRate,
    SourceRate,
    SourceBytes,
    OpenRate,
}

impl Verdict {
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::Denied => "denied",
            Verdict::Blocked => "blocked",
            Verdict::TooSmall => "too_small",
            Verdict::GlobalRate => "global_rate",
            Verdict::SourceRate => "source_rate",
            Verdict::SourceBytes => "source_bytes",
            Verdict::OpenRate => "open_rate",
        }
    }
}

struct Bucket {
    /// Token buckets refilled continuously: packets, bytes, opens.
    pkts: f64,
    bytes: f64,
    opens: f64,
    refilled: Instant,
    strikes: u32,
    strike_window_start: Instant,
    blocked_until: Option<Instant>,
    blocks: u32,
    last_seen: Instant,
}

#[derive(Default)]
pub struct Counters {
    pub passed: AtomicU64,
    pub denied: AtomicU64,
    pub blocked: AtomicU64,
    pub too_small: AtomicU64,
    pub global_rate: AtomicU64,
    pub source_rate: AtomicU64,
    pub source_bytes: AtomicU64,
    pub open_rate: AtomicU64,
    pub strikes: AtomicU64,
    pub blocks_issued: AtomicU64,
    /// Packets admitted without a per-source bucket because the source table was full.
    pub untracked: AtomicU64,
}

/// Source addresses the guard tracks at once. UDP sources are forged for free, so without a
/// ceiling a spoofed flood grows one bucket per address until the next idle sweep (tens of
/// millions of entries in a minute). Past it a new source is admitted without its own bucket
/// (the global ceiling still applies) after an attempt to evict sources idle for a second.
pub const MAX_TRACKED_SOURCES: usize = 262_144;
const EMERGENCY_SWEEP_IDLE: Duration = Duration::from_secs(1);
/// Longest block (10 years): `Instant + Duration` panics on overflow, so operator-supplied or
/// escalated durations are clamped instead.
const MAX_BLOCK: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

fn block_duration(secs: u64) -> Duration {
    Duration::from_secs(secs).min(MAX_BLOCK)
}

pub struct Guard {
    pub cfg: GuardConfig,
    sources: DashMap<IpAddr, Bucket>,
    global: std::sync::Mutex<(f64, Instant)>,
    pub counters: Counters,
    max_sources: usize,
    /// Last time a full table was swept for idle sources (at most once a second).
    last_emergency_sweep: std::sync::Mutex<Instant>,
}

/// A blocked source as reported by `Guard::blocklist`.
#[derive(Debug, Clone, PartialEq)]
pub struct Blocked {
    pub ip: IpAddr,
    pub remaining_secs: u64,
    pub strikes: u32,
    pub blocks: u32,
}

impl Guard {
    pub fn new(cfg: GuardConfig) -> Self {
        let burst = cfg.global_max_pps as f64 * 2.0;
        Self {
            cfg,
            sources: DashMap::new(),
            global: std::sync::Mutex::new((burst, Instant::now())),
            counters: Counters::default(),
            max_sources: MAX_TRACKED_SOURCES,
            last_emergency_sweep: std::sync::Mutex::new(Instant::now()),
        }
    }

    /// Override [`MAX_TRACKED_SOURCES`] (tests, small boxes).
    pub fn with_max_sources(mut self, n: usize) -> Self {
        self.max_sources = n.max(1);
        self
    }

    /// Room for one more source bucket? A full table is swept for idle sources first (once a
    /// second at most); `false` = admit the packet unaccounted rather than allocate.
    fn can_track(&self, ip: IpAddr, now: Instant) -> bool {
        if self.sources.len() < self.max_sources || self.sources.contains_key(&ip) {
            return true;
        }
        let mut last = self
            .last_emergency_sweep
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if now.saturating_duration_since(*last) >= EMERGENCY_SWEEP_IDLE {
            *last = now;
            drop(last);
            self.sources.retain(|_, b| {
                b.blocked_until.is_some_and(|u| u > now)
                    || now.saturating_duration_since(b.last_seen) < EMERGENCY_SWEEP_IDLE
            });
            return self.sources.len() < self.max_sources;
        }
        false
    }

    pub fn allowed(&self, ip: IpAddr) -> bool {
        self.cfg.allow.iter().any(|c| cidr_contains(c, ip))
    }

    fn count(&self, v: Verdict) {
        let c = &self.counters;
        match v {
            Verdict::Pass => &c.passed,
            Verdict::Denied => &c.denied,
            Verdict::Blocked => &c.blocked,
            Verdict::TooSmall => &c.too_small,
            Verdict::GlobalRate => &c.global_rate,
            Verdict::SourceRate => &c.source_rate,
            Verdict::SourceBytes => &c.source_bytes,
            Verdict::OpenRate => &c.open_rate,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    /// Judge a datagram of `len` bytes from `from`; `opening` = the proxy has no session for this
    /// source (a token lookup would follow). Call before parsing anything.
    pub fn check(&self, from: SocketAddr, len: usize, opening: bool) -> Verdict {
        let v = self.check_inner(from.ip(), len, opening, Instant::now());
        self.count(v);
        v
    }

    fn check_inner(&self, ip: IpAddr, len: usize, opening: bool, now: Instant) -> Verdict {
        if !self.cfg.enabled {
            return Verdict::Pass;
        }
        if self.cfg.deny.iter().any(|c| cidr_contains(c, ip)) {
            return Verdict::Denied;
        }
        if self.allowed(ip) {
            return Verdict::Pass;
        }
        if len < self.cfg.min_packet_bytes {
            if self.can_track(ip, now) {
                self.strike_at(ip, now);
            }
            return Verdict::TooSmall;
        }
        if self.cfg.global_max_pps > 0 {
            let mut g = self.global.lock().unwrap_or_else(|e| e.into_inner());
            let cap = self.cfg.global_max_pps as f64 * 2.0;
            let elapsed = now.saturating_duration_since(g.1).as_secs_f64();
            g.0 = (g.0 + elapsed * self.cfg.global_max_pps as f64).min(cap);
            g.1 = now;
            if g.0 < 1.0 {
                return Verdict::GlobalRate;
            }
            g.0 -= 1.0;
        }
        if !self.can_track(ip, now) {
            self.counters.untracked.fetch_add(1, Ordering::Relaxed);
            return Verdict::Pass;
        }
        let cfg = &self.cfg;
        let mut b = self.sources.entry(ip).or_insert_with(|| Bucket {
            pkts: cfg.max_pps_per_source as f64 * 2.0,
            bytes: cfg.max_bytes_per_source as f64 * 2.0,
            opens: cfg.max_opens_per_source as f64 * 2.0,
            refilled: now,
            strikes: 0,
            strike_window_start: now,
            blocked_until: None,
            blocks: 0,
            last_seen: now,
        });
        b.last_seen = now;
        if let Some(until) = b.blocked_until {
            if now < until {
                return Verdict::Blocked;
            }
            b.blocked_until = None;
            b.strikes = 0;
        }
        let dt = now.saturating_duration_since(b.refilled).as_secs_f64();
        b.refilled = now;
        b.pkts =
            (b.pkts + dt * cfg.max_pps_per_source as f64).min(cfg.max_pps_per_source as f64 * 2.0);
        b.bytes = (b.bytes + dt * cfg.max_bytes_per_source as f64)
            .min(cfg.max_bytes_per_source as f64 * 2.0);
        b.opens = (b.opens + dt * cfg.max_opens_per_source as f64)
            .min(cfg.max_opens_per_source as f64 * 2.0);
        if b.pkts < 1.0 {
            Self::strike_bucket(&mut b, cfg, now, &self.counters);
            return Verdict::SourceRate;
        }
        if b.bytes < len as f64 {
            Self::strike_bucket(&mut b, cfg, now, &self.counters);
            return Verdict::SourceBytes;
        }
        if opening && b.opens < 1.0 {
            Self::strike_bucket(&mut b, cfg, now, &self.counters);
            return Verdict::OpenRate;
        }
        b.pkts -= 1.0;
        b.bytes -= len as f64;
        if opening {
            b.opens -= 1.0;
        }
        Verdict::Pass
    }

    /// The proxy found the packet malformed or its token unknown: counts toward a block.
    pub fn strike(&self, from: SocketAddr) {
        let now = Instant::now();
        if self.cfg.enabled && !self.allowed(from.ip()) && self.can_track(from.ip(), now) {
            self.strike_at(from.ip(), now);
        }
    }

    fn strike_at(&self, ip: IpAddr, now: Instant) {
        let cfg = &self.cfg;
        let mut b = self.sources.entry(ip).or_insert_with(|| Bucket {
            pkts: cfg.max_pps_per_source as f64 * 2.0,
            bytes: cfg.max_bytes_per_source as f64 * 2.0,
            opens: cfg.max_opens_per_source as f64 * 2.0,
            refilled: now,
            strikes: 0,
            strike_window_start: now,
            blocked_until: None,
            blocks: 0,
            last_seen: now,
        });
        Self::strike_bucket(&mut b, cfg, now, &self.counters);
    }

    fn strike_bucket(b: &mut Bucket, cfg: &GuardConfig, now: Instant, c: &Counters) {
        if now
            .saturating_duration_since(b.strike_window_start)
            .as_secs()
            >= cfg.strike_window_secs
        {
            b.strike_window_start = now;
            b.strikes = 0;
        }
        b.strikes += 1;
        c.strikes.fetch_add(1, Ordering::Relaxed);
        if cfg.strikes_to_block > 0
            && b.strikes >= cfg.strikes_to_block
            && b.blocked_until.is_none()
        {
            // repeat offenders stay out longer: 1×, 2×, 4× … up to 16× block_secs
            let factor = 1u64 << b.blocks.min(4);
            b.blocked_until = Some(now + block_duration(cfg.block_secs.saturating_mul(factor)));
            b.blocks += 1;
            c.blocks_issued.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Operator block (`block_secs` × 1) / unblock.
    pub fn block(&self, ip: IpAddr, secs: u64) {
        let now = Instant::now();
        self.strike_at(ip, now);
        if let Some(mut b) = self.sources.get_mut(&ip) {
            b.blocked_until = Some(now + block_duration(secs));
            b.blocks = b.blocks.max(1);
        }
    }

    pub fn unblock(&self, ip: IpAddr) -> bool {
        match self.sources.get_mut(&ip) {
            Some(mut b) if b.blocked_until.is_some() => {
                b.blocked_until = None;
                b.strikes = 0;
                true
            }
            _ => false,
        }
    }

    pub fn blocklist(&self) -> Vec<Blocked> {
        let now = Instant::now();
        let mut out: Vec<Blocked> = self
            .sources
            .iter()
            .filter_map(|e| {
                let until = e.value().blocked_until?;
                (until > now).then(|| Blocked {
                    ip: *e.key(),
                    remaining_secs: (until - now).as_secs(),
                    strikes: e.value().strikes,
                    blocks: e.value().blocks,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            b.remaining_secs
                .cmp(&a.remaining_secs)
                .then(a.ip.cmp(&b.ip))
        });
        out
    }

    pub fn tracked_sources(&self) -> usize {
        self.sources.len()
    }

    /// Forget sources silent for `idle` that are not blocked.
    pub fn sweep(&self, idle: Duration) {
        let now = Instant::now();
        self.sources.retain(|_, b| {
            b.blocked_until.is_some_and(|u| u > now)
                || now.saturating_duration_since(b.last_seen) < idle
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GuardConfig {
        GuardConfig {
            max_pps_per_source: 10,
            max_bytes_per_source: 1000,
            max_opens_per_source: 2,
            strikes_to_block: 3,
            block_secs: 60,
            min_packet_bytes: 4,
            ..Default::default()
        }
    }

    fn src(n: u8) -> SocketAddr {
        SocketAddr::from(([203, 0, 113, n], 4000))
    }

    #[test]
    fn per_source_budget_then_strikes_then_block_and_unblock() {
        let g = Guard::new(cfg());
        let now = Instant::now();
        // burst = 2 s of budget = 20 packets
        for _ in 0..20 {
            assert_eq!(g.check_inner(src(1).ip(), 10, false, now), Verdict::Pass);
        }
        assert_eq!(
            g.check_inner(src(1).ip(), 10, false, now),
            Verdict::SourceRate
        );
        assert_eq!(
            g.check_inner(src(1).ip(), 10, false, now),
            Verdict::SourceRate
        );
        // third strike blocks
        assert_eq!(
            g.check_inner(src(1).ip(), 10, false, now),
            Verdict::SourceRate
        );
        assert_eq!(g.check_inner(src(1).ip(), 10, false, now), Verdict::Blocked);
        let bl = g.blocklist();
        assert_eq!(bl.len(), 1);
        assert_eq!(bl[0].ip, src(1).ip());
        assert!(bl[0].remaining_secs >= 58 && bl[0].remaining_secs <= 60);
        // other sources unaffected
        assert_eq!(g.check_inner(src(2).ip(), 10, false, now), Verdict::Pass);
        // refill after a second: still blocked
        let later = now + Duration::from_secs(1);
        assert_eq!(
            g.check_inner(src(1).ip(), 10, false, later),
            Verdict::Blocked
        );
        assert!(g.unblock(src(1).ip()));
        assert!(!g.unblock(src(1).ip()));
        assert_eq!(g.check_inner(src(1).ip(), 10, false, later), Verdict::Pass);
        // after the block expires on its own
        let g2 = Guard::new(cfg());
        g2.block(src(5).ip(), 1);
        assert_eq!(
            g2.check_inner(src(5).ip(), 10, false, Instant::now()),
            Verdict::Blocked
        );
        assert_eq!(
            g2.check_inner(
                src(5).ip(),
                10,
                false,
                Instant::now() + Duration::from_secs(2)
            ),
            Verdict::Pass
        );
        assert_eq!(g.counters.strikes.load(Ordering::Relaxed), 3);
        assert_eq!(g.counters.blocks_issued.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn bytes_opens_size_deny_allow_and_global() {
        let mut c = cfg();
        c.deny = vec![parse_cidr("198.51.100.0/24").unwrap()];
        c.allow = vec![parse_cidr("203.0.113.9").unwrap()];
        c.global_max_pps = 5;
        let g = Guard::new(c);
        let now = Instant::now();
        assert_eq!(
            g.check_inner("198.51.100.7".parse().unwrap(), 100, false, now),
            Verdict::Denied
        );
        // allowed sources skip every budget
        for _ in 0..100 {
            assert_eq!(g.check_inner(src(9).ip(), 5000, true, now), Verdict::Pass);
        }
        // global ceiling: burst 10
        for _ in 0..10 {
            assert_eq!(g.check_inner(src(3).ip(), 10, false, now), Verdict::Pass);
        }
        assert_eq!(
            g.check_inner(src(4).ip(), 10, false, now),
            Verdict::GlobalRate
        );
        let g = Guard::new(cfg());
        // too small strikes without consuming budget
        assert_eq!(g.check_inner(src(1).ip(), 3, false, now), Verdict::TooSmall);
        // byte budget: 2000 burst
        assert_eq!(g.check_inner(src(2).ip(), 1500, false, now), Verdict::Pass);
        assert_eq!(
            g.check_inner(src(2).ip(), 600, false, now),
            Verdict::SourceBytes
        );
        // opens: 4 burst, then refused while plain packets still pass
        for _ in 0..4 {
            assert_eq!(g.check_inner(src(6).ip(), 10, true, now), Verdict::Pass);
        }
        assert_eq!(g.check_inner(src(6).ip(), 10, true, now), Verdict::OpenRate);
        assert_eq!(g.check_inner(src(6).ip(), 10, false, now), Verdict::Pass);
        // disabled guard passes everything
        let off = Guard::new(GuardConfig {
            enabled: false,
            ..cfg()
        });
        for _ in 0..1000 {
            assert_eq!(off.check_inner(src(1).ip(), 1, true, now), Verdict::Pass);
        }
        assert!(parse_cidr("10.0.0.0/33").is_none());
        assert!(parse_cidr("2001:db8::/129").is_none());
        assert!(parse_cidr("2001:db8::/32").is_some());
        assert_eq!(g.tracked_sources(), 3);
        g.sweep(Duration::from_secs(0));
        assert_eq!(
            g.tracked_sources(),
            0,
            "idle unblocked sources are forgotten"
        );
    }

    /// A spoofed flood of distinct sources stops allocating buckets at the ceiling; packets from
    /// new sources still pass (the global budget covers them) and the table shrinks again once
    /// the sources go idle.
    #[test]
    fn source_table_is_bounded() {
        let g = Guard::new(cfg()).with_max_sources(8);
        let now = Instant::now();
        for n in 0..200u32 {
            let ip = IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + n));
            assert_eq!(g.check_inner(ip, 10, true, now), Verdict::Pass);
        }
        assert_eq!(g.tracked_sources(), 8);
        assert_eq!(g.counters.untracked.load(Ordering::Relaxed), 192);
        // strikes from untracked sources do not allocate either
        g.strike(SocketAddr::from(([10, 9, 9, 9], 1)));
        assert_eq!(g.tracked_sources(), 8);
        // a second later the idle entries are evicted to make room for a new source
        let later = now + Duration::from_secs(2);
        let fresh = IpAddr::from([10, 1, 2, 3]);
        assert_eq!(g.check_inner(fresh, 10, true, later), Verdict::Pass);
        assert_eq!(g.tracked_sources(), 1);
        assert_eq!(g.counters.untracked.load(Ordering::Relaxed), 192);
        // block durations never overflow for absurd configs
        let huge = Guard::new(GuardConfig {
            block_secs: u64::MAX,
            strikes_to_block: 1,
            ..cfg()
        });
        huge.strike(SocketAddr::from(([10, 0, 0, 1], 1)));
        assert_eq!(
            huge.check_inner(IpAddr::from([10, 0, 0, 1]), 10, false, Instant::now()),
            Verdict::Blocked
        );
    }
}
