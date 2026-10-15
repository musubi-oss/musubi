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
}

/// Above this many live buckets a sweep drops the ones idle for a minute.
const SWEEP_AT: usize = 100_000;

impl RateLimiter {
    pub fn new(cfg: &LimitsConfig) -> Self {
        Self {
            cfg: cfg.clone(),
            buckets: DashMap::new(),
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
