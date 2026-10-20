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

pub mod capacity;
pub mod docker;
pub mod edgegap;
pub mod firecracker;
pub mod gamelift;
pub mod hosts;
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

/// An extra named port of a game server (P0-7): the fleet's port range supplies a host port per
/// server (`MUSUBI_PORT_<NAME>` in the environment, `ports.{name}` on the server/allocation),
/// providers publish it with `protocol` (`udp` | `tcp` | `both`) onto `container_port` (defaults
/// to the same number).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct PortSpec {
    pub name: String,
    pub protocol: String,
    pub container_port: Option<i32>,
    /// Filled per server at start time; not part of the stored spec.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_port: Option<i32>,
}

/// Kubernetes placement and disruption options (P0-10), applied to the Pod (Pods mode) or the
/// Agones `GameServer` template. `safe_to_evict`: `never` (default — the pod carries
/// `cluster-autoscaler.kubernetes.io/safe-to-evict: "false"` so scale-down does not kill a live
/// match) | `on_upgrade` | `always` (Agones `spec.eviction.safe`; CA annotation `"true"` only for
/// `always`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct KubePlacement {
    pub node_selector: HashMap<String, String>,
    pub tolerations: Vec<serde_json::Value>,
    pub affinity: Option<serde_json::Value>,
    pub annotations: HashMap<String, String>,
    pub priority_class: Option<String>,
    pub service_account: Option<String>,
    pub safe_to_evict: String,
    pub host_network: bool,
    pub termination_grace_secs: Option<u64>,
}

/// Shipping of a server's captured / SDK-pushed log lines to object storage (Edgegap endpoint
/// storage, GameLift server logs): one gzip NDJSON object per snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogSpec {
    /// Archive every terminated server's lines automatically (needs `[ugc.s3]`).
    pub archive: bool,
    /// Drop the archived rows from Postgres once the object is stored (default true).
    pub purge_after_archive: bool,
    /// Grace after termination for in-flight lines before the archive is cut (default 30 s).
    pub delay_secs: u64,
    /// Key prefix inside the bucket (default `server-logs/`).
    pub prefix: Option<String>,
}

impl Default for LogSpec {
    fn default() -> Self {
        Self {
            archive: false,
            purge_after_archive: true,
            delay_secs: 30,
            prefix: None,
        }
    }
}

pub const LOG_ARCHIVE_PREFIX: &str = "server-logs/";

pub fn validate_logs(spec: &FleetSpec) -> Result<()> {
    let l = &spec.logs;
    if l.delay_secs > 3600 {
        return Err(Error::InvalidArgument(
            "spec.logs.delay_secs must be <= 3600".into(),
        ));
    }
    if let Some(p) = &l.prefix {
        let ok = !p.is_empty()
            && p.len() <= 128
            && !p.starts_with('/')
            && p.ends_with('/')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '_' | '-' | '.'));
        if !ok {
            return Err(Error::InvalidArgument(
                "spec.logs.prefix: 1..=128 chars of [A-Za-z0-9/_-.], no leading '/', trailing '/'"
                    .into(),
            ));
        }
    }
    Ok(())
}

pub type BoxFut<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// Object storage the fleet ships log archives to (implemented over the server's S3 store).
pub trait LogArchive: Send + Sync {
    fn put<'a>(&'a self, key: &'a str, bytes: Vec<u8>, mime: &'a str) -> BoxFut<'a, Result<()>>;
    fn get<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<Vec<u8>>>;
    fn delete<'a>(&'a self, key: &'a str) -> BoxFut<'a, Result<()>>;
}

#[derive(Debug, Clone, Serialize)]
pub struct LogArchiveEntry {
    pub id: Uuid,
    pub server_id: Uuid,
    pub fleet_id: Uuid,
    pub key: String,
    pub bytes: i64,
    pub lines: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub from_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub to_at: Option<OffsetDateTime>,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type LogArchiveRow = (
    Uuid,
    Uuid,
    Uuid,
    String,
    i64,
    i32,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    String,
    OffsetDateTime,
);

const LOG_ARCHIVE_COLS: &str =
    "id, server_id, fleet_id, key, bytes, lines, from_at, to_at, reason, created_at";

fn archive_entry(r: LogArchiveRow) -> LogArchiveEntry {
    let (id, server_id, fleet_id, key, bytes, lines, from_at, to_at, reason, created_at) = r;
    LogArchiveEntry {
        id,
        server_id,
        fleet_id,
        key,
        bytes,
        lines,
        from_at,
        to_at,
        reason,
        created_at,
    }
}

/// Decode an archive object back into its lines (`{at, source, level, message}` per entry).
pub fn decode_log_archive(bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
    use std::io::Read;
    let mut text = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut text)
        .map_err(|e| Error::Internal(format!("log archive: {e}")))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Hourly cost of one live server (cloud VM share, Anywhere/bare-metal amortisation, licence).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CostSpec {
    pub hourly_per_server: f64,
    pub currency: String,
    /// Price of one GiB of egress reported by the servers (`POST /v1/gameserver/egress`); feeds
    /// `GET /v1/server/fleets/{id}/egress`.
    #[serde(default)]
    pub egress_per_gb: f64,
}

impl Default for CostSpec {
    fn default() -> Self {
        Self {
            egress_per_gb: 0.0,
            hourly_per_server: 0.0,
            currency: "USD".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetSpec {
    /// Build artifact `name@version` from the artifact store (`PUT /v1/server/builds/{name}/{version}`);
    /// resolved when the fleet/build is saved into `MUSUBI_ARTIFACT_URL|SHA256|FILENAME|SIZE` env.
    pub artifact: Option<String>,
    /// Cost attribute for spend estimates (`GET /v1/server/fleets/{id}/cost`).
    pub cost: Option<CostSpec>,
    /// Kubernetes node placement / disruption (P0-10); ignored by other providers.
    pub kubernetes: KubePlacement,
    /// Log archival to object storage.
    pub logs: LogSpec,
    /// Extra named ports besides the primary `MUSUBI_PORT` (P0-7).
    pub ports: Vec<PortSpec>,
    /// Protocol(s) published for the primary port: `udp` | `tcp` | `both` (default).
    pub port_protocol: String,
    /// `dynamic` (default: a host port from the fleet range is published) | `none` (no host port —
    /// host networking / service mesh; the process still gets `MUSUBI_PORT` as a hint).
    pub port_policy: String,
    /// Executable + args for the `local` provider (or image for container providers).
    pub command: Vec<String>,
    pub image: Option<String>,
    /// Check at save time that `image` can be pulled: the registry's manifest is fetched
    /// (anonymous or bearer-token auth; `secret_env.REGISTRY_AUTH = "user:password"` for private
    /// registries) and its digest pinned into `image_digest`. A missing / unreachable image is a
    /// 400 instead of a fleet that never comes up.
    pub verify_image: bool,
    /// Manifest digest resolved by `verify_image` (`sha256:…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    pub env: HashMap<String, String>,
    /// Secret environment (P1): values are sealed with the `[security] master_keys` ring when
    /// the fleet/build is stored, shown as `***` by the API, and decrypted only on the node
    /// that starts the server, where they join `env` for the provider.
    pub secret_env: HashMap<String, String>,
    pub working_dir: Option<String>,
    /// vCPUs one server uses, for usage accounting (default 1).
    pub vcpu: Option<f64>,
    /// Per-server resource specification handed to the provider (Kubernetes requests/limits,
    /// Docker `--cpus`/`--memory`, Firecracker machine config).
    pub resources: Resources,
    /// Concurrent allocations one process may host (rooms per process); `None`/1 = one match
    /// per server. Allocation packs onto the fullest live server with a free slot first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots_per_server: Option<u32>,
    /// `ssh` provider: only hosts of this hardware class / with these labels take the fleet.
    pub host_class: Option<String>,
    pub host_labels: serde_json::Value,
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

/// Metric-threshold scaling rule (P0-6): once `metric op threshold` has held for `for_secs`, add
/// (`scale_up`) or remove (`scale_down`) `step` servers from the target, then rest `cooldown_secs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ScaleRule {
    pub name: String,
    /// `ready` | `ready_pct` | `allocated` | `allocated_pct` | `players` | `players_pct` |
    /// `cpu_pct_avg` | `cpu_pct_max` | `mem_pct_avg` | `tick_p99_ms_max` | `custom.<key>`
    pub metric: String,
    /// `gt` | `gte` | `lt` | `lte`
    pub op: String,
    pub threshold: f64,
    pub for_secs: u64,
    /// `scale_up` | `scale_down`
    pub action: String,
    pub step: i32,
    pub cooldown_secs: u64,
}

/// External scaler (P0-6, Agones `FleetAutoscaler` webhook policy): every reconcile the fleet's
/// status is POSTed and the answer `{scale, replicas}` (or Agones' `{response: {scale, replicas}}`)
/// sets the desired total server count; a failed call keeps the local decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ScaleWebhook {
    pub url: String,
    pub timeout_ms: u64,
    pub headers: HashMap<String, String>,
}

fn one() -> u32 {
    1
}

/// Autoscale policy (plan 05): buffer overrides by schedule plus an optional demand forecast.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ScalePolicy {
    /// Autoscaler paused (P0-6): health reaping, rollouts and lifecycle limits still run, the
    /// server count is left alone until `resume`.
    pub suspended: bool,
    pub rules: Vec<ScaleRule>,
    pub webhook: Option<ScaleWebhook>,
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
    /// Keep this much aggregate free counter capacity across the fleet (adds to the ready target).
    pub counter_target: Option<CounterTarget>,
    /// List this fleet's servers in the player server browser (`GET /v1/servers`).
    pub browsable: bool,
    /// Health-check policy (P1-28): how long a server may take to become ready and how many
    /// missed `POST /v1/gameserver/health` periods make it unhealthy.
    pub health: HealthPolicy,
    /// Which `allocated` (in-game) servers the fleet may stop: `never` (only lifecycle limits
    /// end them), `on_upgrade` (default: rollouts may drain long allocations per
    /// `rollout.drain_allocated_after_secs`) or `always` (scale-down may also stop the emptiest
    /// allocated servers when the fleet is over target).
    pub eviction: String,
    /// Servers started concurrently per reconcile batch (1 = one after another; Agones
    /// `maxCreationParallelism`). `step_up` / `step_down` cap each batch.
    pub creation_parallelism: u32,
    /// Servers terminated concurrently per reconcile batch (Agones `maxDeletionParallelism`);
    /// `step_down` caps the batch itself.
    #[serde(default = "one")]
    pub deletion_parallelism: u32,
    /// Never have more than this many servers in `starting` at once (Agones
    /// `maxPodPendingCount`): a pass starts only `max_pending - starting` more, the rest waits for
    /// the next pass. Protects the node / cluster from a thundering herd of cold starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pending: Option<u32>,
    /// Ordered policy chain: the first entry whose window matches `now` supplies the buffer,
    /// utilisation target and/or webhook; a failing webhook falls through to the next matching
    /// entry, then to the fleet-level `webhook`/`buffer`. Agones `Chain`.
    pub chain: Vec<ChainEntry>,
    /// Labels merged onto the emptiest allocated servers that keep the fleet above `max_servers`
    /// after a scale-down (idle ones are stopped first, allocated ones are never killed); the
    /// game server sees them via `GET /v1/gameserver/self` and winds down. Agones `allocationOverflow`.
    pub overflow_labels: HashMap<String, String>,
}

/// One link of `ScalePolicy::chain`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ChainEntry {
    pub id: String,
    /// When this entry applies (UTC weekday/hours, like `ScheduleWindow`); absent = always.
    pub when: Option<ChainWindow>,
    pub buffer: Option<i32>,
    pub target_utilization: Option<f64>,
    pub webhook: Option<ScaleWebhook>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct ChainWindow {
    pub days: Vec<u8>,
    pub from_hour: u8,
    pub to_hour: u8,
}

impl ChainWindow {
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

impl ChainEntry {
    pub fn applies(&self, now: OffsetDateTime) -> bool {
        self.when
            .as_ref()
            .is_none_or(|w| w.matches(now.weekday().number_days_from_sunday(), now.hour()))
    }
}

/// Health-check policy (Agones `health` + eviction semantics). A server that does not report
/// `ready` within `initial_delay_secs` is `stuck_starting`; after ready, missing health reports
/// for `period_secs × failure_threshold` makes it `unhealthy` and the reconciler reaps it.
/// `disabled` keeps servers alive regardless of reports (debuggers, pause-on-breakpoint).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct HealthPolicy {
    pub disabled: bool,
    pub initial_delay_secs: u64,
    pub period_secs: u64,
    pub failure_threshold: u32,
}

impl Default for HealthPolicy {
    fn default() -> Self {
        Self {
            disabled: false,
            initial_delay_secs: 120,
            period_secs: 10,
            failure_threshold: 3,
        }
    }
}

pub const EVICTIONS: [&str; 3] = ["never", "on_upgrade", "always"];

impl HealthPolicy {
    /// Silence that makes a ready/allocated server unhealthy.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.period_secs.max(1) * self.failure_threshold.max(1) as u64)
    }

    pub fn validate(&self) -> Result<()> {
        if self.period_secs < 1 || self.failure_threshold < 1 || self.initial_delay_secs < 1 {
            return Err(Error::InvalidArgument(
                "policy.health: period_secs, failure_threshold and initial_delay_secs must be >= 1"
                    .into(),
            ));
        }
        if self.period_secs > 3600 || self.initial_delay_secs > 86_400 {
            return Err(Error::InvalidArgument(
                "policy.health: period_secs <= 3600, initial_delay_secs <= 86400".into(),
            ));
        }
        Ok(())
    }

    /// Status of one server given its state, seconds since its last health report and seconds
    /// since it was created.
    pub fn status(&self, state: &str, since_health: i64, since_created: i64) -> &'static str {
        match state {
            "terminated" => "terminated",
            "scheduled" | "starting" => {
                if since_created > self.initial_delay_secs as i64 {
                    "stuck_starting"
                } else {
                    "starting"
                }
            }
            _ if self.disabled => "unchecked",
            _ if since_health > self.timeout().as_secs() as i64 => "unhealthy",
            _ if since_health > self.period_secs as i64 => "suspect",
            _ => "healthy",
        }
    }
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
            suspended: false,
            rules: Vec::new(),
            webhook: None,
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
            counter_target: None,
            browsable: false,
            health: HealthPolicy::default(),
            eviction: "on_upgrade".into(),
            creation_parallelism: 1,
            deletion_parallelism: 1,
            max_pending: None,
            chain: Vec::new(),
            overflow_labels: HashMap::new(),
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
        self.health.validate()?;
        if !EVICTIONS.contains(&self.eviction.as_str()) {
            return Err(Error::InvalidArgument(
                "policy.eviction must be never|on_upgrade|always".into(),
            ));
        }
        if let Some(ct) = &self.counter_target {
            valid_slot_name(&ct.counter)?;
            if ct.buffer < 0 {
                return Err(Error::InvalidArgument(
                    "policy.counter_target.buffer must be >= 0".into(),
                ));
            }
        }
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
        if !(1..=64).contains(&self.creation_parallelism) {
            return Err(Error::InvalidArgument(
                "policy.creation_parallelism must be 1..=64".into(),
            ));
        }
        if !(1..=64).contains(&self.deletion_parallelism) {
            return Err(Error::InvalidArgument(
                "policy.deletion_parallelism must be 1..=64".into(),
            ));
        }
        if self.max_pending == Some(0) {
            return Err(Error::InvalidArgument(
                "policy.max_pending must be >= 1 (omit to disable)".into(),
            ));
        }
        if self.chain.len() > 16 {
            return Err(Error::InvalidArgument(
                "policy.chain: at most 16 entries".into(),
            ));
        }
        let mut ids = std::collections::HashSet::new();
        for e in &self.chain {
            if e.id.is_empty() || e.id.len() > 64 || !ids.insert(e.id.as_str()) {
                return Err(Error::InvalidArgument(
                    "policy.chain[].id must be unique and 1..=64 chars".into(),
                ));
            }
            if e.buffer.is_none() && e.target_utilization.is_none() && e.webhook.is_none() {
                return Err(Error::InvalidArgument(format!(
                    "policy.chain[{}] needs buffer, target_utilization or webhook",
                    e.id
                )));
            }
            if e.buffer.is_some_and(|b| b < 0) {
                return Err(Error::InvalidArgument(format!(
                    "policy.chain[{}].buffer must be >= 0",
                    e.id
                )));
            }
            if e.target_utilization.is_some_and(|u| !(u > 0.0 && u < 1.0)) {
                return Err(Error::InvalidArgument(format!(
                    "policy.chain[{}].target_utilization must be in (0, 1)",
                    e.id
                )));
            }
            if let Some(w) = &e.when {
                if w.days.iter().any(|d| *d > 6) || w.from_hour > 23 || w.to_hour > 24 {
                    return Err(Error::InvalidArgument(format!(
                        "policy.chain[{}].when: days 0..=6, from_hour 0..=23, to_hour 0..=24",
                        e.id
                    )));
                }
            }
            if let Some(w) = &e.webhook {
                if !(w.url.starts_with("http://") || w.url.starts_with("https://")) {
                    return Err(Error::InvalidArgument(format!(
                        "policy.chain[{}].webhook.url must be http(s)",
                        e.id
                    )));
                }
            }
        }
        if self.overflow_labels.len() > 16
            || self.overflow_labels.iter().any(|(k, v)| {
                k.is_empty()
                    || k.len() > 64
                    || v.len() > 256
                    || !k
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "_.-/".contains(c))
            })
        {
            return Err(Error::InvalidArgument(
                "policy.overflow_labels: <= 16 labels, keys [A-Za-z0-9_.-/]{1,64}, values <= 256 chars".into(),
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
        for r in &self.rules {
            if r.name.is_empty() || r.name.len() > 64 {
                return Err(Error::InvalidArgument(
                    "policy.rules[].name 1..=64 chars".into(),
                ));
            }
            let known = matches!(
                r.metric.as_str(),
                "ready"
                    | "ready_pct"
                    | "allocated"
                    | "allocated_pct"
                    | "players"
                    | "players_pct"
                    | "cpu_pct_avg"
                    | "cpu_pct_max"
                    | "mem_pct_avg"
                    | "tick_p99_ms_max"
            ) || r
                .metric
                .strip_prefix("custom.")
                .is_some_and(|k| !k.is_empty());
            if !known {
                return Err(Error::InvalidArgument(format!(
                    "policy.rules `{}`: unknown metric `{}`",
                    r.name, r.metric
                )));
            }
            if !matches!(r.op.as_str(), "gt" | "gte" | "lt" | "lte") {
                return Err(Error::InvalidArgument(format!(
                    "policy.rules `{}`: op must be gt|gte|lt|lte",
                    r.name
                )));
            }
            if !matches!(r.action.as_str(), "scale_up" | "scale_down") {
                return Err(Error::InvalidArgument(format!(
                    "policy.rules `{}`: action must be scale_up|scale_down",
                    r.name
                )));
            }
            if r.step <= 0 || r.step > max_servers {
                return Err(Error::InvalidArgument(format!(
                    "policy.rules `{}`: step in 1..=max_servers",
                    r.name
                )));
            }
        }
        if let Some(w) = &self.webhook {
            if !(w.url.starts_with("http://") || w.url.starts_with("https://")) {
                return Err(Error::InvalidArgument(
                    "policy.webhook.url must be http(s)".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Fleet-level numbers the scaling rules and the external scaler see.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FleetGauges {
    pub live: i32,
    pub ready: i32,
    pub starting: i32,
    pub allocated: i32,
    pub in_game: i32,
    pub players: i64,
    pub capacity: i64,
    pub cpu_pct_avg: Option<f64>,
    pub cpu_pct_max: Option<f64>,
    pub mem_pct_avg: Option<f64>,
    pub tick_p99_ms_max: Option<f64>,
    pub custom: HashMap<String, f64>,
}

pub fn fleet_metric(g: &FleetGauges, name: &str) -> Option<f64> {
    let pct = |n: i64, d: i64| (d > 0).then(|| n as f64 * 100.0 / d as f64);
    match name {
        "ready" => Some(f64::from(g.ready)),
        "ready_pct" => pct(i64::from(g.ready), i64::from(g.live)),
        "allocated" => Some(f64::from(g.allocated + g.in_game)),
        "allocated_pct" => pct(i64::from(g.allocated + g.in_game), i64::from(g.live)),
        "players" => Some(g.players as f64),
        "players_pct" => pct(g.players, g.capacity),
        "cpu_pct_avg" => g.cpu_pct_avg,
        "cpu_pct_max" => g.cpu_pct_max,
        "mem_pct_avg" => g.mem_pct_avg,
        "tick_p99_ms_max" => g.tick_p99_ms_max,
        other => other
            .strip_prefix("custom.")
            .and_then(|k| g.custom.get(k).copied()),
    }
}

pub fn rule_holds(r: &ScaleRule, v: f64) -> bool {
    match r.op.as_str() {
        "gt" => v > r.threshold,
        "gte" => v >= r.threshold,
        "lt" => v < r.threshold,
        "lte" => v <= r.threshold,
        _ => false,
    }
}

/// One evaluation of a rule: `since` = when its condition first held (None = not holding),
/// `last_fired` = unix seconds of its last action. Returns the target delta and the new state.
pub fn step_rule(
    r: &ScaleRule,
    value: Option<f64>,
    now: i64,
    since: Option<i64>,
    last_fired: Option<i64>,
) -> (i32, Option<i64>, Option<i64>) {
    let Some(v) = value else {
        return (0, None, last_fired);
    };
    if !rule_holds(r, v) {
        return (0, None, last_fired);
    }
    let since = since.unwrap_or(now);
    let held = (now - since).max(0) as u64 >= r.for_secs;
    let cooled = last_fired.is_none_or(|t| (now - t).max(0) as u64 >= r.cooldown_secs);
    if held && cooled {
        let d = if r.action == "scale_down" {
            -r.step
        } else {
            r.step
        };
        (d, Some(since), Some(now))
    } else {
        (0, Some(since), last_fired)
    }
}

/// An external scaler's desired total `replicas` → the ready/available target the planner uses.
pub fn webhook_target(replicas: i32, busy: i32, max: i32) -> i32 {
    (replicas.clamp(0, max) - busy).max(0)
}

fn protos(p: &str) -> &'static [&'static str] {
    match p {
        "udp" => &["udp"],
        "tcp" => &["tcp"],
        _ => &["udp", "tcp"],
    }
}

pub fn port_policy_none(spec: &FleetSpec) -> bool {
    spec.port_policy == "none"
}

/// Every (name, host port, container port, protocol) a server exposes: the primary `game` port
/// plus `spec.ports` (with their per-server `host_port`); `both` expands to udp + tcp.
pub fn port_bindings(spec: &FleetSpec, primary: i32) -> Vec<(String, i32, i32, &'static str)> {
    let mut out = Vec::new();
    for proto in protos(&spec.port_protocol) {
        out.push(("game".to_string(), primary, primary, *proto));
    }
    for p in &spec.ports {
        let Some(host) = p.host_port else { continue };
        for proto in protos(&p.protocol) {
            out.push((
                p.name.clone(),
                host,
                p.container_port.unwrap_or(host),
                *proto,
            ));
        }
    }
    out
}

pub fn validate_secret_env(spec: &FleetSpec) -> Result<()> {
    if spec.secret_env.len() > 64 {
        return Err(Error::InvalidArgument(
            "spec.secret_env: at most 64 entries".into(),
        ));
    }
    for k in spec.secret_env.keys() {
        let ok = !k.is_empty()
            && k.len() <= 128
            && !k.as_bytes()[0].is_ascii_digit()
            && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !ok {
            return Err(Error::InvalidArgument(format!(
                "spec.secret_env key `{k}`: [A-Za-z_][A-Za-z0-9_]*"
            )));
        }
        if spec.env.contains_key(k) {
            return Err(Error::InvalidArgument(format!(
                "`{k}` is in both env and secret_env"
            )));
        }
    }
    Ok(())
}

/// Seal plaintext `secret_env` values with the current master key (no-op for already sealed
/// values or when no key ring is configured — then the value is stored as given).
pub fn seal_secret_env(spec: &mut FleetSpec) {
    for v in spec.secret_env.values_mut() {
        if !musubi_core::secrets::is_sealed(v) {
            *v = musubi_core::secrets::seal(v);
        }
    }
}

/// Decrypt `secret_env` into `env` for the process/container about to start.
pub fn open_secret_env(spec: &mut FleetSpec) -> Result<()> {
    let sealed = std::mem::take(&mut spec.secret_env);
    for (k, v) in sealed {
        let plain = musubi_core::secrets::open(&v).map_err(|e| {
            Error::internal(std::io::Error::other(format!("fleet secret `{k}`: {e}")))
        })?;
        spec.env.insert(k, plain);
    }
    Ok(())
}

pub const REDACTED: &str = "***";

impl FleetSpec {
    /// Copy for API responses: secret values replaced by `***`.
    pub fn redacted(mut self) -> Self {
        for v in self.secret_env.values_mut() {
            *v = REDACTED.into();
        }
        self
    }
}

impl Fleet {
    pub fn redacted(mut self) -> Self {
        self.spec = self.spec.redacted();
        self
    }
}

impl FleetBuild {
    pub fn redacted(mut self) -> Self {
        self.spec = self.spec.redacted();
        self
    }
}

pub fn validate_kubernetes(spec: &FleetSpec) -> Result<()> {
    let k = &spec.kubernetes;
    if !matches!(
        k.safe_to_evict.as_str(),
        "" | "never" | "on_upgrade" | "always"
    ) {
        return Err(Error::InvalidArgument(
            "spec.kubernetes.safe_to_evict must be never|on_upgrade|always".into(),
        ));
    }
    if k.tolerations.iter().any(|t| !t.is_object()) {
        return Err(Error::InvalidArgument(
            "spec.kubernetes.tolerations must be objects".into(),
        ));
    }
    if k.affinity.as_ref().is_some_and(|a| !a.is_object()) {
        return Err(Error::InvalidArgument(
            "spec.kubernetes.affinity must be an object".into(),
        ));
    }
    if k.node_selector.keys().any(|k| k.is_empty()) || k.annotations.keys().any(|k| k.is_empty()) {
        return Err(Error::InvalidArgument(
            "spec.kubernetes: empty node_selector/annotations key".into(),
        ));
    }
    if k.termination_grace_secs.is_some_and(|g| g > 86_400) {
        return Err(Error::InvalidArgument(
            "spec.kubernetes.termination_grace_secs <= 86400".into(),
        ));
    }
    Ok(())
}

pub fn validate_ports(spec: &FleetSpec) -> Result<()> {
    let proto_ok = |p: &str| matches!(p, "" | "udp" | "tcp" | "both");
    if !proto_ok(&spec.port_protocol) {
        return Err(Error::InvalidArgument(
            "spec.port_protocol must be udp|tcp|both".into(),
        ));
    }
    if !matches!(spec.port_policy.as_str(), "" | "dynamic" | "none") {
        return Err(Error::InvalidArgument(
            "spec.port_policy must be dynamic|none".into(),
        ));
    }
    if spec.ports.len() > 16 {
        return Err(Error::InvalidArgument("spec.ports: at most 16".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for p in &spec.ports {
        let name_ok = !p.name.is_empty()
            && p.name.len() <= 10
            && p.name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !name_ok || p.name == "game" {
            return Err(Error::InvalidArgument(format!(
                "spec.ports: name `{}` must be 1..=10 of [a-z0-9-] and not `game`",
                p.name
            )));
        }
        if !seen.insert(p.name.clone()) {
            return Err(Error::InvalidArgument(format!(
                "spec.ports: duplicate name `{}`",
                p.name
            )));
        }
        if !proto_ok(&p.protocol) {
            return Err(Error::InvalidArgument(format!(
                "spec.ports `{}`: protocol must be udp|tcp|both",
                p.name
            )));
        }
        if p.container_port.is_some_and(|c| !(1..=65535).contains(&c)) {
            return Err(Error::InvalidArgument(format!(
                "spec.ports `{}`: container_port in 1..=65535",
                p.name
            )));
        }
    }
    Ok(())
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
pub fn validate_slots(spec: &FleetSpec) -> Result<()> {
    if let Some(n) = spec.slots_per_server {
        if n == 0 || n > 1024 {
            return Err(Error::InvalidArgument(
                "spec.slots_per_server must be 1..=1024".into(),
            ));
        }
    }
    Ok(())
}

pub fn slots_per_server(spec: &FleetSpec) -> i32 {
    spec.slots_per_server.unwrap_or(1).max(1) as i32
}

/// Free allocation slots on live packed servers — they count as capacity the autoscaler does not
/// need to start fresh servers for.
pub fn packed_free_slots(spec: &FleetSpec, servers: &[GameServer]) -> i32 {
    let n = slots_per_server(spec);
    if n <= 1 {
        return 0;
    }
    servers
        .iter()
        .filter(|s| s.state == "allocated" || s.state == "in_game")
        .map(|s| (n - s.slots_used).max(0))
        .sum()
}

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
    /// Named counters `{name: {count, capacity}}` (rooms, bots, …) the server reports; allocation
    /// selectors and `policy.counter_target` use them.
    #[serde(default)]
    pub counters: serde_json::Value,
    /// Named lists `{name: {values: [..], capacity}}` (player ids, reserved slots, …).
    #[serde(default)]
    pub lists: serde_json::Value,
    /// Allocations currently hosted (see `FleetSpec::slots_per_server`).
    #[serde(default)]
    pub slots_used: i32,
    /// `ssh` provider: host-pool entry the server runs on.
    pub host_id: Option<Uuid>,
    /// Extra named host ports `{name: port}` (P0-7).
    #[serde(default)]
    pub ports: serde_json::Value,
}

/// Which servers an allocation may take (all conditions must hold).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AllocSelector {
    /// Every key must be present with this exact value.
    pub labels: serde_json::Map<String, serde_json::Value>,
    pub build_version: Option<String>,
    /// States a server may be in to be picked; default `["ready"]` — add `"allocated"` to share live
    /// servers (room/sub-match hosting): the caller then gets the server's existing connect token.
    pub states: Vec<String>,
    pub counters: HashMap<String, CounterSel>,
    pub lists: HashMap<String, ListSel>,
    /// Exactly this server (server browser joins).
    pub server_id: Option<Uuid>,
    /// Set-based label requirements (P0-30): `[{key, op: in|not_in|exists|does_not_exist, values}]`,
    /// all must hold. `in`/`not_in` compare against `values` (strings or JSON scalars).
    pub match_expressions: Vec<MatchExpression>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MatchOp {
    In,
    NotIn,
    #[default]
    Exists,
    DoesNotExist,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchExpression {
    pub key: String,
    pub op: MatchOp,
    pub values: Vec<serde_json::Value>,
}

/// How candidates that satisfy a selector are ranked (P0-30): by a counter's count or a list's
/// length, ascending or descending; earlier entries win, ties fall through to `scheduling`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Priority {
    /// `counter` | `list`
    pub kind: String,
    pub key: String,
    /// `asc` (default) | `desc`
    pub order: String,
}

/// Tie-break placement for an allocation request (P0-30). `packed` (default) fills the fullest /
/// oldest server first so idle ones can scale down; `distributed` prefers a fresh server, then the
/// host with most free servers, the emptiest and newest — spreading load and blast radius.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Scheduling {
    #[default]
    Packed,
    Distributed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CounterSel {
    pub min_count: Option<i64>,
    pub max_count: Option<i64>,
    /// `capacity - count` must be at least this.
    pub min_available: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListSel {
    pub contains: Option<String>,
    pub excludes: Option<String>,
    pub min_available: Option<i64>,
}

/// Change to one counter: set `count` and/or `capacity`, then add `delta` (checked against capacity).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CounterAction {
    pub delta: Option<i64>,
    pub count: Option<i64>,
    pub capacity: Option<i64>,
}

/// Change to one list: replace `values`, set `capacity`, then `remove` and `add` (unique, checked).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ListAction {
    pub add: Vec<String>,
    pub remove: Vec<String>,
    pub values: Option<Vec<String>>,
    pub capacity: Option<i64>,
}

/// Full allocation request: who may be taken and what to change on the server atomically.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AllocateRequest {
    pub payload: serde_json::Value,
    pub backfill: bool,
    pub selector: AllocSelector,
    pub counter_actions: HashMap<String, CounterAction>,
    pub list_actions: HashMap<String, ListAction>,
    /// Ordered fallback chain tried after `selector` (or instead of it when `selector` is empty):
    /// the first selector with a matching server wins; `Allocation::matched_selector` says which.
    pub selectors: Vec<AllocSelector>,
    pub priorities: Vec<Priority>,
    pub scheduling: Scheduling,
}

fn valid_slot_name(n: &str) -> Result<()> {
    if n.is_empty()
        || n.len() > 64
        || !n
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(Error::InvalidArgument(format!(
            "bad counter/list name {n:?}"
        )));
    }
    Ok(())
}

fn counter_parts(v: &serde_json::Value, name: &str) -> (i64, i64) {
    let c = &v[name];
    (
        c["count"].as_i64().unwrap_or(0),
        c["capacity"].as_i64().unwrap_or(0),
    )
}

fn list_parts(v: &serde_json::Value, name: &str) -> (Vec<String>, i64) {
    let l = &v[name];
    let values = l["values"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    (values, l["capacity"].as_i64().unwrap_or(0))
}

/// Apply one counter action; over capacity → `ResourceExhausted`, below zero → `InvalidArgument`.
pub fn apply_counter(
    counters: &mut serde_json::Value,
    name: &str,
    a: &CounterAction,
) -> Result<()> {
    valid_slot_name(name)?;
    if !counters.is_object() {
        *counters = serde_json::json!({});
    }
    let (mut count, mut cap) = counter_parts(counters, name);
    if let Some(c) = a.capacity {
        if c < 0 {
            return Err(Error::InvalidArgument("capacity must be >= 0".into()));
        }
        cap = c;
    }
    if let Some(c) = a.count {
        if c < 0 {
            return Err(Error::InvalidArgument("count must be >= 0".into()));
        }
        count = c;
    }
    if let Some(d) = a.delta {
        count += d;
        if count < 0 {
            return Err(Error::InvalidArgument(format!(
                "counter {name} would go below zero"
            )));
        }
    }
    if count > cap {
        return Err(Error::ResourceExhausted(format!(
            "counter {name}: {count} exceeds capacity {cap}"
        )));
    }
    counters[name] = serde_json::json!({ "count": count, "capacity": cap });
    Ok(())
}

/// Apply one list action (values stay unique and ordered); over capacity → `ResourceExhausted`.
pub fn apply_list(lists: &mut serde_json::Value, name: &str, a: &ListAction) -> Result<()> {
    valid_slot_name(name)?;
    if !lists.is_object() {
        *lists = serde_json::json!({});
    }
    let (mut values, mut cap) = list_parts(lists, name);
    if let Some(c) = a.capacity {
        if c < 0 {
            return Err(Error::InvalidArgument("capacity must be >= 0".into()));
        }
        cap = c;
    }
    if let Some(v) = &a.values {
        values.clear();
        for x in v {
            if !values.contains(x) {
                values.push(x.clone());
            }
        }
    }
    values.retain(|v| !a.remove.contains(v));
    for x in &a.add {
        if x.len() > 256 {
            return Err(Error::InvalidArgument("list value too long".into()));
        }
        if !values.contains(x) {
            values.push(x.clone());
        }
    }
    if values.len() as i64 > cap {
        return Err(Error::ResourceExhausted(format!(
            "list {name}: {} values exceed capacity {cap}",
            values.len()
        )));
    }
    lists[name] = serde_json::json!({ "values": values, "capacity": cap });
    Ok(())
}

/// Does `s` satisfy the selector (labels subset, build, counters, lists)? States are checked by SQL.
pub fn selector_matches(s: &GameServer, sel: &AllocSelector) -> bool {
    if sel.labels.iter().any(|(k, v)| s.labels.get(k) != Some(v)) {
        return false;
    }
    if !sel.states.is_empty() && !sel.states.contains(&s.state) {
        return false;
    }
    if sel.server_id.is_some_and(|id| id != s.id) {
        return false;
    }
    for e in &sel.match_expressions {
        let have = s.labels.get(&e.key);
        let listed = have.is_some_and(|v| e.values.iter().any(|w| label_eq(v, w)));
        let ok = match e.op {
            MatchOp::Exists => have.is_some(),
            MatchOp::DoesNotExist => have.is_none(),
            MatchOp::In => listed,
            MatchOp::NotIn => !listed,
        };
        if !ok {
            return false;
        }
    }
    if sel
        .build_version
        .as_ref()
        .is_some_and(|b| s.build_version.as_deref() != Some(b.as_str()))
    {
        return false;
    }
    for (name, c) in &sel.counters {
        let (count, cap) = counter_parts(&s.counters, name);
        if c.min_count.is_some_and(|m| count < m)
            || c.max_count.is_some_and(|m| count > m)
            || c.min_available.is_some_and(|m| cap - count < m)
        {
            return false;
        }
    }
    for (name, l) in &sel.lists {
        let (values, cap) = list_parts(&s.lists, name);
        if l.contains.as_ref().is_some_and(|v| !values.contains(v))
            || l.excludes.as_ref().is_some_and(|v| values.contains(v))
            || l.min_available
                .is_some_and(|m| (cap - values.len() as i64) < m)
        {
            return false;
        }
    }
    true
}

/// Labels are compared as JSON, and a string label also matches the textual form of a scalar
/// (`"3"` vs `3`, `"true"` vs `true`) so CLI/query-string callers need not care about types.
fn label_eq(have: &serde_json::Value, want: &serde_json::Value) -> bool {
    if have == want {
        return true;
    }
    let text = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    (have.is_string() || want.is_string()) && text(have) == text(want)
}

/// Reject malformed selectors/priorities before touching the store (P0-30).
pub fn validate_alloc(req: &AllocateRequest) -> Result<()> {
    for sel in std::iter::once(&req.selector).chain(req.selectors.iter()) {
        for e in &sel.match_expressions {
            if e.key.is_empty() {
                return Err(Error::InvalidArgument(
                    "match_expressions[].key is required".into(),
                ));
            }
            if matches!(e.op, MatchOp::In | MatchOp::NotIn) && e.values.is_empty() {
                return Err(Error::InvalidArgument(format!(
                    "match_expressions `{}`: in/not_in need values",
                    e.key
                )));
            }
        }
    }
    for p in &req.priorities {
        if !matches!(p.kind.as_str(), "counter" | "list") {
            return Err(Error::InvalidArgument(
                "priorities[].kind must be `counter` or `list`".into(),
            ));
        }
        valid_slot_name(&p.key)?;
        if !matches!(p.order.as_str(), "" | "asc" | "desc") {
            return Err(Error::InvalidArgument(
                "priorities[].order must be `asc` or `desc`".into(),
            ));
        }
    }
    Ok(())
}

/// The selectors an allocation tries, in order: the primary one when it says anything (or when
/// there is nothing else), then `selectors[]`.
pub fn selector_chain(req: &AllocateRequest) -> Vec<&AllocSelector> {
    let p = &req.selector;
    let primary_set = !p.labels.is_empty()
        || p.build_version.is_some()
        || !p.counters.is_empty()
        || !p.lists.is_empty()
        || !p.match_expressions.is_empty()
        || !p.states.is_empty()
        || p.server_id.is_some();
    let mut chain = Vec::with_capacity(req.selectors.len() + 1);
    if primary_set || req.selectors.is_empty() {
        chain.push(p);
    }
    chain.extend(req.selectors.iter());
    chain
}

fn priority_value(s: &GameServer, p: &Priority) -> i64 {
    match p.kind.as_str() {
        "list" => list_parts(&s.lists, &p.key).0.len() as i64,
        _ => counter_parts(&s.counters, &p.key).0,
    }
}

fn host_key(s: &GameServer) -> String {
    s.host_id
        .map(|h| h.to_string())
        .or_else(|| s.node_id.map(|n| n.to_string()))
        .unwrap_or_else(|| s.address.clone())
}

/// Choose the server for an allocation from `candidates` (in the store's packed order: fullest /
/// ready-first, oldest first): walk the selector chain, rank the matches by `priorities`, break
/// ties by `scheduling`. Returns the server and the index of the selector that matched.
pub fn pick_candidate(
    mut candidates: Vec<GameServer>,
    req: &AllocateRequest,
) -> Option<(GameServer, usize)> {
    let per_host: HashMap<String, usize> = candidates.iter().fold(HashMap::new(), |mut m, g| {
        *m.entry(host_key(g)).or_default() += 1;
        m
    });
    for (idx, sel) in selector_chain(req).into_iter().enumerate() {
        let mut matched: Vec<usize> = (0..candidates.len())
            .filter(|i| selector_matches(&candidates[*i], sel))
            .collect();
        if matched.is_empty() {
            continue;
        }
        let by_priority = |a: &GameServer, b: &GameServer| {
            for p in &req.priorities {
                let (ka, kb) = (priority_value(a, p), priority_value(b, p));
                let ord = if p.order == "desc" {
                    kb.cmp(&ka)
                } else {
                    ka.cmp(&kb)
                };
                if ord != std::cmp::Ordering::Equal {
                    return ord;
                }
            }
            std::cmp::Ordering::Equal
        };
        matched.sort_by(|&a, &b| {
            let (ga, gb) = (&candidates[a], &candidates[b]);
            let ord = by_priority(ga, gb);
            if req.scheduling != Scheduling::Distributed {
                return ord;
            }
            ord.then_with(|| per_host[&host_key(gb)].cmp(&per_host[&host_key(ga)]))
                .then_with(|| ga.slots_used.cmp(&gb.slots_used))
                .then_with(|| gb.created_at.cmp(&ga.created_at))
        });
        let i = matched[0];
        return Some((candidates.swap_remove(i), idx));
    }
    None
}

/// Keep at least `buffer` units of `counter` capacity free across the fleet's live servers
/// (`policy.counter_target`): the autoscaler adds servers until the aggregate is covered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct CounterTarget {
    pub counter: String,
    pub buffer: i64,
}

/// Servers to add so the fleet has `buffer` free units of the counter: free capacity on live
/// servers plus the per-server capacity (largest seen, else 1) assumed for servers still starting.
pub fn counter_shortfall(ct: &CounterTarget, servers: &[GameServer]) -> i32 {
    if ct.counter.is_empty() || ct.buffer <= 0 {
        return 0;
    }
    let mut per_server = 1i64;
    let mut available = 0i64;
    let mut starting = 0i64;
    for s in servers.iter().filter(|s| s.state != "terminated") {
        if s.state == "starting" || s.state == "scheduled" {
            starting += 1;
            continue;
        }
        let (count, cap) = counter_parts(&s.counters, &ct.counter);
        per_server = per_server.max(cap);
        available += (cap - count).max(0);
    }
    available += starting * per_server;
    if available >= ct.buffer {
        0
    } else {
        ((ct.buffer - available + per_server - 1) / per_server) as i32
    }
}

/// Cross-fleet server inventory filters (`GET /v1/server/gameservers`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ServerSearch {
    pub fleet_id: Option<Uuid>,
    pub fleet: Option<String>,
    pub pool: Option<String>,
    pub region: Option<String>,
    pub provider: Option<String>,
    /// Comma-separated states; default = every non-terminated state.
    pub state: Option<String>,
    pub build_version: Option<String>,
    pub exit_reason: Option<String>,
    pub node_id: Option<Uuid>,
    /// `key=value,key2=value2` — every pair must match.
    pub labels: Option<String>,
    pub min_age_secs: Option<i64>,
    pub max_age_secs: Option<i64>,
    pub min_players: Option<i32>,
    /// Only servers with `players < capacity`.
    pub free_slots: Option<bool>,
    pub counter: Option<String>,
    pub counter_min_available: Option<i64>,
    pub list: Option<String>,
    pub list_contains: Option<String>,
    /// `created_at|last_health|players|age` with optional leading `-` for descending.
    pub order: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// A server with its fleet's identity for inventory views.
#[derive(Debug, Clone, Serialize)]
pub struct ServerView {
    #[serde(flatten)]
    pub server: GameServer,
    pub fleet: String,
    pub region: String,
    pub pool: String,
    pub provider: String,
    pub exit_reason: Option<String>,
    pub age_secs: i64,
}

#[derive(sqlx::FromRow)]
struct ViewRow {
    #[sqlx(flatten)]
    s: ServerRow,
    fleet: String,
    region: String,
    pool: String,
    provider: String,
}

fn push_server_filters(
    qb: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>,
    q: &ServerSearch,
) -> Result<()> {
    if let Some(id) = q.fleet_id {
        qb.push(" and g.fleet_id = ").push_bind(id);
    }
    if let Some(v) = q.fleet.clone() {
        qb.push(" and f.name = ").push_bind(v);
    }
    if let Some(v) = q.pool.clone() {
        qb.push(" and f.pool = ").push_bind(v);
    }
    if let Some(v) = q.region.clone() {
        qb.push(" and f.region = ").push_bind(v);
    }
    if let Some(v) = q.provider.clone() {
        qb.push(" and f.provider = ").push_bind(v);
    }
    match q.state.as_deref() {
        Some("all") => {}
        Some(st) => {
            let states: Vec<String> = st
                .split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect();
            qb.push(" and g.state = any(").push_bind(states).push(")");
        }
        None => {
            qb.push(" and g.state <> 'terminated'");
        }
    }
    if let Some(v) = q.build_version.clone() {
        qb.push(" and g.build_version = ").push_bind(v);
    }
    if let Some(v) = q.exit_reason.clone() {
        qb.push(" and g.labels->>'exit_reason' = ").push_bind(v);
    }
    if let Some(v) = q.node_id {
        qb.push(" and g.node_id = ").push_bind(v);
    }
    if let Some(l) = &q.labels {
        let mut m = serde_json::Map::new();
        for pair in l.split(',').filter(|p| !p.trim().is_empty()) {
            let (k, v) = pair.split_once('=').ok_or_else(|| {
                Error::InvalidArgument(format!("labels: expected key=value, got {pair:?}"))
            })?;
            m.insert(
                k.trim().to_string(),
                serde_json::Value::String(v.trim().to_string()),
            );
        }
        qb.push(" and g.labels @> ")
            .push_bind(serde_json::Value::Object(m));
    }
    if let Some(v) = q.min_age_secs {
        qb.push(" and g.created_at <= now() - make_interval(secs => ")
            .push_bind(v as f64)
            .push(")");
    }
    if let Some(v) = q.max_age_secs {
        qb.push(" and g.created_at >= now() - make_interval(secs => ")
            .push_bind(v as f64)
            .push(")");
    }
    if let Some(v) = q.min_players {
        qb.push(" and g.players >= ").push_bind(v);
    }
    if q.free_slots == Some(true) {
        qb.push(" and g.capacity > 0 and g.players < g.capacity");
    }
    if let Some(c) = q.counter.clone() {
        valid_slot_name(&c)?;
        let min = q.counter_min_available.unwrap_or(1);
        qb.push(" and coalesce((g.counters->")
            .push_bind(c.clone())
            .push("->>'capacity')::bigint, 0) - coalesce((g.counters->")
            .push_bind(c)
            .push("->>'count')::bigint, 0) >= ")
            .push_bind(min);
    }
    if let Some(l) = q.list.clone() {
        valid_slot_name(&l)?;
        if let Some(v) = q.list_contains.clone() {
            qb.push(" and g.lists->")
                .push_bind(l)
                .push("->'values' @> ")
                .push_bind(serde_json::json!([v]));
        } else {
            qb.push(" and g.lists ? ").push_bind(l);
        }
    }
    Ok(())
}

const VIEW_SELECT: &str = "select g.id, g.fleet_id, g.state, g.node_id, g.address, g.port, g.pid, g.labels, g.allocation, g.last_health, g.created_at, g.build_version, g.players, g.capacity, g.version, g.ready_at, g.counters, g.lists, g.slots_used, g.host_id, g.ports, f.name as fleet, f.region, f.pool, f.provider from musubi.game_servers g join musubi.fleets f on f.id = g.fleet_id where true";

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
    /// Index into the request's selector chain that matched (0 = the primary selector).
    #[serde(default)]
    pub matched_selector: usize,
    /// Extra named host ports of the server `{name: port}` (P0-7).
    #[serde(default)]
    pub ports: serde_json::Value,
    /// Why this fleet/region was chosen by a routed allocation (`region`, `reason:
    /// preferred|ping|distance|hint|fallback`, `ping_ms`, `distance_km`, `rank`, `tried`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<serde_json::Value>,
}

/// Where the player is, for distance-based region choice.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct GeoPoint {
    pub lat: f64,
    pub lon: f64,
}

/// Great-circle distance in kilometres.
pub fn haversine_km(a: GeoPoint, b: GeoPoint) -> f64 {
    let (la1, la2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = la2 - la1;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

/// How a routed allocation should pick among the fleets of a pool: an explicit ordered region
/// preference, the client's measured pings per region, a ping ceiling, the client's position,
/// and a plain region hint (e.g. the matchmaker's best region).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct RouteHint {
    pub region: Option<String>,
    pub regions: Vec<String>,
    pub pings: HashMap<String, f64>,
    pub max_ping_ms: Option<f64>,
    pub client: Option<GeoPoint>,
}

impl RouteHint {
    pub fn region(region: Option<&str>) -> Self {
        Self {
            region: region.map(str::to_string),
            ..Default::default()
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.pings.len() > 256 || self.regions.len() > 64 {
            return Err(Error::InvalidArgument(
                "pings: at most 256 regions, regions: at most 64".into(),
            ));
        }
        if self.pings.values().any(|p| !p.is_finite() || *p < 0.0) {
            return Err(Error::InvalidArgument(
                "pings must be non-negative milliseconds".into(),
            ));
        }
        if self.max_ping_ms.is_some_and(|m| !m.is_finite() || m <= 0.0) {
            return Err(Error::InvalidArgument("max_ping_ms must be > 0".into()));
        }
        if let Some(c) = self.client {
            if !(-90.0..=90.0).contains(&c.lat) || !(-180.0..=180.0).contains(&c.lon) {
                return Err(Error::InvalidArgument(
                    "client.lat in -90..90, client.lon in -180..180".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Order the fleets of a pool for a routed allocation and explain each choice. Tiers: explicit
/// `regions` order → lowest ping → nearest by coordinates → the `region` hint → the rest (stable
/// by name). Fleets whose region pings above `max_ping_ms` are dropped.
/// (tier, preference index, ping or distance, fleet name) — sort key of `rank_fleets`.
type RouteKey = (u8, usize, f64, String);

pub fn rank_fleets(
    fleets: Vec<Fleet>,
    hint: &RouteHint,
    coords: &HashMap<String, GeoPoint>,
) -> Vec<(Fleet, serde_json::Value)> {
    let mut scored: Vec<(RouteKey, Fleet, serde_json::Value)> = Vec::new();
    for f in fleets {
        let ping = hint.pings.get(&f.region).copied();
        if let (Some(p), Some(max)) = (ping, hint.max_ping_ms) {
            if p > max {
                continue;
            }
        }
        let dist = match (hint.client, coords.get(&f.region)) {
            (Some(c), Some(r)) => Some(haversine_km(c, *r)),
            _ => None,
        };
        let (key, mut why) = if let Some(i) = hint.regions.iter().position(|r| *r == f.region) {
            (
                (0u8, i, 0.0, f.name.clone()),
                serde_json::json!({ "reason": "preferred" }),
            )
        } else if let Some(p) = ping {
            (
                (1, 0, p, f.name.clone()),
                serde_json::json!({ "reason": "ping", "ping_ms": p }),
            )
        } else if let Some(d) = dist {
            (
                (2, 0, d, f.name.clone()),
                serde_json::json!({ "reason": "distance", "distance_km": d.round() }),
            )
        } else if hint.region.as_deref() == Some(f.region.as_str()) {
            (
                (3, 0, 0.0, f.name.clone()),
                serde_json::json!({ "reason": "hint" }),
            )
        } else {
            (
                (4, 0, 0.0, f.name.clone()),
                serde_json::json!({ "reason": "fallback" }),
            )
        };
        why["region"] = serde_json::json!(f.region);
        why["fleet"] = serde_json::json!(f.name);
        if let Some(p) = ping {
            why["ping_ms"] = serde_json::json!(p);
        }
        if let Some(d) = dist {
            why["distance_km"] = serde_json::json!(d.round());
        }
        scored.push((key, f, why));
    }
    scored.sort_by(|a, b| {
        a.0 .0
            .cmp(&b.0 .0)
            .then(a.0 .1.cmp(&b.0 .1))
            .then(a.0 .2.total_cmp(&b.0 .2))
            .then(a.0 .3.cmp(&b.0 .3))
    });
    scored
        .into_iter()
        .enumerate()
        .map(|(i, (_, f, mut why))| {
            why["rank"] = serde_json::json!(i);
            (f, why)
        })
        .collect()
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
    /// Host-pool id the server runs on (`ssh` provider), for preemption and capacity accounting.
    fn host_of(&self, _server_id: Uuid) -> Option<Uuid> {
        None
    }

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

/// (fleet, rule) → (condition holding since, last fired) in unix seconds.
type RuleState = HashMap<(Uuid, String), (Option<i64>, Option<i64>)>;

#[derive(Clone)]
pub struct FleetService {
    region_coords: Arc<HashMap<String, GeoPoint>>,
    log_archive: Option<Arc<dyn LogArchive>>,
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
    /// (fleet, rule) → (condition holding since, last fired) in unix seconds.
    rule_state: Arc<std::sync::Mutex<RuleState>>,
    /// Last autoscaler decision per fleet (`GET …/autoscaler`, Prometheus gauges).
    scale_status: Arc<std::sync::Mutex<HashMap<Uuid, serde_json::Value>>>,
    log_tx: LogSink,
    log_rx: Arc<std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<LogLine>>>>,
    /// Live fan-out of every stored line (`…/logs/stream` followers).
    log_tap: tokio::sync::broadcast::Sender<LogLine>,
    /// Host pool + capacity pools behind the `ssh` provider (`enable_ssh`).
    capacity: Arc<std::sync::RwLock<Option<Arc<capacity::CapacityState>>>>,
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

#[derive(sqlx::FromRow)]
struct ServerRow {
    id: Uuid,
    fleet_id: Uuid,
    state: String,
    node_id: Option<Uuid>,
    address: String,
    port: i32,
    pid: Option<i64>,
    labels: serde_json::Value,
    allocation: Option<serde_json::Value>,
    last_health: OffsetDateTime,
    created_at: OffsetDateTime,
    build_version: Option<String>,
    players: i32,
    capacity: i32,
    version: i64,
    ready_at: Option<OffsetDateTime>,
    counters: serde_json::Value,
    lists: serde_json::Value,
    slots_used: i32,
    host_id: Option<Uuid>,
    ports: serde_json::Value,
}

fn row_to_server(r: ServerRow) -> GameServer {
    GameServer {
        id: r.id,
        fleet_id: r.fleet_id,
        state: r.state,
        node_id: r.node_id,
        address: r.address,
        port: r.port,
        pid: r.pid,
        labels: r.labels,
        allocation: r.allocation,
        last_health: r.last_health,
        created_at: r.created_at,
        build_version: r.build_version,
        players: r.players,
        capacity: r.capacity,
        version: r.version,
        ready_at: r.ready_at,
        counters: r.counters,
        lists: r.lists,
        slots_used: r.slots_used,
        host_id: r.host_id,
        ports: r.ports,
    }
}

const FLEET_COLS: &str =
    "id, name, provider, region, spec, buffer, max_servers, port_start, port_end, created_at, min_servers, policy, pool";
const SERVER_COLS: &str =
    "id, fleet_id, state, node_id, address, port, pid, labels, allocation, last_health, created_at, build_version, players, capacity, version, ready_at, counters, lists, slots_used, host_id, ports";

impl FleetService {
    /// Base URL game servers use to reach this API (`MUSUBI_API`).
    pub fn api_url(&self) -> &str {
        &self.api_url
    }

    /// Constant-time-irrelevant equality for the shared server key (used by capability routes).
    pub fn server_key_matches(&self, k: &str) -> bool {
        !self.server_key.is_empty() && self.server_key == k
    }

    pub fn new(db: PgPool, node_id: Uuid, api_url: &str, server_key: &str) -> Self {
        let (log_tx, log_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut providers: HashMap<String, Arc<dyn Provider>> = HashMap::new();
        let local = LocalProcessProvider::default();
        local.set_log_sink(log_tx.clone());
        providers.insert("local".into(), Arc::new(local));
        providers.insert("docker".into(), Arc::new(docker::DockerProvider));
        Self {
            region_coords: Arc::new(HashMap::new()),
            log_archive: None,
            db,
            node_id,
            providers: Arc::new(std::sync::RwLock::new(providers)),
            api_url: api_url.to_string(),
            server_key: server_key.to_string(),
            edge: None,
            last_scale: Arc::new(std::sync::Mutex::new(HashMap::new())),
            rule_state: Arc::new(std::sync::Mutex::new(HashMap::new())),
            scale_status: Arc::new(std::sync::Mutex::new(HashMap::new())),
            log_tx,
            log_rx: Arc::new(std::sync::Mutex::new(Some(log_rx))),
            log_tap: tokio::sync::broadcast::channel(4096).0,
            capacity: Arc::new(std::sync::RwLock::new(None)),
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
        let tap = self.log_tap.clone();
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
                if tap.receiver_count() > 0 {
                    for l in &buf {
                        let _ = tap.send(l.clone());
                    }
                }
                buf.clear();
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        });
    }

    /// Subscribe to every line stored from now on (all fleets; filter by `server_id`). Slow
    /// readers lag and skip, they never block the writer.
    pub fn log_tap(&self) -> tokio::sync::broadcast::Receiver<LogLine> {
        self.log_tap.subscribe()
    }

    /// Stored log lines of one server, newest first (`limit` clamped to 1..=5000).
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
        .bind(q.limit.clamp(1, 5000) as i64)
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
    /// `[fleet.regions]` coordinates for distance-based routing.
    pub fn with_region_coords(mut self, coords: HashMap<String, GeoPoint>) -> Self {
        self.region_coords = Arc::new(coords);
        self
    }

    /// Object storage for log archives (`spec.logs.archive`, `POST …/logs/archive`).
    pub fn with_log_archive(mut self, store: Arc<dyn LogArchive>) -> Self {
        self.log_archive = Some(store);
        self
    }

    pub fn has_log_archive(&self) -> bool {
        self.log_archive.is_some()
    }

    /// Cut an archive of everything stored for `server_id` so far: gzip NDJSON in object storage,
    /// a row in `musubi.server_log_archives`, rows purged from Postgres when the fleet says so.
    pub async fn archive_server_logs(
        &self,
        fleet: &Fleet,
        server_id: Uuid,
        reason: &str,
    ) -> Result<LogArchiveEntry> {
        use std::io::Write;
        let store = self.log_archive.clone().ok_or_else(|| {
            Error::Unavailable("log archive: no object storage configured ([ugc.s3])".into())
        })?;
        type Row = (Uuid, OffsetDateTime, String, String, String);
        let rows: Vec<Row> = sqlx::query_as(
            "select id, at, source, level, message from musubi.server_logs
             where server_id = $1 and fleet_id = $2 order by at, id limit 500000",
        )
        .bind(server_id)
        .bind(fleet.id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        if rows.is_empty() {
            return Err(Error::NotFound("no stored log lines to archive".into()));
        }
        let (from_at, to_at) = (rows[0].1, rows[rows.len() - 1].1);
        let last_id = rows[rows.len() - 1].0;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        for (_, at, source, level, message) in &rows {
            let line = serde_json::json!({
                "at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
                "source": source, "level": level, "message": message,
            });
            enc.write_all(line.to_string().as_bytes())
                .and_then(|_| enc.write_all(b"\n"))
                .map_err(|e| Error::Internal(format!("gzip: {e}")))?;
        }
        let bytes = enc
            .finish()
            .map_err(|e| Error::Internal(format!("gzip: {e}")))?;
        let id = Uuid::now_v7();
        let prefix = fleet
            .spec
            .logs
            .prefix
            .clone()
            .unwrap_or_else(|| LOG_ARCHIVE_PREFIX.to_string());
        let key = format!(
            "{prefix}{}/{server_id}/{}-{}.ndjson.gz",
            fleet.id,
            to_at.unix_timestamp(),
            id.simple()
        );
        store.put(&key, bytes.clone(), "application/gzip").await?;
        let row: LogArchiveRow = sqlx::query_as(&format!(
            "insert into musubi.server_log_archives (id, server_id, fleet_id, key, bytes, lines, from_at, to_at, reason)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning {LOG_ARCHIVE_COLS}"
        ))
        .bind(id)
        .bind(server_id)
        .bind(fleet.id)
        .bind(&key)
        .bind(bytes.len() as i64)
        .bind(rows.len() as i32)
        .bind(from_at)
        .bind(to_at)
        .bind(reason)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut purged = 0u64;
        if fleet.spec.logs.purge_after_archive {
            purged = sqlx::query(
                "delete from musubi.server_logs where server_id = $1 and (at < $2 or (at = $2 and id <= $3))",
            )
            .bind(server_id)
            .bind(to_at)
            .bind(last_id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        }
        self.record_event(
            fleet.id,
            Some(server_id),
            "server.logs_archived",
            format!("{} log lines archived to {key} ({reason})", rows.len()),
            serde_json::json!({
                "archive_id": id, "key": key, "bytes": bytes.len(), "lines": rows.len(),
                "reason": reason, "purged": purged,
            }),
        )
        .await;
        Ok(archive_entry(row))
    }

    /// Archive pass for one fleet: terminated servers past `spec.logs.delay_secs` that still have
    /// stored lines and no archive yet. Returns how many archives were cut.
    pub async fn archive_terminated_logs(&self, fleet: &Fleet) -> Result<usize> {
        if self.log_archive.is_none() || !fleet.spec.logs.archive {
            return Ok(0);
        }
        let ids: Vec<(Uuid,)> = sqlx::query_as(
            "select gs.id from musubi.game_servers gs
             where gs.fleet_id = $1 and gs.state = 'terminated'
               and coalesce(gs.terminated_at, gs.updated_at) < now() - make_interval(secs => $2::double precision)
               and exists (select 1 from musubi.server_logs l where l.server_id = gs.id)
               and not exists (select 1 from musubi.server_log_archives a
                               where a.server_id = gs.id and a.created_at >= coalesce(gs.terminated_at, gs.updated_at))
             order by gs.terminated_at limit 50",
        )
        .bind(fleet.id)
        .bind(fleet.spec.logs.delay_secs as f64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut n = 0;
        for (sid,) in ids {
            match self.archive_server_logs(fleet, sid, "terminated").await {
                Ok(_) => n += 1,
                Err(e) => tracing::warn!(server = %sid, error = %e, "log archive failed"),
            }
        }
        Ok(n)
    }

    pub async fn log_archives(
        &self,
        fleet_id: Uuid,
        server_id: Uuid,
    ) -> Result<Vec<LogArchiveEntry>> {
        let rows: Vec<LogArchiveRow> = sqlx::query_as(&format!(
            "select {LOG_ARCHIVE_COLS} from musubi.server_log_archives
             where fleet_id = $1 and server_id = $2 order by created_at desc"
        ))
        .bind(fleet_id)
        .bind(server_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(archive_entry).collect())
    }

    pub async fn log_archive(&self, fleet_id: Uuid, id: Uuid) -> Result<LogArchiveEntry> {
        let row: Option<LogArchiveRow> = sqlx::query_as(&format!(
            "select {LOG_ARCHIVE_COLS} from musubi.server_log_archives where fleet_id = $1 and id = $2"
        ))
        .bind(fleet_id)
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(archive_entry)
            .ok_or_else(|| Error::NotFound("log archive not found".into()))
    }

    /// The archive object itself (gzip NDJSON).
    pub async fn log_archive_bytes(&self, entry: &LogArchiveEntry) -> Result<Vec<u8>> {
        let store = self.log_archive.clone().ok_or_else(|| {
            Error::Unavailable("log archive: no object storage configured ([ugc.s3])".into())
        })?;
        store.get(&entry.key).await
    }

    pub async fn delete_log_archive(&self, fleet_id: Uuid, id: Uuid) -> Result<()> {
        let entry = self.log_archive(fleet_id, id).await?;
        if let Some(store) = &self.log_archive {
            store.delete(&entry.key).await?;
        }
        sqlx::query("delete from musubi.server_log_archives where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn log_archive_count(&self, server_id: Uuid) -> Result<i64> {
        let (n,): (i64,) =
            sqlx::query_as("select count(*) from musubi.server_log_archives where server_id = $1")
                .bind(server_id)
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(n)
    }

    pub async fn allocate_routed(
        &self,
        target: &str,
        region: Option<&str>,
        payload: serde_json::Value,
        backfill: bool,
        wait: std::time::Duration,
    ) -> Result<Allocation> {
        self.allocate_routed_with(
            target,
            &RouteHint::region(region),
            AllocateRequest {
                payload,
                backfill,
                ..Default::default()
            },
            wait,
        )
        .await
    }

    /// `allocate_routed` with a full request (selector + counter/list actions).
    pub async fn allocate_routed_with(
        &self,
        target: &str,
        hint: &RouteHint,
        req: AllocateRequest,
        wait: std::time::Duration,
    ) -> Result<Allocation> {
        hint.validate()?;
        let all: Vec<Fleet> = self
            .list()
            .await?
            .into_iter()
            .filter(|f| f.name == target || (!f.pool.is_empty() && f.pool == target))
            .collect();
        if all.is_empty() {
            return Err(Error::NotFound(format!("no fleet or pool {target}")));
        }
        let ranked = rank_fleets(all, hint, &self.region_coords);
        if ranked.is_empty() {
            return Err(Error::ResourceExhausted(format!(
                "no region of {target} within max_ping_ms"
            )));
        }
        let fleets: Vec<Fleet> = ranked.iter().map(|(f, _)| f.clone()).collect();
        for (i, (f, why)) in ranked.iter().enumerate() {
            match self.allocate_with(f.id, req.clone()).await {
                Ok(mut a) => {
                    let mut why = why.clone();
                    why["tried"] = serde_json::json!(i + 1);
                    a.route = Some(why);
                    return Ok(a);
                }
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
                match self.allocate_with(f.id, req.clone()).await {
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

    pub async fn create(&self, mut c: CreateFleet) -> Result<Fleet> {
        validate_slots(&c.spec)?;
        validate_secret_env(&c.spec)?;
        seal_secret_env(&mut c.spec);
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
        validate_ports(&c.spec)?;
        validate_kubernetes(&c.spec)?;
        validate_logs(&c.spec)?;
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

    /// `GET /v1/server/fleets/{id}/health`: every live server classified under the fleet's
    /// health policy, with counts per status.
    pub async fn health_report(&self, fleet_id: Uuid) -> Result<serde_json::Value> {
        let f = self.get(fleet_id).await?;
        let servers = self.servers(fleet_id, None).await?;
        let h = &f.policy.health;
        let now = OffsetDateTime::now_utc();
        let mut counts: std::collections::BTreeMap<&'static str, i64> = Default::default();
        let mut out = Vec::new();
        for s in servers.iter().filter(|s| s.state != "terminated") {
            // ceil so the report agrees with the reconciler's `>` on sub-second remainders
            let since_health_f = (now - s.last_health).as_seconds_f64().max(0.0);
            let since_health = since_health_f.ceil() as i64;
            let since_created = (now - s.created_at).as_seconds_f64().ceil().max(0.0) as i64;
            let status = h.status(&s.state, since_health, since_created);
            *counts.entry(status).or_default() += 1;
            let missed = if matches!(status, "starting" | "stuck_starting" | "unchecked") {
                0
            } else {
                (since_health_f / h.period_secs.max(1) as f64).floor() as i64
            };
            out.push(serde_json::json!({
                "id": s.id, "state": s.state, "status": status,
                "last_health_secs_ago": since_health, "missed_checks": missed,
                "age_secs": since_created, "players": s.players, "build_version": s.build_version,
                "reap_in_secs": match status {
                    "healthy" | "suspect" => Some((h.timeout().as_secs() as i64 - since_health).max(0)),
                    "starting" => Some((h.initial_delay_secs as i64 - since_created).max(0)),
                    _ => None,
                },
            }));
        }
        Ok(serde_json::json!({
            "fleet_id": fleet_id,
            "policy": h,
            "eviction": f.policy.eviction,
            "timeout_secs": h.timeout().as_secs(),
            "counts": counts,
            "servers": out,
        }))
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
             where id = $1 and state in ('allocated', 'in_game')
               and (connect_token = $2 or coalesce(allocation->'slots', '[]'::jsonb) @> jsonb_build_array(jsonb_build_object('token', $2::text)))
             returning allocation",
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
        self.allocate_with(
            fleet_id,
            AllocateRequest {
                payload,
                backfill,
                ..Default::default()
            },
        )
        .await
    }

    /// Player-slot backfill: a live server with free player slots takes the newcomers (same token).
    async fn try_backfill(
        &self,
        fleet_id: Uuid,
        payload: &serde_json::Value,
    ) -> Result<Option<Allocation>> {
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
        Ok(match r {
            Some((id, address, port, Some(token))) => {
                let (address, port) = match self.edge {
                    Some(e) => (e.ip().to_string(), e.port() as i32),
                    None => (address, port),
                };
                Some(Allocation {
                    route: None,
                    server_id: id,
                    fleet_id,
                    address,
                    port,
                    connect_token: token,
                    backfill: true,
                    matched_selector: 0,
                    ports: serde_json::json!({}),
                })
            }
            _ => None,
        })
    }

    /// Hand a server to a caller. Candidates are the fleet's servers in `selector.states`
    /// (default `ready`), oldest ready first; the first one matching the selector (labels, build,
    /// counters, lists) is taken, the counter/list actions are applied in the same transaction
    /// (an action exceeding a capacity fails the allocation), and the server becomes `allocated`
    /// with a fresh connect token — or, when it was already allocated, keeps its token and the
    /// payload is appended to its backfill list.
    /// P1-46: place the allocation on the fullest live server that still has a free slot. Each
    /// slot gets its own connect token (`allocation.slots[]`), released with `sdk_release`.
    async fn pack_allocation(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        fleet_id: Uuid,
        slots: i32,
        req: &AllocateRequest,
    ) -> Result<Option<Allocation>> {
        let rows: Vec<ServerRow> = sqlx::query_as(&format!(
            "select {SERVER_COLS} from musubi.game_servers where fleet_id = $1 and state in ('allocated', 'in_game')
               and slots_used < $2 and ($3::uuid is null or id = $3)
             order by slots_used desc, created_at for update skip locked"
        ))
        .bind(fleet_id)
        .bind(slots)
        .bind(req.selector.server_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let Some((mut gs, matched)) =
            pick_candidate(rows.into_iter().map(row_to_server).collect(), req)
        else {
            return Ok(None);
        };
        for (name, a) in &req.counter_actions {
            apply_counter(&mut gs.counters, name, a)?;
        }
        for (name, a) in &req.list_actions {
            apply_list(&mut gs.lists, name, a)?;
        }
        let token = format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple());
        let entry = serde_json::json!({ "payload": req.payload, "at": OffsetDateTime::now_utc().unix_timestamp(), "token": token });
        sqlx::query(
            "update musubi.game_servers set allocation = coalesce(allocation, '{}'::jsonb) || jsonb_build_object('slots',
                    coalesce(allocation->'slots', '[]'::jsonb) || jsonb_build_array($2::jsonb)),
                 slots_used = slots_used + 1, counters = $3, lists = $4, updated_at = now() where id = $1",
        )
        .bind(gs.id)
        .bind(&entry)
        .bind(&gs.counters)
        .bind(&gs.lists)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
        let (address, port) = match self.edge {
            Some(e) => (e.ip().to_string(), e.port() as i32),
            None => (gs.address, gs.port),
        };
        Ok(Some(Allocation {
            route: None,
            server_id: gs.id,
            fleet_id,
            address,
            port,
            connect_token: token,
            backfill: true,
            matched_selector: matched,
            ports: gs.ports.clone(),
        }))
    }

    /// `POST /v1/gameserver/release {token}`: the match for one allocation ended — free its slot;
    /// the server returns to `ready` when the last slot is released.
    pub async fn sdk_release(&self, id: Uuid, token: &str) -> Result<serde_json::Value> {
        let left: Option<i32> = sqlx::query_scalar(
            "update musubi.game_servers set slots_used = greatest(slots_used - 1, 0),
                 allocation = coalesce(allocation, '{}'::jsonb) || jsonb_build_object('slots',
                    (select coalesce(jsonb_agg(e), '[]'::jsonb) from jsonb_array_elements(coalesce(allocation->'slots', '[]'::jsonb)) e
                      where e->>'token' <> $2)),
                 updated_at = now()
             where id = $1 and state in ('allocated', 'in_game')
               and (connect_token = $2 or coalesce(allocation->'slots', '[]'::jsonb) @> jsonb_build_array(jsonb_build_object('token', $2::text)))
             returning slots_used",
        )
        .bind(id)
        .bind(token)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some(left) = left else {
            return Err(Error::NotFound("allocation for this token".into()));
        };
        let mut state = "allocated";
        if left == 0 {
            sqlx::query(
                "update musubi.game_servers set state = 'ready', allocation = null, connect_token = null, updated_at = now()
                 where id = $1 and state in ('allocated', 'in_game')",
            )
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
            state = "ready";
        }
        if let Ok(s) = self.server(id).await {
            self.record_event(
                s.fleet_id,
                Some(id),
                "server.released",
                "allocation released",
                serde_json::json!({ "slots_used": left, "state": state }),
            )
            .await;
        }
        Ok(serde_json::json!({ "slots_used": left, "state": state }))
    }

    pub async fn allocate_with(&self, fleet_id: Uuid, req: AllocateRequest) -> Result<Allocation> {
        validate_alloc(&req)?;
        if req.backfill {
            if let Some(a) = self.try_backfill(fleet_id, &req.payload).await? {
                return Ok(a);
            }
        }
        for n in req.counter_actions.keys().chain(req.list_actions.keys()) {
            valid_slot_name(n)?;
        }
        let mut states: Vec<String> = selector_chain(&req)
            .iter()
            .flat_map(|s| s.states.iter().cloned())
            .collect();
        states.sort();
        states.dedup();
        if states.is_empty() {
            states.push("ready".into());
        }
        let slots = slots_per_server(&self.get(fleet_id).await?.spec);
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        if slots > 1 && req.scheduling == Scheduling::Packed {
            if let Some(a) = self.pack_allocation(&mut tx, fleet_id, slots, &req).await? {
                tx.commit().await.map_err(Error::internal)?;
                self.record_event(
                    fleet_id,
                    Some(a.server_id),
                    "server.allocated",
                    "allocation packed onto a live server",
                    serde_json::json!({ "self": false, "packed": true, "selector": req.selector }),
                )
                .await;
                return Ok(a);
            }
        }
        let rows: Vec<ServerRow> = sqlx::query_as(&format!(
            "select {SERVER_COLS} from musubi.game_servers where fleet_id = $1 and state = any($2)
               and ($3::uuid is null or id = $3)
             order by (case state when 'ready' then 0 else 1 end), created_at for update skip locked"
        ))
        .bind(fleet_id)
        .bind(&states)
        .bind(req.selector.server_id)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((mut gs, matched)) =
            pick_candidate(rows.into_iter().map(row_to_server).collect(), &req)
        else {
            if slots > 1 && req.scheduling == Scheduling::Distributed {
                if let Some(a) = self.pack_allocation(&mut tx, fleet_id, slots, &req).await? {
                    tx.commit().await.map_err(Error::internal)?;
                    self.record_event(
                        fleet_id,
                        Some(a.server_id),
                        "server.allocated",
                        "allocation packed onto a live server",
                        serde_json::json!({ "self": false, "packed": true, "selector": req.selector }),
                    )
                    .await;
                    return Ok(a);
                }
            }
            return Err(Error::ResourceExhausted(
                "no ready server in fleet matches the selector".into(),
            ));
        };
        for (name, a) in &req.counter_actions {
            apply_counter(&mut gs.counters, name, a)?;
        }
        for (name, a) in &req.list_actions {
            apply_list(&mut gs.lists, name, a)?;
        }
        let fresh_token = format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple());
        let entry = serde_json::json!({ "payload": req.payload, "at": OffsetDateTime::now_utc().unix_timestamp(), "token": fresh_token });
        let shared = gs.state != "ready";
        let token = if shared {
            let existing: Option<String> =
                sqlx::query_scalar("select connect_token from musubi.game_servers where id = $1")
                    .bind(gs.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
            let token = existing.unwrap_or_else(|| {
                format!("{}{}", Uuid::now_v7().simple(), Uuid::now_v7().simple())
            });
            sqlx::query(
                "update musubi.game_servers set allocation = coalesce(allocation, '{}'::jsonb) || jsonb_build_object('backfill',
                        coalesce(allocation->'backfill', '[]'::jsonb) || jsonb_build_array($2::jsonb)),
                     connect_token = $3, counters = $4, lists = $5, slots_used = slots_used + 1, updated_at = now() where id = $1",
            )
            .bind(gs.id)
            .bind(&entry)
            .bind(&token)
            .bind(&gs.counters)
            .bind(&gs.lists)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            token
        } else {
            let token = fresh_token.clone();
            sqlx::query(
                "update musubi.game_servers set state = 'allocated', allocation = $2, connect_token = $3,
                     counters = $4, lists = $5, slots_used = 1, updated_at = now() where id = $1",
            )
            .bind(gs.id)
            .bind(&entry)
            .bind(&token)
            .bind(&gs.counters)
            .bind(&gs.lists)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            token
        };
        tx.commit().await.map_err(Error::internal)?;
        self.record_event(
            fleet_id,
            Some(gs.id),
            "server.allocated",
            if shared {
                "server shared with another allocation"
            } else {
                "server allocated"
            },
            serde_json::json!({ "self": false, "shared": shared, "selector": req.selector }),
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
            None => (gs.address, gs.port),
        };
        Ok(Allocation {
            route: None,
            server_id: gs.id,
            fleet_id,
            address,
            port,
            connect_token: token,
            backfill: shared,
            matched_selector: matched,
            ports: gs.ports.clone(),
        })
    }

    /// Apply counter actions reported by the server itself (or an operator).
    pub async fn update_counters(
        &self,
        id: Uuid,
        actions: &HashMap<String, CounterAction>,
    ) -> Result<GameServer> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut counters: serde_json::Value =
            sqlx::query_scalar("select counters from musubi.game_servers where id = $1 for update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?
                .ok_or_else(|| Error::NotFound("game server".into()))?;
        for (name, a) in actions {
            apply_counter(&mut counters, name, a)?;
        }
        sqlx::query(
            "update musubi.game_servers set counters = $2, updated_at = now() where id = $1",
        )
        .bind(id)
        .bind(&counters)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        self.server(id).await
    }

    pub async fn update_lists(
        &self,
        id: Uuid,
        actions: &HashMap<String, ListAction>,
    ) -> Result<GameServer> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut lists: serde_json::Value =
            sqlx::query_scalar("select lists from musubi.game_servers where id = $1 for update")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?
                .ok_or_else(|| Error::NotFound("game server".into()))?;
        for (name, a) in actions {
            apply_list(&mut lists, name, a)?;
        }
        sqlx::query("update musubi.game_servers set lists = $2, updated_at = now() where id = $1")
            .bind(id)
            .bind(&lists)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        self.server(id).await
    }

    /// Aggregate counters/lists over the fleet's live servers.
    pub async fn fleet_counters(&self, fleet_id: Uuid) -> Result<serde_json::Value> {
        let servers = self.servers(fleet_id, None).await?;
        let mut counters: std::collections::BTreeMap<String, (i64, i64, i64)> = Default::default();
        let mut lists: std::collections::BTreeMap<String, (i64, i64, i64)> = Default::default();
        for s in servers.iter().filter(|s| s.state != "terminated") {
            if let Some(o) = s.counters.as_object() {
                for name in o.keys() {
                    let (count, cap) = counter_parts(&s.counters, name);
                    let e = counters.entry(name.clone()).or_default();
                    e.0 += count;
                    e.1 += cap;
                    e.2 += 1;
                }
            }
            if let Some(o) = s.lists.as_object() {
                for name in o.keys() {
                    let (values, cap) = list_parts(&s.lists, name);
                    let e = lists.entry(name.clone()).or_default();
                    e.0 += values.len() as i64;
                    e.1 += cap;
                    e.2 += 1;
                }
            }
        }
        let f = self.get(fleet_id).await?;
        Ok(serde_json::json!({
            "fleet_id": fleet_id,
            "counters": counters.iter().map(|(k, (count, cap, n))| (k.clone(), serde_json::json!({
                "count": count, "capacity": cap, "available": cap - count, "servers": n }))).collect::<serde_json::Map<_, _>>(),
            "lists": lists.iter().map(|(k, (len, cap, n))| (k.clone(), serde_json::json!({
                "count": len, "capacity": cap, "available": cap - len, "servers": n }))).collect::<serde_json::Map<_, _>>(),
            "counter_target": f.policy.counter_target.as_ref().map(|ct| serde_json::json!({
                "counter": ct.counter, "buffer": ct.buffer, "shortfall_servers": counter_shortfall(ct, &servers) })),
        }))
    }

    /// Cross-fleet inventory: matching servers (page) and the total.
    pub async fn search_servers(&self, q: &ServerSearch) -> Result<(Vec<ServerView>, i64)> {
        let mut qb = sqlx::QueryBuilder::new(VIEW_SELECT);
        push_server_filters(&mut qb, q)?;
        let (col, dir) = match q.order.as_deref().unwrap_or("-created_at") {
            "created_at" => ("g.created_at", "asc"),
            "-created_at" | "age" => ("g.created_at", "desc"),
            "-age" => ("g.created_at", "asc"),
            "last_health" => ("g.last_health", "asc"),
            "-last_health" => ("g.last_health", "desc"),
            "players" => ("g.players", "asc"),
            "-players" => ("g.players", "desc"),
            other => {
                return Err(Error::InvalidArgument(format!("bad order {other:?}")));
            }
        };
        qb.push(format!(" order by {col} {dir}, g.id"));
        qb.push(" limit ")
            .push_bind(q.limit.unwrap_or(100).clamp(1, 1000));
        qb.push(" offset ").push_bind(q.offset.unwrap_or(0).max(0));
        let rows: Vec<ViewRow> = qb
            .build_query_as()
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        let mut cq = sqlx::QueryBuilder::new(
            "select count(*) from musubi.game_servers g join musubi.fleets f on f.id = g.fleet_id where true",
        );
        push_server_filters(&mut cq, q)?;
        let total: i64 = cq
            .build_query_scalar()
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
        let now = OffsetDateTime::now_utc();
        let views = rows
            .into_iter()
            .map(|r| {
                let server = row_to_server(r.s);
                ServerView {
                    exit_reason: server.labels["exit_reason"].as_str().map(str::to_string),
                    age_secs: (now - server.created_at).whole_seconds(),
                    fleet: r.fleet,
                    region: r.region,
                    pool: r.pool,
                    provider: r.provider,
                    server,
                }
            })
            .collect();
        Ok((views, total))
    }

    /// Inventory counts by `group`: state, region, fleet, pool, provider, build, exit_reason, node.
    pub async fn server_counts(&self, q: &ServerSearch, group: &str) -> Result<Vec<(String, i64)>> {
        let expr = match group {
            "state" => "g.state",
            "region" => "f.region",
            "fleet" => "f.name",
            "pool" => "f.pool",
            "provider" => "f.provider",
            "build" => "coalesce(g.build_version, '')",
            "exit_reason" => "coalesce(g.labels->>'exit_reason', '')",
            "node" => "coalesce(g.node_id::text, '')",
            _ => {
                return Err(Error::InvalidArgument(
                    "group must be state|region|fleet|pool|provider|build|exit_reason|node".into(),
                ))
            }
        };
        let mut qb = sqlx::QueryBuilder::new(format!(
            "select {expr} as k, count(*) from musubi.game_servers g join musubi.fleets f on f.id = g.fleet_id where true"
        ));
        push_server_filters(&mut qb, q)?;
        qb.push(" group by 1 order by 2 desc, 1");
        qb.build_query_as::<(String, i64)>()
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)
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
        let drain = if f.policy.eviction == "never" {
            0
        } else {
            f.policy.rollout.drain_allocated_after_secs
        };
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

    /// `n` distinct free ports of the fleet's range on this node (primary + named extras).
    async fn free_ports(&self, f: &Fleet, n: usize) -> Result<Vec<i32>> {
        let mut used: Vec<i32> = sqlx::query_scalar(
            "select port from musubi.game_servers where node_id = $1 and state <> 'terminated'
             union all
             select (jsonb_each_text(ports)).value::int from musubi.game_servers
              where node_id = $1 and state <> 'terminated'",
        )
        .bind(self.node_id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            let p = (f.port_start..=f.port_end)
                .find(|p| !used.contains(p))
                .ok_or_else(|| {
                    Error::ResourceExhausted("fleet port range exhausted on this node".into())
                })?;
            used.push(p);
            out.push(p);
        }
        Ok(out)
    }

    /// Start one server of the fleet on this node.
    pub async fn start_one(&self, f: &Fleet) -> Result<GameServer> {
        self.spawn_log_writer();
        let provider = self
            .provider_of(&f.provider)
            .ok_or_else(|| Error::InvalidArgument(format!("unknown provider {}", f.provider)))?;
        let id = Uuid::now_v7();
        let build = self.pick_build(f).await?;
        let mut with_build = f.clone();
        if let Some(b) = &build {
            with_build.spec = b.spec.clone();
        }
        open_secret_env(&mut with_build.spec)?;
        let all_ports = self.free_ports(f, 1 + with_build.spec.ports.len()).await?;
        let port = all_ports[0];
        let mut named = serde_json::Map::new();
        for (ps, hp) in with_build
            .spec
            .ports
            .iter_mut()
            .zip(all_ports.iter().skip(1))
        {
            ps.host_port = Some(*hp);
            with_build.spec.env.insert(
                format!("MUSUBI_PORT_{}", ps.name.to_uppercase().replace('-', "_")),
                hp.to_string(),
            );
            named.insert(ps.name.clone(), serde_json::json!(hp));
        }
        let named = serde_json::Value::Object(named);
        let f = &with_build;
        sqlx::query("insert into musubi.game_servers (id, fleet_id, state, node_id, port, build_version, ports) values ($1, $2, 'starting', $3, $4, $5, $6)")
            .bind(id)
            .bind(f.id)
            .bind(self.node_id)
            .bind(port)
            .bind(build.as_ref().map(|b| b.version.clone()))
            .bind(&named)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        self.record_event(
            f.id,
            Some(id),
            "server.starting",
            "server starting",
            serde_json::json!({ "port": port, "ports": named, "build_version": build.as_ref().map(|b| b.version.clone()), "node_id": self.node_id }),
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
                sqlx::query("update musubi.game_servers set address = $2, port = $3, pid = $4, host_id = $5, updated_at = now() where id = $1")
                    .bind(id)
                    .bind(&st.address)
                    .bind(st.port)
                    .bind(st.pid)
                    .bind(provider.host_of(id))
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
    pub async fn add_build(&self, fleet_id: Uuid, mut c: CreateBuild) -> Result<FleetBuild> {
        validate_secret_env(&c.spec)?;
        seal_secret_env(&mut c.spec);
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
        let r = self.reconcile_fleets(fleets.clone()).await;
        if self.capacity().is_some() {
            if let Err(e) = self.capacity_pass().await {
                tracing::warn!(error = %e, "capacity pass failed");
            }
        }
        if self.log_archive.is_some() {
            for f in &fleets {
                if let Err(e) = self.archive_terminated_logs(f).await {
                    tracing::warn!(fleet = %f.id, error = %e, "log archive pass failed");
                }
            }
        }
        r
    }

    /// One fleet only (tests, `POST …/reconcile`): never touches the other fleets.
    pub async fn reconcile_one(&self, fleet_id: Uuid) -> Result<(usize, usize)> {
        let f = self.get(fleet_id).await?;
        let r = self.reconcile_fleets(vec![f.clone()]).await;
        if let Err(e) = self.archive_terminated_logs(&f).await {
            tracing::warn!(fleet = %f.id, error = %e, "log archive pass failed");
        }
        r
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
                let h = &f.policy.health;
                let stale = !h.disabled
                    && s.state != "starting"
                    && s.state != "scheduled"
                    && now - s.last_health > h.timeout();
                let stuck_starting = s.state == "starting"
                    && now - s.created_at > Duration::from_secs(h.initial_delay_secs);
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
            let available = available + packed_free_slots(&f.spec, &servers);
            let allocated = servers.iter().filter(|s| s.state == "allocated").count() as i32;
            let per_hour = if f.policy.forecast {
                self.forecast_per_hour(f.id, now).await.unwrap_or(0.0)
            } else {
                0.0
            };
            let counter_extra = f
                .policy
                .counter_target
                .as_ref()
                .map(|ct| counter_shortfall(ct, &servers))
                .unwrap_or(0);
            let chain: Vec<&ChainEntry> =
                f.policy.chain.iter().filter(|e| e.applies(now)).collect();
            let chain_buffer = chain.iter().find_map(|e| e.buffer);
            let chain_util = chain.iter().find_map(|e| e.target_utilization);
            let mut eff = f.policy.clone();
            if chain_util.is_some() {
                eff.target_utilization = chain_util;
            }
            let buffer = chain_buffer.unwrap_or_else(|| f.policy.buffer_at(now, f.buffer));
            let base =
                target_ready(&eff, buffer, per_hour).max(utilization_target(&eff, allocated));
            let chain_note: Vec<&str> = chain.iter().map(|e| e.id.as_str()).collect();
            let gauges = self.gauges(f.id, &servers).await;
            let now_s = now.unix_timestamp();
            let mut rule_delta = 0i32;
            let mut rules_fired: Vec<String> = Vec::new();
            let mut rules_holding: Vec<String> = Vec::new();
            {
                let mut rs = self.rule_state.lock().unwrap();
                for r in &f.policy.rules {
                    let key = (f.id, r.name.clone());
                    let (since, last) = rs.get(&key).copied().unwrap_or((None, None));
                    let (d, since, last) =
                        step_rule(r, fleet_metric(&gauges, &r.metric), now_s, since, last);
                    rs.insert(key, (since, last));
                    if since.is_some() {
                        rules_holding.push(r.name.clone());
                    }
                    if d != 0 {
                        rule_delta += d;
                        rules_fired.push(r.name.clone());
                    }
                }
            }
            let mut target = (base + surge + counter_extra + rule_delta).max(0);
            let mut webhook_note: Option<String> = None;
            let hooks: Vec<(&str, &ScaleWebhook)> = chain
                .iter()
                .filter_map(|e| e.webhook.as_ref().map(|w| (e.id.as_str(), w)))
                .chain(f.policy.webhook.as_ref().map(|w| ("fleet", w)))
                .collect();
            let busy = gauges.allocated + gauges.in_game;
            let mut errors: Vec<String> = Vec::new();
            for (id, w) in hooks {
                let tag = if id == "fleet" {
                    String::new()
                } else {
                    format!("{id}: ")
                };
                match self.ask_webhook(w, &f, &gauges, target).await {
                    Ok(Some(replicas)) => {
                        target = webhook_target(replicas, busy, f.max_servers);
                        webhook_note = Some(format!("{tag}replicas {replicas}"));
                        break;
                    }
                    Ok(None) => {
                        webhook_note = Some(format!("{tag}no scale"));
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(fleet = %f.name, hook = id, error = %e, "scale webhook failed");
                        errors.push(format!("{tag}error: {e}"));
                    }
                }
            }
            if webhook_note.is_none() && !errors.is_empty() {
                webhook_note = Some(errors.join("; "));
            }
            let plan = plan_scale(
                target,
                f.min_servers,
                f.max_servers,
                live,
                available,
                idle_ready,
            );
            let last = self.last_scale.lock().unwrap().get(&f.id).copied();
            let (start, stop) = apply_policy(
                &f.policy,
                plan,
                last.map(|(up, _)| (now_s - up).max(0) as u64),
                last.map(|(_, any)| (now_s - any).max(0) as u64),
            );
            let pending = servers.iter().filter(|s| s.state == "starting").count() as i32;
            let start = match f.policy.max_pending {
                Some(m) => start.min((m as i32 - pending).max(0)),
                None => start,
            };
            let limited = (start, stop) != plan;
            let (start, stop) = if f.policy.suspended {
                (0, 0)
            } else {
                (start, stop)
            };
            self.scale_status.lock().unwrap().insert(
                f.id,
                serde_json::json!({
                    "at": now_s, "suspended": f.policy.suspended, "target": target, "base": base,
                    "surge": surge, "counter_extra": counter_extra, "rule_delta": rule_delta,
                    "rules_fired": &rules_fired, "rules_holding": rules_holding, "webhook": &webhook_note,
                    "chain": chain_note, "buffer": buffer,
                    "plan": {"start": plan.0, "stop": plan.1}, "applied": {"start": start, "stop": stop},
                    "limited": limited, "pending": pending, "max_pending": f.policy.max_pending,
                    "able_to_scale": !f.policy.suspended && (live < f.max_servers || live > f.min_servers),
                    "live": live, "available": available, "idle_ready": idle_ready, "gauges": gauges,
                }),
            );
            if start > 0 || stop > 0 {
                self.record_event(
                    f.id,
                    None,
                    "autoscale.scaled",
                    format!("autoscaler: start {start}, stop {stop} (target {target})"),
                    serde_json::json!({ "start": start, "stop": stop, "target": target,
                        "rules_fired": rules_fired, "webhook": webhook_note }),
                )
                .await;
            }
            if start > 0 || stop > 0 {
                let mut g = self.last_scale.lock().unwrap();
                let e = g.entry(f.id).or_insert((i64::MIN / 2, i64::MIN / 2));
                if start > 0 {
                    e.0 = now_s;
                }
                e.1 = now_s;
            }
            let par = f.policy.creation_parallelism.max(1) as usize;
            let mut remaining = start.max(0) as usize;
            'starts: while remaining > 0 {
                let n = remaining.min(par);
                let results = futures::future::join_all((0..n).map(|_| self.start_one(&f))).await;
                for r in results {
                    match r {
                        Ok(_) => started += 1,
                        Err(e) => {
                            tracing::warn!(fleet = %f.name, error = %e, "could not start game server");
                            break 'starts;
                        }
                    }
                }
                remaining -= n;
            }
            if stop > 0 {
                let mut idle: Vec<&GameServer> =
                    servers.iter().filter(|s| s.state == "ready").collect();
                idle.sort_by_key(|s| s.created_at);
                if f.policy.eviction == "always" && idle.len() < stop as usize {
                    let mut busy: Vec<&GameServer> =
                        servers.iter().filter(|s| s.state == "allocated").collect();
                    busy.sort_by_key(|s| (s.players, s.created_at));
                    idle.extend(busy);
                }
                let victims: Vec<Uuid> = idle.iter().take(stop as usize).map(|s| s.id).collect();
                for batch in victims.chunks(f.policy.deletion_parallelism.max(1) as usize) {
                    futures::future::join_all(batch.iter().map(|id| {
                        tracing::info!(server = %id, fleet = %f.name, target, "scaling down idle game server");
                        self.terminate_reason(&f, *id, "scale_down")
                    }))
                    .await;
                    reaped += batch.len();
                }
            }
            let overflow = live - stop - f.max_servers;
            if overflow > 0 && !f.policy.suspended && !f.policy.overflow_labels.is_empty() {
                let patch = serde_json::Value::Object(
                    f.policy
                        .overflow_labels
                        .iter()
                        .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                        .collect(),
                );
                let is_marked = |s: &GameServer| {
                    f.policy
                        .overflow_labels
                        .iter()
                        .all(|(k, v)| s.labels.get(k).and_then(|x| x.as_str()) == Some(v.as_str()))
                };
                let already = servers
                    .iter()
                    .filter(|s| s.state == "allocated" && is_marked(s))
                    .count() as i32;
                let mut busy: Vec<&GameServer> = servers
                    .iter()
                    .filter(|s| s.state == "allocated" && !is_marked(s))
                    .collect();
                busy.sort_by_key(|s| (s.players, s.created_at));
                let mut marked = Vec::new();
                for s in busy.into_iter().take((overflow - already).max(0) as usize) {
                    if self.sdk_labels(s.id, patch.clone()).await.is_ok() {
                        marked.push(s.id);
                    }
                }
                if !marked.is_empty() {
                    self.record_event(
                        f.id,
                        None,
                        "autoscale.overflow_marked",
                        format!("autoscaler: {} allocated server(s) over target labelled", marked.len()),
                        serde_json::json!({ "servers": marked, "labels": patch, "overflow": overflow, "target": target }),
                    )
                    .await;
                }
            }
        }
        Ok((started, reaped))
    }

    /// Background reconciler; `active` gates each pass (cluster-wide: only the lease holder runs it).
    /// Fleet gauges for rules / the external scaler: server states from `servers`, resource
    /// numbers from each live server's latest metrics sample (≤ 5 min old).
    async fn gauges(&self, fleet_id: Uuid, servers: &[GameServer]) -> FleetGauges {
        let mut g = FleetGauges::default();
        for s in servers.iter().filter(|s| s.state != "terminated") {
            g.live += 1;
            match s.state.as_str() {
                "ready" => g.ready += 1,
                "starting" | "scheduled" => g.starting += 1,
                "allocated" => g.allocated += 1,
                "in_game" => g.in_game += 1,
                _ => {}
            }
            g.players += i64::from(s.players);
            g.capacity += i64::from(s.capacity);
        }
        type Row = (
            Option<f32>,
            Option<i64>,
            Option<i64>,
            Option<f32>,
            serde_json::Value,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select m.cpu_pct, m.mem_bytes, m.mem_limit_bytes, m.tick_p99_ms, m.custom from musubi.game_servers g
               join lateral (select * from musubi.server_metrics s where s.server_id = g.id order by at desc limit 1) m on true
              where g.fleet_id = $1 and g.state in ('ready', 'allocated', 'in_game') and m.at > now() - interval '5 minutes'",
        )
        .bind(fleet_id)
        .fetch_all(&self.db)
        .await
        .unwrap_or_default();
        let mut cpu: Vec<f64> = Vec::new();
        let mut mem: Vec<f64> = Vec::new();
        let mut custom: HashMap<String, Vec<f64>> = HashMap::new();
        for (c, mb, ml, tick, cu) in rows {
            if let Some(c) = c {
                cpu.push(f64::from(c));
            }
            if let (Some(mb), Some(ml)) = (mb, ml) {
                if ml > 0 {
                    mem.push(mb as f64 * 100.0 / ml as f64);
                }
            }
            if let Some(t) = tick {
                let t = f64::from(t);
                g.tick_p99_ms_max = Some(g.tick_p99_ms_max.map_or(t, |m| m.max(t)));
            }
            if let Some(o) = cu.as_object() {
                for (k, v) in o {
                    if let Some(n) = v.as_f64() {
                        custom.entry(k.clone()).or_default().push(n);
                    }
                }
            }
        }
        let avg = |v: &[f64]| (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64);
        g.cpu_pct_avg = avg(&cpu);
        g.cpu_pct_max = cpu.iter().copied().reduce(f64::max);
        g.mem_pct_avg = avg(&mem);
        g.custom = custom
            .iter()
            .filter_map(|(k, v)| avg(v).map(|a| (k.clone(), a)))
            .collect();
        g
    }

    async fn ask_webhook(
        &self,
        w: &ScaleWebhook,
        f: &Fleet,
        g: &FleetGauges,
        target: i32,
    ) -> std::result::Result<Option<i32>, String> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(w.timeout_ms.clamp(100, 10_000)))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client.post(&w.url).json(&serde_json::json!({
            "fleet": { "id": f.id, "name": f.name, "region": f.region, "pool": f.pool,
                "min_servers": f.min_servers, "max_servers": f.max_servers },
            "status": g,
            "target": target,
        }));
        for (k, v) in &w.headers {
            req = req.header(k, v);
        }
        let res = req.send().await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("status {}", res.status()));
        }
        let v: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
        let body = v.get("response").unwrap_or(&v);
        if body["scale"].as_bool() != Some(true) {
            return Ok(None);
        }
        body["replicas"]
            .as_i64()
            .map(|r| Some(r as i32))
            .ok_or_else(|| "scale without replicas".to_string())
    }

    /// `GET /v1/server/fleets/{id}/autoscaler` — the last decision (target breakdown, plan vs
    /// applied, rules, webhook answer, gauges) and every rule's holding/fired state.
    pub fn autoscaler_status(&self, fleet_id: Uuid) -> serde_json::Value {
        let last = self.scale_status.lock().unwrap().get(&fleet_id).cloned();
        let mut rules: Vec<serde_json::Value> = self
            .rule_state
            .lock()
            .unwrap()
            .iter()
            .filter(|((f, _), _)| *f == fleet_id)
            .map(|((_, name), (since, fired))| {
                serde_json::json!({ "name": name, "holding_since": since, "last_fired": fired })
            })
            .collect();
        rules.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        serde_json::json!({ "fleet_id": fleet_id, "last": last, "rules": rules })
    }

    /// `POST …/autoscaler/suspend|resume` — persist `policy.suspended` and log it.
    pub async fn set_suspended(&self, fleet_id: Uuid, suspended: bool) -> Result<Fleet> {
        let f = self.get(fleet_id).await?;
        let mut policy = f.policy.clone();
        policy.suspended = suspended;
        let f = self.scale(fleet_id, None, None, None, Some(policy)).await?;
        let (code, msg) = if suspended {
            ("autoscale.suspended", "autoscaler suspended")
        } else {
            ("autoscale.resumed", "autoscaler resumed")
        };
        self.record_event(fleet_id, None, code, msg, serde_json::json!({}))
            .await;
        Ok(f)
    }

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
            counters: serde_json::json!({}),
            lists: serde_json::json!({}),
            slots_used: 0,
            host_id: None,
            ports: serde_json::json!({}),
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

#[cfg(test)]
mod health_tests {
    use super::*;

    #[test]
    fn health_policy_timeout_and_status() {
        let h = HealthPolicy::default();
        assert_eq!(h.timeout(), Duration::from_secs(30));
        assert_eq!(h.status("ready", 3, 500), "healthy");
        assert_eq!(h.status("allocated", 15, 500), "suspect");
        assert_eq!(h.status("ready", 31, 500), "unhealthy");
        assert_eq!(h.status("starting", 0, 10), "starting");
        assert_eq!(h.status("starting", 0, 121), "stuck_starting");
        let off = HealthPolicy {
            disabled: true,
            ..HealthPolicy::default()
        };
        assert_eq!(off.status("ready", 10_000, 10_000), "unchecked");
        assert!(HealthPolicy {
            period_secs: 0,
            ..HealthPolicy::default()
        }
        .validate()
        .is_err());
        assert!(HealthPolicy {
            failure_threshold: 0,
            ..HealthPolicy::default()
        }
        .validate()
        .is_err());
        let mut p = ScalePolicy::default();
        assert!(p.validate(10).is_ok());
        p.eviction = "sometimes".into();
        assert!(p.validate(10).is_err());
    }
}

#[cfg(test)]
mod alloc_pick_tests {
    use super::*;

    fn gs(labels: serde_json::Value, rooms: i64, slots_used: i32, host: &str) -> GameServer {
        GameServer {
            id: Uuid::now_v7(),
            fleet_id: Uuid::nil(),
            state: "ready".into(),
            node_id: None,
            address: host.into(),
            port: 7777,
            pid: None,
            labels,
            allocation: None,
            last_health: OffsetDateTime::UNIX_EPOCH,
            created_at: OffsetDateTime::UNIX_EPOCH,
            build_version: None,
            players: 0,
            capacity: 0,
            version: 0,
            ready_at: None,
            counters: serde_json::json!({"rooms": {"count": rooms, "capacity": 8}}),
            lists: serde_json::json!({"players": {"values": ["a", "b"], "capacity": 10}}),
            slots_used,
            host_id: None,
            ports: serde_json::json!({}),
        }
    }

    fn req(v: serde_json::Value) -> AllocateRequest {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn match_expressions_and_chain() {
        let c = vec![
            gs(
                serde_json::json!({"map": "dust", "tier": "gold"}),
                1,
                1,
                "h1",
            ),
            gs(
                serde_json::json!({"map": "inferno", "tier": "silver"}),
                3,
                2,
                "h1",
            ),
            gs(serde_json::json!({"map": "nuke"}), 2, 0, "h2"),
        ];
        let r = req(serde_json::json!({"selector": {"match_expressions": [
            {"key": "map", "op": "in", "values": ["dust", "inferno"]}]},
            "priorities": [{"kind": "counter", "key": "rooms", "order": "desc"}]}));
        let (g, i) = pick_candidate(c.clone(), &r).unwrap();
        assert_eq!((g.labels["map"].as_str(), i), (Some("inferno"), 0));
        let r = req(serde_json::json!({"selectors": [
            {"labels": {"map": "mirage"}},
            {"match_expressions": [{"key": "tier", "op": "does_not_exist"}]}]}));
        let (g, i) = pick_candidate(c.clone(), &r).unwrap();
        assert_eq!((g.labels["map"].as_str(), i), (Some("nuke"), 1));
        let r = req(serde_json::json!({"selector": {"match_expressions": [
            {"key": "map", "op": "not_in", "values": ["dust", "inferno", "nuke"]}]}}));
        assert!(pick_candidate(c.clone(), &r).is_none());
        let r = req(serde_json::json!({"selector": {"match_expressions": [
            {"key": "map", "op": "in", "values": []}]}}));
        assert!(validate_alloc(&r).is_err());
        let r = req(serde_json::json!({"priorities": [{"kind": "weird", "key": "rooms"}]}));
        assert!(validate_alloc(&r).is_err());
        // packed keeps store order (first candidate); distributed spreads to the host with
        // most free servers, then the emptiest server
        let r = req(serde_json::json!({}));
        assert_eq!(
            pick_candidate(c.clone(), &r).unwrap().0.labels["map"],
            "dust"
        );
        let r = req(serde_json::json!({"scheduling": "distributed"}));
        assert_eq!(
            pick_candidate(c.clone(), &r).unwrap().0.labels["map"],
            "dust"
        );
        let mut c2 = c.clone();
        c2[2].address = "h1".into();
        c2.push(gs(serde_json::json!({"map": "train"}), 0, 0, "h3"));
        let (g, _) = pick_candidate(c2, &r).unwrap();
        assert_eq!(
            g.labels["map"], "nuke",
            "h1 has three free servers, h3 one; the emptiest on h1 wins"
        );
    }
}

#[cfg(test)]
mod autoscale_tests {
    use super::*;

    fn rule(metric: &str, op: &str, threshold: f64, action: &str) -> ScaleRule {
        ScaleRule {
            name: "r".into(),
            metric: metric.into(),
            op: op.into(),
            threshold,
            for_secs: 30,
            action: action.into(),
            step: 2,
            cooldown_secs: 60,
        }
    }

    #[test]
    fn rules_hold_for_a_while_then_fire_once_per_cooldown() {
        let r = rule("ready_pct", "lt", 20.0, "scale_up");
        // not holding → no state
        assert_eq!(step_rule(&r, Some(50.0), 1000, None, None), (0, None, None));
        // starts holding at t=1000, fires at t=1030 (for_secs 30)
        assert_eq!(
            step_rule(&r, Some(10.0), 1000, None, None),
            (0, Some(1000), None)
        );
        assert_eq!(
            step_rule(&r, Some(10.0), 1020, Some(1000), None),
            (0, Some(1000), None)
        );
        assert_eq!(
            step_rule(&r, Some(10.0), 1030, Some(1000), None),
            (2, Some(1000), Some(1030))
        );
        // still holding but cooling down; fires again after 60 s
        assert_eq!(
            step_rule(&r, Some(10.0), 1060, Some(1000), Some(1030)),
            (0, Some(1000), Some(1030))
        );
        assert_eq!(
            step_rule(&r, Some(10.0), 1090, Some(1000), Some(1030)),
            (2, Some(1000), Some(1090))
        );
        // condition clears → holding resets, cooldown memory kept
        assert_eq!(
            step_rule(&r, Some(90.0), 1100, Some(1000), Some(1090)),
            (0, None, Some(1090))
        );
        // missing metric never fires
        assert_eq!(step_rule(&r, None, 1200, Some(1000), None), (0, None, None));
        let down = rule("players_pct", "lte", 10.0, "scale_down");
        assert_eq!(
            step_rule(&down, Some(5.0), 100, Some(0), None),
            (-2, Some(0), Some(100))
        );
    }

    #[test]
    fn gauges_feed_metrics_and_webhook_target_clamps() {
        let g = FleetGauges {
            live: 10,
            ready: 2,
            starting: 1,
            allocated: 6,
            in_game: 1,
            players: 70,
            capacity: 100,
            cpu_pct_avg: Some(55.0),
            cpu_pct_max: Some(91.0),
            mem_pct_avg: None,
            tick_p99_ms_max: Some(40.0),
            custom: HashMap::from([("queue".to_string(), 3.0)]),
        };
        assert_eq!(fleet_metric(&g, "ready"), Some(2.0));
        assert_eq!(fleet_metric(&g, "ready_pct"), Some(20.0));
        assert_eq!(fleet_metric(&g, "allocated"), Some(7.0));
        assert_eq!(fleet_metric(&g, "allocated_pct"), Some(70.0));
        assert_eq!(fleet_metric(&g, "players_pct"), Some(70.0));
        assert_eq!(fleet_metric(&g, "cpu_pct_max"), Some(91.0));
        assert_eq!(fleet_metric(&g, "mem_pct_avg"), None);
        assert_eq!(fleet_metric(&g, "custom.queue"), Some(3.0));
        assert_eq!(fleet_metric(&g, "custom.nope"), None);
        assert_eq!(fleet_metric(&g, "bogus"), None);
        assert_eq!(fleet_metric(&FleetGauges::default(), "ready_pct"), None);
        // external scaler asks for 12 total with 7 busy → 5 available wanted; clamped to max
        assert_eq!(webhook_target(12, 7, 20), 5);
        assert_eq!(webhook_target(12, 7, 8), 1);
        assert_eq!(webhook_target(3, 7, 20), 0);
    }

    #[test]
    fn policy_validation_rejects_bad_rules_and_webhooks() {
        let mut p = ScalePolicy::default();
        assert!(p.validate(4).is_ok());
        p.rules.push(rule("cpu_pct_avg", "gt", 80.0, "scale_up"));
        assert!(p.validate(4).is_ok());
        p.rules[0].metric = "latency".into();
        assert!(p.validate(4).is_err(), "unknown metric");
        p.rules[0].metric = "custom.queue".into();
        p.rules[0].op = "between".into();
        assert!(p.validate(4).is_err(), "bad op");
        p.rules[0].op = "gte".into();
        p.rules[0].action = "explode".into();
        assert!(p.validate(4).is_err(), "bad action");
        p.rules[0].action = "scale_down".into();
        p.rules[0].step = 9;
        assert!(p.validate(4).is_err(), "step above max_servers");
        p.rules[0].step = 1;
        assert!(p.validate(4).is_ok());
        p.webhook = Some(ScaleWebhook {
            url: "ftp://scaler".into(),
            ..Default::default()
        });
        assert!(p.validate(4).is_err(), "webhook scheme");
        p.webhook.as_mut().unwrap().url = "https://scaler.internal/review".into();
        assert!(p.validate(4).is_ok());
    }
}

#[cfg(test)]
mod ports_tests {
    use super::*;

    fn spec(ports: Vec<PortSpec>) -> FleetSpec {
        FleetSpec {
            ports,
            ..FleetSpec::default()
        }
    }

    fn port(name: &str, protocol: &str) -> PortSpec {
        PortSpec {
            name: name.into(),
            protocol: protocol.into(),
            container_port: None,
            host_port: None,
        }
    }

    #[test]
    fn validation_covers_names_protocols_and_policy() {
        assert!(validate_ports(&FleetSpec::default()).is_ok());
        assert!(validate_ports(&spec(vec![port("query", "tcp"), port("voice", "udp")])).is_ok());
        assert!(
            validate_ports(&spec(vec![port("game", "udp")])).is_err(),
            "reserved"
        );
        assert!(
            validate_ports(&spec(vec![port("Query", "udp")])).is_err(),
            "case"
        );
        assert!(validate_ports(&spec(vec![port("a-very-long-name", "udp")])).is_err());
        assert!(
            validate_ports(&spec(vec![port("q", "udp"), port("q", "tcp")])).is_err(),
            "dup"
        );
        assert!(
            validate_ports(&spec(vec![port("q", "sctp")])).is_err(),
            "protocol"
        );
        let mut bad = spec(vec![port("q", "udp")]);
        bad.ports[0].container_port = Some(70000);
        assert!(validate_ports(&bad).is_err(), "container port");
        let mut s = FleetSpec {
            port_protocol: "quic".into(),
            ..FleetSpec::default()
        };
        assert!(validate_ports(&s).is_err());
        s.port_protocol = "udp".into();
        s.port_policy = "static".into();
        assert!(validate_ports(&s).is_err());
        s.port_policy = "none".into();
        assert!(validate_ports(&s).is_ok());
        assert!(port_policy_none(&s));
    }

    #[test]
    fn bindings_expand_protocols_and_skip_unassigned_extras() {
        let mut s = spec(vec![port("query", "tcp"), port("voice", "both")]);
        s.port_protocol = "udp".into();
        s.ports[0].host_port = Some(7001);
        s.ports[0].container_port = Some(27015);
        s.ports[1].host_port = Some(7002);
        let b = port_bindings(&s, 7000);
        assert_eq!(
            b,
            vec![
                ("game".to_string(), 7000, 7000, "udp"),
                ("query".to_string(), 7001, 27015, "tcp"),
                ("voice".to_string(), 7002, 7002, "udp"),
                ("voice".to_string(), 7002, 7002, "tcp"),
            ]
        );
        // default primary protocol is both; an extra port without a host port is not published
        let s = spec(vec![port("query", "tcp")]);
        assert_eq!(port_bindings(&s, 7000).len(), 2);
    }
}
