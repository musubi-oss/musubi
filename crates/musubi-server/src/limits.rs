//! Rate limiting: in-process token buckets keyed by scope + subject. Per node, not cluster-wide
//! — a client that hits several nodes gets N× the budget, which is the usual trade-off for
//! keeping the hot path off the network.

use std::time::Instant;

use dashmap::DashMap;
use musubi_core::config::LimitsConfig;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Bus topic: an account's limit override changed somewhere in the cluster.
pub const LIMITS_TOPIC: &str = "admin.limits";

/// Per-account override of the `[limits]` defaults plus a daily request budget
/// (`None` = default, `daily` 0/None = unlimited).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct AccountLimit {
    #[serde(default)]
    pub http_per_second: Option<u32>,
    #[serde(default)]
    pub http_burst: Option<u32>,
    #[serde(default)]
    pub rpc_per_second: Option<u32>,
    #[serde(default)]
    pub rpc_burst: Option<u32>,
    #[serde(default)]
    pub daily: Option<u64>,
}

impl AccountLimit {
    pub fn is_empty(&self) -> bool {
        *self == AccountLimit::default()
    }
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}

pub struct RateLimiter {
    overrides: DashMap<Uuid, AccountLimit>,
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
            overrides: DashMap::new(),
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

    /// Authenticated HTTP requests per account: the `[limits]` defaults unless the account has
    /// an override, plus its daily budget when it has one.
    pub fn account(&self, id: AccountId) -> Result<()> {
        let o = self.override_of(id.0).unwrap_or_default();
        self.check(
            "http",
            &id.0.to_string(),
            o.http_per_second
                .unwrap_or(self.cfg.http_per_second_per_account) as f64,
            o.http_burst.unwrap_or(self.cfg.http_burst) as f64,
        )
        .map_err(|r| Self::limited("http", r))?;
        self.daily("quota", &id.0.to_string(), o.daily.unwrap_or(0))
    }

    /// Runtime RPCs per account (on top of the HTTP budget).
    pub fn rpc(&self, id: AccountId) -> Result<()> {
        let o = self.override_of(id.0).unwrap_or_default();
        self.check(
            "rpc",
            &id.0.to_string(),
            o.rpc_per_second
                .unwrap_or(self.cfg.rpc_per_second_per_account) as f64,
            o.rpc_burst.unwrap_or(self.cfg.rpc_burst) as f64,
        )
        .map_err(|r| Self::limited("rpc", r))
    }

    /// Requests of one server key: its own `KeyLimit` or the `[limits] key_*` defaults
    /// (0 = unlimited). The bootstrap config key is never limited.
    pub fn key(&self, caller: &crate::keys::Caller) -> Result<()> {
        if caller.id.is_nil() {
            return Ok(());
        }
        let l = caller.limit.unwrap_or_default();
        let per_sec = l.per_second.unwrap_or(self.cfg.key_per_second);
        let burst = l.burst.unwrap_or(self.cfg.key_burst).max(per_sec);
        let key = caller.id.to_string();
        if per_sec > 0 {
            self.check("key", &key, per_sec as f64, burst as f64)
                .map_err(|r| Self::limited("key", r))?;
        }
        self.daily("key_quota", &key, l.daily.unwrap_or(self.cfg.key_daily))
    }

    /// A daily budget as a slow bucket: `daily` tokens that refill over 24 h, so a subject can
    /// spend its whole day's budget at once and then waits for it to come back gradually.
    /// Cluster-wide like every other bucket when `cluster_wide` is on.
    fn daily(&self, scope: &str, key: &str, daily: u64) -> Result<()> {
        if daily == 0 {
            return Ok(());
        }
        self.check(scope, key, daily as f64 / 86_400.0, daily as f64)
            .map_err(|r| {
                Error::ResourceExhausted(format!(
                    "daily budget of {daily} requests spent ({scope}); next in {:.0}s",
                    r.max(1.0)
                ))
            })
    }

    pub fn override_of(&self, id: Uuid) -> Option<AccountLimit> {
        self.overrides.get(&id).map(|o| *o)
    }

    /// Load every account override from the database (startup and `admin.limits`).
    pub async fn load_overrides(&self, db: &PgPool) -> Result<usize> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(Uuid, Option<i32>, Option<i32>, Option<i32>, Option<i32>, Option<i64>)> =
            sqlx::query_as(
                "select account_id, http_per_second, http_burst, rpc_per_second, rpc_burst, daily from musubi.account_limits",
            )
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
        self.overrides.clear();
        for (id, hp, hb, rp, rb, d) in &rows {
            self.overrides.insert(
                *id,
                AccountLimit {
                    http_per_second: hp.map(|v| v.max(0) as u32),
                    http_burst: hb.map(|v| v.max(0) as u32),
                    rpc_per_second: rp.map(|v| v.max(0) as u32),
                    rpc_burst: rb.map(|v| v.max(0) as u32),
                    daily: d.map(|v| v.max(0) as u64),
                },
            );
        }
        Ok(rows.len())
    }

    pub async fn set_override(
        &self,
        db: &PgPool,
        id: Uuid,
        l: AccountLimit,
        note: &str,
    ) -> Result<()> {
        sqlx::query(
            "insert into musubi.account_limits (account_id, http_per_second, http_burst, rpc_per_second, rpc_burst, daily, note, updated_at)
             values ($1, $2, $3, $4, $5, $6, $7, now())
             on conflict (account_id) do update set http_per_second = $2, http_burst = $3, rpc_per_second = $4, rpc_burst = $5, daily = $6, note = $7, updated_at = now()",
        )
        .bind(id)
        .bind(l.http_per_second.map(|v| v as i32))
        .bind(l.http_burst.map(|v| v as i32))
        .bind(l.rpc_per_second.map(|v| v as i32))
        .bind(l.rpc_burst.map(|v| v as i32))
        .bind(l.daily.map(|v| v as i64))
        .bind(note)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.overrides.insert(id, l);
        Ok(())
    }

    pub async fn clear_override(&self, db: &PgPool, id: Uuid) -> Result<bool> {
        let r = sqlx::query("delete from musubi.account_limits where account_id = $1")
            .bind(id)
            .execute(db)
            .await
            .map_err(Error::internal)?;
        self.overrides.remove(&id);
        Ok(r.rows_affected() > 0)
    }

    /// Reload overrides on `admin.limits` from any node.
    pub fn start(
        self: &std::sync::Arc<Self>,
        cluster: &std::sync::Arc<musubi_cluster::Cluster>,
        db: PgPool,
    ) {
        let me = self.clone();
        let mut rx = cluster.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) if m.topic == LIMITS_TOPIC => {
                        if let Err(e) = me.load_overrides(&db).await {
                            tracing::warn!(error = %e, "account limits reload failed");
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
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
