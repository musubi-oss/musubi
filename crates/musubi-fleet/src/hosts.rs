//! Host pool for the `ssh` provider (P1-11). Hosts used to be a static `[fleet.ssh] hosts` list;
//! now they live in `musubi.fleet_hosts` so operators can register bare-metal boxes at runtime,
//! drain them, relay spot/preemption notices, and capacity pools (`capacity.rs`) can provision
//! and tear machines down on demand. Static config hosts are seeded into the table at start.
//!
//! Host states: `provisioning` (machine ordered, waiting for ssh) → `ready` (takes starts) →
//! `draining` (no new starts, existing servers finish) / `terminating` (preempted: servers get a
//! deadline) → `gone` (deprovisioned / deleted; kept for history) or `failed` (never answered).

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};

use musubi_core::config::SshFleetConfig;
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const STATES: &[&str] = &[
    "provisioning",
    "ready",
    "draining",
    "terminating",
    "failed",
    "gone",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub id: Uuid,
    pub name: String,
    /// Address `ssh` connects to.
    pub addr: String,
    /// Address players connect to (defaults to `addr`).
    pub public_addr: Option<String>,
    pub user: String,
    pub port: i32,
    pub key: String,
    /// Servers this host runs at once.
    pub capacity: i32,
    pub region: String,
    /// Hardware class (`ax41`, `c5.large`, `spot-m6i`…) — fleets may pin `spec.host_class`.
    pub class: String,
    pub labels: serde_json::Value,
    pub state: String,
    /// `static` (config), `manual` (API), `provisioned` (capacity pool).
    pub source: String,
    /// Capacity pool that owns the host (empty for static/manual hosts).
    pub pool: String,
    /// Cloud-side id (Hetzner server id, webhook handle) for deprovisioning.
    pub external_id: Option<String>,
    pub spot: bool,
    pub cost_per_hour: f64,
    #[serde(with = "time::serde::rfc3339::option")]
    pub termination_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ready_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_probe_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

impl Host {
    pub fn public(&self) -> String {
        self.public_addr
            .clone()
            .unwrap_or_else(|| self.addr.clone())
    }

    /// Does this host take new starts for a fleet in `region` with these spec pins?
    pub fn accepts(
        &self,
        region: &str,
        class: Option<&str>,
        labels: &serde_json::Map<String, serde_json::Value>,
    ) -> bool {
        if self.state != "ready" {
            return false;
        }
        if !region.is_empty() && !self.region.is_empty() && self.region != region {
            return false;
        }
        if let Some(c) = class {
            if !c.is_empty() && self.class != c {
                return false;
            }
        }
        labels.iter().all(|(k, v)| self.labels.get(k) == Some(v))
    }
}

/// `POST /v1/server/fleets/hosts` / a provisioned machine.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NewHost {
    pub name: String,
    pub addr: String,
    pub public_addr: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub key: Option<String>,
    pub capacity: Option<u32>,
    pub region: String,
    pub class: String,
    pub labels: serde_json::Value,
    pub spot: bool,
    pub cost_per_hour: f64,
    /// `ready` (default, host already set up) or `provisioning` (probe first).
    pub state: Option<String>,
    pub source: Option<String>,
    pub pool: Option<String>,
    pub external_id: Option<String>,
}

/// `PATCH /v1/server/fleets/hosts/{id}`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PatchHost {
    pub name: Option<String>,
    pub public_addr: Option<String>,
    pub capacity: Option<u32>,
    pub class: Option<String>,
    pub region: Option<String>,
    pub labels: Option<serde_json::Value>,
    pub cost_per_hour: Option<f64>,
    pub spot: Option<bool>,
    /// `ready` (undrain) or `draining`.
    pub state: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HostFilter {
    pub state: Option<String>,
    pub region: Option<String>,
    pub class: Option<String>,
    pub pool: Option<String>,
    pub source: Option<String>,
    pub include_gone: bool,
}

#[derive(sqlx::FromRow)]
struct HostRow {
    id: Uuid,
    name: String,
    addr: String,
    public_addr: Option<String>,
    ssh_user: String,
    ssh_port: i32,
    ssh_key: String,
    capacity: i32,
    region: String,
    class: String,
    labels: serde_json::Value,
    state: String,
    source: String,
    pool: String,
    external_id: Option<String>,
    spot: bool,
    cost_per_hour: f64,
    termination_at: Option<OffsetDateTime>,
    ready_at: Option<OffsetDateTime>,
    last_probe_at: Option<OffsetDateTime>,
    last_error: Option<String>,
    created_at: OffsetDateTime,
}

const COLS: &str = "id, name, addr, public_addr, ssh_user, ssh_port, ssh_key, capacity, region, class, labels, state, source, pool, external_id, spot, cost_per_hour, termination_at, ready_at, last_probe_at, last_error, created_at";

fn row(r: HostRow) -> Host {
    Host {
        id: r.id,
        name: r.name,
        addr: r.addr,
        public_addr: r.public_addr,
        user: r.ssh_user,
        port: r.ssh_port,
        key: r.ssh_key,
        capacity: r.capacity,
        region: r.region,
        class: r.class,
        labels: r.labels,
        state: r.state,
        source: r.source,
        pool: r.pool,
        external_id: r.external_id,
        spot: r.spot,
        cost_per_hour: r.cost_per_hour,
        termination_at: r.termination_at,
        ready_at: r.ready_at,
        last_probe_at: r.last_probe_at,
        last_error: r.last_error,
        created_at: r.created_at,
    }
}

fn validate_addr(a: &str) -> Result<()> {
    if a.is_empty()
        || a.len() > 253
        || a.contains(char::is_whitespace)
        || a.contains('\'')
        || a.contains('@')
    {
        return Err(Error::InvalidArgument("addr: host name or ip".into()));
    }
    Ok(())
}

fn validate_label_map(v: &serde_json::Value) -> Result<()> {
    match v.as_object() {
        Some(m) if m.len() <= 32 && m.values().all(|v| v.is_string() || v.is_number()) => Ok(()),
        _ => Err(Error::InvalidArgument(
            "labels: object of up to 32 string/number values".into(),
        )),
    }
}

/// Deterministic id for a config host (sha256 of the address, uuid-shaped).
fn stable_id(addr: &str) -> Uuid {
    let d = ring::digest::digest(
        &ring::digest::SHA256,
        format!("musubi-host:{addr}").as_bytes(),
    );
    let mut b = [0u8; 16];
    b.copy_from_slice(&d.as_ref()[..16]);
    Uuid::from_bytes(b)
}

/// Static hosts (no database) keep working for unit tests and tiny setups: ids are derived from
/// the address so they are stable across restarts.
fn static_hosts(cfg: &SshFleetConfig) -> Vec<Host> {
    cfg.hosts
        .iter()
        .map(|h| Host {
            id: stable_id(&h.addr),
            name: h.addr.clone(),
            addr: h.addr.clone(),
            public_addr: h.public_addr.clone(),
            user: h.user.clone(),
            port: h.port as i32,
            key: h.key.clone(),
            capacity: h.capacity as i32,
            region: String::new(),
            class: String::new(),
            labels: serde_json::json!({}),
            state: "ready".into(),
            source: "static".into(),
            pool: String::new(),
            external_id: None,
            spot: false,
            cost_per_hour: 0.0,
            termination_at: None,
            ready_at: None,
            last_probe_at: None,
            last_error: None,
            created_at: OffsetDateTime::now_utc(),
        })
        .collect()
}

/// Hosts the `ssh` provider may start servers on. The cache is what `pick` reads (sync, hot
/// path); every mutation refreshes it from the database.
pub struct HostRegistry {
    db: Option<PgPool>,
    ssh: SshFleetConfig,
    cache: RwLock<Vec<Host>>,
    /// `(region, class)` → starts that found no free host since the last capacity pass.
    starved: Mutex<HashMap<(String, String), (u32, OffsetDateTime)>>,
}

impl HostRegistry {
    pub fn static_only(cfg: &SshFleetConfig) -> Self {
        Self {
            db: None,
            ssh: cfg.clone(),
            cache: RwLock::new(static_hosts(cfg)),
            starved: Mutex::new(HashMap::new()),
        }
    }

    pub fn new(db: PgPool, cfg: &SshFleetConfig) -> Self {
        Self {
            db: Some(db),
            ssh: cfg.clone(),
            cache: RwLock::new(static_hosts(cfg)),
            starved: Mutex::new(HashMap::new()),
        }
    }

    pub fn ssh_config(&self) -> &SshFleetConfig {
        &self.ssh
    }

    pub fn snapshot(&self) -> Vec<Host> {
        self.cache.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn host(&self, id: Uuid) -> Option<Host> {
        self.cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|h| h.id == id)
            .cloned()
    }

    pub fn by_addr(&self, addr: &str) -> Option<Host> {
        self.cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|h| {
                h.state != "gone" && (h.addr == addr || h.external_id.as_deref() == Some(addr))
            })
            .cloned()
    }

    /// A start found every eligible host full: remembered for the capacity pass.
    pub fn note_starved(&self, region: &str, class: &str) {
        let mut g = self.starved.lock().unwrap_or_else(|e| e.into_inner());
        let e = g
            .entry((region.to_string(), class.to_string()))
            .or_insert((0, OffsetDateTime::now_utc()));
        e.0 += 1;
        e.1 = OffsetDateTime::now_utc();
    }

    /// Starved starts matching a pool (same region or any, same class or any) since the last call.
    pub fn take_starved(&self, region: &str, class: &str) -> u32 {
        let mut g = self.starved.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0;
        g.retain(|(r, c), (count, _)| {
            let hit = (r.is_empty() || region.is_empty() || r == region)
                && (c.is_empty() || class.is_empty() || c == class);
            if hit {
                n += *count;
            }
            !hit
        });
        n
    }

    pub fn starved_snapshot(&self) -> serde_json::Value {
        let g = self.starved.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::Value::Array(
            g.iter()
                .map(|((r, c), (n, at))| {
                    serde_json::json!({ "region": r, "class": c, "count": n, "last_at": at.unix_timestamp() })
                })
                .collect(),
        )
    }

    fn db(&self) -> Result<&PgPool> {
        self.db.as_ref().ok_or_else(|| {
            Error::InvalidArgument(
                "host pool is static ([fleet.ssh] hosts without a database)".into(),
            )
        })
    }

    /// Upsert the config hosts (`source = static`) and drop static rows no longer configured.
    pub async fn seed_static(&self) -> Result<()> {
        let db = self.db()?;
        for h in &self.ssh.hosts {
            sqlx::query(
                "insert into musubi.fleet_hosts (id, name, addr, public_addr, ssh_user, ssh_port, ssh_key, capacity, state, source, ready_at)
                 values ($1, $2, $2, $3, $4, $5, $6, $7, 'ready', 'static', now())
                 on conflict (addr) where state <> 'gone' do update set public_addr = excluded.public_addr, ssh_user = excluded.ssh_user,
                   ssh_port = excluded.ssh_port, ssh_key = excluded.ssh_key, capacity = excluded.capacity, updated_at = now()",
            )
            .bind(Uuid::now_v7())
            .bind(&h.addr)
            .bind(&h.public_addr)
            .bind(&h.user)
            .bind(h.port as i32)
            .bind(&h.key)
            .bind(h.capacity as i32)
            .execute(db)
            .await
            .map_err(Error::internal)?;
        }
        let keep: Vec<String> = self.ssh.hosts.iter().map(|h| h.addr.clone()).collect();
        sqlx::query(
            "update musubi.fleet_hosts set state = 'gone', updated_at = now()
             where source = 'static' and state <> 'gone' and not (addr = any($1))",
        )
        .bind(&keep)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.refresh().await
    }

    pub async fn refresh(&self) -> Result<()> {
        let Some(db) = &self.db else { return Ok(()) };
        let rows: Vec<HostRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.fleet_hosts where state <> 'gone' order by created_at"
        ))
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        *self.cache.write().unwrap_or_else(|e| e.into_inner()) =
            rows.into_iter().map(row).collect();
        Ok(())
    }

    pub async fn list(&self, f: &HostFilter) -> Result<Vec<Host>> {
        let hosts = match &self.db {
            Some(db) if f.include_gone => {
                let rows: Vec<HostRow> = sqlx::query_as(&format!(
                    "select {COLS} from musubi.fleet_hosts order by created_at"
                ))
                .fetch_all(db)
                .await
                .map_err(Error::internal)?;
                rows.into_iter().map(row).collect()
            }
            _ => self.snapshot(),
        };
        Ok(hosts
            .into_iter()
            .filter(|h| f.state.as_deref().is_none_or(|s| h.state == s))
            .filter(|h| f.region.as_deref().is_none_or(|s| h.region == s))
            .filter(|h| f.class.as_deref().is_none_or(|s| h.class == s))
            .filter(|h| f.pool.as_deref().is_none_or(|s| h.pool == s))
            .filter(|h| f.source.as_deref().is_none_or(|s| h.source == s))
            .collect())
    }

    pub async fn get(&self, id: Uuid) -> Result<Host> {
        if let Some(h) = self.host(id) {
            return Ok(h);
        }
        let db = self.db()?;
        let r: Option<HostRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.fleet_hosts where id = $1"
        ))
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?;
        r.map(row)
            .ok_or_else(|| Error::NotFound(format!("host {id}")))
    }

    pub async fn register(&self, n: NewHost) -> Result<Host> {
        let db = self.db()?;
        validate_addr(&n.addr)?;
        if n.name.len() > 64 || n.region.len() > 64 || n.class.len() > 64 {
            return Err(Error::InvalidArgument(
                "name/region/class: up to 64 chars".into(),
            ));
        }
        let labels = if n.labels.is_null() {
            serde_json::json!({})
        } else {
            n.labels.clone()
        };
        validate_label_map(&labels)?;
        let capacity = n.capacity.unwrap_or(8);
        if !(1..=4096).contains(&capacity) {
            return Err(Error::InvalidArgument("capacity: 1..=4096".into()));
        }
        let state = n.state.as_deref().unwrap_or("ready");
        if !["ready", "provisioning"].contains(&state) {
            return Err(Error::InvalidArgument("state: ready|provisioning".into()));
        }
        let source = n.source.as_deref().unwrap_or("manual");
        if !["manual", "provisioned"].contains(&source) {
            return Err(Error::InvalidArgument("source: manual|provisioned".into()));
        }
        if n.cost_per_hour < 0.0 {
            return Err(Error::InvalidArgument("cost_per_hour: >= 0".into()));
        }
        let id = Uuid::now_v7();
        let name = if n.name.is_empty() {
            n.addr.clone()
        } else {
            n.name.clone()
        };
        let res = sqlx::query(
            "insert into musubi.fleet_hosts (id, name, addr, public_addr, ssh_user, ssh_port, ssh_key, capacity, region, class, labels, state, source, pool, external_id, spot, cost_per_hour, ready_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, case when $12 = 'ready' then now() end)",
        )
        .bind(id)
        .bind(&name)
        .bind(&n.addr)
        .bind(&n.public_addr)
        .bind(n.user.as_deref().unwrap_or("musubi"))
        .bind(n.port.unwrap_or(22) as i32)
        .bind(n.key.as_deref().unwrap_or(""))
        .bind(capacity as i32)
        .bind(&n.region)
        .bind(&n.class)
        .bind(&labels)
        .bind(state)
        .bind(source)
        .bind(n.pool.as_deref().unwrap_or(""))
        .bind(&n.external_id)
        .bind(n.spot)
        .bind(n.cost_per_hour)
        .execute(db)
        .await;
        match res {
            Ok(_) => {}
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(Error::AlreadyExists(format!(
                    "host {} already registered",
                    n.addr
                )));
            }
            Err(e) => return Err(Error::internal(e)),
        }
        self.refresh().await?;
        self.get(id).await
    }

    pub async fn patch(&self, id: Uuid, p: PatchHost) -> Result<Host> {
        let db = self.db()?;
        let cur = self.get(id).await?;
        if cur.state == "gone" {
            return Err(Error::NotFound(format!("host {id}")));
        }
        if let Some(c) = p.capacity {
            if !(1..=4096).contains(&c) {
                return Err(Error::InvalidArgument("capacity: 1..=4096".into()));
            }
        }
        if let Some(l) = &p.labels {
            validate_label_map(l)?;
        }
        if let Some(s) = &p.state {
            match (cur.state.as_str(), s.as_str()) {
                ("ready", "draining") | ("draining", "ready") | ("failed", "provisioning") => {}
                (a, b) if a == b => {}
                _ => {
                    return Err(Error::InvalidArgument(format!(
                        "state: {} → {s} not allowed (ready ⇄ draining, failed → provisioning)",
                        cur.state
                    )))
                }
            }
        }
        if p.cost_per_hour.is_some_and(|c| c < 0.0) {
            return Err(Error::InvalidArgument("cost_per_hour: >= 0".into()));
        }
        sqlx::query(
            "update musubi.fleet_hosts set name = coalesce($2, name), public_addr = coalesce($3, public_addr),
                 capacity = coalesce($4, capacity), class = coalesce($5, class), region = coalesce($6, region),
                 labels = coalesce($7, labels), cost_per_hour = coalesce($8, cost_per_hour), spot = coalesce($9, spot),
                 state = coalesce($10, state), last_error = case when $10 = 'provisioning' then null else last_error end,
                 updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(&p.name)
        .bind(&p.public_addr)
        .bind(p.capacity.map(|c| c as i32))
        .bind(&p.class)
        .bind(&p.region)
        .bind(&p.labels)
        .bind(p.cost_per_hour)
        .bind(p.spot)
        .bind(&p.state)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.refresh().await?;
        self.get(id).await
    }

    /// State transition with bookkeeping (`ready_at`, `termination_at`, `last_error`).
    pub async fn set_state(
        &self,
        id: Uuid,
        state: &str,
        error: Option<&str>,
        termination_at: Option<OffsetDateTime>,
    ) -> Result<Host> {
        let db = self.db()?;
        sqlx::query(
            "update musubi.fleet_hosts set state = $2, last_error = $3,
                 ready_at = case when $2 = 'ready' then coalesce(ready_at, now()) else ready_at end,
                 termination_at = coalesce($4, termination_at), updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(state)
        .bind(error)
        .bind(termination_at)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.refresh().await?;
        self.get(id).await
    }

    pub async fn touch_probe(&self, id: Uuid, error: Option<&str>) -> Result<()> {
        let db = self.db()?;
        sqlx::query(
            "update musubi.fleet_hosts set last_probe_at = now(), last_error = $2, updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(error)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.refresh().await
    }

    /// Servers still on each host (`game_servers.host_id`, not terminated).
    pub async fn used(&self) -> Result<HashMap<Uuid, i64>> {
        let Some(db) = &self.db else {
            return Ok(HashMap::new());
        };
        let rows: Vec<(Uuid, i64)> = sqlx::query_as(
            "select host_id, count(*) from musubi.game_servers where host_id is not null and state <> 'terminated' group by host_id",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().collect())
    }

    /// When the host last ran something: the newest server termination on it, else `ready_at`.
    pub async fn idle_since(&self, h: &Host) -> Result<Option<OffsetDateTime>> {
        let Some(db) = &self.db else {
            return Ok(h.ready_at);
        };
        let last: Option<Option<OffsetDateTime>> = sqlx::query_scalar(
            "select max(terminated_at) from musubi.game_servers where host_id = $1",
        )
        .bind(h.id)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?;
        Ok(last
            .flatten()
            .or(h.ready_at)
            .map(|t| t.max(h.ready_at.unwrap_or(t))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_filters_state_region_class_labels() {
        let mut h = static_hosts(&SshFleetConfig {
            hosts: vec![musubi_core::config::SshHost {
                addr: "10.0.0.1".into(),
                ..Default::default()
            }],
            ..Default::default()
        })
        .remove(0);
        let none = serde_json::Map::new();
        assert!(h.accepts("fsn1", None, &none));
        h.region = "nbg1".into();
        assert!(!h.accepts("fsn1", None, &none));
        assert!(h.accepts("", None, &none));
        h.class = "ax41".into();
        assert!(h.accepts("nbg1", Some("ax41"), &none));
        assert!(!h.accepts("nbg1", Some("c5"), &none));
        assert!(h.accepts("nbg1", Some(""), &none));
        h.labels = serde_json::json!({"gpu": "a10", "rack": 3});
        let want: serde_json::Map<_, _> = serde_json::json!({"gpu": "a10"})
            .as_object()
            .unwrap()
            .clone();
        assert!(h.accepts("nbg1", None, &want));
        let want: serde_json::Map<_, _> = serde_json::json!({"gpu": "h100"})
            .as_object()
            .unwrap()
            .clone();
        assert!(!h.accepts("nbg1", None, &want));
        h.state = "draining".into();
        assert!(!h.accepts("nbg1", None, &none));
    }

    #[test]
    fn starvation_is_counted_per_region_class_and_drained_once() {
        let r = HostRegistry::static_only(&SshFleetConfig::default());
        r.note_starved("fsn1", "ax41");
        r.note_starved("fsn1", "ax41");
        r.note_starved("nbg1", "");
        assert_eq!(r.take_starved("fsn1", "ax41"), 2);
        assert_eq!(r.take_starved("fsn1", "ax41"), 0);
        // a pool without a class pin matches any class; region "" matches any region
        assert_eq!(r.take_starved("", ""), 1);
        assert_eq!(r.starved_snapshot().as_array().unwrap().len(), 0);
    }

    #[test]
    fn validation() {
        assert!(validate_addr("10.0.0.1").is_ok());
        assert!(validate_addr("box-1.example.net").is_ok());
        assert!(validate_addr("").is_err());
        assert!(validate_addr("root@box").is_err());
        assert!(validate_addr("a b").is_err());
        assert!(validate_label_map(&serde_json::json!({"a": "b", "n": 1})).is_ok());
        assert!(validate_label_map(&serde_json::json!({"a": ["x"]})).is_err());
        assert!(validate_label_map(&serde_json::json!("x")).is_err());
    }
}
