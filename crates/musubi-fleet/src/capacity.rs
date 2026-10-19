//! Node-level autoscaling for the `ssh` host pool (P1-11, "cloud burst"): capacity pools order
//! machines when fleets starve for host slots (or the pool's free-slot target is not met) and
//! release idle ones. Drivers: `manual` (raise `capacity.needed`, an operator racks a box),
//! `webhook` (any provisioner behind `POST {url}/provision|deprovision`), `hetzner` (Hetzner
//! Cloud API). Spot / preemption notices (`preempt_host`) give the servers on a host a deadline,
//! and the next pass replaces the capacity.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use musubi_core::config::{CapacityConfig, CapacityPoolConfig, SshFleetConfig};
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::hosts::{Host, HostRegistry, NewHost};
use crate::ssh::SshProvider;
use crate::{FleetService, GameServer};

pub const KINDS: &[&str] = &["manual", "webhook", "hetzner"];

pub fn validate_pools(pools: &[CapacityPoolConfig]) -> Result<()> {
    let mut names = std::collections::HashSet::new();
    for p in pools {
        if p.name.is_empty() || p.name.len() > 64 {
            return Err(Error::InvalidArgument(
                "fleet.capacity.pools[].name: 1..=64 chars".into(),
            ));
        }
        if !names.insert(p.name.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "fleet.capacity.pools: duplicate name {}",
                p.name
            )));
        }
        if !KINDS.contains(&p.kind.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "pool {}: kind manual|webhook|hetzner",
                p.name
            )));
        }
        if p.capacity_per_host == 0 || p.max_hosts < p.min_hosts {
            return Err(Error::InvalidArgument(format!(
                "pool {}: capacity_per_host >= 1 and max_hosts >= min_hosts",
                p.name
            )));
        }
        if p.kind == "webhook" && p.url.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "pool {}: url required",
                p.name
            )));
        }
        if p.kind == "hetzner"
            && (p.token.is_empty() || p.server_type.is_empty() || p.image.is_empty())
        {
            return Err(Error::InvalidArgument(format!(
                "pool {}: token, server_type and image required",
                p.name
            )));
        }
    }
    Ok(())
}

/// A machine a driver handed back.
#[derive(Debug, Clone)]
pub struct Provisioned {
    pub external_id: String,
    pub addr: String,
    pub public_addr: Option<String>,
    pub name: String,
}

pub struct CapacityState {
    pub hosts: Arc<HostRegistry>,
    pub ssh: Arc<SshProvider>,
    pub pools: Vec<CapacityPoolConfig>,
    client: reqwest::Client,
    /// pool → (unix secs, what happened)
    last_action: Mutex<HashMap<String, (i64, String)>>,
}

impl CapacityState {
    pub fn pool(&self, name: &str) -> Option<&CapacityPoolConfig> {
        self.pools.iter().find(|p| p.name == name)
    }

    fn last(&self, pool: &str) -> Option<(i64, String)> {
        self.last_action
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(pool)
            .cloned()
    }

    fn note(&self, pool: &str, what: impl Into<String>) {
        self.last_action
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                pool.to_string(),
                (OffsetDateTime::now_utc().unix_timestamp(), what.into()),
            );
    }

    fn in_cooldown(&self, p: &CapacityPoolConfig, now: i64) -> bool {
        self.last(&p.name).is_some_and(|(at, what)| {
            !what.starts_with("needed") && now - at < p.cooldown_secs as i64
        })
    }

    async fn provision(&self, p: &CapacityPoolConfig, n: u32) -> Result<Vec<Provisioned>> {
        match p.kind.as_str() {
            "manual" => Err(Error::Unavailable(format!(
                "pool {}: manual capacity — add {n} host(s) via POST /v1/server/fleets/hosts",
                p.name
            ))),
            "webhook" => {
                let r = self
                    .client
                    .post(format!("{}/provision", p.url.trim_end_matches('/')))
                    .bearer_auth(&p.token)
                    .json(&json!({
                        "pool": p.name, "region": p.region, "class": p.class, "count": n,
                        "capacity": p.capacity_per_host, "spot": p.spot, "labels": p.labels,
                    }))
                    .send()
                    .await
                    .map_err(|e| Error::Unavailable(format!("provisioner: {e}")))?;
                if !r.status().is_success() {
                    return Err(Error::Unavailable(format!(
                        "provisioner answered {}",
                        r.status()
                    )));
                }
                let v: Value = r
                    .json()
                    .await
                    .map_err(|e| Error::Unavailable(format!("provisioner: bad json: {e}")))?;
                let hosts = v["hosts"]
                    .as_array()
                    .ok_or_else(|| Error::Unavailable("provisioner: no hosts[]".into()))?;
                hosts
                    .iter()
                    .map(|h| {
                        let addr = h["addr"].as_str().unwrap_or("").to_string();
                        if addr.is_empty() {
                            return Err(Error::Unavailable(
                                "provisioner: host without addr".into(),
                            ));
                        }
                        Ok(Provisioned {
                            external_id: h["external_id"]
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| addr.clone()),
                            name: h["name"].as_str().unwrap_or(&addr).to_string(),
                            public_addr: h["public_addr"].as_str().map(str::to_string),
                            addr,
                        })
                    })
                    .collect()
            }
            "hetzner" => {
                let mut out = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let name = format!(
                        "musubi-{}-{}",
                        p.name,
                        &Uuid::now_v7().simple().to_string()[24..]
                    );
                    let mut labels: serde_json::Map<String, Value> = p
                        .labels
                        .iter()
                        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
                        .collect();
                    labels.insert("musubi".into(), Value::String("host".into()));
                    labels.insert("musubi-pool".into(), Value::String(p.name.clone()));
                    let mut body = json!({
                        "name": name, "server_type": p.server_type, "image": p.image,
                        "ssh_keys": p.ssh_keys, "labels": labels, "start_after_create": true,
                    });
                    if !p.location.is_empty() {
                        body["location"] = Value::String(p.location.clone());
                    }
                    if !p.user_data.is_empty() {
                        body["user_data"] = Value::String(p.user_data.clone());
                    }
                    let r = self
                        .client
                        .post(format!("{}/servers", p.api_url.trim_end_matches('/')))
                        .bearer_auth(&p.token)
                        .json(&body)
                        .send()
                        .await
                        .map_err(|e| Error::Unavailable(format!("hetzner: {e}")))?;
                    let status = r.status();
                    let v: Value = r.json().await.unwrap_or(Value::Null);
                    if !status.is_success() {
                        return Err(Error::Unavailable(format!(
                            "hetzner answered {status}: {}",
                            v["error"]["message"].as_str().unwrap_or("")
                        )));
                    }
                    let id = v["server"]["id"]
                        .as_i64()
                        .ok_or_else(|| Error::Unavailable("hetzner: no server.id".into()))?;
                    let ip = v["server"]["public_net"]["ipv4"]["ip"]
                        .as_str()
                        .ok_or_else(|| Error::Unavailable("hetzner: no public ipv4".into()))?
                        .to_string();
                    out.push(Provisioned {
                        external_id: id.to_string(),
                        addr: ip,
                        public_addr: None,
                        name,
                    });
                }
                Ok(out)
            }
            k => Err(Error::InvalidArgument(format!("pool kind {k}"))),
        }
    }

    async fn deprovision(&self, p: &CapacityPoolConfig, external_id: &str) -> Result<()> {
        match p.kind.as_str() {
            "manual" => Ok(()),
            "webhook" => {
                let r = self
                    .client
                    .post(format!("{}/deprovision", p.url.trim_end_matches('/')))
                    .bearer_auth(&p.token)
                    .json(&json!({ "pool": p.name, "external_id": external_id }))
                    .send()
                    .await
                    .map_err(|e| Error::Unavailable(format!("provisioner: {e}")))?;
                if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND {
                    Ok(())
                } else {
                    Err(Error::Unavailable(format!(
                        "provisioner answered {}",
                        r.status()
                    )))
                }
            }
            "hetzner" => {
                let r = self
                    .client
                    .delete(format!(
                        "{}/servers/{external_id}",
                        p.api_url.trim_end_matches('/')
                    ))
                    .bearer_auth(&p.token)
                    .send()
                    .await
                    .map_err(|e| Error::Unavailable(format!("hetzner: {e}")))?;
                if r.status().is_success() || r.status() == reqwest::StatusCode::NOT_FOUND {
                    Ok(())
                } else {
                    Err(Error::Unavailable(format!(
                        "hetzner answered {}",
                        r.status()
                    )))
                }
            }
            k => Err(Error::InvalidArgument(format!("pool kind {k}"))),
        }
    }
}

fn hosts_of<'a>(hosts: &'a [Host], pool: &'a str) -> impl Iterator<Item = &'a Host> + 'a {
    hosts
        .iter()
        .filter(move |h| h.pool == pool && h.state != "gone")
}

impl FleetService {
    /// Register the `ssh` provider over a database-backed host pool and the capacity pools.
    pub async fn enable_ssh(
        &self,
        db: PgPool,
        ssh: &SshFleetConfig,
        cap: &CapacityConfig,
    ) -> Result<Arc<CapacityState>> {
        validate_pools(&cap.pools)?;
        let hosts = Arc::new(HostRegistry::new(db, ssh));
        hosts.seed_static().await?;
        let provider = Arc::new(SshProvider::with_registry(hosts.clone()));
        self.register_provider("ssh", provider.clone());
        let state = Arc::new(CapacityState {
            hosts,
            ssh: provider,
            pools: cap.pools.clone(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            last_action: Mutex::new(HashMap::new()),
        });
        *self.capacity.write().unwrap_or_else(|e| e.into_inner()) = Some(state.clone());
        Ok(state)
    }

    pub fn capacity(&self) -> Option<Arc<CapacityState>> {
        self.capacity
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn capacity_required(&self) -> Result<Arc<CapacityState>> {
        self.capacity().ok_or_else(|| {
            Error::InvalidArgument(
                "host pool not enabled: configure [fleet.ssh] hosts or [fleet.capacity] pools"
                    .into(),
            )
        })
    }

    /// Non-terminated servers on a host (any fleet).
    pub async fn host_servers(&self, host: Uuid) -> Result<Vec<GameServer>> {
        let fleets = self.list().await?;
        let mut out = Vec::new();
        for f in fleets {
            out.extend(
                self.servers(f.id, None)
                    .await?
                    .into_iter()
                    .filter(|s| s.host_id == Some(host) && s.state != "terminated"),
            );
        }
        Ok(out)
    }

    async fn host_event(&self, host: &Host, code: &str, message: String, data: Value) {
        let mut data = data;
        data["host_id"] = json!(host.id);
        data["addr"] = json!(host.addr);
        data["pool"] = json!(host.pool);
        data["class"] = json!(host.class);
        data["region"] = json!(host.region);
        self.record_event(Uuid::nil(), None, code, message, data)
            .await;
    }

    /// Spot / preemption notice for a host: no new starts, idle servers stop now, busy ones get
    /// `deadline_secs` (event `server.preempted`) and are stopped by the capacity pass when it
    /// runs out; the pool replaces the capacity on its next pass.
    pub async fn preempt_host(
        &self,
        host: Uuid,
        deadline_secs: u64,
        reason: &str,
    ) -> Result<Value> {
        let cap = self.capacity_required()?;
        let h = cap.hosts.get(host).await?;
        if ["gone", "failed"].contains(&h.state.as_str()) {
            return Err(Error::Conflict(format!("host {host} is {}", h.state)));
        }
        let deadline = OffsetDateTime::now_utc() + Duration::from_secs(deadline_secs.min(86_400));
        let h = cap
            .hosts
            .set_state(host, "terminating", Some(reason), Some(deadline))
            .await?;
        let mut stopped = 0;
        let mut warned = 0;
        for s in self.host_servers(host).await? {
            let Ok(f) = self.get(s.fleet_id).await else {
                continue;
            };
            if ["ready", "starting", "scheduled"].contains(&s.state.as_str()) {
                self.terminate_reason(&f, s.id, "host_preempted").await;
                stopped += 1;
            } else {
                let _ = sqlx::query(
                    "update musubi.game_servers set labels = labels || jsonb_build_object('preempt_at', $2::text, 'preempt_reason', $3::text), updated_at = now() where id = $1",
                )
                .bind(s.id)
                .bind(deadline.unix_timestamp().to_string())
                .bind(reason)
                .execute(&self.db)
                .await;
                self.record_event(
                    f.id,
                    Some(s.id),
                    "server.preempted",
                    format!("host {} is being reclaimed ({reason}); server stops in {deadline_secs}s", h.addr),
                    json!({ "host_id": host, "deadline_at": deadline.unix_timestamp(), "deadline_secs": deadline_secs, "reason": reason, "players": s.players }),
                )
                .await;
                warned += 1;
            }
        }
        self.host_event(
            &h,
            "host.preempted",
            format!("host {} preempted ({reason}): {stopped} idle stopped, {warned} busy warned", h.addr),
            json!({ "reason": reason, "deadline_at": deadline.unix_timestamp(), "stopped": stopped, "warned": warned }),
        )
        .await;
        cap.hosts.take_starved(&h.region, &h.class);
        Ok(
            json!({ "host": h, "stopped": stopped, "warned": warned, "deadline_at": deadline.unix_timestamp() }),
        )
    }

    pub async fn drain_host(&self, host: Uuid, drain: bool) -> Result<Host> {
        let cap = self.capacity_required()?;
        let h = cap.hosts.get(host).await?;
        let (from, to) = if drain {
            ("ready", "draining")
        } else {
            ("draining", "ready")
        };
        if h.state != from {
            return Err(Error::Conflict(format!(
                "host {host} is {}, expected {from}",
                h.state
            )));
        }
        let h = cap.hosts.set_state(host, to, None, None).await?;
        self.host_event(
            &h,
            if drain { "host.draining" } else { "host.ready" },
            format!(
                "host {} {}",
                h.addr,
                if drain { "draining" } else { "back in service" }
            ),
            json!({}),
        )
        .await;
        Ok(h)
    }

    /// Reach the host over ssh now; `provisioning`/`failed` hosts become `ready` on success,
    /// `ready` hosts that stop answering are marked `failed` (no new starts).
    pub async fn probe_host(&self, host: Uuid) -> Result<Value> {
        let cap = self.capacity_required()?;
        let h = cap.hosts.get(host).await?;
        if ["gone"].contains(&h.state.as_str()) {
            return Err(Error::Conflict(format!("host {host} is gone")));
        }
        let res = cap.ssh.probe_host(&h, None).await;
        let (ok, err) = match &res {
            Ok(()) => (true, None),
            Err(e) => (false, Some(e.to_string())),
        };
        cap.hosts.touch_probe(host, err.as_deref()).await?;
        let mut h = cap.hosts.get(host).await?;
        match (ok, h.state.as_str()) {
            (true, "provisioning") | (true, "failed") => {
                h = cap.hosts.set_state(host, "ready", None, None).await?;
                self.host_event(
                    &h,
                    "host.ready",
                    format!("host {} answered", h.addr),
                    json!({}),
                )
                .await;
            }
            (false, "ready") => {
                h = cap
                    .hosts
                    .set_state(host, "failed", err.as_deref(), None)
                    .await?;
                self.host_event(
                    &h,
                    "host.failed",
                    format!(
                        "host {} unreachable: {}",
                        h.addr,
                        err.clone().unwrap_or_default()
                    ),
                    json!({ "error": err }),
                )
                .await;
            }
            _ => {}
        }
        Ok(json!({ "ok": ok, "error": err, "host": h }))
    }

    pub async fn register_host(&self, n: NewHost) -> Result<Host> {
        let cap = self.capacity_required()?;
        let h = cap.hosts.register(n).await?;
        self.host_event(
            &h,
            "host.registered",
            format!(
                "host {} registered ({}, {} slots)",
                h.addr, h.source, h.capacity
            ),
            json!({ "capacity": h.capacity, "source": h.source, "state": h.state }),
        )
        .await;
        Ok(h)
    }

    /// Forget a host: refuses while servers still run on it (drain first); provisioned
    /// machines are released through their pool's driver.
    pub async fn delete_host(&self, host: Uuid, force: bool) -> Result<Value> {
        let cap = self.capacity_required()?;
        let h = cap.hosts.get(host).await?;
        if h.state == "gone" {
            return Err(Error::NotFound(format!("host {host}")));
        }
        let running = self.host_servers(host).await?;
        if !running.is_empty() && !force {
            return Err(Error::Conflict(format!(
                "host {host} still runs {} server(s); drain it or pass force=true",
                running.len()
            )));
        }
        for s in running {
            if let Ok(f) = self.get(s.fleet_id).await {
                self.terminate_reason(&f, s.id, "host_removed").await;
            }
        }
        let mut released = false;
        if let (Some(ext), Some(p)) = (&h.external_id, cap.pool(&h.pool)) {
            cap.deprovision(p, ext).await?;
            released = true;
        }
        let h = cap.hosts.set_state(host, "gone", None, None).await?;
        self.host_event(
            &h,
            "host.removed",
            format!(
                "host {} removed{}",
                h.addr,
                if released { " and released" } else { "" }
            ),
            json!({ "released": released, "forced": force }),
        )
        .await;
        Ok(json!({ "ok": true, "released": released }))
    }

    /// Per-pool picture: hosts by state, slots, starvation, last action, cost.
    pub async fn capacity_summary(&self) -> Result<Value> {
        let cap = self.capacity_required()?;
        cap.hosts.refresh().await?;
        let hosts = cap.hosts.snapshot();
        let used = cap.hosts.used().await?;
        let summarize = |hs: Vec<&Host>| {
            let mut by_state: HashMap<&str, u32> = HashMap::new();
            let (mut total, mut busy, mut cost) = (0i64, 0i64, 0f64);
            for h in &hs {
                *by_state.entry(h.state.as_str()).or_default() += 1;
                if ["ready", "draining", "terminating"].contains(&h.state.as_str()) {
                    total += h.capacity as i64;
                    busy += used.get(&h.id).copied().unwrap_or(0);
                }
                if h.state != "failed" {
                    cost += h.cost_per_hour;
                }
            }
            let free: i64 = hs
                .iter()
                .filter(|h| h.state == "ready")
                .map(|h| (h.capacity as i64 - used.get(&h.id).copied().unwrap_or(0)).max(0))
                .sum();
            json!({ "hosts": hs.len(), "by_state": by_state, "slots": { "total": total, "used": busy, "free": free }, "cost_per_hour": cost })
        };
        let pools: Vec<Value> = cap
            .pools
            .iter()
            .map(|p| {
                let hs: Vec<&Host> = hosts_of(&hosts, &p.name).collect();
                let mut v = summarize(hs);
                v["name"] = json!(p.name);
                v["kind"] = json!(p.kind);
                v["region"] = json!(p.region);
                v["class"] = json!(p.class);
                v["min_hosts"] = json!(p.min_hosts);
                v["max_hosts"] = json!(p.max_hosts);
                v["keep_free_slots"] = json!(p.keep_free_slots);
                v["capacity_per_host"] = json!(p.capacity_per_host);
                v["spot"] = json!(p.spot);
                v["last_action"] = cap
                    .last(&p.name)
                    .map(|(at, what)| json!({ "at": at, "what": what }))
                    .unwrap_or(Value::Null);
                v
            })
            .collect();
        let unpooled: Vec<&Host> = hosts.iter().filter(|h| h.pool.is_empty()).collect();
        let mut classes: HashMap<String, Vec<&Host>> = HashMap::new();
        for h in &hosts {
            classes.entry(h.class.clone()).or_default().push(h);
        }
        let classes: Vec<Value> = classes
            .into_iter()
            .map(|(c, hs)| {
                let mut v = summarize(hs);
                v["class"] = json!(c);
                v
            })
            .collect();
        Ok(json!({
            "pools": pools,
            "unpooled": summarize(unpooled),
            "classes": classes,
            "starved": cap.hosts.starved_snapshot(),
            "total": summarize(hosts.iter().collect()),
        }))
    }

    /// One capacity pass: probe provisioning hosts, enforce preemption deadlines, release empty
    /// draining/terminating hosts, then scale each pool up (starvation / free-slot target /
    /// min_hosts) or down (idle hosts above the target). Returns what it did.
    pub async fn capacity_pass(&self) -> Result<Value> {
        let cap = self.capacity_required()?;
        cap.hosts.refresh().await?;
        let now = OffsetDateTime::now_utc();
        let mut actions: Vec<Value> = Vec::new();
        // hosts that became ready in this pass are never scaled down in the same pass: the fleet
        // reconciler has not had a chance to place servers on them yet
        let mut fresh: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let hosts = cap.hosts.snapshot();
        // 1. provisioning → probe; stale → failed + release
        for h in hosts.iter().filter(|h| h.state == "provisioning") {
            let timeout = cap
                .pool(&h.pool)
                .map(|p| p.provision_timeout_secs)
                .unwrap_or(600);
            match cap.ssh.probe_host(h, None).await {
                Ok(()) => {
                    let _ = cap.hosts.touch_probe(h.id, None).await;
                    let h = cap.hosts.set_state(h.id, "ready", None, None).await?;
                    fresh.insert(h.id);
                    self.host_event(
                        &h,
                        "host.ready",
                        format!("host {} answered", h.addr),
                        json!({}),
                    )
                    .await;
                    actions.push(json!({ "host": h.id, "action": "ready" }));
                }
                Err(e) => {
                    let _ = cap.hosts.touch_probe(h.id, Some(&e.to_string())).await;
                    if now - h.created_at > time::Duration::seconds(timeout as i64) {
                        let mut released = false;
                        if let (Some(ext), Some(p)) = (&h.external_id, cap.pool(&h.pool)) {
                            released = cap.deprovision(p, ext).await.is_ok();
                        }
                        let h = cap
                            .hosts
                            .set_state(h.id, "failed", Some(&e.to_string()), None)
                            .await?;
                        self.host_event(
                            &h,
                            "host.failed",
                            format!("host {} never answered: {e}", h.addr),
                            json!({ "error": e.to_string(), "released": released }),
                        )
                        .await;
                        if released {
                            let _ = cap.hosts.set_state(h.id, "gone", None, None).await;
                        }
                        actions.push(
                            json!({ "host": h.id, "action": "failed", "released": released }),
                        );
                    }
                }
            }
        }
        // 2. terminating hosts past their deadline: stop what is left
        for h in hosts.iter().filter(|h| h.state == "terminating") {
            if h.termination_at.is_some_and(|t| t <= now) {
                for s in self.host_servers(h.id).await? {
                    if let Ok(f) = self.get(s.fleet_id).await {
                        self.terminate_reason(&f, s.id, "host_preempted").await;
                        actions.push(json!({ "host": h.id, "server": s.id, "action": "stopped" }));
                    }
                }
            }
        }
        // 3. empty draining/terminating hosts of a pool are released
        let used = cap.hosts.used().await?;
        for h in hosts
            .iter()
            .filter(|h| h.state == "draining" || h.state == "terminating")
        {
            if used.get(&h.id).copied().unwrap_or(0) > 0 {
                continue;
            }
            if h.source != "provisioned" && h.state == "draining" {
                continue;
            }
            let mut released = false;
            if let (Some(ext), Some(p)) = (&h.external_id, cap.pool(&h.pool)) {
                match cap.deprovision(p, ext).await {
                    Ok(()) => released = true,
                    Err(e) => {
                        tracing::warn!(host = %h.addr, error = %e, "host release failed");
                        continue;
                    }
                }
            }
            let h = cap.hosts.set_state(h.id, "gone", None, None).await?;
            self.host_event(
                &h,
                "host.removed",
                format!("host {} released", h.addr),
                json!({ "released": released }),
            )
            .await;
            if let Some(p) = cap.pool(&h.pool) {
                cap.note(&p.name, format!("released {}", h.addr));
            }
            actions.push(json!({ "host": h.id, "action": "released" }));
        }
        // 4. per pool: scale up / down
        cap.hosts.refresh().await?;
        let hosts = cap.hosts.snapshot();
        let used = cap.hosts.used().await?;
        let now_s = now.unix_timestamp();
        for p in &cap.pools {
            let mine: Vec<&Host> = hosts_of(&hosts, &p.name).collect();
            let live: Vec<&Host> = mine
                .iter()
                .copied()
                .filter(|h| h.state == "ready" || h.state == "provisioning")
                .collect();
            let free: i64 = mine
                .iter()
                .filter(|h| h.state == "ready")
                .map(|h| (h.capacity as i64 - used.get(&h.id).copied().unwrap_or(0)).max(0))
                .sum::<i64>()
                + mine
                    .iter()
                    .filter(|h| h.state == "provisioning")
                    .map(|h| h.capacity as i64)
                    .sum::<i64>();
            let starved = cap.hosts.take_starved(&p.region, &p.class) as i64;
            let deficit = (p.keep_free_slots as i64 - free).max(starved);
            let mut want = if deficit > 0 {
                {
                    let per = p.capacity_per_host.max(1) as i64;
                    (deficit + per - 1) / per
                }
            } else {
                0
            };
            want = want.max(p.min_hosts as i64 - live.len() as i64);
            want = want.min(p.max_hosts as i64 - live.len() as i64);
            if want > 0 {
                if cap.in_cooldown(p, now_s) && live.len() as i64 >= p.min_hosts as i64 {
                    actions.push(json!({ "pool": p.name, "action": "cooldown", "wanted": want }));
                    continue;
                }
                match cap.provision(p, want as u32).await {
                    Ok(list) => {
                        for m in list {
                            let reg = cap
                                .hosts
                                .register(NewHost {
                                    name: m.name.clone(),
                                    addr: m.addr.clone(),
                                    public_addr: m.public_addr,
                                    user: Some(p.ssh_user.clone()),
                                    port: Some(p.ssh_port),
                                    key: Some(p.ssh_key.clone()),
                                    capacity: Some(p.capacity_per_host),
                                    region: p.region.clone(),
                                    class: p.class.clone(),
                                    labels: json!(p.labels),
                                    spot: p.spot,
                                    cost_per_hour: p.cost_per_hour,
                                    state: Some("provisioning".into()),
                                    source: Some("provisioned".into()),
                                    pool: Some(p.name.clone()),
                                    external_id: Some(m.external_id.clone()),
                                })
                                .await;
                            let h = match reg {
                                Ok(h) => h,
                                Err(e) => {
                                    tracing::warn!(pool = %p.name, addr = %m.addr, error = %e, "provisioned host not registered");
                                    actions.push(json!({ "pool": p.name, "action": "register_failed", "addr": m.addr, "external_id": m.external_id, "error": e.to_string() }));
                                    continue;
                                }
                            };
                            self.host_event(
                                &h,
                                "host.registered",
                                format!("pool {} ordered host {} ({})", p.name, h.addr, p.kind),
                                json!({ "capacity": h.capacity, "source": "provisioned", "deficit_slots": deficit, "starved": starved }),
                            )
                            .await;
                            actions.push(
                                json!({ "pool": p.name, "host": h.id, "action": "provisioned" }),
                            );
                        }
                        cap.note(&p.name, format!("scaled up by {want}"));
                        self.record_event(
                            Uuid::nil(),
                            None,
                            "capacity.scaled_up",
                            format!("pool {}: +{want} host(s) (deficit {deficit} slots, {starved} starved starts)", p.name),
                            json!({ "pool": p.name, "hosts": want, "deficit_slots": deficit, "starved": starved }),
                        )
                        .await;
                    }
                    Err(e) => {
                        let what = format!("needed {want}: {e}");
                        let repeat = cap.last(&p.name).is_some_and(|(at, w)| {
                            w.starts_with("needed") && now_s - at < p.cooldown_secs as i64
                        });
                        cap.note(&p.name, what.clone());
                        if !repeat {
                            self.record_event(
                                Uuid::nil(),
                                None,
                                "capacity.needed",
                                format!("pool {}: {want} more host(s) needed — {e}", p.name),
                                json!({ "pool": p.name, "hosts": want, "deficit_slots": deficit, "starved": starved, "kind": p.kind, "error": e.to_string() }),
                            )
                            .await;
                        }
                        actions.push(json!({ "pool": p.name, "action": "needed", "wanted": want, "error": e.to_string() }));
                    }
                }
                continue;
            }
            // scale down: idle provisioned hosts the pool can spare
            if cap.in_cooldown(p, now_s) {
                continue;
            }
            let mut spare = free - p.keep_free_slots as i64;
            let mut live_n = live.len() as i64;
            let mut idle: Vec<&Host> = mine
                .iter()
                .copied()
                .filter(|h| {
                    h.state == "ready"
                        && h.source == "provisioned"
                        && !fresh.contains(&h.id)
                        && used.get(&h.id).copied().unwrap_or(0) == 0
                })
                .collect();
            idle.sort_by_key(|h| h.ready_at);
            for h in idle {
                if live_n <= p.min_hosts as i64 || spare - (h.capacity as i64) < 0 {
                    break;
                }
                let since = cap.hosts.idle_since(h).await?;
                let idle_for = since.map(|t| (now - t).whole_seconds()).unwrap_or(0);
                if idle_for < p.scale_down_idle_secs as i64 {
                    continue;
                }
                let h = cap.hosts.set_state(h.id, "draining", None, None).await?;
                self.host_event(
                    &h,
                    "host.draining",
                    format!(
                        "pool {}: host {} idle for {idle_for}s, releasing",
                        p.name, h.addr
                    ),
                    json!({ "idle_secs": idle_for }),
                )
                .await;
                self.record_event(
                    Uuid::nil(),
                    None,
                    "capacity.scaled_down",
                    format!(
                        "pool {}: releasing {} after {idle_for}s idle",
                        p.name, h.addr
                    ),
                    json!({ "pool": p.name, "host_id": h.id, "idle_secs": idle_for }),
                )
                .await;
                cap.note(&p.name, format!("scaled down {}", h.addr));
                spare -= h.capacity as i64;
                live_n -= 1;
                actions.push(json!({ "pool": p.name, "host": h.id, "action": "draining" }));
            }
        }
        Ok(json!({ "actions": actions, "at": now_s }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_validation() {
        let ok = CapacityPoolConfig {
            name: "fsn1-ax41".into(),
            ..Default::default()
        };
        assert!(validate_pools(std::slice::from_ref(&ok)).is_ok());
        assert!(
            validate_pools(&[ok.clone(), ok.clone()]).is_err(),
            "duplicate"
        );
        let bad = CapacityPoolConfig {
            kind: "aws".into(),
            ..ok.clone()
        };
        assert!(validate_pools(std::slice::from_ref(&bad)).is_err());
        let bad = CapacityPoolConfig {
            min_hosts: 5,
            max_hosts: 2,
            ..ok.clone()
        };
        assert!(validate_pools(std::slice::from_ref(&bad)).is_err());
        let bad = CapacityPoolConfig {
            kind: "webhook".into(),
            ..ok.clone()
        };
        assert!(validate_pools(std::slice::from_ref(&bad)).is_err(), "url");
        let bad = CapacityPoolConfig {
            kind: "hetzner".into(),
            token: "t".into(),
            ..ok.clone()
        };
        assert!(
            validate_pools(std::slice::from_ref(&bad)).is_err(),
            "server_type/image"
        );
        let good = CapacityPoolConfig {
            kind: "hetzner".into(),
            token: "t".into(),
            server_type: "cx22".into(),
            image: "ubuntu-24.04".into(),
            ..ok
        };
        assert!(validate_pools(&[good]).is_ok());
    }
}
