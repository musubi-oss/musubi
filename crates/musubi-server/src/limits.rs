//! Rate limiting: in-process token buckets keyed by scope + subject. Per node, not cluster-wide
//! — a client that hits several nodes gets N× the budget, which is the usual trade-off for
//! keeping the hot path off the network.

use std::time::Instant;

use dashmap::DashMap;
use musubi_core::config::LimitsConfig;
use musubi_core::{AccountId, Error, Result};

struct Bucket {
    tokens: f64,
    updated: Instant,
}

pub struct RateLimiter {
    cfg: LimitsConfig,
    buckets: DashMap<String, Bucket>,
    /// Tokens consumed locally since the last gossip flush, per `scope:key`.
    pending: DashMap<String, u64>,
}

/// Bus topic carrying `{ "scope:key": consumed }` maps between nodes.
pub const USAGE_TOPIC: &str = "limits.usage";
pub const GOSSIP_EVERY: std::time::Duration = std::time::Duration::from_secs(1);

/// Above this many live buckets a sweep drops the ones idle for a minute.
const SWEEP_AT: usize = 100_000;

impl RateLimiter {
    pub fn new(cfg: &LimitsConfig) -> Self {
        Self {
            cfg: cfg.clone(),
            buckets: DashMap::new(),
            pending: DashMap::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    /// Take one token from `scope:key` (refilling at `per_sec`, capped at `burst`).
    /// `Err(seconds)` tells how long until the next token.
    pub fn check(
        &self,
        scope: &str,
        key: &str,
        per_sec: f64,
        burst: f64,
    ) -> std::result::Result<(), f64> {
        if !self.cfg.enabled || per_sec <= 0.0 {
            return Ok(());
        }
        let now = Instant::now();
        let burst = burst.max(1.0);
        let mut b = self
            .buckets
            .entry(format!("{scope}:{key}"))
            .or_insert_with(|| Bucket {
                tokens: burst,
                updated: now,
            });
        let elapsed = now.duration_since(b.updated).as_secs_f64();
        b.tokens = (b.tokens + elapsed * per_sec).min(burst);
        b.updated = now;
        let r = if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            if self.cfg.cluster_wide {
                *self.pending.entry(format!("{scope}:{key}")).or_insert(0) += 1;
            }
            Ok(())
        } else {
            metrics::counter!("musubi_ratelimited_total", "scope" => scope.to_string())
                .increment(1);
            Err((1.0 - b.tokens) / per_sec)
        };
        drop(b);
        if self.buckets.len() > SWEEP_AT {
            self.buckets
                .retain(|_, b| now.duration_since(b.updated).as_secs() < 60);
        }
        r
    }

    /// Drain the locally consumed counts for gossiping to peers.
    pub fn flush(&self) -> std::collections::HashMap<String, u64> {
        let mut out = std::collections::HashMap::new();
        self.pending.retain(|k, v| {
            out.insert(k.clone(), *v);
            false
        });
        out
    }

    /// Debit what a peer consumed: every node's bucket for a key drains by the cluster-wide
    /// consumption, so the summed rate converges to `per_sec` regardless of node count.
    /// Buckets may go negative down to `-burst`, making heavy global use wait longer.
    pub fn apply_remote(&self, usage: &std::collections::HashMap<String, u64>) {
        if !self.cfg.enabled || !self.cfg.cluster_wide {
            return;
        }
        let now = Instant::now();
        for (k, n) in usage {
            let Some((scope, _)) = k.split_once(':') else {
                continue;
            };
            let (per_sec, burst) = match scope {
                "auth" => {
                    let m = self.cfg.auth_per_minute_per_ip as f64;
                    (m / 60.0, m)
                }
                "http" => (
                    self.cfg.http_per_second_per_account as f64,
                    self.cfg.http_burst as f64,
                ),
                "rpc" => (
                    self.cfg.rpc_per_second_per_account as f64,
                    self.cfg.rpc_burst as f64,
                ),
                "socket" => (
                    self.cfg.socket_per_second as f64,
                    self.cfg.socket_burst as f64,
                ),
                _ => continue,
            };
            if per_sec <= 0.0 {
                continue;
            }
            let burst = burst.max(1.0);
            let mut b = self.buckets.entry(k.clone()).or_insert_with(|| Bucket {
                tokens: burst,
                updated: now,
            });
            let elapsed = now.duration_since(b.updated).as_secs_f64();
            b.tokens = (b.tokens + elapsed * per_sec).min(burst);
            b.updated = now;
            b.tokens = (b.tokens - *n as f64).max(-burst);
        }
    }

    /// Gossip consumption over the cluster bus: flush every second, debit what peers report.
    pub fn start_gossip(
        self: &std::sync::Arc<Self>,
        cluster: std::sync::Arc<musubi_cluster::Cluster>,
    ) {
        if !self.cfg.enabled || !self.cfg.cluster_wide {
            return;
        }
        let me = self.clone();
        let c = cluster.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(GOSSIP_EVERY);
            loop {
                tick.tick().await;
                let usage = me.flush();
                if usage.is_empty() {
                    continue;
                }
                if let Err(e) = c.publish(USAGE_TOPIC, &usage).await {
                    tracing::debug!(error = %e, "limits gossip publish failed");
                }
            }
        });
        let me = self.clone();
        let mut rx = cluster.subscribe();
        let self_id = cluster.node_id();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) if m.topic == USAGE_TOPIC && m.origin != self_id => {
                        if let Ok(u) = serde_json::from_value::<
                            std::collections::HashMap<String, u64>,
                        >(m.payload)
                        {
                            me.apply_remote(&u);
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }

    fn limited(scope: &str, retry: f64) -> Error {
        Error::ResourceExhausted(format!(
            "rate limited ({scope}); retry in {:.1}s",
            retry.max(0.1)
        ))
    }

    /// Login / refresh attempts per client IP.
    pub fn auth_ip(&self, ip: &str) -> Result<()> {
        let per_min = self.cfg.auth_per_minute_per_ip as f64;
        self.check("auth", ip, per_min / 60.0, per_min)
            .map_err(|r| Self::limited("auth", r))
    }

    /// Authenticated HTTP requests per account.
    pub fn account(&self, id: AccountId) -> Result<()> {
        self.check(
            "http",
            &id.0.to_string(),
            self.cfg.http_per_second_per_account as f64,
            self.cfg.http_burst as f64,
        )
        .map_err(|r| Self::limited("http", r))
    }

    /// Runtime RPCs per account (on top of the HTTP budget).
    pub fn rpc(&self, id: AccountId) -> Result<()> {
        self.check(
            "rpc",
            &id.0.to_string(),
            self.cfg.rpc_per_second_per_account as f64,
            self.cfg.rpc_burst as f64,
        )
        .map_err(|r| Self::limited("rpc", r))
    }

    /// Inbound realtime messages per account.
    pub fn socket(&self, id: AccountId) -> Result<()> {
        self.check(
            "socket",
            &id.0.to_string(),
            self.cfg.socket_per_second as f64,
            self.cfg.socket_burst as f64,
        )
        .map_err(|r| Self::limited("socket", r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_drains_and_refills() {
        let cfg = LimitsConfig {
            enabled: true,
            ..Default::default()
        };
        let rl = RateLimiter::new(&cfg);
        for _ in 0..3 {
            assert!(rl.check("t", "k", 10.0, 3.0).is_ok());
        }
        let retry = rl.check("t", "k", 10.0, 3.0).unwrap_err();
        assert!(retry > 0.0 && retry <= 0.1, "{retry}");
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(rl.check("t", "k", 10.0, 3.0).is_ok());
        assert!(
            rl.check("t", "other", 10.0, 3.0).is_ok(),
            "keys are independent"
        );
        let off = RateLimiter::new(&LimitsConfig {
            enabled: false,
            ..Default::default()
        });
        for _ in 0..1000 {
            assert!(off.check("t", "k", 1.0, 1.0).is_ok());
        }
    }

    #[test]
    fn scoped_helpers_use_their_budgets() {
        let cfg = LimitsConfig {
            enabled: true,
            auth_per_minute_per_ip: 2,
            rpc_per_second_per_account: 1,
            rpc_burst: 1,
            ..Default::default()
        };
        let rl = RateLimiter::new(&cfg);
        assert!(rl.auth_ip("1.2.3.4").is_ok());
        assert!(rl.auth_ip("1.2.3.4").is_ok());
        let e = rl.auth_ip("1.2.3.4").unwrap_err();
        assert!(matches!(e, Error::ResourceExhausted(_)), "{e:?}");
        assert!(rl.auth_ip("5.6.7.8").is_ok());
        let acc = AccountId(uuid::Uuid::now_v7());
        assert!(rl.rpc(acc).is_ok());
        assert!(rl.rpc(acc).is_err());
        assert!(rl.account(acc).is_ok(), "http budget is separate from rpc");
    }
}

#[cfg(test)]
mod cluster_tests {
    use super::*;

    fn cfg() -> LimitsConfig {
        LimitsConfig {
            enabled: true,
            http_per_second_per_account: 10,
            http_burst: 10,
            cluster_wide: true,
            ..LimitsConfig::default()
        }
    }

    #[test]
    fn remote_usage_drains_local_bucket() {
        let a = RateLimiter::new(&cfg());
        let b = RateLimiter::new(&cfg());
        let id = AccountId(uuid::Uuid::now_v7());
        for _ in 0..10 {
            a.account(id).unwrap();
        }
        assert!(a.account(id).is_err(), "local burst exhausted");
        let usage = a.flush();
        assert_eq!(usage.get(&format!("http:{}", id.0)), Some(&10));
        assert!(a.flush().is_empty(), "flush drains");
        assert!(b.account(id).is_ok(), "b untouched before gossip");
        b.apply_remote(&usage);
        assert!(b.account(id).is_err(), "cluster-wide budget spent on a");
        // negative floor: a second remote debit does not push below -burst
        b.apply_remote(&usage);
        let t = b.buckets.get(&format!("http:{}", id.0)).unwrap().tokens;
        assert!((-10.0..0.0).contains(&t), "{t}");
    }

    #[test]
    fn remote_usage_ignored_when_not_cluster_wide() {
        let b = RateLimiter::new(&LimitsConfig {
            cluster_wide: false,
            ..cfg()
        });
        let id = AccountId(uuid::Uuid::now_v7());
        let mut u = std::collections::HashMap::new();
        u.insert(format!("http:{}", id.0), 100u64);
        b.apply_remote(&u);
        assert!(b.account(id).is_ok());
        for _ in 0..9 {
            let _ = b.account(id);
        }
        assert!(b.flush().is_empty(), "nothing is gossiped");
    }
}
