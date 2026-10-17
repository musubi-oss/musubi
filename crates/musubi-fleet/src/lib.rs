//! Dedicated-server orchestration v0: fleets describe what to run; the reconciler keeps `buffer`
//! servers ready (≤ `max_servers`) through a `Provider`; game servers talk back with an Agones-like
//! SDK surface (ready / health / allocate / shutdown / labels). Sizing follows `buffer`, a
//! `min_servers` floor, a weekday/hour schedule and an optional demand forecast (hour-of-week EWMA
//! of allocations, see `ScalePolicy`); idle ready servers above the target are stopped. Providers: `local` (child processes on this
//! node), `docker` (containers via the Docker CLI), `kubernetes` (pods) and `agones`
//! (GameServer CRs); edgegap/gamelift plug into the same trait.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub mod docker;
pub mod edgegap;
pub mod firecracker;
pub mod gamelift;
pub mod kubernetes;

pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetSpec {
    /// Executable + args for the `local` provider (or image for container providers).
    pub command: Vec<String>,
    pub image: Option<String>,
    pub env: HashMap<String, String>,
    pub working_dir: Option<String>,
    /// vCPUs one server uses, for usage accounting (default 1).
    pub vcpu: Option<f64>,
}

/// Autoscale policy (plan 05): buffer overrides by schedule plus an optional demand forecast.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScalePolicy {
    /// Also size the ready pool from allocation history: ready ≈ allocations/hour × `horizon_min`/60.
    pub forecast: bool,
    /// How far ahead the forecast must cover, in minutes.
    pub horizon_min: u32,
    /// Buffer overrides by weekday/hour (UTC); the first matching window wins over `buffer`.
    pub schedule: Vec<ScheduleWindow>,
}

impl Default for ScalePolicy {
    fn default() -> Self {
        Self {
            forecast: false,
            horizon_min: 10,
            schedule: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ScheduleWindow {
    /// Weekdays, 0 = Monday … 6 = Sunday; empty = every day.
    pub days: Vec<u8>,
    pub from_hour: u8,
    /// Exclusive; `to_hour <= from_hour` wraps past midnight (equal = whole day).
    pub to_hour: u8,
    pub buffer: i32,
}

impl ScheduleWindow {
    pub fn matches(&self, weekday: u8, hour: u8) -> bool {
        let day_ok = self.days.is_empty() || self.days.contains(&weekday);
        let hour_ok = if self.from_hour < self.to_hour {
            hour >= self.from_hour && hour < self.to_hour
        } else {
            hour >= self.from_hour || hour < self.to_hour
        };
        day_ok && hour_ok
    }
}

impl ScalePolicy {
    /// Buffer in force at `now`: the first matching schedule window, else `default`.
    pub fn buffer_at(&self, now: OffsetDateTime, default: i32) -> i32 {
        let wd = now.weekday().number_days_from_monday();
        self.schedule
            .iter()
            .find(|w| w.matches(wd, now.hour()))
            .map(|w| w.buffer)
            .unwrap_or(default)
    }

    fn validate(&self, max_servers: i32) -> Result<()> {
        if self.horizon_min == 0 || self.horizon_min > 24 * 60 {
            return Err(Error::InvalidArgument(
                "policy.horizon_min in 1..=1440".into(),
            ));
        }
        for w in &self.schedule {
            if w.from_hour > 23 || w.to_hour > 23 || w.days.iter().any(|d| *d > 6) {
                return Err(Error::InvalidArgument(
                    "schedule: hours 0..=23, days 0..=6".into(),
                ));
            }
            if w.buffer < 0 || w.buffer > max_servers {
                return Err(Error::InvalidArgument(
                    "schedule: 0 <= buffer <= max_servers".into(),
                ));
            }
        }
        Ok(())
    }
}

fn validate_shape(min: i32, buffer: i32, max: i32, policy: &ScalePolicy) -> Result<()> {
    if max < 1 || buffer < 0 || buffer > max || min < 0 || min > max {
        return Err(Error::InvalidArgument(
            "need 0 <= buffer <= max_servers, 0 <= min_servers <= max_servers, max_servers >= 1"
                .into(),
        ));
    }
    policy.validate(max)
}

/// Weight of the latest occurrence of an hour-of-week slot in its EWMA.
pub const DEMAND_ALPHA: f64 = 0.3;

/// Hour-of-week slot (UTC): weekday × 24 + hour, 0..168.
pub fn demand_slot(t: OffsetDateTime) -> i16 {
    t.weekday().number_days_from_monday() as i16 * 24 + t.hour() as i16
}

/// Allocations/hour expected for a slot. `fresh`: the slot is the one we are in now, so `samples`
/// is still counting and is a lower bound; otherwise fold the last occurrence into the EWMA.
pub fn slot_estimate(ewma: f64, samples: i32, fresh: bool) -> f64 {
    if fresh {
        ewma.max(samples as f64)
    } else {
        ewma * (1.0 - DEMAND_ALPHA) + samples as f64 * DEMAND_ALPHA
    }
}

/// Ready servers to hold: the buffer, or enough for `per_hour` allocations over the horizon.
pub fn target_ready(policy: &ScalePolicy, buffer: i32, per_hour: f64) -> i32 {
    let forecast = if policy.forecast {
        (per_hour * policy.horizon_min as f64 / 60.0).ceil() as i32
    } else {
        0
    };
    buffer.max(forecast)
}

/// Pure sizing step → (servers to start, idle ready servers to stop). `available` = ready +
/// starting + scheduled, `live` = everything not terminated. Never below `min`, never above `max`.
pub fn plan_scale(
    target: i32,
    min: i32,
    max: i32,
    live: i32,
    available: i32,
    idle_ready: i32,
) -> (i32, i32) {
    let start = (target - available).max(min - live).min(max - live).max(0);
    let stop = if start > 0 {
        0
    } else {
        (available - target).min(idle_ready).min(live - min).max(0)
    };
    (start, stop)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemandSlot {
    pub slot: i16,
    pub ewma: f64,
    pub samples: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fleet {
    pub id: Uuid,
    pub name: String,
    pub provider: String,
    pub region: String,
    /// Routing group: `allocate_routed` picks any fleet of the pool, nearest region first.
    pub pool: String,
    pub spec: FleetSpec,
    pub buffer: i32,
    pub max_servers: i32,
    pub min_servers: i32,
    pub policy: ScalePolicy,
    pub port_start: i32,
    pub port_end: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CreateFleet {
    pub name: String,
    pub provider: String,
    pub region: String,
    pub pool: String,
    pub spec: FleetSpec,
    pub buffer: i32,
    pub max_servers: i32,
    pub min_servers: i32,
    pub policy: ScalePolicy,
    pub port_start: i32,
    pub port_end: i32,
}

impl Default for CreateFleet {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: "local".into(),
            region: String::new(),
            pool: String::new(),
            spec: FleetSpec::default(),
            buffer: 0,
            max_servers: 1,
            min_servers: 0,
            policy: ScalePolicy::default(),
            port_start: 7777,
            port_end: 7877,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameServer {
    pub id: Uuid,
    pub fleet_id: Uuid,
    pub state: String,
    pub node_id: Option<Uuid>,
    pub address: String,
    pub port: i32,
    pub pid: Option<i64>,
    pub labels: serde_json::Value,
    pub allocation: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub last_health: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    /// Fleet build this server was started from (`None` = the fleet's own spec).
    pub build_version: Option<String>,
    /// Occupancy reported by the server (`POST /v1/gameserver/players`); `capacity` 0 = unknown.
    #[serde(default)]
    pub players: i32,
    #[serde(default)]
    pub capacity: i32,
    /// Change counter, bumped by every update; `GET /v1/gameserver/watch` long-polls on it.
    #[serde(default)]
    pub version: i64,
}

/// A versioned build of a fleet: `active` is what new servers run, a `canary` takes
/// `canary_percent` of new servers (optionally only in `canary_regions`), `retired` is kept for rollback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetBuild {
    pub fleet_id: Uuid,
    pub version: String,
    pub spec: FleetSpec,
    pub state: String,
    pub canary_percent: i32,
    pub canary_regions: Vec<String>,
    pub notes: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CreateBuild {
    pub version: String,
    pub spec: FleetSpec,
    /// > 0 makes this a canary taking that share of new servers; 0 makes it the active build at once.
    pub canary_percent: i32,
    pub canary_regions: Vec<String>,
    pub notes: String,
}

/// vCPU-hours consumed by a fleet's servers inside a window.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRow {
    pub region: String,
    pub provider: String,
    pub build_version: Option<String>,
    pub servers: i64,
    pub vcpu_hours: f64,
}

type BuildRow = (
    Uuid,
    String,
    serde_json::Value,
    String,
    i32,
    Vec<String>,
    String,
    OffsetDateTime,
);

fn row_to_build(r: BuildRow) -> FleetBuild {
    FleetBuild {
        fleet_id: r.0,
        version: r.1,
        spec: serde_json::from_value(r.2).unwrap_or_default(),
        state: r.3,
        canary_percent: r.4,
        canary_regions: r.5,
        notes: r.6,
        created_at: r.7,
    }
}

const BUILD_COLS: &str =
    "fleet_id, version, spec, state, canary_percent, canary_regions, notes, created_at";

/// What a client gets back from an allocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub server_id: Uuid,
    pub fleet_id: Uuid,
    pub address: String,
    pub port: i32,
    /// Joined a live server with free slots instead of starting on a fresh one.
    #[serde(default)]
    pub backfill: bool,
    /// Token the game server validates through `/v1/gameserver/connect` (shared by the group).
    pub connect_token: String,
}

#[derive(Debug)]
pub struct Started {
    pub address: String,
    pub port: i32,
    pub pid: Option<i64>,
}

pub struct StartRequest<'a> {
    pub fleet: &'a Fleet,
    pub server_id: Uuid,
    pub port: i32,
    /// Base URL of this node's HTTP API and the server key, exposed to the game server.
    pub api_url: &'a str,
    pub server_key: &'a str,
}

/// How servers are started and stopped. Implementations must be cancel-safe and idempotent.
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>>;
    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>>;
    /// `Some(false)` when the provider knows the process/container is gone.
    fn alive(&self, server_id: Uuid) -> Option<bool>;
}

/// Child processes on this node.
#[derive(Default)]
pub struct LocalProcessProvider {
    children: DashMap<Uuid, Arc<tokio::sync::Mutex<tokio::process::Child>>>,
    exited: DashMap<Uuid, i32>,
}

impl Provider for LocalProcessProvider {
    fn name(&self) -> &'static str {
        "local"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let Some((bin, args)) = req.fleet.spec.command.split_first() else {
                return Err(Error::InvalidArgument("fleet spec.command is empty".into()));
            };
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(args)
                .envs(&req.fleet.spec.env)
                .env("MUSUBI_SERVER_ID", req.server_id.to_string())
                .env("MUSUBI_FLEET_ID", req.fleet.id.to_string())
                .env("MUSUBI_PORT", req.port.to_string())
                .env("MUSUBI_API", req.api_url)
                .env("MUSUBI_SERVER_KEY", req.server_key)
                .kill_on_drop(true)
                .stdin(std::process::Stdio::null());
            if let Some(d) = &req.fleet.spec.working_dir {
                cmd.current_dir(d);
            }
            let child = cmd
                .spawn()
                .map_err(|e| Error::Internal(format!("spawn {bin}: {e}")))?;
            let pid = child.id().map(|p| p as i64);
            let child = Arc::new(tokio::sync::Mutex::new(child));
            self.children.insert(req.server_id, child.clone());
            Ok(Started {
                address: "127.0.0.1".into(),
                port: req.port,
                pid,
            })
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some((_, child)) = self.children.remove(&server_id) {
                let mut c = child.lock().await;
                let _ = c.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(5), c.wait()).await;
            }
        })
    }

    fn alive(&self, server_id: Uuid) -> Option<bool> {
        let child = self.children.get(&server_id)?;
        let mut guard = child.try_lock().ok()?;
        match guard.try_wait() {
            Ok(Some(status)) => {
                self.exited.insert(server_id, status.code().unwrap_or(-1));
                Some(false)
            }
            Ok(None) => Some(true),
            Err(_) => None,
        }
    }
}

#[derive(Clone)]
pub struct FleetService {
    db: PgPool,
    node_id: Uuid,
    providers: Arc<std::sync::RwLock<HashMap<String, Arc<dyn Provider>>>>,
    api_url: String,
    server_key: String,
    /// Public address of the UDP edge proxy; when set, allocations hand clients this address
    /// instead of the game server's own (the proxy routes by connect token).
    edge: Option<SocketAddr>,
}

type FleetRow = (
    Uuid,
    String,
    String,
    String,
    serde_json::Value,
    i32,
    i32,
    i32,
    i32,
    OffsetDateTime,
    i32,
    serde_json::Value,
    String,
);

fn row_to_fleet(r: FleetRow) -> Fleet {
    Fleet {
        id: r.0,
        name: r.1,
        provider: r.2,
        region: r.3,
        spec: serde_json::from_value(r.4).unwrap_or_default(),
        buffer: r.5,
        max_servers: r.6,
        port_start: r.7,
        port_end: r.8,
        created_at: r.9,
        min_servers: r.10,
        policy: serde_json::from_value(r.11).unwrap_or_default(),
        pool: r.12,
    }
}

type ServerRow = (
    Uuid,
    Uuid,
    String,
    Option<Uuid>,
    String,
    i32,
    Option<i64>,
    serde_json::Value,
    Option<serde_json::Value>,
    OffsetDateTime,
    OffsetDateTime,
    Option<String>,
    i32,
    i32,
    i64,
);

fn row_to_server(r: ServerRow) -> GameServer {
    GameServer {
        id: r.0,
        fleet_id: r.1,
        state: r.2,
        node_id: r.3,
        address: r.4,
        port: r.5,
        pid: r.6,
        labels: r.7,
        allocation: r.8,
        last_health: r.9,
        created_at: r.10,
        build_version: r.11,
        players: r.12,
        capacity: r.13,
        version: r.14,
    }
}

const FLEET_COLS: &str =
    "id, name, provider, region, spec, buffer, max_servers, port_start, port_end, created_at, min_servers, policy, pool";
const SERVER_COLS: &str =
    "id, fleet_id, state, node_id, address, port, pid, labels, allocation, last_health, created_at, build_version, players, capacity, version";

impl FleetService {
    pub fn new(db: PgPool, node_id: Uuid, api_url: &str, server_key: &str) -> Self {
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        providers.insert("local".into(), Arc::new(LocalProcessProvider::default()));
        providers.insert("docker".into(), Arc::new(docker::DockerProvider));
        Self {
            db,
            node_id,
            providers: Arc::new(std::sync::RwLock::new(providers)),
            api_url: api_url.to_string(),
            server_key: server_key.to_string(),
            edge: None,
        }
    }

    /// Hide game servers behind the UDP edge proxy reachable at `addr` (plan 05: clients never see
    /// server IPs).
    pub fn with_edge(mut self, addr: Option<SocketAddr>) -> Self {
        self.edge = addr;
        self
    }

    pub fn edge(&self) -> Option<SocketAddr> {
        self.edge
    }

    /// Edge proxy routing: the game server an allocated/in-game connect token belongs to.
    pub async fn route_token(&self, token: &str) -> Option<SocketAddr> {
        let r: (String, i32) = sqlx::query_as(
            "select address, port from musubi.game_servers
             where connect_token = $1 and state in ('allocated', 'in_game') and address <> ''
             order by updated_at desc limit 1",
        )
        .bind(token)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| tracing::warn!(error = %e, "edge route lookup failed"))
        .ok()
        .flatten()?;
        let ip: std::net::IpAddr = r.0.parse().ok()?;
        Some(SocketAddr::new(ip, u16::try_from(r.1).ok()?))
    }

    /// Register an extra provider under `name` (e.g. `kubernetes` / `agones` when configured).
    pub fn with_provider(self, name: &str, provider: Arc<dyn Provider>) -> Self {
        self.register_provider(name, provider);
        self
    }

    /// Add (or replace) a provider at runtime.
    pub fn register_provider(&self, name: &str, provider: Arc<dyn Provider>) {
        self.providers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_string(), provider);
    }

    fn provider_of(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// Allocate from a pool (or a fleet name): fleets in the hinted `region` first, then the
    /// rest; with `wait` > 0 and nothing ready anywhere, a server is started in the best fleet
    /// that has room and the call waits for it (scale-to-zero cold start).
    pub async fn allocate_routed(
        &self,
        target: &str,
        region: Option<&str>,
        payload: serde_json::Value,
        backfill: bool,
        wait: std::time::Duration,
    ) -> Result<Allocation> {
        let mut fleets: Vec<Fleet> = self
            .list()
            .await?
            .into_iter()
            .filter(|f| f.name == target || (!f.pool.is_empty() && f.pool == target))
            .collect();
        if fleets.is_empty() {
            return Err(Error::NotFound(format!("no fleet or pool {target}")));
        }
        if let Some(r) = region {
            fleets.sort_by_key(|f| if f.region == r { 0 } else { 1 });
        }
        for f in &fleets {
            match self.allocate(f.id, payload.clone(), backfill).await {
                Ok(a) => return Ok(a),
                Err(Error::ResourceExhausted(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        if wait.is_zero() {
            return Err(Error::ResourceExhausted(format!(
                "no ready server in {target}"
            )));
        }
        for f in &fleets {
            let live = self
                .servers(f.id, None)
                .await?
                .into_iter()
                .filter(|s| {
                    matches!(
                        s.state.as_str(),
                        "starting" | "ready" | "allocated" | "in_game"
                    )
                })
                .count() as i32;
            if live >= f.max_servers {
                continue;
            }
            let started = self.start_one(f).await?;
            let deadline = tokio::time::Instant::now() + wait;
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                match self.allocate(f.id, payload.clone(), backfill).await {
                    Ok(a) => return Ok(a),
                    Err(Error::ResourceExhausted(_)) => {}
                    Err(e) => return Err(e),
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::ResourceExhausted(format!(
                        "no ready server in {target}: started {} in fleet {}, not ready after {}s",
                        started.id,
                        f.name,
                        wait.as_secs()
                    )));
                }
            }
        }
        Err(Error::ResourceExhausted(format!(
            "no ready server in {target} and every fleet is at max_servers"
        )))
    }

    pub fn providers(&self) -> Vec<&'static str> {
        self.providers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|p| p.name())
            .collect()
    }

    pub async fn create(&self, c: CreateFleet) -> Result<Fleet> {
        if c.name.is_empty() || c.name.len() > 64 {
            return Err(Error::InvalidArgument("fleet name 1..=64 chars".into()));
        }
        if self.provider_of(&c.provider).is_none() {
            return Err(Error::InvalidArgument(format!(
                "unknown provider {}",
                c.provider
            )));
        }
        validate_shape(c.min_servers, c.buffer, c.max_servers, &c.policy)?;
        if c.port_start < 1 || c.port_end < c.port_start || c.port_end > 65535 {
            return Err(Error::InvalidArgument("bad port range".into()));
        }
        let id = Uuid::now_v7();
        let r = sqlx::query_as::<_, FleetRow>(&format!(
            "insert into musubi.fleets (id, name, provider, region, spec, buffer, max_servers, port_start, port_end, min_servers, policy, pool)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) returning {FLEET_COLS}"
        ))
        .bind(id)
        .bind(&c.name)
        .bind(&c.provider)
        .bind(&c.region)
        .bind(serde_json::to_value(&c.spec)?)
        .bind(c.buffer)
        .bind(c.max_servers)
        .bind(c.port_start)
        .bind(c.port_end)
        .bind(c.min_servers)
        .bind(serde_json::to_value(&c.policy)?)
        .bind(&c.pool)
        .fetch_one(&self.db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => Error::Conflict("fleet name taken".into()),
            e => Error::internal(e),
        })?;
        Ok(row_to_fleet(r))
    }

    pub async fn list(&self) -> Result<Vec<Fleet>> {
        let rows = sqlx::query_as(&format!(
            "select {FLEET_COLS} from musubi.fleets order by created_at"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_fleet).collect())
    }

    pub async fn get(&self, id: Uuid) -> Result<Fleet> {
        sqlx::query_as(&format!(
            "select {FLEET_COLS} from musubi.fleets where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(row_to_fleet)
        .ok_or_else(|| Error::NotFound("fleet".into()))
    }

    pub async fn get_by_name(&self, name: &str) -> Result<Fleet> {
        sqlx::query_as(&format!(
            "select {FLEET_COLS} from musubi.fleets where name = $1"
        ))
        .bind(name)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(row_to_fleet)
        .ok_or_else(|| Error::NotFound(format!("fleet {name}")))
    }

    /// Change buffer / max_servers / min_servers / policy; the reconciler follows.
    pub async fn scale(
        &self,
        id: Uuid,
        buffer: Option<i32>,
        max_servers: Option<i32>,
        min_servers: Option<i32>,
        policy: Option<ScalePolicy>,
    ) -> Result<Fleet> {
        let f = self.get(id).await?;
        let buffer = buffer.unwrap_or(f.buffer);
        let max = max_servers.unwrap_or(f.max_servers);
        let min = min_servers.unwrap_or(f.min_servers);
        let policy = policy.unwrap_or(f.policy);
        validate_shape(min, buffer, max, &policy)?;
        sqlx::query(
            "update musubi.fleets set buffer = $2, max_servers = $3, min_servers = $4, policy = $5, updated_at = now() where id = $1",
        )
            .bind(id)
            .bind(buffer)
            .bind(max)
            .bind(min)
            .bind(serde_json::to_value(&policy)?)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.get(id).await
    }

    /// Stop every server of the fleet, then delete it.
    pub async fn delete(&self, id: Uuid) -> Result<()> {
        let f = self.get(id).await?;
        for s in self.servers(id, None).await? {
            self.terminate(&f, s.id).await;
        }
        sqlx::query("delete from musubi.fleets where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn servers(&self, fleet_id: Uuid, state: Option<&str>) -> Result<Vec<GameServer>> {
        let rows = sqlx::query_as::<_, ServerRow>(&format!(
            "select {SERVER_COLS} from musubi.game_servers where fleet_id = $1 and ($2::text is null or state = $2) order by created_at"
        ))
        .bind(fleet_id)
        .bind(state)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_server).collect())
    }

    pub async fn server(&self, id: Uuid) -> Result<GameServer> {
        sqlx::query_as::<_, ServerRow>(&format!(
            "select {SERVER_COLS} from musubi.game_servers where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .map(row_to_server)
        .ok_or_else(|| Error::NotFound("game server".into()))
    }

    async fn set_state(&self, id: Uuid, from: &[&str], to: &str) -> Result<GameServer> {
        let froms: Vec<String> = from.iter().map(|s| s.to_string()).collect();
        let r = sqlx::query_as::<_, ServerRow>(&format!(
            "update musubi.game_servers set state = $2, updated_at = now(), last_health = now(),
               ready_at = case when $2 = 'ready' then coalesce(ready_at, now()) else ready_at end,
               terminated_at = case when $2 = 'terminated' then coalesce(terminated_at, now()) else terminated_at end
             where id = $1 and (cardinality($3::text[]) = 0 or state = any($3)) returning {SERVER_COLS}"
        ))
        .bind(id)
        .bind(to)
        .bind(&froms)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        match r {
            Some(r) => Ok(row_to_server(r)),
            None => {
                let cur = self.server(id).await?;
                Err(Error::Conflict(format!(
                    "server is {}, cannot move to {to}",
                    cur.state
                )))
            }
        }
    }

    // ----- SDK surface (called by the game server itself) -----

    pub async fn sdk_ready(&self, id: Uuid) -> Result<GameServer> {
        self.set_state(id, &["scheduled", "starting", "ready"], "ready")
            .await
    }

    pub async fn sdk_health(&self, id: Uuid) -> Result<()> {
        sqlx::query("update musubi.game_servers set last_health = now() where id = $1 and state not in ('terminated')")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Self-allocation (the server decided it is in use, e.g. a player connected directly).
    pub async fn sdk_allocate(&self, id: Uuid) -> Result<GameServer> {
        self.set_state(id, &["ready", "allocated"], "allocated")
            .await
    }

    pub async fn sdk_labels(&self, id: Uuid, labels: serde_json::Value) -> Result<GameServer> {
        if !labels.is_object() || labels.to_string().len() > 4096 {
            return Err(Error::InvalidArgument(
                "labels must be an object ≤ 4 KB".into(),
            ));
        }
        sqlx::query("update musubi.game_servers set labels = labels || $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(labels)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.server(id).await
    }

    /// Occupancy report for backfill: `players` in game now, `capacity` total slots (kept when `None`).
    pub async fn sdk_players(
        &self,
        id: Uuid,
        players: i32,
        capacity: Option<i32>,
    ) -> Result<GameServer> {
        if players < 0 || capacity.is_some_and(|c| c < 0) {
            return Err(Error::InvalidArgument(
                "players/capacity must be >= 0".into(),
            ));
        }
        sqlx::query(
            "update musubi.game_servers set players = $2, capacity = coalesce($3, capacity), updated_at = now()
             where id = $1",
        )
        .bind(id)
        .bind(players)
        .bind(capacity)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        self.server(id).await
    }

    /// WatchGameServer: return the record as soon as its `version` passes `since` (or at once
    /// without `since`), else after `budget` with whatever is current.
    pub async fn watch(
        &self,
        id: Uuid,
        since: Option<i64>,
        budget: Duration,
    ) -> Result<GameServer> {
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            let gs = self.server(id).await?;
            if since.is_none_or(|v| gs.version > v) || tokio::time::Instant::now() >= deadline {
                return Ok(gs);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Validate the allocation's connect token (shared by the whole group; the game server calls
    /// this when a client arrives). The first valid connect moves the server to `in_game`.
    pub async fn sdk_connect(&self, id: Uuid, token: &str) -> Result<serde_json::Value> {
        let r: Option<Option<serde_json::Value>> = sqlx::query_scalar(
            "update musubi.game_servers set state = case when state = 'allocated' then 'in_game' else state end, updated_at = now()
             where id = $1 and connect_token = $2 and state in ('allocated', 'in_game') returning allocation",
        )
        .bind(id)
        .bind(token)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        r.flatten()
            .ok_or_else(|| Error::PermissionDenied("bad connect token".into()))
    }

    pub async fn sdk_shutdown(&self, id: Uuid) -> Result<()> {
        let s = self.server(id).await?;
        let f = self.get(s.fleet_id).await?;
        self.terminate(&f, id).await;
        Ok(())
    }

    // ----- control surface -----

    /// Hand a ready server to a caller; `payload` travels to the game server with the connect token.
    /// With `backfill`, a live (`allocated`/`in_game`) server that reported free slots is reused
    /// — same connect token, so the newcomers join the running match — before a `ready` one is taken.
    pub async fn allocate(
        &self,
        fleet_id: Uuid,
        payload: serde_json::Value,
        backfill: bool,
    ) -> Result<Allocation> {
        if backfill {
            let r = sqlx::query_as::<_, (Uuid, String, i32, Option<String>)>(
                "update musubi.game_servers set allocation = allocation || jsonb_build_object('backfill',
                        coalesce(allocation->'backfill', '[]'::jsonb) || jsonb_build_array($2::jsonb)),
                     updated_at = now()
                 where id = (select id from musubi.game_servers
                             where fleet_id = $1 and state in ('allocated', 'in_game') and capacity > 0
                               and players < capacity and connect_token is not null
                             order by (capacity - players) desc, created_at limit 1 for update skip locked)
                 returning id, address, port, connect_token",
            )
            .bind(fleet_id)
            .bind(serde_json::json!({ "payload": payload, "at": OffsetDateTime::now_utc().unix_timestamp() }))
            .fetch_optional(&self.db)
            .await
            .map_err(Error::internal)?;
            if let Some((id, address, port, Some(token))) = r {
                let (address, port) = match self.edge {
                    Some(e) => (e.ip().to_string(), e.port() as i32),
                    None => (address, port),
                };
                return Ok(Allocation {
                    server_id: id,
                    fleet_id,
                    address,
                    port,
                    connect_token: token,
                    backfill: true,
                });
            }
        }
        let token = format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple());
        let alloc = serde_json::json!({ "payload": payload, "at": OffsetDateTime::now_utc().unix_timestamp() });
        let r = sqlx::query_as::<_, (Uuid, String, i32)>(
            "update musubi.game_servers set state = 'allocated', allocation = $2, connect_token = $3, updated_at = now()
             where id = (select id from musubi.game_servers where fleet_id = $1 and state = 'ready'
                         order by created_at limit 1 for update skip locked)
             returning id, address, port",
        )
        .bind(fleet_id)
        .bind(alloc)
        .bind(&token)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::ResourceExhausted("no ready server in fleet".into()))?;
        if let Err(e) = self
            .record_demand(fleet_id, OffsetDateTime::now_utc())
            .await
        {
            tracing::warn!(fleet = %fleet_id, error = %e, "fleet demand not recorded");
        }
        let (address, port) = match self.edge {
            Some(e) => (e.ip().to_string(), e.port() as i32),
            None => (r.1, r.2),
        };
        Ok(Allocation {
            server_id: r.0,
            fleet_id,
            address,
            port,
            connect_token: token,
            backfill: false,
        })
    }

    /// Count one allocation at `at` into its hour-of-week slot. A slot not touched for 23 h is a
    /// new occurrence: the previous count is folded into the EWMA and counting restarts.
    pub async fn record_demand(&self, fleet_id: Uuid, at: OffsetDateTime) -> Result<()> {
        sqlx::query(
            "insert into musubi.fleet_demand (fleet_id, slot, ewma, samples, updated_at) values ($1, $2, 0, 1, $3)
             on conflict (fleet_id, slot) do update set
               ewma = case when musubi.fleet_demand.updated_at < $3 - interval '23 hours'
                           then musubi.fleet_demand.ewma * $4 + musubi.fleet_demand.samples * $5
                           else musubi.fleet_demand.ewma end,
               samples = case when musubi.fleet_demand.updated_at < $3 - interval '23 hours'
                              then 1 else musubi.fleet_demand.samples + 1 end,
               updated_at = greatest(musubi.fleet_demand.updated_at, $3)",
        )
        .bind(fleet_id)
        .bind(demand_slot(at))
        .bind(at)
        .bind(1.0 - DEMAND_ALPHA)
        .bind(DEMAND_ALPHA)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Demand history of a fleet (one row per hour-of-week slot that saw allocations).
    pub async fn demand(&self, fleet_id: Uuid) -> Result<Vec<DemandSlot>> {
        let rows = sqlx::query_as::<_, (i16, f64, i32, OffsetDateTime)>(
            "select slot, ewma, samples, updated_at from musubi.fleet_demand where fleet_id = $1 order by slot",
        )
        .bind(fleet_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| DemandSlot {
                slot: r.0,
                ewma: r.1,
                samples: r.2,
                updated_at: r.3,
            })
            .collect())
    }

    /// Expected allocations/hour for the slot `now` falls in (0 when the slot has no history).
    pub async fn forecast_per_hour(&self, fleet_id: Uuid, now: OffsetDateTime) -> Result<f64> {
        let r = sqlx::query_as::<_, (f64, i32, OffsetDateTime)>(
            "select ewma, samples, updated_at from musubi.fleet_demand where fleet_id = $1 and slot = $2",
        )
        .bind(fleet_id)
        .bind(demand_slot(now))
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.map(|(ewma, samples, at)| {
            slot_estimate(ewma, samples, now - at < time::Duration::hours(23))
        })
        .unwrap_or(0.0))
    }

    pub async fn terminate(&self, fleet: &Fleet, id: Uuid) {
        if let Some(p) = self.provider_of(&fleet.provider) {
            p.stop(id).await;
        }
        let _ = sqlx::query(
            "update musubi.game_servers set state = 'terminated', terminated_at = coalesce(terminated_at, now()), updated_at = now() where id = $1",
        )
        .bind(id)
        .execute(&self.db)
        .await;
    }

    async fn free_port(&self, f: &Fleet) -> Result<i32> {
        let used: Vec<i32> = sqlx::query_scalar(
            "select port from musubi.game_servers where node_id = $1 and state <> 'terminated'",
        )
        .bind(self.node_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        (f.port_start..=f.port_end)
            .find(|p| !used.contains(p))
            .ok_or_else(|| {
                Error::ResourceExhausted("fleet port range exhausted on this node".into())
            })
    }

    /// Start one server of the fleet on this node.
    pub async fn start_one(&self, f: &Fleet) -> Result<GameServer> {
        let provider = self
            .provider_of(&f.provider)
            .ok_or_else(|| Error::InvalidArgument(format!("unknown provider {}", f.provider)))?;
        let id = Uuid::now_v7();
        let port = self.free_port(f).await?;
        let build = self.pick_build(f).await?;
        let mut with_build = f.clone();
        if let Some(b) = &build {
            with_build.spec = b.spec.clone();
        }
        let f = &with_build;
        sqlx::query("insert into musubi.game_servers (id, fleet_id, state, node_id, port, build_version) values ($1, $2, 'starting', $3, $4, $5)")
            .bind(id)
            .bind(f.id)
            .bind(self.node_id)
            .bind(port)
            .bind(build.as_ref().map(|b| b.version.clone()))
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        match provider
            .start(StartRequest {
                fleet: f,
                server_id: id,
                port,
                api_url: &self.api_url,
                server_key: &self.server_key,
            })
            .await
        {
            Ok(st) => {
                sqlx::query("update musubi.game_servers set address = $2, port = $3, pid = $4, updated_at = now() where id = $1")
                    .bind(id)
                    .bind(&st.address)
                    .bind(st.port)
                    .bind(st.pid)
                    .execute(&self.db)
                    .await
                    .map_err(Error::internal)?;
                self.server(id).await
            }
            Err(e) => {
                let _ = sqlx::query("update musubi.game_servers set state = 'terminated', terminated_at = coalesce(terminated_at, now()), labels = labels || jsonb_build_object('error', $2::text) where id = $1")
                    .bind(id)
                    .bind(e.to_string())
                    .execute(&self.db)
                    .await;
                Err(e)
            }
        }
    }

    /// Which build a new server of `f` should run: the canary with probability `canary_percent`
    /// (when its regions allow the fleet's region), else the active build, else none (fleet spec).
    async fn pick_build(&self, f: &Fleet) -> Result<Option<FleetBuild>> {
        let builds = self.builds(f.id).await?;
        let canary = builds.iter().find(|b| {
            b.state == "canary"
                && b.canary_percent > 0
                && (b.canary_regions.is_empty() || b.canary_regions.iter().any(|r| r == &f.region))
        });
        if let Some(c) = canary {
            let roll = (Uuid::now_v7().as_bytes()[15] % 100) as i32;
            if roll < c.canary_percent {
                return Ok(Some(c.clone()));
            }
        }
        Ok(builds.into_iter().find(|b| b.state == "active"))
    }

    pub async fn builds(&self, fleet_id: Uuid) -> Result<Vec<FleetBuild>> {
        let rows: Vec<BuildRow> = sqlx::query_as(&format!(
            "select {BUILD_COLS} from musubi.fleet_builds where fleet_id = $1 order by created_at desc"
        ))
        .bind(fleet_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_build).collect())
    }

    /// Register a build. `canary_percent` 0 makes it active right away (the previous active build
    /// is retired, kept for rollback); > 0 makes it a canary alongside the current active build.
    pub async fn add_build(&self, fleet_id: Uuid, c: CreateBuild) -> Result<FleetBuild> {
        let v = c.version.trim();
        if v.is_empty()
            || v.len() > 64
            || !v
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
        {
            return Err(Error::InvalidArgument(
                "version must be 1..=64 of [A-Za-z0-9._-+]".into(),
            ));
        }
        if !(0..=100).contains(&c.canary_percent) {
            return Err(Error::InvalidArgument(
                "canary_percent must be 0..=100".into(),
            ));
        }
        self.get(fleet_id).await?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let state = if c.canary_percent > 0 {
            "canary"
        } else {
            "active"
        };
        if state == "active" {
            sqlx::query("update musubi.fleet_builds set state = 'retired', updated_at = now() where fleet_id = $1 and state = 'active'")
                .bind(fleet_id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        }
        let ins = sqlx::query_as::<_, BuildRow>(&format!(
            "insert into musubi.fleet_builds (fleet_id, version, spec, state, canary_percent, canary_regions, notes)
             values ($1, $2, $3, $4, $5, $6, $7) returning {BUILD_COLS}"
        ))
        .bind(fleet_id)
        .bind(v)
        .bind(serde_json::to_value(&c.spec).map_err(Error::internal)?)
        .bind(state)
        .bind(c.canary_percent)
        .bind(&c.canary_regions)
        .bind(&c.notes)
        .fetch_one(&mut *tx)
        .await;
        let row = match ins {
            Ok(r) => r,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(Error::AlreadyExists(format!("build {v} exists")))
            }
            Err(e) => return Err(Error::internal(e)),
        };
        tx.commit().await.map_err(Error::internal)?;
        Ok(row_to_build(row))
    }

    /// Make `version` the active build (promote a canary, or roll back to a retired build);
    /// every other build is retired. Running servers are not touched.
    pub async fn promote_build(&self, fleet_id: Uuid, version: &str) -> Result<FleetBuild> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<BuildRow> = sqlx::query_as(&format!(
            "update musubi.fleet_builds set state = 'active', canary_percent = 0, updated_at = now()
             where fleet_id = $1 and version = $2 returning {BUILD_COLS}"
        ))
        .bind(fleet_id)
        .bind(version)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(row) = row else {
            return Err(Error::NotFound("build".into()));
        };
        sqlx::query("update musubi.fleet_builds set state = 'retired', updated_at = now() where fleet_id = $1 and version <> $2 and state <> 'retired'")
            .bind(fleet_id)
            .bind(version)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(row_to_build(row))
    }

    /// Retire a canary or an old build; the active build cannot be retired (promote another first).
    pub async fn retire_build(&self, fleet_id: Uuid, version: &str) -> Result<()> {
        let state: Option<String> = sqlx::query_scalar(
            "select state from musubi.fleet_builds where fleet_id = $1 and version = $2",
        )
        .bind(fleet_id)
        .bind(version)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        match state.as_deref() {
            None => Err(Error::NotFound("build".into())),
            Some("active") => Err(Error::Conflict("cannot retire the active build".into())),
            Some(_) => {
                sqlx::query("update musubi.fleet_builds set state = 'retired', canary_percent = 0, updated_at = now() where fleet_id = $1 and version = $2")
                    .bind(fleet_id)
                    .bind(version)
                    .execute(&self.db)
                    .await
                    .map_err(Error::internal)?;
                Ok(())
            }
        }
    }

    /// vCPU-hours of this fleet's servers overlapping [from, to), by build: a server counts from
    /// its start until termination (or `to` while still running) × the spec's vCPUs (default 1).
    pub async fn usage(
        &self,
        fleet_id: Uuid,
        from: OffsetDateTime,
        to: OffsetDateTime,
    ) -> Result<Vec<UsageRow>> {
        let f = self.get(fleet_id).await?;
        let builds = self.builds(fleet_id).await?;
        let rows: Vec<(Option<String>, i64, f64)> = sqlx::query_as(
            "select build_version, count(*),
                    coalesce(sum(extract(epoch from (least(coalesce(terminated_at, now()), $3) - greatest(created_at, $2)))), 0)::float8 / 3600
             from musubi.game_servers
             where fleet_id = $1 and created_at < $3 and coalesce(terminated_at, now()) > $2
             group by build_version order by build_version",
        )
        .bind(fleet_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(bv, servers, hours)| {
                let vcpu = bv
                    .as_deref()
                    .and_then(|v| builds.iter().find(|b| b.version == v))
                    .map(|b| b.spec.vcpu)
                    .unwrap_or(f.spec.vcpu)
                    .unwrap_or(1.0);
                UsageRow {
                    region: f.region.clone(),
                    provider: f.provider.clone(),
                    build_version: bv,
                    servers,
                    vcpu_hours: hours * vcpu,
                }
            })
            .collect())
    }

    /// One reconcile pass over every fleet: reap dead/unhealthy servers, then size the ready pool to
    /// the target (`buffer`, schedule, forecast; floor `min_servers`, cap `max_servers`): start what is
    /// missing or stop idle ready servers above it, oldest first. Returns (started, reaped + stopped).
    pub async fn reconcile(&self) -> Result<(usize, usize)> {
        let mut started = 0;
        let mut reaped = 0;
        for f in self.list().await? {
            let provider = self.provider_of(&f.provider);
            let servers = self.servers(f.id, None).await?;
            let now = OffsetDateTime::now_utc();
            for s in servers.iter().filter(|s| s.state != "terminated") {
                let mine = s.node_id == Some(self.node_id);
                let dead_process =
                    mine && provider.as_ref().and_then(|p| p.alive(s.id)) == Some(false);
                let stale = now - s.last_health > HEALTH_TIMEOUT && s.state != "starting";
                let stuck_starting =
                    s.state == "starting" && now - s.created_at > Duration::from_secs(120);
                if dead_process || stale || stuck_starting {
                    tracing::warn!(server = %s.id, fleet = %f.name, state = %s.state, dead_process, stale, "reaping game server");
                    self.terminate(&f, s.id).await;
                    reaped += 1;
                }
            }
            let servers = self.servers(f.id, None).await?;
            let live = servers.iter().filter(|s| s.state != "terminated").count() as i32;
            let available = servers
                .iter()
                .filter(|s| s.state == "ready" || s.state == "starting" || s.state == "scheduled")
                .count() as i32;
            let idle_ready = servers.iter().filter(|s| s.state == "ready").count() as i32;
            let per_hour = if f.policy.forecast {
                self.forecast_per_hour(f.id, now).await.unwrap_or(0.0)
            } else {
                0.0
            };
            let target = target_ready(&f.policy, f.policy.buffer_at(now, f.buffer), per_hour);
            let (start, stop) = plan_scale(
                target,
                f.min_servers,
                f.max_servers,
                live,
                available,
                idle_ready,
            );
            for _ in 0..start {
                match self.start_one(&f).await {
                    Ok(_) => started += 1,
                    Err(e) => {
                        tracing::warn!(fleet = %f.name, error = %e, "could not start game server");
                        break;
                    }
                }
            }
            if stop > 0 {
                let mut idle: Vec<&GameServer> =
                    servers.iter().filter(|s| s.state == "ready").collect();
                idle.sort_by_key(|s| s.created_at);
                for s in idle.into_iter().take(stop as usize) {
                    tracing::info!(server = %s.id, fleet = %f.name, target, "scaling down idle game server");
                    self.terminate(&f, s.id).await;
                    reaped += 1;
                }
            }
        }
        Ok((started, reaped))
    }

    /// Background reconciler; `active` gates each pass (cluster-wide: only the lease holder runs it).
    pub fn spawn_reconciler(
        self: Arc<Self>,
        every: Duration,
        active: impl Fn() -> bool + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                if !active() {
                    continue;
                }
                if let Err(e) = self.reconcile().await {
                    tracing::warn!(error = %e, "fleet reconcile failed");
                }
            }
        })
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn schedule_windows_match_days_and_wrap_midnight() {
        let w = ScheduleWindow {
            days: vec![5, 6],
            from_hour: 22,
            to_hour: 2,
            buffer: 9,
        };
        assert!(w.matches(5, 23));
        assert!(w.matches(6, 1));
        assert!(!w.matches(6, 2));
        assert!(!w.matches(0, 23));
        let all_day = ScheduleWindow {
            from_hour: 0,
            to_hour: 0,
            ..Default::default()
        };
        assert!(all_day.matches(3, 12));
        let p = ScalePolicy {
            schedule: vec![w],
            ..Default::default()
        };
        // 2024-01-06 is a Saturday (weekday 5).
        assert_eq!(p.buffer_at(datetime!(2024-01-06 23:30 UTC), 1), 9);
        assert_eq!(p.buffer_at(datetime!(2024-01-06 12:00 UTC), 1), 1);
        assert_eq!(demand_slot(datetime!(2024-01-06 23:30 UTC)), 5 * 24 + 23);
    }

    #[test]
    fn forecast_sizes_ready_pool_from_demand() {
        let p = ScalePolicy {
            forecast: true,
            horizon_min: 10,
            ..Default::default()
        };
        assert_eq!(target_ready(&p, 1, 0.0), 1);
        assert_eq!(target_ready(&p, 1, 30.0), 5); // 30/h over 10 min = 5
        assert_eq!(target_ready(&ScalePolicy::default(), 1, 30.0), 1);
        assert_eq!(slot_estimate(4.0, 2, true), 4.0);
        assert_eq!(slot_estimate(4.0, 10, true), 10.0);
        assert!((slot_estimate(4.0, 10, false) - 5.8).abs() < 1e-9);
    }

    #[test]
    fn plan_respects_floor_and_cap_and_stops_only_idle() {
        assert_eq!(plan_scale(0, 2, 5, 0, 0, 0), (2, 0));
        assert_eq!(plan_scale(9, 0, 5, 3, 3, 3), (2, 0));
        assert_eq!(plan_scale(1, 3, 5, 4, 3, 3), (0, 1));
        assert_eq!(plan_scale(0, 0, 5, 4, 0, 0), (0, 0));
        assert_eq!(plan_scale(1, 0, 5, 3, 3, 1), (0, 1));
        assert_eq!(plan_scale(2, 0, 5, 2, 2, 2), (0, 0));
    }
}
