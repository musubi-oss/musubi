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
pub mod ssh;
pub mod webhook;

pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(30);

/// One captured game server log line (competitive P0-9).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogLine {
    pub server_id: Uuid,
    pub fleet_id: Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// `stdout` | `stderr` | `sdk`
    pub source: String,
    /// `debug` | `info` | `warn` | `error`
    pub level: String,
    pub message: String,
}

pub type LogSink = tokio::sync::mpsc::UnboundedSender<LogLine>;

pub const LOG_LINE_MAX: usize = 8 * 1024;

/// Level guessed from a raw stdout/stderr line (`[ERROR]`, `WARN:`, `level=debug`, …).
pub fn level_of(line: &str, source: &str) -> &'static str {
    let l = line.get(..96).unwrap_or(line).to_ascii_lowercase();
    for (needle, lvl) in [
        ("error", "error"),
        ("fatal", "error"),
        ("panic", "error"),
        ("warn", "warn"),
        ("debug", "debug"),
        ("trace", "debug"),
    ] {
        if l.contains(needle) {
            return lvl;
        }
    }
    if source == "stderr" {
        "warn"
    } else {
        "info"
    }
}

/// Read a child's stream line by line into the sink until EOF.
pub fn pump_lines<R>(
    reader: R,
    sink: LogSink,
    server_id: Uuid,
    fleet_id: Uuid,
    source: &'static str,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncBufReadExt;
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(reader).lines();
        while let Ok(Some(mut line)) = lines.next_line().await {
            if line.len() > LOG_LINE_MAX {
                let mut cut = LOG_LINE_MAX;
                while !line.is_char_boundary(cut) {
                    cut -= 1;
                }
                line.truncate(cut);
            }
            let level = level_of(&line, source).to_string();
            if sink
                .send(LogLine {
                    server_id,
                    fleet_id,
                    at: OffsetDateTime::now_utc(),
                    source: source.to_string(),
                    level,
                    message: line,
                })
                .is_err()
            {
                break;
            }
        }
    });
}

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
    /// Per-server resource specification handed to the provider (Kubernetes requests/limits,
    /// Docker `--cpus`/`--memory`, Firecracker machine config).
    pub resources: Resources,
}

/// Per-server resources (competitive P0-7). Unset fields leave the provider default.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Resources {
    /// CPU request in millicores (1000 = one core).
    pub cpu_millis: Option<u32>,
    /// CPU limit in millicores; defaults to the request.
    pub cpu_limit_millis: Option<u32>,
    /// Memory request in MiB.
    pub memory_mb: Option<u32>,
    /// Memory limit in MiB; defaults to the request.
    pub memory_limit_mb: Option<u32>,
    /// Ephemeral disk in MiB (Kubernetes `ephemeral-storage`, Firecracker rootfs size hint).
    pub disk_mb: Option<u32>,
    /// GPUs (`nvidia.com/gpu` on Kubernetes, `--gpus` on Docker).
    pub gpu: Option<u32>,
}

impl Resources {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Kubernetes `resources: {requests, limits}`; `None` when nothing is set.
    pub fn kubernetes(&self) -> Option<serde_json::Value> {
        if self.is_empty() {
            return None;
        }
        let mut req = serde_json::Map::new();
        let mut lim = serde_json::Map::new();
        if let Some(c) = self.cpu_millis {
            req.insert("cpu".into(), format!("{c}m").into());
        }
        if let Some(c) = self.cpu_limit_millis.or(self.cpu_millis) {
            lim.insert("cpu".into(), format!("{c}m").into());
        }
        if let Some(m) = self.memory_mb {
            req.insert("memory".into(), format!("{m}Mi").into());
        }
        if let Some(m) = self.memory_limit_mb.or(self.memory_mb) {
            lim.insert("memory".into(), format!("{m}Mi").into());
        }
        if let Some(d) = self.disk_mb {
            req.insert("ephemeral-storage".into(), format!("{d}Mi").into());
            lim.insert("ephemeral-storage".into(), format!("{d}Mi").into());
        }
        if let Some(g) = self.gpu.filter(|g| *g > 0) {
            lim.insert("nvidia.com/gpu".into(), g.to_string().into());
        }
        Some(serde_json::json!({ "requests": req, "limits": lim }))
    }

    /// Extra `docker run` flags.
    pub fn docker_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(c) = self.cpu_limit_millis.or(self.cpu_millis) {
            out.push("--cpus".into());
            out.push(format!("{:.3}", c as f64 / 1000.0));
        }
        if let Some(m) = self.memory_limit_mb.or(self.memory_mb) {
            out.push("--memory".into());
            out.push(format!("{m}m"));
        }
        if let Some(g) = self.gpu.filter(|g| *g > 0) {
            out.push("--gpus".into());
            out.push(g.to_string());
        }
        out
    }

    /// Firecracker machine config: (vcpus, MiB) when either is set.
    pub fn firecracker(&self) -> (Option<u32>, Option<u32>) {
        (
            self.cpu_limit_millis
                .or(self.cpu_millis)
                .map(|c| c.div_ceil(1000).max(1)),
            self.memory_limit_mb.or(self.memory_mb),
        )
    }
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
    /// Keep `allocated / (allocated + ready)` near this fraction (0 < u < 1): the ready pool is
    /// sized to `allocated × (1 − u) / u`, never below the buffer/forecast target.
    pub target_utilization: Option<f64>,
    /// Most servers started per reconcile pass (default: whatever the target needs).
    pub step_up: Option<i32>,
    /// Most idle servers stopped per reconcile pass.
    pub step_down: Option<i32>,
    /// Seconds after a scale-up before the next scale-up (0 = none).
    pub cooldown_up_secs: u64,
    /// Seconds after any scale action before a scale-down (0 = none).
    pub cooldown_down_secs: u64,
    /// Lifecycle guards (P1-12): a server is terminated once it has lived this long …
    pub max_duration_secs: Option<u64>,
    /// … or has sat `ready` without an allocation this long (never below `min_servers`) …
    pub idle_ttl_secs: Option<u64>,
    /// … or has been allocated / in game this long (runaway or zombie matches).
    pub allocated_max_secs: Option<u64>,
    /// How a promoted build reaches servers that are already running.
    pub rollout: RolloutPolicy,
}

/// Build rollout strategy (plan P1-29): `manual` only affects new servers; `rolling` replaces
/// `ready` servers of other builds while keeping at most `max_surge` extra servers and taking
/// away at most `max_unavailable` ready servers beyond those already replaced; `recreate`
/// replaces every ready server at once. `allocated` servers are never touched unless
/// `drain_allocated_after_secs` says a long-running allocation may be ended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RolloutPolicy {
    pub strategy: String,
    pub max_surge: i32,
    pub max_unavailable: i32,
    /// Re-promote the previous build after `failure_threshold` servers of the new one failed to
    /// start or died within two minutes of becoming ready.
    pub auto_rollback: bool,
    pub failure_threshold: i32,
    pub drain_allocated_after_secs: u64,
}

impl Default for RolloutPolicy {
    fn default() -> Self {
        Self {
            strategy: "rolling".into(),
            max_surge: 1,
            max_unavailable: 1,
            auto_rollback: true,
            failure_threshold: 3,
            drain_allocated_after_secs: 0,
        }
    }
}

impl RolloutPolicy {
    pub fn validate(&self) -> Result<()> {
        if !["manual", "rolling", "recreate"].contains(&self.strategy.as_str()) {
            return Err(Error::InvalidArgument(
                "rollout.strategy must be manual|rolling|recreate".into(),
            ));
        }
        if self.max_surge < 0 || self.max_unavailable < 0 || self.failure_threshold < 1 {
            return Err(Error::InvalidArgument(
                "rollout: max_surge/max_unavailable >= 0, failure_threshold >= 1".into(),
            ));
        }
        Ok(())
    }
}

/// One build rollout of a fleet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rollout {
    pub id: Uuid,
    pub fleet_id: Uuid,
    pub from_version: Option<String>,
    pub to_version: String,
    pub strategy: String,
    pub max_surge: i32,
    pub max_unavailable: i32,
    pub state: String,
    pub replaced: i32,
    pub failed_starts: i32,
    pub note: String,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
}

type RolloutRow = (
    Uuid,
    Uuid,
    Option<String>,
    String,
    String,
    i32,
    i32,
    String,
    i32,
    i32,
    String,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

const ROLLOUT_COLS: &str = "id, fleet_id, from_version, to_version, strategy, max_surge, max_unavailable, state, replaced, failed_starts, note, started_at, updated_at, finished_at";

fn row_to_rollout(r: RolloutRow) -> Rollout {
    Rollout {
        id: r.0,
        fleet_id: r.1,
        from_version: r.2,
        to_version: r.3,
        strategy: r.4,
        max_surge: r.5,
        max_unavailable: r.6,
        state: r.7,
        replaced: r.8,
        failed_starts: r.9,
        note: r.10,
        started_at: r.11,
        updated_at: r.12,
        finished_at: r.13,
    }
}

/// Lifecycle event of a fleet (`server.starting|ready|allocated|shutdown|terminated|error`,
/// `build.added|promoted`, `rollout.started|paused|resumed|finished|rolled_back|failure`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetEvent {
    pub id: i64,
    pub fleet_id: Uuid,
    pub server_id: Option<Uuid>,
    pub code: String,
    pub message: String,
    pub data: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

type EventRow = (
    i64,
    Uuid,
    Option<Uuid>,
    String,
    String,
    serde_json::Value,
    OffsetDateTime,
);

fn row_to_event(r: EventRow) -> FleetEvent {
    FleetEvent {
        id: r.0,
        fleet_id: r.1,
        server_id: r.2,
        code: r.3,
        message: r.4,
        data: r.5,
        at: r.6,
    }
}

/// `GET …/events` filters.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EventQuery {
    pub code: Option<String>,
    pub server_id: Option<Uuid>,
    /// Only events with id > `after`.
    pub after: Option<i64>,
    pub limit: Option<i64>,
}

impl Default for ScalePolicy {
    fn default() -> Self {
        Self {
            forecast: false,
            horizon_min: 10,
            schedule: Vec::new(),
            target_utilization: None,
            step_up: None,
            step_down: None,
            cooldown_up_secs: 0,
            cooldown_down_secs: 0,
            max_duration_secs: None,
            idle_ttl_secs: None,
            allocated_max_secs: None,
            rollout: RolloutPolicy::default(),
        }
    }
}

/// `GET …/servers/{id}/logs` filters.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LogQuery {
    pub level: Option<String>,
    pub source: Option<String>,
    /// Case-insensitive substring.
    pub q: Option<String>,
    /// Unix seconds, exclusive bounds.
    pub before: Option<i64>,
    pub after: Option<i64>,
    pub limit: u32,
}

async fn write_logs(db: &PgPool, lines: &[LogLine]) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = lines.iter().map(|_| Uuid::now_v7()).collect();
    let servers: Vec<Uuid> = lines.iter().map(|l| l.server_id).collect();
    let fleets: Vec<Uuid> = lines.iter().map(|l| l.fleet_id).collect();
    let ats: Vec<OffsetDateTime> = lines.iter().map(|l| l.at).collect();
    let sources: Vec<&str> = lines.iter().map(|l| l.source.as_str()).collect();
    let levels: Vec<&str> = lines.iter().map(|l| l.level.as_str()).collect();
    let msgs: Vec<&str> = lines.iter().map(|l| l.message.as_str()).collect();
    sqlx::query(
        "insert into musubi.server_logs (id, server_id, fleet_id, at, source, level, message)
         select * from unnest($1::uuid[], $2::uuid[], $3::uuid[], $4::timestamptz[], $5::text[], $6::text[], $7::text[])",
    )
    .bind(&ids)
    .bind(&servers)
    .bind(&fleets)
    .bind(&ats)
    .bind(&sources)
    .bind(&levels)
    .bind(&msgs)
    .execute(db)
    .await
    .map_err(Error::internal)?;
    Ok(())
}

/// Ready servers a utilization target asks for given `allocated` busy servers.
pub fn utilization_target(policy: &ScalePolicy, allocated: i32) -> i32 {
    match policy.target_utilization {
        Some(u) if allocated > 0 => ((allocated as f64) * (1.0 - u) / u).ceil() as i32,
        _ => 0,
    }
}

/// Clamp a `plan_scale` result by the policy's step sizes and cooldowns. `since_up` / `since_any`
/// are seconds since the last scale-up / last scale action (`None` = never).
pub fn apply_policy(
    policy: &ScalePolicy,
    (start, stop): (i32, i32),
    since_up: Option<u64>,
    since_any: Option<u64>,
) -> (i32, i32) {
    let mut start = start;
    let mut stop = stop;
    if start > 0 && since_up.is_some_and(|s| s < policy.cooldown_up_secs) {
        start = 0;
    }
    if stop > 0 && since_any.is_some_and(|s| s < policy.cooldown_down_secs) {
        stop = 0;
    }
    if let Some(step) = policy.step_up {
        start = start.min(step);
    }
    if let Some(step) = policy.step_down {
        stop = stop.min(step);
    }
    (start, stop)
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
        self.rollout.validate()?;
        if let Some(u) = self.target_utilization {
            if !(u > 0.0 && u < 1.0) {
                return Err(Error::InvalidArgument(
                    "policy.target_utilization must be in (0, 1)".into(),
                ));
            }
        }
        if self.step_up.is_some_and(|v| v <= 0) || self.step_down.is_some_and(|v| v <= 0) {
            return Err(Error::InvalidArgument(
                "policy.step_up / step_down must be positive".into(),
            ));
        }
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
    /// When the server last became `ready` (idle TTL counts from here).
    pub ready_at: Option<OffsetDateTime>,
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

    /// Last `tail` lines straight from the provider (container / VM / remote file), for providers
    /// that keep the process output themselves. `None` = not supported.
    fn logs<'a>(
        &'a self,
        _server_id: Uuid,
        _tail: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send + 'a>>
    {
        Box::pin(async { Ok(None) })
    }
    /// One-off preparation that makes later starts faster (firecracker: boot once, snapshot,
    /// fill the warm pool). Providers without such a step reject.
    fn prepare<'a>(
        &'a self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value>> + Send + 'a>>
    {
        Box::pin(async {
            Err(Error::InvalidArgument(
                "this provider has no prepare step".into(),
            ))
        })
    }
}

/// Child processes on this node.
#[derive(Default)]
pub struct LocalProcessProvider {
    children: DashMap<Uuid, Arc<tokio::sync::Mutex<tokio::process::Child>>>,
    exited: DashMap<Uuid, i32>,
    sink: std::sync::RwLock<Option<LogSink>>,
}

impl LocalProcessProvider {
    /// Capture stdout/stderr of every server started from now on into `sink`.
    pub fn set_log_sink(&self, sink: LogSink) {
        *self.sink.write().unwrap() = Some(sink);
    }
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
            let sink = self.sink.read().unwrap().clone();
            if sink.is_some() {
                cmd.stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped());
            }
            if let Some(d) = &req.fleet.spec.working_dir {
                cmd.current_dir(d);
            }
            let mut child = cmd
                .spawn()
                .map_err(|e| Error::Internal(format!("spawn {bin}: {e}")))?;
            if let Some(sink) = sink {
                if let Some(out) = child.stdout.take() {
                    pump_lines(out, sink.clone(), req.server_id, req.fleet.id, "stdout");
                }
                if let Some(err) = child.stderr.take() {
                    pump_lines(err, sink, req.server_id, req.fleet.id, "stderr");
                }
            }
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
    /// Per fleet: unix seconds of the last scale-up and of the last scale action (cooldowns).
    last_scale: Arc<std::sync::Mutex<HashMap<Uuid, (i64, i64)>>>,
    log_tx: LogSink,
    log_rx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<LogLine>>>>,
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
    Option<OffsetDateTime>,
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
        ready_at: r.15,
    }
}

const FLEET_COLS: &str =
    "id, name, provider, region, spec, buffer, max_servers, port_start, port_end, created_at, min_servers, policy, pool";
const SERVER_COLS: &str =
    "id, fleet_id, state, node_id, address, port, pid, labels, allocation, last_health, created_at, build_version, players, capacity, version, ready_at";

impl FleetService {
    pub fn new(db: PgPool, node_id: Uuid, api_url: &str, server_key: &str) -> Self {
        let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        let local = LocalProcessProvider::default();
        local.set_log_sink(log_tx.clone());
        providers.insert("local".into(), Arc::new(local));
        providers.insert("docker".into(), Arc::new(docker::DockerProvider));
        Self {
            db,
            node_id,
            providers: Arc::new(std::sync::RwLock::new(providers)),
            api_url: api_url.to_string(),
            server_key: server_key.to_string(),
            edge: None,
            last_scale: Arc::new(std::sync::Mutex::new(HashMap::new())),
            log_tx,
            log_rx: Arc::new(std::sync::Mutex::new(Some(log_rx))),
        }
    }

    /// Where captured / SDK-pushed log lines go (batched into `musubi.server_logs` by
    /// `spawn_log_writer`).
    pub fn log_sink(&self) -> LogSink {
        self.log_tx.clone()
    }

    /// Start the batching writer (idempotent; also called before the first server start).
    pub fn spawn_log_writer(&self) {
        let Some(mut rx) = self.log_rx.lock().unwrap().take() else {
            return;
        };
        let db = self.db.clone();
        tokio::spawn(async move {
            let mut buf = Vec::with_capacity(512);
            loop {
                let n = rx.recv_many(&mut buf, 512).await;
                if n == 0 {
                    break;
                }
                if let Err(e) = write_logs(&db, &buf).await {
                    tracing::warn!(error = %e, lines = buf.len(), "server log write failed");
                }
                buf.clear();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// Stored log lines of one server, newest first.
    pub async fn logs(
        &self,
        fleet_id: Uuid,
        server_id: Uuid,
        q: &LogQuery,
    ) -> Result<Vec<LogLine>> {
        type Row = (Uuid, OffsetDateTime, String, String, String);
        let rows: Vec<Row> = sqlx::query_as(
            "select server_id, at, source, level, message from musubi.server_logs
             where fleet_id = $1 and server_id = $2
               and ($3::text is null or level = $3)
               and ($4::text is null or source = $4)
               and ($5::text is null or message ilike '%' || $5 || '%')
               and ($6::timestamptz is null or at < $6)
               and ($7::timestamptz is null or at > $7)
             order by at desc limit $8",
        )
        .bind(fleet_id)
        .bind(server_id)
        .bind(q.level.as_deref())
        .bind(q.source.as_deref())
        .bind(q.q.as_deref())
        .bind(
            q.before
                .and_then(|b| OffsetDateTime::from_unix_timestamp(b).ok()),
        )
        .bind(
            q.after
                .and_then(|a| OffsetDateTime::from_unix_timestamp(a).ok()),
        )
        .bind(q.limit.clamp(1, 1000) as i64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(server_id, at, source, level, message)| LogLine {
                server_id,
                fleet_id,
                at,
                source,
                level,
                message,
            })
            .collect())
    }

    /// Raw tail straight from the provider (docker / kubernetes / ssh), if it keeps one.
    pub async fn provider_logs(
        &self,
        fleet: &Fleet,
        server_id: Uuid,
        tail: u32,
    ) -> Result<Option<String>> {
        match self.provider_of(&fleet.provider) {
            Some(p) => p.logs(server_id, tail).await,
            None => Ok(None),
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

    /// `POST /v1/server/fleets/providers/{name}/prepare`.
    pub async fn prepare_provider(&self, name: &str) -> Result<serde_json::Value> {
        let p = self
            .provider_of(name)
            .ok_or_else(|| Error::NotFound(format!("provider {name}")))?;
        p.prepare().await
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
        let was = self.server(id).await?.state;
        let s = self
            .set_state(id, &["scheduled", "starting", "ready"], "ready")
            .await?;
        if was != "ready" {
            self.record_event(
                s.fleet_id,
                Some(id),
                "server.ready",
                "server ready",
                serde_json::json!({ "build_version": s.build_version, "startup_secs": (OffsetDateTime::now_utc() - s.created_at).whole_seconds(), "address": s.address, "port": s.port }),
            )
            .await;
        }
        Ok(s)
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
        let was = self.server(id).await?.state;
        let s = self
            .set_state(id, &["ready", "allocated"], "allocated")
            .await?;
        if was != "allocated" {
            self.record_event(
                s.fleet_id,
                Some(id),
                "server.allocated",
                "server allocated itself",
                serde_json::json!({ "self": true, "build_version": s.build_version }),
            )
            .await;
        }
        Ok(s)
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
        self.record_event(
            f.id,
            Some(id),
            "server.shutdown",
            "server asked to shut down",
            serde_json::json!({ "state": s.state, "players": s.players }),
        )
        .await;
        self.terminate_reason(&f, id, "shutdown").await;
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
        self.record_event(
            fleet_id,
            Some(r.0),
            "server.allocated",
            "server allocated",
            serde_json::json!({ "self": false }),
        )
        .await;
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
        self.terminate_reason(fleet, id, "manual").await;
    }

    /// Stop a server and record why (`labels.exit_reason`, `server.terminated` event). Early
    /// failures of a build under rollout count towards its auto-rollback.
    pub async fn terminate_reason(&self, fleet: &Fleet, id: Uuid, reason: &str) {
        let prev = self.server(id).await.ok();
        if let Some(p) = self.provider_of(&fleet.provider) {
            p.stop(id).await;
        }
        let _ = sqlx::query(
            "update musubi.game_servers set state = 'terminated', terminated_at = coalesce(terminated_at, now()), updated_at = now(),
                 labels = labels || jsonb_build_object('exit_reason', $2::text) where id = $1 and state <> 'terminated'",
        )
        .bind(id)
        .bind(reason)
        .execute(&self.db)
        .await;
        let Some(prev) = prev else { return };
        if prev.state == "terminated" {
            return;
        }
        let now = OffsetDateTime::now_utc();
        self.record_event(
            fleet.id,
            Some(id),
            "server.terminated",
            format!("{} server terminated: {reason}", prev.state),
            serde_json::json!({
                "reason": reason, "prev_state": prev.state, "build_version": prev.build_version,
                "uptime_secs": (now - prev.created_at).whole_seconds(), "players": prev.players,
            }),
        )
        .await;
        let early = prev.state == "starting"
            || prev.state == "scheduled"
            || (prev.state == "ready"
                && prev
                    .ready_at
                    .is_some_and(|r| now - r < Duration::from_secs(120)));
        if early && ["dead_process", "unhealthy", "stuck_starting"].contains(&reason) {
            self.note_rollout_failure(fleet, prev.build_version.as_deref(), reason)
                .await;
        }
    }

    /// Append to the fleet's lifecycle log (best effort; the server crate forwards undispatched
    /// rows to the event bus / webhooks).
    pub async fn record_event(
        &self,
        fleet_id: Uuid,
        server_id: Option<Uuid>,
        code: &str,
        message: impl Into<String>,
        data: serde_json::Value,
    ) {
        let message: String = message.into();
        if let Err(e) = sqlx::query(
            "insert into musubi.fleet_events (fleet_id, server_id, code, message, data) values ($1, $2, $3, $4, $5)",
        )
        .bind(fleet_id)
        .bind(server_id)
        .bind(code)
        .bind(&message)
        .bind(data)
        .execute(&self.db)
        .await
        {
            tracing::warn!(error = %e, code, "fleet event not recorded");
        }
    }

    pub async fn events(&self, fleet_id: Uuid, q: &EventQuery) -> Result<Vec<FleetEvent>> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "select id, fleet_id, server_id, code, message, data, at from musubi.fleet_events
             where fleet_id = $1 and ($2::text is null or code = $2 or code like $2 || '.%')
               and ($3::uuid is null or server_id = $3) and ($4::bigint is null or id > $4)
             order by id desc limit $5",
        )
        .bind(fleet_id)
        .bind(&q.code)
        .bind(q.server_id)
        .bind(q.after)
        .bind(q.limit.unwrap_or(100).clamp(1, 1000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_event).collect())
    }

    /// Oldest events not yet forwarded to the event bus.
    pub async fn undispatched_events(&self, limit: i64) -> Result<Vec<FleetEvent>> {
        let rows: Vec<EventRow> = sqlx::query_as(
            "select id, fleet_id, server_id, code, message, data, at from musubi.fleet_events
             where not dispatched order by id limit $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_event).collect())
    }

    pub async fn mark_dispatched(&self, ids: &[i64]) -> Result<()> {
        sqlx::query("update musubi.fleet_events set dispatched = true where id = any($1)")
            .bind(ids)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    // ----- build rollouts -----

    /// Latest rollout of the fleet (any state).
    pub async fn rollout(&self, fleet_id: Uuid) -> Result<Option<Rollout>> {
        let r: Option<RolloutRow> = sqlx::query_as(&format!(
            "select {ROLLOUT_COLS} from musubi.fleet_rollouts where fleet_id = $1 order by started_at desc, id desc limit 1"
        ))
        .bind(fleet_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.map(row_to_rollout))
    }

    pub async fn rollouts(&self, fleet_id: Uuid, limit: i64) -> Result<Vec<Rollout>> {
        let rows: Vec<RolloutRow> = sqlx::query_as(&format!(
            "select {ROLLOUT_COLS} from musubi.fleet_rollouts where fleet_id = $1 order by started_at desc, id desc limit $2"
        ))
        .bind(fleet_id)
        .bind(limit.clamp(1, 200))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(row_to_rollout).collect())
    }

    /// Rollout card: the latest rollout plus live servers per build/state and how far it got.
    pub async fn rollout_status(&self, fleet_id: Uuid) -> Result<serde_json::Value> {
        let f = self.get(fleet_id).await?;
        let r = self.rollout(fleet_id).await?;
        let rows: Vec<(Option<String>, String, i64)> = sqlx::query_as(
            "select build_version, state, count(*) from musubi.game_servers
             where fleet_id = $1 and state <> 'terminated' group by 1, 2 order by 1, 2",
        )
        .bind(fleet_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let builds: Vec<serde_json::Value> = rows
            .iter()
            .map(|(b, st, n)| serde_json::json!({ "build_version": b, "state": st, "servers": n }))
            .collect();
        let live: i64 = rows.iter().map(|r| r.2).sum();
        let (updated, outdated) = match &r {
            Some(r) => rows.iter().fold((0i64, 0i64), |(u, o), (b, _, n)| {
                if b.as_deref() == Some(r.to_version.as_str()) {
                    (u + n, o)
                } else {
                    (u, o + n)
                }
            }),
            None => (live, 0),
        };
        Ok(serde_json::json!({
            "fleet_id": fleet_id,
            "policy": f.policy.rollout,
            "rollout": r,
            "servers": builds,
            "live": live,
            "updated": updated,
            "outdated": outdated,
            "progress_pct": if live > 0 { updated * 100 / live } else { 100 },
        }))
    }

    async fn start_rollout(
        &self,
        f: &Fleet,
        from: Option<&str>,
        to: &str,
        note: &str,
    ) -> Result<Option<Rollout>> {
        let pol = &f.policy.rollout;
        if pol.strategy == "manual" || from == Some(to) {
            return Ok(None);
        }
        sqlx::query(
            "update musubi.fleet_rollouts set state = 'superseded', finished_at = now(), updated_at = now()
             where fleet_id = $1 and state in ('running', 'paused')",
        )
        .bind(f.id)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let row: RolloutRow = sqlx::query_as(&format!(
            "insert into musubi.fleet_rollouts (id, fleet_id, from_version, to_version, strategy, max_surge, max_unavailable, note)
             values ($1, $2, $3, $4, $5, $6, $7, $8) returning {ROLLOUT_COLS}"
        ))
        .bind(Uuid::now_v7())
        .bind(f.id)
        .bind(from)
        .bind(to)
        .bind(&pol.strategy)
        .bind(pol.max_surge)
        .bind(pol.max_unavailable)
        .bind(note)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let r = row_to_rollout(row);
        self.record_event(
            f.id,
            None,
            "rollout.started",
            format!("rollout {} -> {to} ({})", from.unwrap_or("fleet spec"), pol.strategy),
            serde_json::json!({ "rollout_id": r.id, "from_version": from, "to_version": to, "strategy": pol.strategy, "note": note }),
        )
        .await;
        Ok(Some(r))
    }

    async fn set_rollout_state(
        &self,
        fleet_id: Uuid,
        from: &[&str],
        to: &str,
        code: &str,
        note: Option<&str>,
    ) -> Result<Rollout> {
        let Some(r) = self.rollout(fleet_id).await? else {
            return Err(Error::NotFound("no rollout".into()));
        };
        if !from.contains(&r.state.as_str()) {
            return Err(Error::Conflict(format!("rollout is {}", r.state)));
        }
        let finished = matches!(to, "done" | "rolled_back" | "superseded");
        let row: RolloutRow = sqlx::query_as(&format!(
            "update musubi.fleet_rollouts set state = $2, updated_at = now(), note = coalesce($3, note),
                 finished_at = case when $4 then now() else finished_at end
             where id = $1 returning {ROLLOUT_COLS}"
        ))
        .bind(r.id)
        .bind(to)
        .bind(note)
        .bind(finished)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let r = row_to_rollout(row);
        self.record_event(
            fleet_id,
            None,
            code,
            format!("rollout {} -> {} {to}", r.from_version.clone().unwrap_or_default(), r.to_version),
            serde_json::json!({ "rollout_id": r.id, "to_version": r.to_version, "from_version": r.from_version, "replaced": r.replaced, "failed_starts": r.failed_starts, "note": r.note }),
        )
        .await;
        Ok(r)
    }

    pub async fn pause_rollout(&self, fleet_id: Uuid) -> Result<Rollout> {
        self.set_rollout_state(fleet_id, &["running"], "paused", "rollout.paused", None)
            .await
    }

    pub async fn resume_rollout(&self, fleet_id: Uuid) -> Result<Rollout> {
        self.set_rollout_state(fleet_id, &["paused"], "running", "rollout.resumed", None)
            .await
    }

    /// Abort the current rollout and promote the build it replaced (which starts a rollout back).
    pub async fn rollback_rollout(&self, fleet_id: Uuid, reason: &str) -> Result<Rollout> {
        let Some(r) = self.rollout(fleet_id).await? else {
            return Err(Error::NotFound("no rollout".into()));
        };
        if !["running", "paused"].contains(&r.state.as_str()) {
            return Err(Error::Conflict(format!("rollout is {}", r.state)));
        }
        let Some(from) = r.from_version.clone() else {
            return Err(Error::Conflict(
                "rollout has no previous build to roll back to".into(),
            ));
        };
        self.set_rollout_state(
            fleet_id,
            &["running", "paused"],
            "rolled_back",
            "rollout.rolled_back",
            Some(reason),
        )
        .await?;
        self.promote_build_noted(fleet_id, &from, &format!("rollback: {reason}"))
            .await?;
        self.rollout(fleet_id)
            .await?
            .ok_or_else(|| Error::NotFound("no rollout".into()))
    }

    /// A server of the build under rollout failed early: count it, roll back past the threshold.
    async fn note_rollout_failure(&self, f: &Fleet, build: Option<&str>, reason: &str) {
        let Ok(Some(r)) = self.rollout(f.id).await else {
            return;
        };
        if r.state != "running" || build != Some(r.to_version.as_str()) {
            return;
        }
        let failed = r.failed_starts + 1;
        let _ = sqlx::query(
            "update musubi.fleet_rollouts set failed_starts = $2, updated_at = now() where id = $1",
        )
        .bind(r.id)
        .bind(failed)
        .execute(&self.db)
        .await;
        self.record_event(
            f.id,
            None,
            "rollout.failure",
            format!("server of {} failed: {reason} ({failed}/{})", r.to_version, f.policy.rollout.failure_threshold),
            serde_json::json!({ "rollout_id": r.id, "to_version": r.to_version, "reason": reason, "failed_starts": failed, "threshold": f.policy.rollout.failure_threshold }),
        )
        .await;
        if f.policy.rollout.auto_rollback
            && failed >= f.policy.rollout.failure_threshold
            && r.from_version.is_some()
        {
            tracing::warn!(fleet = %f.name, build = %r.to_version, failed, "rollout auto-rollback");
            if let Err(e) = self
                .rollback_rollout(f.id, &format!("auto: {failed} failed starts"))
                .await
            {
                tracing::warn!(fleet = %f.name, error = %e, "auto-rollback failed");
            }
        }
    }

    /// One rollout step for a fleet: replace ready servers of other builds within the policy and
    /// return how many extra servers the scaler may run meanwhile (surge).
    async fn advance_rollout(&self, f: &Fleet, servers: &[GameServer]) -> Result<i32> {
        let Some(r) = self.rollout(f.id).await? else {
            return Ok(0);
        };
        if r.state != "running" {
            return Ok(0);
        }
        let live: Vec<&GameServer> = servers.iter().filter(|s| s.state != "terminated").collect();
        let outdated: Vec<&GameServer> = live
            .iter()
            .copied()
            .filter(|s| s.build_version.as_deref() != Some(r.to_version.as_str()))
            .collect();
        if outdated.is_empty() {
            self.set_rollout_state(f.id, &["running"], "done", "rollout.finished", None)
                .await?;
            return Ok(0);
        }
        let new_ready = live
            .iter()
            .filter(|s| {
                s.state == "ready" && s.build_version.as_deref() == Some(r.to_version.as_str())
            })
            .count() as i32;
        let mut victims: Vec<&GameServer> = outdated
            .iter()
            .copied()
            .filter(|s| s.state == "ready")
            .collect();
        victims.sort_by_key(|s| s.created_at);
        let allowed = if r.strategy == "recreate" {
            victims.len()
        } else {
            (r.max_unavailable + new_ready).max(0) as usize
        };
        let now = OffsetDateTime::now_utc();
        let drain = f.policy.rollout.drain_allocated_after_secs;
        let mut drained: Vec<&GameServer> = Vec::new();
        if drain > 0 {
            drained = outdated
                .iter()
                .copied()
                .filter(|s| {
                    s.state == "allocated"
                        && s.allocation
                            .as_ref()
                            .and_then(|a| a["at"].as_i64())
                            .is_some_and(|at| now.unix_timestamp() - at > drain as i64)
                })
                .collect();
        }
        let mut replaced = 0;
        for s in victims.into_iter().take(allowed).chain(drained) {
            tracing::info!(server = %s.id, fleet = %f.name, build = ?s.build_version, to = %r.to_version, "rollout: replacing server");
            self.terminate_reason(f, s.id, "rollout").await;
            replaced += 1;
        }
        if replaced > 0 {
            let _ = sqlx::query(
                "update musubi.fleet_rollouts set replaced = replaced + $2, updated_at = now() where id = $1",
            )
            .bind(r.id)
            .bind(replaced)
            .execute(&self.db)
            .await;
        }
        let remaining = outdated.len() as i32 - replaced;
        let surge = if r.strategy == "recreate" {
            remaining
        } else {
            r.max_surge
        };
        Ok(surge.min(remaining).max(0))
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
        self.spawn_log_writer();
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
        self.record_event(
            f.id,
            Some(id),
            "server.starting",
            "server starting",
            serde_json::json!({ "port": port, "build_version": build.as_ref().map(|b| b.version.clone()), "node_id": self.node_id }),
        )
        .await;
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
                self.record_event(
                    f.id,
                    Some(id),
                    "server.error",
                    format!("server failed to start: {e}"),
                    serde_json::json!({ "error": e.to_string(), "build_version": build.as_ref().map(|b| b.version.clone()) }),
                )
                .await;
                self.note_rollout_failure(
                    f,
                    build.as_ref().map(|b| b.version.as_str()),
                    "start_failed",
                )
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
        let prev_active: Option<String> = sqlx::query_scalar(
            "select version from musubi.fleet_builds where fleet_id = $1 and state = 'active'",
        )
        .bind(fleet_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
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
        let b = row_to_build(row);
        self.record_event(
            fleet_id,
            None,
            "build.added",
            format!("build {} added ({state})", b.version),
            serde_json::json!({ "version": b.version, "state": state, "canary_percent": b.canary_percent }),
        )
        .await;
        if state == "active" {
            let f = self.get(fleet_id).await?;
            self.start_rollout(
                &f,
                prev_active.as_deref(),
                &b.version,
                "build added as active",
            )
            .await?;
        }
        Ok(b)
    }

    /// Make `version` the active build (promote a canary, or roll back to a retired build);
    /// every other build is retired. Running servers are not touched.
    pub async fn promote_build(&self, fleet_id: Uuid, version: &str) -> Result<FleetBuild> {
        self.promote_build_noted(fleet_id, version, "promoted")
            .await
    }

    async fn promote_build_noted(
        &self,
        fleet_id: Uuid,
        version: &str,
        note: &str,
    ) -> Result<FleetBuild> {
        let f = self.get(fleet_id).await?;
        let prev_active: Option<String> = sqlx::query_scalar(
            "select version from musubi.fleet_builds where fleet_id = $1 and state = 'active'",
        )
        .bind(fleet_id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
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
        let b = row_to_build(row);
        self.record_event(
            fleet_id,
            None,
            "build.promoted",
            format!("build {} promoted ({note})", b.version),
            serde_json::json!({ "version": b.version, "previous": prev_active, "note": note }),
        )
        .await;
        self.start_rollout(&f, prev_active.as_deref(), &b.version, note)
            .await?;
        Ok(b)
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
        let fleets = self.list().await?;
        self.reconcile_fleets(fleets).await
    }

    /// One fleet only (tests, `POST …/reconcile`): never touches the other fleets.
    pub async fn reconcile_one(&self, fleet_id: Uuid) -> Result<(usize, usize)> {
        let f = self.get(fleet_id).await?;
        self.reconcile_fleets(vec![f]).await
    }

    async fn reconcile_fleets(&self, fleets: Vec<Fleet>) -> Result<(usize, usize)> {
        let mut started = 0;
        let mut reaped = 0;
        for f in fleets {
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
                if let Some(why) = lifecycle_expired(&f, s, now, live_min_guard(&servers, &f)) {
                    tracing::info!(server = %s.id, fleet = %f.name, state = %s.state, why, "server lifecycle limit reached");
                    self.terminate_reason(&f, s.id, why).await;
                    reaped += 1;
                    continue;
                }
                if dead_process || stale || stuck_starting {
                    tracing::warn!(server = %s.id, fleet = %f.name, state = %s.state, dead_process, stale, "reaping game server");
                    let reason = if dead_process {
                        "dead_process"
                    } else if stale {
                        "unhealthy"
                    } else {
                        "stuck_starting"
                    };
                    self.terminate_reason(&f, s.id, reason).await;
                    reaped += 1;
                }
            }
            let servers = self.servers(f.id, None).await?;
            let surge = match self.advance_rollout(&f, &servers).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(fleet = %f.name, error = %e, "rollout step failed");
                    0
                }
            };
            let servers = self.servers(f.id, None).await?;
            let live = servers.iter().filter(|s| s.state != "terminated").count() as i32;
            let available = servers
                .iter()
                .filter(|s| s.state == "ready" || s.state == "starting" || s.state == "scheduled")
                .count() as i32;
            let idle_ready = servers.iter().filter(|s| s.state == "ready").count() as i32;
            let allocated = servers.iter().filter(|s| s.state == "allocated").count() as i32;
            let per_hour = if f.policy.forecast {
                self.forecast_per_hour(f.id, now).await.unwrap_or(0.0)
            } else {
                0.0
            };
            let target = target_ready(&f.policy, f.policy.buffer_at(now, f.buffer), per_hour)
                .max(utilization_target(&f.policy, allocated))
                + surge;
            let plan = plan_scale(
                target,
                f.min_servers,
                f.max_servers,
                live,
                available,
                idle_ready,
            );
            let now_s = now.unix_timestamp();
            let last = self.last_scale.lock().unwrap().get(&f.id).copied();
            let (start, stop) = apply_policy(
                &f.policy,
                plan,
                last.map(|(up, _)| (now_s - up).max(0) as u64),
                last.map(|(_, any)| (now_s - any).max(0) as u64),
            );
            if start > 0 || stop > 0 {
                let mut g = self.last_scale.lock().unwrap();
                let e = g.entry(f.id).or_insert((i64::MIN / 2, i64::MIN / 2));
                if start > 0 {
                    e.0 = now_s;
                }
                e.1 = now_s;
            }
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
                    self.terminate_reason(&f, s.id, "scale_down").await;
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

#[cfg(test)]
mod policy_tests {
    use super::*;

    fn pol() -> ScalePolicy {
        ScalePolicy {
            target_utilization: Some(0.8),
            step_up: Some(2),
            step_down: Some(1),
            cooldown_up_secs: 60,
            cooldown_down_secs: 300,
            ..Default::default()
        }
    }

    #[test]
    fn utilization_sizes_the_ready_pool() {
        // 8 busy at 80 % → 2 ready
        assert_eq!(utilization_target(&pol(), 8), 2);
        assert_eq!(utilization_target(&pol(), 0), 0);
        assert_eq!(utilization_target(&ScalePolicy::default(), 8), 0);
        // 7 busy → 1.75 → 2
        assert_eq!(utilization_target(&pol(), 7), 2);
    }

    #[test]
    fn steps_and_cooldowns_clamp_the_plan() {
        let p = pol();
        assert_eq!(apply_policy(&p, (5, 0), None, None), (2, 0));
        assert_eq!(apply_policy(&p, (0, 3), None, None), (0, 1));
        // scale-up inside the up cooldown waits
        assert_eq!(apply_policy(&p, (5, 0), Some(30), Some(30)), (0, 0));
        assert_eq!(apply_policy(&p, (5, 0), Some(61), Some(61)), (2, 0));
        // scale-down waits for the long cooldown after any action
        assert_eq!(apply_policy(&p, (0, 3), Some(200), Some(200)), (0, 0));
        assert_eq!(apply_policy(&p, (0, 3), Some(400), Some(400)), (0, 1));
        assert_eq!(
            apply_policy(&ScalePolicy::default(), (5, 3), Some(0), Some(0)),
            (5, 3)
        );
    }

    #[test]
    fn policy_validation() {
        assert!(pol().validate(10).is_ok());
        assert!(ScalePolicy {
            target_utilization: Some(1.0),
            ..Default::default()
        }
        .validate(10)
        .is_err());
        assert!(ScalePolicy {
            step_up: Some(0),
            ..Default::default()
        }
        .validate(10)
        .is_err());
    }

    #[test]
    fn resources_map_to_providers() {
        let r = Resources {
            cpu_millis: Some(500),
            memory_mb: Some(512),
            memory_limit_mb: Some(1024),
            disk_mb: Some(2048),
            gpu: Some(1),
            ..Default::default()
        };
        let k = r.kubernetes().unwrap();
        assert_eq!(k["requests"]["cpu"], "500m");
        assert_eq!(k["limits"]["cpu"], "500m");
        assert_eq!(k["requests"]["memory"], "512Mi");
        assert_eq!(k["limits"]["memory"], "1024Mi");
        assert_eq!(k["limits"]["ephemeral-storage"], "2048Mi");
        assert_eq!(k["limits"]["nvidia.com/gpu"], "1");
        assert_eq!(
            r.docker_args(),
            vec!["--cpus", "0.500", "--memory", "1024m", "--gpus", "1"]
        );
        assert_eq!(r.firecracker(), (Some(1), Some(1024)));
        assert!(Resources::default().kubernetes().is_none());
        assert!(Resources::default().docker_args().is_empty());
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn levels_are_guessed_from_lines() {
        assert_eq!(level_of("[2025-01-01] ERROR boom", "stdout"), "error");
        assert_eq!(level_of("warn: slow tick", "stdout"), "warn");
        assert_eq!(level_of("level=debug x", "stdout"), "debug");
        assert_eq!(level_of("hello", "stdout"), "info");
        assert_eq!(level_of("hello", "stderr"), "warn");
    }
}

/// How many servers may be reaped for idleness before the fleet drops below `min_servers`.
fn live_min_guard(servers: &[GameServer], f: &Fleet) -> i32 {
    let live = servers.iter().filter(|s| s.state != "terminated").count() as i32;
    (live - f.min_servers).max(0)
}

/// Which lifecycle limit (if any) a server has hit: `max_duration`, `idle_ttl` or
/// `allocated_max`. `spare` is how many servers may go before `min_servers` is violated.
pub fn lifecycle_expired(
    f: &Fleet,
    s: &GameServer,
    now: OffsetDateTime,
    spare: i32,
) -> Option<&'static str> {
    let p = &f.policy;
    let age = (now - s.created_at).whole_seconds().max(0) as u64;
    if p.max_duration_secs.is_some_and(|m| m > 0 && age >= m) {
        return Some("max_duration");
    }
    if s.state == "ready" && spare > 0 {
        if let (Some(ttl), Some(ready_at)) = (p.idle_ttl_secs, s.ready_at) {
            if ttl > 0 && (now - ready_at).whole_seconds().max(0) as u64 >= ttl {
                return Some("idle_ttl");
            }
        }
    }
    if s.state == "allocated" || s.state == "in_game" {
        if let Some(m) = p.allocated_max_secs.filter(|m| *m > 0) {
            let since = s
                .allocation
                .as_ref()
                .and_then(|a| a.get("at"))
                .and_then(|a| a.as_i64())
                .unwrap_or_else(|| s.created_at.unix_timestamp());
            if now.unix_timestamp() - since >= m as i64 {
                return Some("allocated_max");
            }
        }
    }
    None
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn fleet(p: ScalePolicy) -> Fleet {
        let json = serde_json::json!({
            "id": Uuid::nil(), "name": "f", "provider": "local", "region": "eu", "pool": "default",
            "spec": {}, "buffer": 0, "max_servers": 10, "min_servers": 1, "policy": {},
            "port_start": 1, "port_end": 2, "created_at": "2026-01-01T00:00:00Z",
        });
        let mut f: Fleet = serde_json::from_value(json).expect("fleet");
        f.policy = p;
        f
    }

    fn server(
        state: &str,
        age_secs: u64,
        ready_secs: Option<u64>,
        alloc_secs: Option<i64>,
    ) -> GameServer {
        let now = OffsetDateTime::now_utc();
        GameServer {
            id: Uuid::now_v7(),
            fleet_id: Uuid::nil(),
            state: state.into(),
            node_id: None,
            address: String::new(),
            port: 0,
            pid: None,
            labels: serde_json::json!({}),
            allocation: alloc_secs.map(|a| serde_json::json!({ "at": (now.unix_timestamp() - a) })),
            last_health: now,
            created_at: now - Duration::from_secs(age_secs),
            build_version: None,
            players: 0,
            capacity: 0,
            version: 0,
            ready_at: ready_secs.map(|r| now - Duration::from_secs(r)),
        }
    }

    #[test]
    fn limits_fire_and_respect_min_servers() {
        let now = OffsetDateTime::now_utc();
        let f = fleet(ScalePolicy {
            max_duration_secs: Some(3600),
            idle_ttl_secs: Some(600),
            allocated_max_secs: Some(1800),
            ..Default::default()
        });
        assert_eq!(
            lifecycle_expired(&f, &server("ready", 4000, Some(10), None), now, 1),
            Some("max_duration")
        );
        assert_eq!(
            lifecycle_expired(&f, &server("ready", 100, Some(700), None), now, 1),
            Some("idle_ttl")
        );
        assert_eq!(
            lifecycle_expired(&f, &server("ready", 100, Some(700), None), now, 0),
            None,
            "min_servers guard"
        );
        assert_eq!(
            lifecycle_expired(&f, &server("ready", 100, Some(100), None), now, 1),
            None
        );
        assert_eq!(
            lifecycle_expired(&f, &server("in_game", 100, None, Some(2000)), now, 0),
            Some("allocated_max")
        );
        assert_eq!(
            lifecycle_expired(&f, &server("allocated", 100, None, Some(100)), now, 0),
            None
        );
        let none = fleet(ScalePolicy::default());
        assert_eq!(
            lifecycle_expired(&none, &server("ready", 99999, Some(99999), None), now, 5),
            None
        );
    }
}
