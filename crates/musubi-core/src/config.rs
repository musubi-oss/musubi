use std::net::SocketAddr;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Roles a node can run. In `musubi dev` all of them run in one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Edge,
    Api,
    Realtime,
    Sim,
    Orchestrator,
    Control,
    /// Dedicated telemetry backend (Pragma `DedicatedTelemetryMode`): a node with this role and
    /// without `api` serves only `/v1/telemetry/*` (+ health/metrics) and runs the sink
    /// flushers, so event floods never share a process with the game API.
    Telemetry,
}

impl Role {
    pub const ALL: [Role; 7] = [
        Role::Edge,
        Role::Api,
        Role::Realtime,
        Role::Sim,
        Role::Orchestrator,
        Role::Control,
        Role::Telemetry,
    ];
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub node: NodeConfig,
    pub http: HttpConfig,
    pub database: DatabaseConfig,
    pub session: SessionConfig,
    pub log: LogConfig,
    pub runtime: RuntimeConfig,
    pub quic: QuicConfig,
    pub webtransport: WebTransportConfig,
    pub webrtc: WebRtcConfig,
    pub udp: UdpConfig,
    pub edge: EdgeConfig,
    pub mesh_net: MeshNetConfig,
    pub geoip: GeoIpConfig,
    pub zanshin: ZanshinConfig,
    pub analytics: AnalyticsConfig,
    pub aurix: AurixConfig,
    pub iap: IapConfig,
    pub ads: AdsConfig,
    /// P1-30: master keys for secrets stored in Postgres (see `musubi_core::secrets`).
    pub security: SecurityConfig,
    /// P1-62: development aids (simulated network conditions).
    pub dev: DevConfig,
    /// Config paths whose values contained `${env:…}` / `${file:…}` references (filled by `load`).
    #[serde(skip)]
    pub resolved_refs: Vec<String>,
    pub market: MarketConfig,
    /// Catalog workflow (P1-48): review requirement for changesets, locales for catalog strings.
    pub economy: EconomyConfig,
    pub fleet: FleetConfig,
    pub limits: LimitsConfig,
    pub metrics: MetricsConfig,
    pub realtime: RealtimeConfig,
    pub retention: RetentionConfig,
    pub storage: StorageConfig,
    pub matchmaker: MatchmakerConfig,
    pub social: SocialConfig,
    pub tracing: TracingConfig,
    pub push: PushConfig,
    pub email: EmailConfig,
    pub anticheat: AntiCheatConfig,
    pub chat: ChatConfig,
    pub compliance: ComplianceConfig,
    pub fraud: FraudConfig,
    pub regions: RegionsConfig,
    pub operators: OperatorsConfig,
    pub ugc: UgcConfig,
    pub files: FilesConfig,
    /// Further games/environments served by this same process, each on its own database and
    /// cluster group; requests pick one by `x-musubi-project` or `Host`.
    pub projects: Vec<ProjectConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// Stable name; defaults to the hostname.
    pub name: String,
    pub roles: Vec<Role>,
    pub region: String,
    pub data_dir: String,
    /// QUIC node bus listen address (e.g. `0.0.0.0:7447`); empty = Postgres bus only.
    pub bus_listen: String,
    /// Address other nodes dial for the bus; defaults to the bound address.
    pub bus_advertise: String,
    /// Cluster group: independent clusters can share one Postgres.
    pub cluster_group: String,
    /// Name of the project this node's base configuration serves (`[[projects]]` add more).
    pub project: String,
    /// Environment this cluster is (`dev`, `stage`, `prod`…): stamped on exported content/config
    /// bundles and checked on import when the caller names the target environment.
    pub environment: String,
    /// Upper bound for the SIGTERM drain (hand off matches/worlds, leave the cluster, close sessions).
    pub drain_timeout_secs: u64,
    /// Let the server start with the built-in `server_key` / JWT secret. `musubi dev` sets it;
    /// deployments must set their own secrets instead (see `Config::validate`).
    pub allow_insecure_defaults: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    pub listen: SocketAddr,
    pub max_body_bytes: usize,
    /// Shared secret for server-to-server calls (`X-Musubi-Server-Key`). Change it outside dev.
    pub server_key: String,
    /// Browser / WebGL origins allowed to call the API cross-site: exact (`https://play.example.com`),
    /// wildcard subdomain (`*.example.com`) or `*`. Empty = no CORS headers (same-origin only).
    pub cors_origins: Vec<String>,
    /// How long browsers may cache a preflight answer.
    pub cors_max_age_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    pub max_connections: u32,
    pub run_migrations: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionConfig {
    /// HS256 secret in dev; production must use an ES256 key pair (set `signing_key_pem`).
    pub secret: String,
    pub signing_key_pem: Option<String>,
    pub access_ttl_secs: u64,
    pub refresh_ttl_secs: u64,
    /// Failed email logins (per address) before the address is locked; 0 disables lockout.
    pub lockout_threshold: u32,
    /// First lock lasts this long; every further failure doubles it up to `lockout_max_secs`.
    pub lockout_base_secs: u64,
    pub lockout_max_secs: u64,
    /// Failed email logins (per address) after which a captcha token is required with the next
    /// attempts; 0 disables. Verified by the `hook_auth_captcha` module hook when present, else by
    /// `captcha_verify_url` + `captcha_secret` (Turnstile / hCaptcha / reCAPTCHA siteverify).
    pub captcha_after: u32,
    pub captcha_verify_url: Option<String>,
    pub captcha_secret: Option<String>,
    /// Online players (accounts with a live socket anywhere in the cluster) above which new
    /// logins are queued instead of served; 0 disables the login queue.
    pub login_queue_capacity: usize,
    /// How many queued logins may be admitted per second once room frees up.
    pub login_queue_admit_per_sec: u32,
    /// A queued client that has not polled for this long loses its place.
    pub login_queue_ttl_secs: u64,
    /// Live sessions (refresh-token families) one account may hold; a new login evicts the oldest
    /// beyond this. 0 = unlimited.
    pub max_sessions_per_account: u32,
    /// Password policy for email accounts (set / change / reset).
    pub password_min_length: usize,
    /// Character classes (lower, upper, digit, other) a password must mix; 0–4.
    pub password_require_classes: u8,
    /// Refuse the usual suspects (`password1`, `qwerty123`, one repeated character, a keyboard
    /// walk) and anything containing the account's email local part.
    pub password_deny_common: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub filter: String,
    pub json: bool,
    /// Lines kept in memory for `GET /v1/admin/logs` (operator log tail).
    pub ring_lines: usize,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            name: hostname(),
            roles: Role::ALL.to_vec(),
            region: "local".into(),
            bus_listen: String::new(),
            bus_advertise: String::new(),
            cluster_group: "default".into(),
            project: "default".into(),
            environment: "dev".into(),
            drain_timeout_secs: 20,
            allow_insecure_defaults: false,
            data_dir: "./data".into(),
        }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:7350".parse().unwrap(),
            max_body_bytes: 1 << 20,
            server_key: "defaultkey".into(),
            cors_origins: Vec::new(),
            cors_max_age_secs: 600,
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://musubi:musubi@localhost:5432/musubi".into(),
            max_connections: 32,
            run_migrations: true,
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            secret: "dev-only-change-me".into(),
            signing_key_pem: None,
            access_ttl_secs: 15 * 60,
            refresh_ttl_secs: 30 * 24 * 3600,
            lockout_threshold: 5,
            lockout_base_secs: 30,
            lockout_max_secs: 15 * 60,
            captcha_after: 3,
            max_sessions_per_account: 0,
            password_min_length: 8,
            password_require_classes: 0,
            password_deny_common: true,
            captcha_verify_url: None,
            captcha_secret: None,
            login_queue_capacity: 0,
            login_queue_admit_per_sec: 50,
            login_queue_ttl_secs: 120,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: "info".into(),
            json: false,
            ring_lines: 5000,
        }
    }
}

/// Built-in secrets that must not reach a deployment.
pub const DEFAULT_SERVER_KEY: &str = "defaultkey";
pub const DEFAULT_SESSION_SECRET: &str = "dev-only-change-me";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The server refuses to start.
    Error,
    /// Logged at startup, listed by `musubi doctor`.
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    pub severity: Severity,
    pub key: String,
    pub message: String,
}

impl Config {
    /// Static sanity checks (secrets, TTLs, addresses). Errors stop `musubi-server`; warnings are
    /// logged. Runtime checks (database, ports, data dir) live in `musubi doctor`.
    pub fn validate(&self) -> Vec<Problem> {
        let mut out = Vec::new();
        let mut push = |severity: Severity, key: &str, message: String| {
            out.push(Problem {
                severity,
                key: key.to_string(),
                message,
            })
        };
        let insecure = |s: Severity| {
            if self.node.allow_insecure_defaults {
                Severity::Warning
            } else {
                s
            }
        };
        if self.http.server_key == DEFAULT_SERVER_KEY {
            push(
                insecure(Severity::Error),
                "http.server_key",
                "built-in server key; set your own or issue keys via /v1/admin/keys".into(),
            );
        } else if self.http.server_key.len() < 16 {
            push(
                Severity::Warning,
                "http.server_key",
                "shorter than 16 characters".into(),
            );
        }
        if self.session.signing_key_pem.is_none() {
            if self.session.secret == DEFAULT_SESSION_SECRET {
                push(
                    insecure(Severity::Error),
                    "session.secret",
                    "built-in HS256 secret; set session.signing_key_pem (ES256) or a long random secret".into(),
                );
            } else if self.session.secret.len() < 32 {
                push(
                    Severity::Warning,
                    "session.secret",
                    "HS256 secret shorter than 32 characters".into(),
                );
            } else {
                push(
                    Severity::Warning,
                    "session.signing_key_pem",
                    "HS256 in use; ES256 (signing_key_pem) lets other services verify tokens without the secret".into(),
                );
            }
        }
        if self.session.access_ttl_secs == 0 || self.session.refresh_ttl_secs == 0 {
            push(
                Severity::Error,
                "session",
                "access_ttl_secs and refresh_ttl_secs must be > 0".into(),
            );
        } else if self.session.access_ttl_secs >= self.session.refresh_ttl_secs {
            push(
                Severity::Error,
                "session",
                "access_ttl_secs must be shorter than refresh_ttl_secs".into(),
            );
        } else if self.session.access_ttl_secs > 24 * 3600 {
            push(
                Severity::Warning,
                "session.access_ttl_secs",
                "access tokens live longer than a day; bans take that long to bite on cached JWTs"
                    .into(),
            );
        }
        if self.database.url.is_empty() {
            push(Severity::Error, "database.url", "empty".into());
        }
        if self.database.max_connections < 3 {
            push(
                Severity::Error,
                "database.max_connections",
                "must be >= 3 (cluster listener holds one connection; a request may hold a \
                 transaction while a nested call takes another)"
                    .into(),
            );
        }
        if !self.node.bus_listen.is_empty() && self.node.bus_listen.parse::<SocketAddr>().is_err() {
            push(
                Severity::Error,
                "node.bus_listen",
                format!("not a socket address: {}", self.node.bus_listen),
            );
        }
        if !self.node.bus_advertise.is_empty()
            && self.node.bus_advertise.parse::<SocketAddr>().is_err()
        {
            push(
                Severity::Error,
                "node.bus_advertise",
                format!("not a socket address: {}", self.node.bus_advertise),
            );
        }
        if !self.node.bus_listen.is_empty() && self.node.data_dir.is_empty() {
            push(
                Severity::Warning,
                "node.data_dir",
                "empty: node identity and the Raft log will not survive a restart".into(),
            );
        }
        if self.node.drain_timeout_secs == 0 {
            push(
                Severity::Warning,
                "node.drain_timeout_secs",
                "0: SIGTERM drops matches and worlds without hand-off".into(),
            );
        }
        if self.metrics.enabled
            && !self.metrics.require_server_key
            && self.metrics.allow_ips.is_empty()
        {
            push(Severity::Warning, "metrics", "/metrics is open to anyone who can reach the port; set require_server_key or allow_ips".into());
        }
        if !self.limits.enabled {
            push(
                Severity::Warning,
                "limits.enabled",
                "rate limits are off".into(),
            );
        }
        if self.retention.interval_secs < 5 {
            push(
                Severity::Warning,
                "retention.interval_secs",
                "clamped to 5 seconds".into(),
            );
        }
        if self.realtime.resume_ttl_secs > 600 {
            push(
                Severity::Warning,
                "realtime.resume_ttl_secs",
                "parked sessions hold matches/parties for over 10 minutes".into(),
            );
        }
        out
    }

    /// Layered: defaults ← optional TOML/YAML file ← `MUSUBI__SECTION__KEY` env vars.
    pub fn load(path: Option<&Path>) -> Result<Self, config::ConfigError> {
        let mut b = config::Config::builder();
        if let Some(p) = path {
            b = b.add_source(config::File::from(p));
        }
        b = b.add_source(
            config::Environment::with_prefix("MUSUBI")
                .separator("__")
                .list_separator(",")
                .with_list_parse_key("node.roles")
                .try_parsing(true),
        );
        let mut v: serde_json::Value = b.build()?.try_deserialize()?;
        let mut refs = Vec::new();
        crate::secrets::resolve_refs(&mut v, "", &mut refs)
            .map_err(config::ConfigError::Message)?;
        let mut c: Config =
            serde_json::from_value(v).map_err(|e| config::ConfigError::Message(e.to_string()))?;
        c.resolved_refs = refs;
        crate::secrets::install(&c.security.master_keys).map_err(config::ConfigError::Message)?;
        Ok(c)
    }
}

/// `[dev]` — development aids, never meant for production nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DevConfig {
    pub netsim: NetsimConfig,
    pub loadtest: LoadtestConfig,
    /// `development` | `staging` | `production` (default). Development-only actions are open to
    /// everyone outside `production`; in production only developer players may run them.
    pub environment: String,
}

impl Default for DevConfig {
    fn default() -> Self {
        Self {
            netsim: NetsimConfig::default(),
            loadtest: LoadtestConfig::default(),
            environment: "production".into(),
        }
    }
}

/// `[dev.loadtest]` — server-driven simulated players (`POST /v1/admin/dev/loadtests`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LoadtestConfig {
    /// Upper bound on bots per run (a run is one node's tokio tasks; 2000 ≈ 2000 sockets).
    pub max_bots: u32,
    /// Upper bound on run duration.
    pub max_duration_secs: u64,
    /// Allow `url` other than this node (load another environment from here).
    pub allow_remote: bool,
    /// Finished runs kept in `musubi.loadtest_runs`.
    pub keep_runs: u32,
}

impl Default for LoadtestConfig {
    fn default() -> Self {
        Self {
            max_bots: 2000,
            max_duration_secs: 3600,
            allow_remote: false,
            keep_runs: 200,
        }
    }
}

/// `[dev.netsim]` — inject latency/jitter/loss around every realtime connection at startup; the
/// same knobs are switchable at runtime through `/v1/admin/dev/netsim`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NetsimConfig {
    pub enabled: bool,
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub loss_pct: f64,
    /// `both` | `in` | `out`.
    pub direction: String,
}

impl Default for NetsimConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            latency_ms: 0,
            jitter_ms: 0,
            loss_pct: 0.0,
            direction: "both".into(),
        }
    }
}

/// `[security]` — encryption at rest for stored credentials. `master_keys`: 32-byte keys as hex or
/// base64 (use `${env:…}`/`${file:…}`); the first seals, all open. Empty = store plaintext.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub master_keys: Vec<String>,
}

/// One more project in the same process: `name`, the `Host`s that select it, and any config
/// keys that differ from the base configuration (its own `database.url` is required; the
/// cluster group and data dir default to `<base>-<name>` / `<base>/<name>`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectConfig {
    pub name: String,
    pub hosts: Vec<String>,
    #[serde(flatten)]
    pub overrides: serde_json::Map<String, serde_json::Value>,
}

impl Config {
    /// The full configuration of an extra project: this one with the project's overrides on top.
    pub fn for_project(&self, p: &ProjectConfig) -> Result<Config, String> {
        if p.name.is_empty()
            || p.name.len() > 32
            || !p
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("project name: 1..=32 of [A-Za-z0-9_-]".into());
        }
        if p.name == self.node.project {
            return Err(format!(
                "project {} is the base configuration itself",
                p.name
            ));
        }
        let mut base = serde_json::to_value(self).map_err(|e| e.to_string())?;
        base["projects"] = serde_json::Value::Array(Vec::new());
        let has = |path: &[&str]| {
            let mut v: Option<&serde_json::Value> = None;
            for (i, k) in path.iter().enumerate() {
                v = if i == 0 {
                    p.overrides.get(*k)
                } else {
                    v.and_then(|v| v.get(*k))
                };
                v?;
            }
            v
        };
        if has(&["node", "cluster_group"]).is_none() {
            base["node"]["cluster_group"] =
                serde_json::Value::String(format!("{}-{}", self.node.cluster_group, p.name));
        }
        if has(&["node", "data_dir"]).is_none() {
            base["node"]["data_dir"] = serde_json::Value::String(format!(
                "{}/{}",
                self.node.data_dir.trim_end_matches('/'),
                p.name
            ));
        }
        // tokens of one project must not verify in another: derive a secret unless one is given
        if has(&["session", "secret"]).is_none() {
            base["session"]["secret"] =
                serde_json::Value::String(format!("{}/{}", self.session.secret, p.name));
        }
        merge_json(&mut base, &serde_json::Value::Object(p.overrides.clone()));
        base["node"]["project"] = serde_json::Value::String(p.name.clone());
        // one HTTP listener serves every project
        base["http"]["listen"] =
            serde_json::to_value(self.http.listen).map_err(|e| e.to_string())?;
        let cfg: Config = serde_json::from_value(base).map_err(|e| e.to_string())?;
        if cfg.database.url == self.database.url {
            return Err(format!("project {} needs its own database.url", p.name));
        }
        Ok(cfg)
    }
}

fn merge_json(a: &mut serde_json::Value, b: &serde_json::Value) {
    match (a, b) {
        (serde_json::Value::Object(a), serde_json::Value::Object(b)) => {
            for (k, v) in b {
                merge_json(a.entry(k.clone()).or_insert(serde_json::Value::Null), v);
            }
        }
        (a, b) => *a = b.clone(),
    }
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "musubi".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_overlay_keeps_the_base_and_separates_what_must_differ() {
        let mut base = Config::default();
        base.database.url = "postgres://x/base".into();
        base.node.region = "eu".into();
        let p: ProjectConfig = serde_json::from_value(serde_json::json!({
            "name": "game2", "hosts": ["g2.example.com"],
            "database": {"url": "postgres://x/game2", "max_connections": 3},
            "session": {"secret": "other-secret"}
        }))
        .unwrap();
        let c = base.for_project(&p).unwrap();
        assert_eq!(c.node.project, "game2");
        assert_eq!(c.node.region, "eu", "untouched keys come from the base");
        assert_eq!(c.database.url, "postgres://x/game2");
        assert_eq!(c.database.max_connections, 3);
        assert_eq!(c.session.secret, "other-secret");
        assert_eq!(c.node.cluster_group, "default-game2");
        let q: ProjectConfig = serde_json::from_value(serde_json::json!({
            "name": "game3", "database": {"url": "postgres://x/game3"}
        }))
        .unwrap();
        let c3 = base.for_project(&q).unwrap();
        assert_ne!(
            c3.session.secret, base.session.secret,
            "derived per project"
        );
        assert_eq!(c.node.data_dir, "./data/game2");
        assert_eq!(c.http.listen, base.http.listen);
        assert!(c.projects.is_empty());
        let same_db = ProjectConfig {
            name: "game3".into(),
            ..Default::default()
        };
        assert!(base
            .for_project(&same_db)
            .unwrap_err()
            .contains("database.url"));
        let bad = ProjectConfig {
            name: "default".into(),
            ..Default::default()
        };
        assert!(base.for_project(&bad).is_err());
    }

    #[test]
    fn defaults_load_without_file() {
        let c = Config::load(None).unwrap();
        assert_eq!(c.http.listen.port(), 7350);
        assert_eq!(c.node.roles.len(), 6);
    }
}

/// Server-logic (WebAssembly) runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Directory with `*.wasm` / `*.wat` modules loaded at startup (none = runtime empty).
    pub modules_dir: Option<std::path::PathBuf>,
    /// Fuel budget per call (~instructions).
    pub fuel: u64,
    pub max_memory_mb: usize,
    pub timeout_ms: u64,
    pub max_payload_kb: usize,
    /// Dev loop: poll `modules_dir` and hot-reload a module whose file changed (new/edited files
    /// are loaded, removed ones unloaded; a module that fails to load keeps its old version).
    /// `musubi dev` turns it on.
    pub watch_modules: bool,
    /// Per-module crash policy (`restart` | `sleep` | `destroy`) overriding what the module's
    /// `init` declares; `params.crash_policy` on a match beats both.
    pub crash_policies: std::collections::HashMap<String, String>,
    /// Cron-driven calls into module exports named `schedule_<name>` (one runner cluster-wide).
    pub schedules: Vec<ScheduleConfig>,
    /// Hosts modules may reach through the `http.fetch` host op (`api.example.com`,
    /// `*.example.com`); empty = no outbound HTTP for modules.
    pub http_allow: Vec<String>,
    pub http_timeout_ms: u64,
    /// Largest response body `http.fetch` returns.
    pub http_max_body_kb: usize,
    /// Outbound requests per second across all modules on this node (token bucket).
    pub http_rate_per_sec: u32,
    /// Named secrets for the `secrets.get` host op; a value `env:NAME` is read from the
    /// environment at use. Modules never see the process environment itself.
    pub secrets: std::collections::BTreeMap<String, String>,
}

/// `[[runtime.schedules]] name = "nightly" cron = "0 3 * * *"` → calls export `schedule_nightly`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleConfig {
    pub name: String,
    /// 5-field crontab (minute hour day month weekday), UTC; 6/7 fields with seconds also accepted.
    pub cron: String,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            modules_dir: None,
            fuel: 50_000_000,
            max_memory_mb: 64,
            timeout_ms: 2000,
            max_payload_kb: 1024,
            schedules: Vec::new(),
            watch_modules: false,
            crash_policies: Default::default(),
            http_allow: Vec::new(),
            http_timeout_ms: 5000,
            http_max_body_kb: 256,
            http_rate_per_sec: 50,
            secrets: Default::default(),
        }
    }
}

/// QUIC realtime transport (same envelopes as WebSocket).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuicConfig {
    /// UDP listen address; unset = QUIC disabled.
    pub listen: Option<SocketAddr>,
    /// Answer the first Initial of an unvalidated source with a Retry token (RFC 9000 §8.1): a
    /// spoofed address never costs a handshake, and amplification stays at 1×. Default on.
    pub retry: Option<bool>,
    /// Handshakes in flight at once; above it new connections are refused (default 1024).
    pub max_pending_handshakes: Option<usize>,
    /// PEM cert chain + key. Unset = self-signed `localhost` cert (development only).
    pub cert_pem: Option<std::path::PathBuf>,
    pub key_pem: Option<std::path::PathBuf>,
}

/// WebTransport realtime transport for browsers (HTTP/3; same envelopes and framing as QUIC).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebTransportConfig {
    /// UDP listen address; unset = WebTransport disabled.
    pub listen: Option<SocketAddr>,
    /// PEM cert chain + key (browsers require a publicly trusted or hash-pinned certificate).
    /// Unset = self-signed `localhost` cert (development only).
    pub cert_pem: Option<std::path::PathBuf>,
    pub key_pem: Option<std::path::PathBuf>,
}

/// WebRTC DataChannel realtime transport (browsers without WebTransport). Signalling is
/// `POST /v1/rtc/offer` on the HTTP port; media/data flow over this UDP socket.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WebRtcConfig {
    /// UDP listen address; unset = WebRTC disabled.
    pub listen: Option<SocketAddr>,
    /// Address browsers reach the UDP socket at (ICE host candidate); defaults to `listen`,
    /// or loopback when `listen` is an unspecified IP.
    pub public_addr: Option<SocketAddr>,
}

/// UDP edge proxy in front of fleet game servers (plan 05, Quilkin-like token routing): clients
/// get the proxy's address from allocations and prefix datagrams with their connect token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgeConfig {
    /// UDP listen address of the proxy on this node; unset = no proxy here.
    pub listen: Option<SocketAddr>,
    /// Address clients should use (defaults to `listen`); allocations advertise it instead of
    /// the game server's own when set or when `listen` is set.
    pub public_addr: Option<SocketAddr>,
    /// Drop a client's proxy session after this long without packets.
    pub idle_secs: u64,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            listen: None,
            public_addr: None,
            idle_secs: 60,
        }
    }
}

/// WireGuard-style private mesh between nodes (`[mesh_net]`): every node owns an x25519 key pair
/// and an address in `subnet`, publishes key + endpoint in the node registry and derives its peer
/// list from it, so hosts rented from different providers share one encrypted private network
/// (bus, simulators, game servers) without public ports. The node renders the WireGuard
/// configuration itself (`GET /v1/server/cluster/mesh_net/config`) and, with `apply = auto|wg`,
/// keeps the kernel interface in sync when `wg`/`ip` are available.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MeshNetConfig {
    pub enabled: bool,
    /// IPv4 overlay subnet shared by the cluster group, e.g. `10.77.0.0/16`.
    pub subnet: String,
    /// WireGuard UDP port this node listens on.
    pub listen_port: u16,
    /// `host:port` peers dial for this node's WireGuard socket; empty = host of `http.listen`
    /// when it is a routable address, otherwise this node only answers (peers must have one).
    pub endpoint: String,
    /// Kernel interface name.
    pub interface: String,
    /// `auto` = configure the interface when `wg` and `ip` are on PATH, `wg` = require them,
    /// `none` = only publish/render (operators apply the rendered config themselves).
    pub apply: String,
    /// PersistentKeepalive seconds (0 = off); keeps NAT mappings alive between providers.
    pub keepalive_secs: u16,
    /// Advertise the QUIC node bus on the overlay address instead of `node.bus_advertise`.
    pub bind_bus: bool,
}

impl Default for MeshNetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            subnet: "10.77.0.0/16".into(),
            listen_port: 51820,
            endpoint: String::new(),
            interface: "musubi0".into(),
            apply: "auto".into(),
            keepalive_secs: 25,
            bind_bus: false,
        }
    }
}

/// Player IP geolocation (`[geoip]`): a MaxMind GeoLite2/GeoIP2 `.mmdb` (City or Country) the
/// leader downloads with the account's licence key and refreshes every `update_days`; every login
/// stamps the account's last known country/region/city/coordinates, which feed audience rules
/// (`countries`), remote-config `countries` selectors (when the client sent no
/// `x-musubi-country`), analytics events without a country and latency-aware allocation. A
/// database older than `stale_days` is ignored (lookups return nothing) until a fresh one lands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GeoIpConfig {
    pub enabled: bool,
    /// `.mmdb` path; empty = `<node.data_dir>/<edition>.mmdb`.
    pub path: String,
    /// `GeoLite2-City` (default), `GeoLite2-Country`, `GeoIP2-City`, …
    pub edition: String,
    /// MaxMind account id + licence key; empty = never download, only read `path`.
    pub account_id: String,
    pub license_key: String,
    /// `{edition}` and `{license_key}` are substituted; default = MaxMind's permalink.
    pub download_url: String,
    /// Re-download when the file is older than this (MaxMind publishes twice a week).
    pub update_days: u32,
    /// Ignore a database whose build is older than this (0 = never stale).
    pub stale_days: u32,
    /// Networks resolved before the database — LAN / QA machines / CGNAT ranges mapped to a
    /// location by hand (`{cidr, country, region, city, lat, lon}`).
    pub overrides: Vec<GeoOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct GeoOverride {
    pub cidr: String,
    pub country: String,
    pub region: String,
    pub city: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
}

impl Default for GeoIpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: String::new(),
            edition: "GeoLite2-City".into(),
            account_id: String::new(),
            license_key: String::new(),
            download_url: "https://download.maxmind.com/app/geoip_download?edition_id={edition}&license_key={license_key}&suffix=tar.gz".into(),
            update_days: 7,
            stale_days: 35,
            overrides: Vec::new(),
        }
    }
}

/// Raw UDP realtime transport (dedicated servers ↔ core, private networks: no encryption).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UdpConfig {
    /// UDP listen address; unset = disabled.
    pub listen: Option<SocketAddr>,
    /// 32-byte hex key for the connection cookies; unset = random per boot (clients then
    /// re-handshake after a restart, which they do anyway).
    pub secret: Option<String>,
}

/// Product analytics: client telemetry ingest (`POST /v1/telemetry/events`), KPIs and export
/// sinks. `telemetry_keys` let pre-login clients send events (`X-Telemetry-Key` header).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalyticsConfig {
    pub enabled: bool,
    pub telemetry_keys: Vec<String>,
    /// Events per ingest request.
    pub max_batch: usize,
    /// Property payload cap per event, bytes.
    pub max_event_bytes: usize,
    /// Events without a registered schema: accept (`true`) or drop.
    pub accept_unregistered: bool,
    /// Also copy server-side domain events (`musubi.events`) into analytics + sinks.
    pub mirror_domain_events: bool,
    pub flush_secs: u64,
    pub sinks: Vec<AnalyticsSink>,
    /// Base URL of the dedicated telemetry nodes (`https://telemetry.example.net`), published to
    /// clients in `GET /v1/telemetry/sampling` as `ingest_url`; empty = events go to the API.
    pub ingest_url: String,
    /// With `ingest_url`: API nodes answer `POST /v1/telemetry/events|anonymous` with 307 to it
    /// instead of ingesting themselves.
    pub redirect_ingest: bool,
    /// Crash reports with one fingerprint from the same player/device inside this window fold
    /// into the earlier report (`duplicates`).
    pub crash_dedupe_secs: u64,
    /// Largest decoded minidump accepted with a crash report, bytes; 0 refuses minidumps.
    pub crash_max_minidump_bytes: usize,
    /// `crash.spike` fires when one group gathers this many reports within an hour (0 = off).
    pub crash_spike_per_hour: u64,
    /// Crash/error reports accepted per player or device per minute.
    pub crash_per_minute: u32,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            telemetry_keys: Vec::new(),
            max_batch: 500,
            max_event_bytes: 16 * 1024,
            accept_unregistered: true,
            mirror_domain_events: false,
            flush_secs: 10,
            sinks: Vec::new(),
            ingest_url: String::new(),
            redirect_ingest: false,
            crash_dedupe_secs: 60,
            crash_max_minidump_bytes: 4 * 1024 * 1024,
            crash_spike_per_hour: 50,
            crash_per_minute: 30,
        }
    }
}

/// Export sink: `jsonl` (rotating files in `path`, ship with any S3 sync), `clickhouse`
/// (HTTP interface `url`, `INSERT INTO table FORMAT JSONEachRow`), `webhook` (batched JSON
/// `POST url {events: [...]}` — Kafka REST proxy, Amplitude/Segment adapters, your own).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AnalyticsSink {
    pub kind: String,
    pub name: String,
    pub path: Option<String>,
    pub url: Option<String>,
    pub table: Option<String>,
    pub headers: std::collections::HashMap<String, String>,
    /// Only these event names (empty = all).
    pub names: Vec<String>,
    /// `s3`: name of a registered storage endpoint (`PUT /v1/server/storage_endpoints/{name}`).
    pub endpoint: Option<String>,
    /// `s3`: key prefix inside the endpoint, default `events/<sink name>/`.
    pub prefix: Option<String>,
    /// `s3`: upload once this many rows are buffered (Pragma `eventBatchSize`, default 10000)…
    pub batch_rows: usize,
    /// …or once the oldest buffered row is this old (`timeWindowPerBatchMillis`, default 60 s).
    pub batch_secs: u64,
    /// `s3`: gzip the NDJSON object (default true).
    pub gzip: Option<bool>,
}

/// Zanshin LiveOps (flags, experiments, analytics). Unset `url` = disabled.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ZanshinConfig {
    pub url: Option<String>,
    pub server_key: String,
    /// Seconds between analytics event flushes.
    pub flush_secs: u64,
}

/// Rate limits (token buckets, per node). `enabled = false` turns every check off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    pub enabled: bool,
    /// Login/refresh attempts per client IP (`X-Forwarded-For` first hop, else the peer address).
    pub auth_per_minute_per_ip: u32,
    pub http_per_second_per_account: u32,
    pub http_burst: u32,
    pub rpc_per_second_per_account: u32,
    pub rpc_burst: u32,
    /// Inbound realtime messages per account.
    pub socket_per_second: u32,
    pub socket_burst: u32,
    /// Share consumption between nodes over the bus so a budget is cluster-wide, not per node.
    pub cluster_wide: bool,
    /// Default budget of an issued server key (requests/s, burst, per day); 0 = unlimited. A key
    /// can carry its own via `PUT /v1/admin/keys/{id}/limits`. The config bootstrap key is never
    /// limited.
    pub key_per_second: u32,
    pub key_burst: u32,
    pub key_daily: u64,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auth_per_minute_per_ip: 30,
            http_per_second_per_account: 50,
            http_burst: 100,
            rpc_per_second_per_account: 20,
            rpc_burst: 40,
            socket_per_second: 60,
            socket_burst: 120,
            cluster_wide: true,
            key_per_second: 0,
            key_burst: 0,
            key_daily: 0,
        }
    }
}

/// Dedicated-server orchestration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetConfig {
    pub kubernetes: KubernetesConfig,
    pub firecracker: FirecrackerConfig,
    pub edgegap: EdgegapConfig,
    pub webhook: WebhookFleetConfig,
    pub gamelift: GameLiftConfig,
    /// Bare-metal / VPS hosts reached over SSH (`hybrid` profile: control plane anywhere,
    /// dedicated servers on Hetzner/OVH boxes).
    pub ssh: SshFleetConfig,
    /// Node-level autoscaling for the `ssh` host pool (P1-11): capacity pools that order and
    /// release machines on demand.
    pub capacity: CapacityConfig,
    /// `[fleet.regions.<name>] lat, lon` — where each region lives, for distance-based routing of
    /// `POST /v1/server/allocate` when the client sends coordinates instead of pings.
    pub regions: std::collections::HashMap<String, RegionCoords>,
}

/// Coordinates of a fleet region (decimal degrees).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RegionCoords {
    pub lat: f64,
    pub lon: f64,
}

/// `[fleet.capacity]` — host pools behind the `ssh` provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CapacityConfig {
    pub pools: Vec<CapacityPoolConfig>,
    /// Seconds between capacity passes (also runs after every fleet reconcile).
    pub pass_secs: u64,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self {
            pools: Vec::new(),
            pass_secs: 15,
        }
    }
}

/// One capacity pool: a hardware class in a region, provisioned by a driver.
/// `kind = manual` never orders machines — it raises `capacity.needed` so an operator racks a
/// box (bare-metal first); `webhook` calls `POST {url}/provision|deprovision`; `hetzner` uses the
/// Hetzner Cloud API (`server_type`, `image`, `location`, `ssh_keys`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CapacityPoolConfig {
    pub name: String,
    pub kind: String,
    pub region: String,
    pub class: String,
    /// Servers one provisioned host runs at once.
    pub capacity_per_host: u32,
    pub min_hosts: u32,
    pub max_hosts: u32,
    /// Keep at least this many free server slots across the pool's ready hosts.
    pub keep_free_slots: u32,
    /// Release a provisioned host after it has been empty this long (and the pool keeps its
    /// free-slot target without it).
    pub scale_down_idle_secs: u64,
    pub cooldown_secs: u64,
    /// Provisioning hosts that do not answer ssh within this time are marked failed and released.
    pub provision_timeout_secs: u64,
    /// Order spot / preemptible machines (cost tag + termination-notice handling).
    pub spot: bool,
    pub cost_per_hour: f64,
    pub ssh_user: String,
    pub ssh_port: u16,
    pub ssh_key: String,
    pub labels: std::collections::HashMap<String, String>,
    /// `webhook`: base url + bearer token.
    pub url: String,
    pub token: String,
    /// `hetzner`: API base (tests point it at a fake), server type, image, location, ssh key
    /// names/ids, cloud-init user data.
    pub api_url: String,
    pub server_type: String,
    pub image: String,
    pub location: String,
    pub ssh_keys: Vec<String>,
    pub user_data: String,
}

impl Default for CapacityPoolConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: "manual".into(),
            region: String::new(),
            class: String::new(),
            capacity_per_host: 8,
            min_hosts: 0,
            max_hosts: 4,
            keep_free_slots: 0,
            scale_down_idle_secs: 600,
            cooldown_secs: 120,
            provision_timeout_secs: 600,
            spot: false,
            cost_per_hour: 0.0,
            ssh_user: "musubi".into(),
            ssh_port: 22,
            ssh_key: String::new(),
            labels: Default::default(),
            url: String::new(),
            token: String::new(),
            api_url: "https://api.hetzner.cloud/v1".into(),
            server_type: String::new(),
            image: String::new(),
            location: String::new(),
            ssh_keys: Vec::new(),
            user_data: String::new(),
        }
    }
}

/// Firecracker microVM fleet provider; registered when `binary` is set. Needs KVM, a kernel
/// image and a read-only rootfs whose init reads `musubi.*` from the kernel command line, and —
/// for networking — a host bridge (`bridge`) with `subnet`/`gateway` the guests are told about.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FirecrackerConfig {
    pub binary: Option<String>,
    pub kernel: String,
    pub rootfs: String,
    pub vcpus: u32,
    pub mem_mib: u32,
    pub run_dir: String,
    pub boot_args: String,
    pub bridge: Option<String>,
    /// CIDR, e.g. `172.16.0.0/24`.
    pub subnet: Option<String>,
    pub gateway: Option<String>,
    /// Snapshot-restore: `prepare` writes `vmstate` + `mem` here; starts restore instead of booting.
    pub snapshot_dir: Option<String>,
    /// Restored-and-paused VMs kept ahead of demand (needs `snapshot_dir`).
    pub warm_pool: u32,
    /// Seconds `prepare` waits for the guest's `musubi: warm` console line.
    pub warm_boot_secs: u64,
}

impl Default for FirecrackerConfig {
    fn default() -> Self {
        Self {
            binary: None,
            kernel: "/var/lib/musubi/firecracker/vmlinux".into(),
            rootfs: "/var/lib/musubi/firecracker/rootfs.ext4".into(),
            vcpus: 1,
            mem_mib: 512,
            run_dir: "/var/lib/musubi/firecracker/run".into(),
            boot_args: String::new(),
            bridge: None,
            subnet: None,
            gateway: None,
            snapshot_dir: None,
            warm_pool: 0,
            warm_boot_secs: 5,
        }
    }
}

/// Kubernetes / Agones fleet providers. With no `url` the node uses the in-cluster service account
/// when it runs in a pod; otherwise the providers are not registered.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct KubernetesConfig {
    pub url: Option<String>,
    pub token: Option<String>,
    pub token_path: Option<String>,
    pub ca_cert_path: Option<String>,
    pub namespace: Option<String>,
    pub insecure: bool,
    /// Also register the `agones` provider (GameServer CRs).
    pub agones: bool,
}

/// Player market / trades.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MarketConfig {
    /// Seller fee in basis points (100 = 1%), taken from every sale.
    pub fee_bps: i64,
    /// Longest a listing may stay open, seconds.
    pub max_listing_secs: i64,
    /// Default listing duration when the seller gives none, seconds.
    pub default_listing_secs: i64,
    /// How long a direct trade offer waits for an answer, seconds.
    pub trade_ttl_secs: i64,
    /// Bids in the last `anti_snipe_secs` push the deadline out to that many seconds.
    pub anti_snipe_secs: i64,
    /// Anti-scam hold for direct trades: an accepted trade settles this many seconds later with
    /// both sides escrowed, and either side may still cancel (0 = settle on accept).
    pub trade_hold_secs: i64,
}

impl Default for MarketConfig {
    fn default() -> Self {
        Self {
            fee_bps: 500,
            max_listing_secs: 7 * 24 * 3600,
            default_listing_secs: 24 * 3600,
            trade_ttl_secs: 24 * 3600,
            anti_snipe_secs: 60,
            trade_hold_secs: 0,
        }
    }
}

/// Aurix voice. Unset `url` = disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AurixConfig {
    pub url: Option<String>,
    pub api_key: String,
    /// Aurix channel type for party / match channels (empty = `team`).
    pub party_channel_type: String,
    pub match_channel_type: String,
    /// Channel type of a world's proximity channel (`positional`: Aurix attenuates by the
    /// positions the client reports, the same ones it sends the world).
    pub world_channel_type: String,
    /// Mirror account bans and friend blocks into Aurix (best effort, asynchronous).
    pub moderation_sync: bool,
}

impl Default for AurixConfig {
    fn default() -> Self {
        Self {
            url: None,
            api_key: String::new(),
            party_channel_type: String::new(),
            match_channel_type: String::new(),
            world_channel_type: "positional".into(),
            moderation_sync: true,
        }
    }
}

/// In-app purchase validation providers. A provider is enabled when its key path is set.
/// Rewarded video / ad placements (competitive P1-53): server-to-server reward callbacks from ad
/// networks. `ironsource` verifies the documented `signature = md5(timestamp + eventId + userId +
/// privateKey)` and answers `eventId:OK`; `custom` is the generic scheme for any mediation or your
/// own ad server: `sig = hex(hmac_sha256(secret, sorted "k=v" pairs joined by "&", without sig))`
/// plus a `ts` within `callback_ttl_secs`. `allowed_ips` (CIDR or exact) further pins a provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdsConfig {
    pub providers: Vec<AdProviderConfig>,
    pub callback_ttl_secs: u64,
}

impl Default for AdsConfig {
    fn default() -> Self {
        Self {
            providers: Vec::new(),
            callback_ttl_secs: 600,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AdProviderConfig {
    /// Name in the callback URL: `/v1/ads/callback/{name}`.
    pub name: String,
    /// `ironsource` | `custom`.
    pub kind: String,
    /// Private key (ironSource) or HMAC secret (custom).
    pub secret: String,
    pub allowed_ips: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IapConfig {
    pub apple: AppleIapConfig,
    pub google: GoogleIapConfig,
    pub steam: SteamIapConfig,
    pub psn: ConsoleStoreConfig,
    pub xbox: ConsoleStoreConfig,
    pub nintendo: ConsoleStoreConfig,
    pub epic: ConsoleStoreConfig,
    pub huawei: HuaweiIapConfig,
    pub samsung: SamsungIapConfig,
}

/// Huawei AppGallery IAP (Order Service). Enabled when `app_id` is set; the client sends the
/// `purchase_token` it got from the IAP SDK, the server verifies it with an app-level OAuth token
/// and (for consumables) confirms delivery so the token cannot be redeemed twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HuaweiIapConfig {
    /// Order Service site for your region (`orders-dre`, `orders-drru`, `orders-dra`…).
    pub url: String,
    /// Subscription Service site.
    pub sub_url: String,
    pub token_url: String,
    pub app_id: String,
    pub app_secret: String,
    /// Confirm consumables server-to-server after granting (default true).
    pub confirm: bool,
}

impl Default for HuaweiIapConfig {
    fn default() -> Self {
        Self {
            url: "https://orders-drcn.iap.cloud.huawei.com.cn".into(),
            sub_url: "https://subscr-drcn.iap.cloud.huawei.com.cn".into(),
            token_url: "https://oauth-login.cloud.huawei.com/oauth2/v3/token".into(),
            app_id: String::new(),
            app_secret: String::new(),
            confirm: true,
        }
    }
}

/// Samsung Galaxy Store IAP. Enabled when `private_key_pem_path` is set: a Galaxy Store
/// Developer API service account (id + RSA key) signs a JWT for an access token, receipts are
/// verified with `GET /iap/v6/receipt?purchaseID=`. `acknowledge_path` (empty = off) is POSTed
/// with `{purchaseID}` after a consumable is granted.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SamsungIapConfig {
    pub url: String,
    pub token_url: String,
    pub service_account_id: String,
    pub private_key_pem_path: String,
    pub acknowledge_path: String,
}

impl Default for SamsungIapConfig {
    fn default() -> Self {
        Self {
            url: "https://iap.samsungapps.com".into(),
            token_url: "https://devapi.samsungapps.com/auth/accessToken".into(),
            service_account_id: String::new(),
            private_key_pem_path: String::new(),
            acknowledge_path: String::new(),
        }
    }
}

/// Console / launcher store with server-to-server entitlement lookup (PSN Entitlement Web API,
/// Microsoft Store Collections, Nintendo consumable service items, Epic Ecom). The server
/// authenticates with client credentials; empty `client_id` = disabled. `url`, `token_url` and the
/// two path templates default per platform and are overridable (Nintendo paths are under NDA, so
/// `url` must be set explicitly there). Placeholders: `{user}` platform account id,
/// `{product}` product / entitlement label, `{entitlement}` entitlement id, `{sandbox}`,
/// `{service_label}`, `{tenant}`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ConsoleStoreConfig {
    pub url: String,
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    /// OAuth scope (Xbox: `https://onestore.microsoft.com/.default`, PSN: `psn:s2s`).
    pub scope: String,
    /// Epic sandbox id; also substituted for `{sandbox}`.
    pub sandbox_id: String,
    /// Epic deployment id (sent with the token request).
    pub deployment_id: String,
    /// Azure AD tenant for Xbox (`{tenant}` in `token_url`).
    pub tenant_id: String,
    /// PSN service label (`{service_label}`), default `0`.
    pub service_label: String,
    pub verify_path: String,
    pub consume_path: String,
    /// Consume consumable entitlements on redeem (default true); off = redeem once per entitlement.
    pub consume: Option<bool>,
    /// Treat the store as a sandbox (receipts get environment `Sandbox`).
    pub sandbox: bool,
}

/// Steamworks Web API microtransactions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SteamIapConfig {
    pub url: String,
    /// Publisher Web API key with microtransaction permissions; empty = disabled.
    pub key: String,
    pub app_id: u32,
    /// Use ISteamMicroTxnSandbox.
    pub sandbox: bool,
}

impl Default for SteamIapConfig {
    fn default() -> Self {
        Self {
            url: "https://partner.steam-api.com".into(),
            key: String::new(),
            app_id: 0,
            sandbox: false,
        }
    }
}

/// Apple App Store Server API (StoreKit 2 transaction ids).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppleIapConfig {
    /// `https://api.storekit.itunes.apple.com` (prod) or `https://api.storekit-sandbox.itunes.apple.com`.
    pub url: String,
    pub issuer_id: String,
    pub key_id: String,
    pub bundle_id: String,
    /// PKCS#8 PEM of the App Store Connect API key (ES256).
    pub private_key_pem_path: String,
}

impl Default for AppleIapConfig {
    fn default() -> Self {
        Self {
            url: "https://api.storekit.itunes.apple.com".into(),
            issuer_id: String::new(),
            key_id: String::new(),
            bundle_id: String::new(),
            private_key_pem_path: String::new(),
        }
    }
}

/// Google Play Developer API via a service account.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GoogleIapConfig {
    pub url: String,
    pub token_url: String,
    pub package_name: String,
    pub client_email: String,
    /// PKCS#8 PEM of the service-account private key (RS256).
    pub private_key_pem_path: String,
    /// Shared secret expected as `?token=` on the Pub/Sub push endpoint for RTDN.
    pub notifications_token: String,
}

impl Default for GoogleIapConfig {
    fn default() -> Self {
        Self {
            url: "https://androidpublisher.googleapis.com".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            package_name: String::new(),
            client_email: String::new(),
            private_key_pem_path: String::new(),
            notifications_token: String::new(),
        }
    }
}

/// Who may scrape `GET /metrics`. By default it is open like `/healthz` (close it at the ingress);
/// `require_server_key` demands `X-Musubi-Server-Key`, `allow_ips` restricts callers to exact IPs
/// or prefixes ending in `.`/`:` (e.g. `10.`, `fd00:`), judged by the same client-IP rules as
/// rate limits (X-Forwarded-For → X-Real-IP → peer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub require_server_key: bool,
    pub allow_ips: Vec<String>,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_server_key: false,
            allow_ips: vec![],
        }
    }
}

/// Realtime session settings (WS/QUIC).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RealtimeConfig {
    /// How long a session that lost its transport is kept (subscriptions, matches, parties,
    /// buffered outbound) for the client to resume with its token. 0 disables resume.
    pub resume_ttl_secs: u64,
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            resume_ttl_secs: 30,
        }
    }
}

/// Per-user quotas for the key-value storage (`0` = unlimited); the system owner is never limited.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub max_objects_per_user: i64,
    pub max_bytes_per_user: i64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            max_objects_per_user: 10_000,
            max_bytes_per_user: 100 << 20,
        }
    }
}

/// Retention windows in days for time-series tables (0 = keep forever); see `retention.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionConfig {
    pub interval_secs: u64,
    pub events_days: u64,
    pub analytics_events_days: u64,
    /// Captured game server log lines (`musubi.server_logs`).
    pub server_logs_days: u64,
    /// Per-server metric samples and resolved fleet alerts (`musubi.server_metrics` / `fleet_alerts`).
    pub server_metrics_days: u64,
    /// Matchmaker ticket traces and formed-match records (`musubi.matchmaker_ticket_log` / `matchmaker_matches`).
    pub matchmaker_log_days: u64,
    /// Delivered or dead webhook deliveries.
    pub deliveries_days: u64,
    pub audit_days: u64,
    pub notifications_days: u64,
    /// Chat history per channel kind: rooms are transient by nature (plan: 90 days), group and
    /// direct history is kept forever unless set. `chat_days` > 0 applies to every kind at once.
    pub chat_room_days: u64,
    pub chat_group_days: u64,
    pub chat_direct_days: u64,
    pub chat_days: u64,
    /// Expired or revoked refresh sessions.
    pub sessions_days: u64,
    /// Terminated game sessions (members, codes) are kept this long for history/support.
    pub game_sessions_days: u64,
    /// World history snapshots (the time machine's reach).
    pub world_history_days: u64,
    /// Recorded matches (`params.record = true`) and their replay frames.
    pub replay_days: u64,
    /// Soft-deleted accounts are purged for good after this many days (0 = never).
    pub deleted_account_days: u64,
    /// Raw crash/error reports (groups and their counters stay).
    pub crash_reports_days: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            interval_secs: 600,
            events_days: 30,
            analytics_events_days: 90,
            server_logs_days: 7,
            server_metrics_days: 7,
            matchmaker_log_days: 14,
            deliveries_days: 7,
            audit_days: 90,
            notifications_days: 30,
            chat_room_days: 90,
            chat_group_days: 0,
            chat_direct_days: 0,
            chat_days: 0,
            sessions_days: 7,
            game_sessions_days: 14,
            world_history_days: 7,
            replay_days: 30,
            deleted_account_days: 30,
            crash_reports_days: 30,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MatchmakerConfig {
    /// A seed waits this long for `max_count` before settling for `min_count`.
    pub min_wait_secs: u64,
    /// Tickets older than this are dropped with `MatchmakerExpired`.
    pub ticket_ttl_secs: u64,
    /// Members must confirm a formed group within this many seconds (0 = no ready-check).
    pub ready_check_secs: u64,
    /// Base keep-out for a member who did not confirm; doubles per repeat within 24 h, up to 8×.
    pub dodge_penalty_secs: u64,
    /// Formed matches with free slots accept compatible later tickets (backfill) for this long; 0 = off.
    pub backfill_secs: u64,
    /// Named queues (`[[matchmaker.queues]]`, see `musubi_matchmaker::QueueConfig`): match size,
    /// teams, MMR/ping rules that widen with wait, role quotas, cross-play, no-repeat.
    pub queues: Vec<serde_json::Value>,
}

impl Default for MatchmakerConfig {
    fn default() -> Self {
        Self {
            min_wait_secs: 15,
            backfill_secs: 60,
            ticket_ttl_secs: 180,
            ready_check_secs: 0,
            dodge_penalty_secs: 60,
            queues: Vec::new(),
        }
    }
}

/// Third-party sign-in (`POST /v1/auth/steam|google|apple`, `/v1/account/link/...`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SocialConfig {
    pub steam: SteamLoginConfig,
    pub google: OidcLoginConfig,
    pub apple: OidcLoginConfig,
    /// Any other OpenID Connect issuer (Discord, Twitch, Epic, Xbox, Keycloak, …) keyed by a
    /// short name: `POST /v1/auth/oidc/{name}` with its ID token; identities are `oidc:<name>`.
    pub oidc: std::collections::HashMap<String, OidcLoginConfig>,
    /// Xbox Live: the client's XSTS token + user hash are checked against the profile service.
    pub xbox: XboxLoginConfig,
    /// PlayStation Network: the title's `id_token` (RS256 JWT). Set `jwks_url`/`issuers`/
    /// `client_ids` from the DevNet "ID token" page for the environment you target.
    pub psn: OidcLoginConfig,
    /// How often one node re-checks imported platform friend ids against newly linked
    /// identities (`friend.platform_match`); 0 disables the loop.
    pub platform_match_secs: u64,
    /// Nintendo Account / NSA `id_token`; `jwks_url` + `issuers` + `client_ids` from NDP docs.
    pub nintendo: OidcLoginConfig,
    pub passkeys: PasskeyConfig,
    /// Facebook Login: classic access tokens are checked with `debug_token` under the app's
    /// credentials; Limited Login id tokens (JWT) against Facebook's OIDC keys. Empty `app_id` =
    /// disabled.
    pub facebook: FacebookLoginConfig,
    /// Game Center identity verification signatures (`fetchItems(forIdentityVerificationSignature:)`).
    pub gamecenter: GameCenterLoginConfig,
    /// Group XP needed to reach level 2, 3, …; the last entry caps the level.
    pub group_level_xp: Vec<i64>,
}

/// Passkeys / WebAuthn relying party. Empty `rp_id` = passkeys disabled.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PasskeyConfig {
    /// Relying-party id: the site's registrable domain, e.g. `example.com`.
    pub rp_id: String,
    pub rp_name: String,
    /// Allowed `origin`s in clientDataJSON, e.g. `https://play.example.com`.
    pub origins: Vec<String>,
}

impl Default for PasskeyConfig {
    fn default() -> Self {
        Self {
            rp_id: String::new(),
            rp_name: "Musubi".into(),
            origins: vec![],
        }
    }
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            steam: SteamLoginConfig::default(),
            google: Self::google_defaults(),
            apple: Self::apple_defaults(),
            oidc: Default::default(),
            xbox: XboxLoginConfig::default(),
            psn: OidcLoginConfig::default(),
            platform_match_secs: 300,
            nintendo: OidcLoginConfig::default(),
            passkeys: PasskeyConfig::default(),
            facebook: FacebookLoginConfig::default(),
            gamecenter: GameCenterLoginConfig::default(),
            group_level_xp: vec![
                1_000, 3_000, 6_000, 10_000, 15_000, 21_000, 28_000, 36_000, 45_000,
            ],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SteamLoginConfig {
    pub url: String,
    /// Steamworks Web API key; empty = Steam login disabled.
    pub web_api_key: String,
    pub app_id: u32,
    /// Check `ISteamUser/CheckAppOwnership` at login and refuse accounts that do not own `app_id`
    /// (pirated or refunded copies). Family-shared and free-weekend copies count as owned.
    pub require_ownership: bool,
    /// Steam Family Sharing borrowers: `allow` (default) or `deny` (owner's copy only).
    pub family_sharing: String,
    /// Ban propagation through Family Sharing: `none`, `owner_to_borrower` (a borrower cannot log
    /// in while the lending account is banned — default) or `both` (the lender is also refused
    /// while any of their borrowers is banned).
    pub ban_propagation: String,
    /// Reuse a stored ownership answer this long before asking Steam again (0 = every login).
    pub ownership_cache_secs: u64,
    /// Encrypted app ticket key (Steamworks → Security → SDK Auth; 64 hex chars). Lets
    /// `POST /v1/auth/steam/app_ticket` and `/v1/server/steam/app_ticket` verify
    /// `ISteamUser::RequestEncryptedAppTicket` tickets offline — no Web API key, no egress to
    /// Steam. Empty = disabled.
    pub app_ticket_key: String,
    /// Reject app tickets issued longer ago than this (replay window; 0 = no age check).
    pub app_ticket_max_age_secs: u64,
}

/// Xbox Live profile service used to validate XSTS tokens (`Authorization: XBL3.0 x=<uhs>;<token>`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct XboxLoginConfig {
    /// Empty = Xbox login disabled. Production: `https://profile.xboxlive.com`.
    pub url: String,
    /// `x-xbl-contract-version` sent with the request.
    pub contract_version: u32,
}

impl Default for XboxLoginConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            contract_version: 2,
        }
    }
}

impl Default for SteamLoginConfig {
    fn default() -> Self {
        Self {
            url: "https://partner.steam-api.com".into(),
            web_api_key: String::new(),
            app_id: 0,
            require_ownership: false,
            family_sharing: "allow".into(),
            ban_propagation: "owner_to_borrower".into(),
            ownership_cache_secs: 3600,
            app_ticket_key: String::new(),
            app_ticket_max_age_secs: 86_400,
        }
    }
}

/// OpenID Connect ID-token verification against a JWKS. `client_ids` empty = audience not checked
/// (set it in production).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OidcLoginConfig {
    /// Explicit JWKS URL; empty = taken from `issuer`'s `/.well-known/openid-configuration`.
    pub jwks_url: String,
    /// Issuer URL; with an empty `jwks_url` this is where discovery happens (and it is also
    /// the accepted `iss` when `issuers` is empty).
    pub issuer: String,
    pub issuers: Vec<String>,
    pub client_ids: Vec<String>,
}

impl SocialConfig {
    pub fn google_defaults() -> OidcLoginConfig {
        OidcLoginConfig {
            jwks_url: "https://www.googleapis.com/oauth2/v3/certs".into(),
            issuer: String::new(),
            issuers: vec![
                "accounts.google.com".into(),
                "https://accounts.google.com".into(),
            ],
            client_ids: vec![],
        }
    }

    pub fn apple_defaults() -> OidcLoginConfig {
        OidcLoginConfig {
            jwks_url: "https://appleid.apple.com/auth/keys".into(),
            issuer: String::new(),
            issuers: vec!["https://appleid.apple.com".into()],
            client_ids: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FacebookLoginConfig {
    /// Graph API base; tests point it at a mock.
    pub url: String,
    pub app_id: String,
    pub app_secret: String,
    /// Limited Login (OIDC) verification; defaults to Facebook's issuer and JWKS, `app_id` is
    /// the accepted audience.
    pub oidc: OidcLoginConfig,
}

impl Default for FacebookLoginConfig {
    fn default() -> Self {
        Self {
            url: "https://graph.facebook.com".into(),
            app_id: String::new(),
            app_secret: String::new(),
            oidc: OidcLoginConfig {
                jwks_url: "https://www.facebook.com/.well-known/oauth/openid-configuration/jwks"
                    .into(),
                issuer: String::new(),
                issuers: vec!["https://www.facebook.com".into()],
                client_ids: vec![],
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GameCenterLoginConfig {
    /// Empty = Game Center login disabled. The signature's `bundle_id` must be one of these.
    pub bundle_ids: Vec<String>,
    /// `public_key_url` must start with one of these (Apple's certificate hosts).
    pub public_key_url_prefixes: Vec<String>,
    /// Signatures older/newer than this are refused; 0 disables the check.
    pub max_skew_secs: u64,
}

impl Default for GameCenterLoginConfig {
    fn default() -> Self {
        Self {
            bundle_ids: vec![],
            public_key_url_prefixes: vec![
                "https://static.gc.apple.com/".into(),
                "https://sandbox.gc.apple.com/".into(),
            ],
            max_skew_secs: 600,
        }
    }
}

/// Built-in issuer/JWKS for well-known OIDC providers, so `[social.oidc.twitch] client_ids = […]`
/// is all an operator writes. Applied when the entry leaves `jwks_url` and `issuer` empty.
pub fn oidc_preset(name: &str) -> Option<OidcLoginConfig> {
    let (issuer, jwks) = match name {
        "discord" => ("https://discord.com", "https://discord.com/api/oauth2/keys"),
        "twitch" => (
            "https://id.twitch.tv/oauth2",
            "https://id.twitch.tv/oauth2/keys",
        ),
        "epic" => (
            "https://api.epicgames.dev/epic/oauth/v2",
            "https://api.epicgames.dev/epic/oauth/v2/.well-known/jwks.json",
        ),
        "microsoft" => (
            "https://login.microsoftonline.com/common/v2.0",
            "https://login.microsoftonline.com/common/discovery/v2.0/keys",
        ),
        "line" => (
            "https://access.line.me",
            "https://api.line.me/oauth2/v2.1/certs",
        ),
        "kakao" => (
            "https://kauth.kakao.com",
            "https://kauth.kakao.com/.well-known/jwks.json",
        ),
        _ => return None,
    };
    Some(OidcLoginConfig {
        jwks_url: jwks.into(),
        issuer: String::new(),
        issuers: vec![issuer.into()],
        client_ids: vec![],
    })
}

/// Distributed tracing export (OpenTelemetry). Empty endpoint = spans stay local.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TracingConfig {
    /// OTLP/HTTP collector base URL, e.g. `http://otel-collector:4318`.
    pub otlp_endpoint: String,
    /// Head sampling ratio for root spans (0.0..=1.0); children follow their parent.
    pub sample_ratio: f64,
    pub service_name: String,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: String::new(),
            sample_ratio: 1.0,
            service_name: "musubi".into(),
        }
    }
}

/// Mobile push delivery for notifications that reach no live session.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PushConfig {
    pub fcm: FcmConfig,
    pub apns: ApnsConfig,
    pub webpush: WebPushConfig,
    pub hms: HmsConfig,
}

/// Firebase Cloud Messaging HTTP v1 with a service account; empty project_id = off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FcmConfig {
    pub project_id: String,
    pub client_email: String,
    /// RSA private key of the service account (PEM).
    pub private_key_pem: String,
    pub token_url: String,
    pub base_url: String,
}

impl Default for FcmConfig {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            client_email: String::new(),
            private_key_pem: String::new(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            base_url: "https://fcm.googleapis.com".into(),
        }
    }
}

/// Huawei Push Kit (HMS) with an app-level OAuth client; empty app_id = off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HmsConfig {
    pub app_id: String,
    pub app_secret: String,
    pub token_url: String,
    pub base_url: String,
}

impl Default for HmsConfig {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            app_secret: String::new(),
            token_url: "https://oauth-login.cloud.huawei.com/oauth2/v3/token".into(),
            base_url: "https://push-api.cloud.huawei.com".into(),
        }
    }
}

/// Apple Push Notification service with token (JWT) auth; empty key_id = off.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ApnsConfig {
    pub team_id: String,
    pub key_id: String,
    /// The .p8 signing key (PEM).
    pub private_key_pem: String,
    pub bundle_id: String,
    /// `https://api.push.apple.com` or `https://api.sandbox.push.apple.com`.
    pub base_url: String,
}

impl Default for ApnsConfig {
    fn default() -> Self {
        Self {
            team_id: String::new(),
            key_id: String::new(),
            private_key_pem: String::new(),
            bundle_id: String::new(),
            base_url: "https://api.push.apple.com".into(),
        }
    }
}

impl MetricsConfig {
    pub fn ip_allowed(&self, ip: &str) -> bool {
        self.allow_ips.is_empty()
            || self.allow_ips.iter().any(|a| {
                if a.ends_with('.') || a.ends_with(':') {
                    ip.starts_with(a.as_str())
                } else {
                    ip == a
                }
            })
    }
}

/// Web Push (RFC 8030/8291, VAPID): browsers and PWAs. Empty key = off. The key is a PKCS#8 PEM
/// P-256 private key (`openssl ecparam -name prime256v1 -genkey | openssl pkcs8 -topk8 -nocrypt`);
/// clients fetch the matching public key from `GET /v1/account/push_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebPushConfig {
    pub vapid_private_key_pem: String,
    /// `mailto:` or https URL the push service may contact about this sender.
    pub subject: String,
    /// How long the push service keeps an undelivered message.
    pub ttl_secs: u64,
}

impl Default for WebPushConfig {
    fn default() -> Self {
        Self {
            vapid_private_key_pem: String::new(),
            subject: "mailto:ops@example.com".into(),
            ttl_secs: 86_400,
        }
    }
}

/// Outgoing e-mail over SMTP for `account.email_verification` / `account.password_reset`. Empty
/// host = off (the events still reach webhooks and modules for a game-side mailer).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    pub host: String,
    pub port: u16,
    /// `starttls` (587), `implicit` (465) or `none` (plain, for local relays/tests).
    pub tls: String,
    pub username: String,
    pub password: String,
    pub from: String,
    pub from_name: String,
    /// Name announced in EHLO.
    pub helo: String,
    pub timeout_secs: u64,
    /// Page that completes verification, `{token}` substituted; empty = the mail carries the token.
    pub verify_url: String,
    /// Page that completes a password reset, `{token}` substituted.
    pub reset_url: String,
    /// Product name used in subjects.
    pub product: String,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 587,
            tls: "starttls".into(),
            username: String::new(),
            password: String::new(),
            from: "no-reply@localhost".into(),
            from_name: String::new(),
            helo: "musubi".into(),
            timeout_secs: 15,
            verify_url: String::new(),
            reset_url: String::new(),
            product: "Musubi".into(),
        }
    }
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    #[test]
    fn defaults_are_refused_unless_allowed() {
        let mut cfg = Config::default();
        let errs = |c: &Config| {
            c.validate()
                .into_iter()
                .filter(|p| p.severity == Severity::Error)
                .count()
        };
        assert!(errs(&cfg) >= 2, "{:?}", cfg.validate());
        cfg.node.allow_insecure_defaults = true;
        assert_eq!(errs(&cfg), 0);
        cfg.node.allow_insecure_defaults = false;
        cfg.http.server_key = "a-much-longer-real-server-key".into();
        cfg.session.secret = "0123456789abcdef0123456789abcdef-really-random".into();
        assert_eq!(errs(&cfg), 0, "{:?}", cfg.validate());
        cfg.session.access_ttl_secs = cfg.session.refresh_ttl_secs;
        assert_eq!(errs(&cfg), 1);
    }
}

/// Server-side anti-cheat signals (see `anticheat.rs` in the server).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AntiCheatConfig {
    pub enabled: bool,
    /// Match inputs per second per session before an `input_rate` signal (0 = off).
    pub max_inputs_per_sec: u32,
    /// Largest accepted match input in bytes (0 = off).
    pub max_input_bytes: usize,
    /// Scoring window for `anticheat` queue and the kick threshold.
    pub window_hours: i64,
    /// Kick the account from every node when its windowed score reaches this (0 = never).
    pub kick_score: i64,
    /// Dotted field of a JSON match input that carries the player's position (`pos`,
    /// `state.position`; `[x, y]` or `[x, y, z]`); empty = no movement heuristics.
    pub movement_field: String,
    /// Fastest legitimate movement in world units per second: a position further from the
    /// previous input than `max_speed × elapsed` (+ `movement_slack`) is an `impossible_move`.
    pub max_speed: f64,
    /// Distance tolerated on top of `max_speed × elapsed` (lag compensation, respawns…).
    pub movement_slack: f64,
    /// Vendor endpoints that receive `{provider, account_id, ticket}` and answer `{ok: bool}`.
    pub eac_url: String,
    pub eac_key: String,
    pub battleye_url: String,
    pub battleye_key: String,
    pub vendor_timeout_secs: u64,
}

/// Chat moderation beyond the built-in filters: an external automod webhook.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatConfig {
    /// POSTed `{channel_id, sender_id, username, content}` for every message; answers
    /// `{"action": "allow" | "block" | "replace", "content"?, "reason"?}`. Empty = off.
    pub automod_url: String,
    /// Bearer token sent to the webhook (empty = none).
    pub automod_token: String,
    pub automod_timeout_ms: u64,
    /// On webhook error/timeout: deliver the message (true) or reject it (false).
    pub automod_fail_open: bool,
    /// POSTed `{channel_id, sender_id, text, source, targets}` for a message with a `text` when
    /// someone present in the channel speaks another language; answers
    /// `{"translations": {"de": "..."}}`, attached to the message as `content.translations`.
    /// Empty = off.
    pub translate_url: String,
    pub translate_token: String,
    pub translate_timeout_ms: u64,
    /// Most target languages asked for per message.
    pub translate_max_langs: usize,
    /// Largest chat attachment (`PUT /v1/channel/{id}/attachments`), bytes. Default 10 MiB.
    pub attachment_max_bytes: u64,
}

/// Age gate / parental controls (COPPA, GDPR-K, platform age ratings).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ComplianceConfig {
    /// Enforce the lists below (off = brackets are stored, nothing is gated).
    pub age_gate: bool,
    /// Younger than this = `child`.
    pub child_until: u32,
    /// Younger than this (and not a child) = `teen`.
    pub adult_from: u32,
    /// Treat accounts without an age as children until they give one.
    pub unknown_as_child: bool,
    /// Features closed for children: `chat`, `friends`, `purchases`, `ugc`, `public_profile`.
    pub child_restrictions: Vec<String>,
    /// Features that stay closed after a parent consented.
    pub consented_child_restrictions: Vec<String>,
    pub teen_restrictions: Vec<String>,
    /// Lifetime of a parental-consent token.
    pub consent_ttl_secs: i64,
    /// How often the leader processes queued data-rights (export / erase) requests.
    pub data_rights_poll_minutes: u32,
    /// Include game-data tables (storage, inventory, …) in queued exports by default.
    pub data_rights_include_game_data: bool,
}

impl Default for ComplianceConfig {
    fn default() -> Self {
        Self {
            age_gate: true,
            child_until: 13,
            adult_from: 18,
            unknown_as_child: false,
            child_restrictions: ["chat", "friends", "purchases", "ugc", "public_profile"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            consented_child_restrictions: vec!["purchases".into()],
            teen_restrictions: vec![],
            consent_ttl_secs: 7 * 24 * 3600,
            data_rights_poll_minutes: 10,
            data_rights_include_game_data: true,
        }
    }
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            attachment_max_bytes: 10 << 20,
            automod_url: String::new(),
            automod_token: String::new(),
            automod_timeout_ms: 500,
            automod_fail_open: true,
            translate_url: String::new(),
            translate_token: String::new(),
            translate_timeout_ms: 800,
            translate_max_langs: 8,
        }
    }
}

impl Default for AntiCheatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_inputs_per_sec: 120,
            max_input_bytes: 4096,
            window_hours: 24,
            kick_score: 0,
            movement_field: String::new(),
            max_speed: 0.0,
            movement_slack: 0.0,
            eac_url: String::new(),
            eac_key: String::new(),
            battleye_url: String::new(),
            battleye_key: String::new(),
            vendor_timeout_secs: 5,
        }
    }
}

/// Edgegap as a fleet provider (`provider = "edgegap"`); registered when `token` is set. A fleet's
/// `spec.image` of the form `app:version` overrides `app`/`version`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EdgegapConfig {
    pub url: String,
    pub token: String,
    pub app: String,
    pub version: String,
    pub poll_secs: u64,
    pub ready_timeout_secs: u64,
}

impl Default for EdgegapConfig {
    fn default() -> Self {
        Self {
            url: "https://api.edgegap.com".into(),
            token: String::new(),
            app: String::new(),
            version: String::new(),
            poll_secs: 2,
            ready_timeout_secs: 180,
        }
    }
}

/// Any orchestrator behind a small HTTP contract (`provider = "webhook"`), e.g. GameFabric or a
/// custom scheduler; registered when `url` is set. `POST {url}/servers` starts a server and
/// returns `{id?, address, port}` (or `202` + `id` to poll at `GET {url}/servers/{id}` until
/// `status: "ready"`), `DELETE {url}/servers/{id}` stops it. `token` is sent as a Bearer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhookFleetConfig {
    pub url: String,
    pub token: String,
    pub poll_secs: u64,
    pub ready_timeout_secs: u64,
}

/// `[fleet.ssh]`: a pool of machines the node starts dedicated servers on over SSH. Each host
/// needs the game server binary (`spec.command`) present; the node runs
/// `ssh user@addr 'cd dir && env … nohup cmd args … & echo $!'` and later `kill`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SshFleetConfig {
    pub hosts: Vec<SshHost>,
    /// `ssh` binary (tests point it at a fake).
    pub ssh_command: String,
    /// Extra `ssh` options (`-o StrictHostKeyChecking=accept-new` by default).
    pub ssh_args: Vec<String>,
    pub connect_timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SshHost {
    /// Address `ssh` connects to.
    pub addr: String,
    /// Address players connect to (defaults to `addr`).
    pub public_addr: Option<String>,
    pub user: String,
    pub port: u16,
    /// Private key path (`-i`); empty = ssh agent / default keys.
    pub key: String,
    /// Servers this host runs at once.
    pub capacity: u32,
}

impl Default for SshFleetConfig {
    fn default() -> Self {
        Self {
            hosts: Vec::new(),
            ssh_command: "ssh".into(),
            ssh_args: vec!["-o".into(), "StrictHostKeyChecking=accept-new".into()],
            connect_timeout_secs: 10,
        }
    }
}

impl Default for SshHost {
    fn default() -> Self {
        Self {
            addr: String::new(),
            public_addr: None,
            user: "musubi".into(),
            port: 22,
            key: String::new(),
            capacity: 8,
        }
    }
}

impl Default for WebhookFleetConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            token: String::new(),
            poll_secs: 2,
            ready_timeout_secs: 180,
        }
    }
}

/// Amazon GameLift as a fleet provider (`provider = "gamelift"`); registered when `access_key` is
/// set. Each Musubi game server is a GameLift game session on `fleet_id` (or `alias_id`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct GameLiftConfig {
    /// Endpoint override (tests / local emulators); default `https://gamelift.{region}.amazonaws.com`.
    pub url: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: String,
    pub fleet_id: String,
    pub alias_id: String,
    pub max_players: u32,
    pub poll_secs: u64,
    pub ready_timeout_secs: u64,
}

impl Default for GameLiftConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            region: "us-east-1".into(),
            access_key: String::new(),
            secret_key: String::new(),
            session_token: String::new(),
            fleet_id: String::new(),
            alias_id: String::new(),
            max_players: 64,
            poll_secs: 2,
            ready_timeout_secs: 120,
        }
    }
}

/// Client georouting: the regions clients may probe and a country → region fallback.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RegionsConfig {
    /// UDP echo for latency probes on this node (datagrams ≤ 64 bytes are echoed back).
    pub echo_listen: Option<SocketAddr>,
    /// Regions to advertise in `GET /v1/regions` (fleet regions are added automatically).
    pub regions: Vec<RegionEntry>,
    /// ISO country code → region, used as `suggested` when the client sends `cf-ipcountry` /
    /// `x-country`.
    pub country_map: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RegionEntry {
    pub name: String,
    /// `host:port` of a UDP echo in that region.
    pub echo: String,
    /// HTTP(S) URL to time a request against in that region.
    pub http: String,
}

/// Console operators: SSO through an OpenID Connect provider mints short-lived operator keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OperatorsConfig {
    pub sso: OperatorSsoConfig,
    /// Refuse SSO logins whose ID token does not prove a second factor (`amr` / `acr` claims,
    /// see `sso.mfa_claims`).
    pub require_mfa: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OperatorSsoConfig {
    /// OIDC issuer (`https://accounts.google.com`, `https://login.microsoftonline.com/<tenant>/v2.0`,
    /// Okta/Keycloak realm…); empty = operator SSO is off. Endpoints come from discovery unless
    /// `authorize_url` / `token_url` / `jwks_url` are set.
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub authorize_url: String,
    pub token_url: String,
    pub jwks_url: String,
    /// Where the provider sends the operator back: `https://api.example.com/v1/admin/sso/callback`.
    pub redirect_url: String,
    /// Extra scopes on top of `openid email profile` (e.g. `groups`).
    pub scopes: Vec<String>,
    /// ID-token claim holding the operator's groups/roles (a string or an array of strings).
    pub role_claim: String,
    /// Claim value → operator role (`admin` / `support` / `liveops` / `ops`); the most privileged
    /// matching role wins.
    pub role_map: std::collections::BTreeMap<String, String>,
    /// Role for operators without a mapped group; empty = refuse them.
    pub default_role: String,
    /// `amr` values (or substrings of `acr`) that count as a second factor when
    /// `[operators] require_mfa` is on. Default: `mfa`, `otp`, `hwk`, `mca`, `sms`, `swk`, `fido`.
    pub mfa_claims: Vec<String>,
    /// Only e-mails of these domains may log in (empty = any).
    pub allowed_domains: Vec<String>,
    /// Lifetime of the minted operator key.
    pub session_hours: u64,
}

impl Default for OperatorSsoConfig {
    fn default() -> Self {
        Self {
            mfa_claims: ["mfa", "otp", "hwk", "mca", "sms", "swk", "fido"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            issuer: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
            authorize_url: String::new(),
            token_url: String::new(),
            jwks_url: String::new(),
            redirect_url: String::new(),
            scopes: Vec::new(),
            role_claim: "groups".into(),
            role_map: Default::default(),
            default_role: String::new(),
            allowed_domains: Vec::new(),
            session_hours: 12,
        }
    }
}

/// Title / global files (`/v1/files`): patches, config blobs, localization tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    /// Largest file `PUT /v1/server/files/{path}` accepts (bytes); 64 MiB unless set.
    pub max_bytes: u64,
    /// CDN origin fronting the files bucket; when set, public manifest URLs are `{base}/{path}`
    /// instead of `/v1/files/{path}`. Blobs live in `[ugc.s3]` (or Postgres when unset).
    pub public_base_url: String,
    /// Largest per-player file (`PUT /v1/account/files/{key}`), bytes; 8 MiB unless set.
    pub player_max_bytes: u64,
    /// Files (keys) a player may keep; 100 unless set.
    pub player_max_files: u32,
    /// Revisions kept per player file (older ones are dropped; any kept one can be promoted back
    /// to current); 5 unless set.
    pub player_keep_revisions: u32,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            max_bytes: 64 << 20,
            public_base_url: String::new(),
            player_max_bytes: 8 << 20,
            player_max_files: 100,
            player_keep_revisions: 5,
        }
    }
}

/// User-generated content storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UgcConfig {
    /// Largest blob a `PUT /v1/ugc/{id}/blob` accepts (bytes); 16 MiB unless set.
    pub max_blob_bytes: u64,
    /// Where blobs go; unset = inside Postgres (fine for small mods, not for gigabytes).
    pub s3: S3Config,
}

impl Default for UgcConfig {
    fn default() -> Self {
        Self {
            max_blob_bytes: 16 << 20,
            s3: S3Config::default(),
        }
    }
}

/// An S3-compatible bucket (AWS S3, MinIO, R2, B2, GCS interop).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct S3Config {
    /// `https://s3.eu-central-1.amazonaws.com`, `http://minio:9000`…; empty = off.
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
    pub session_token: String,
    /// `endpoint/bucket/key` (MinIO, R2) instead of `bucket.endpoint/key`.
    pub path_style: bool,
    /// Key prefix inside the bucket.
    pub prefix: String,
    /// Uploads larger than this go up in parts of this size (AWS needs ≥ 5 MiB; default 8 MiB).
    pub part_bytes: u64,
}

impl Default for S3Config {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            bucket: String::new(),
            region: "us-east-1".into(),
            access_key: String::new(),
            secret_key: String::new(),
            session_token: String::new(),
            path_style: true,
            prefix: String::new(),
            part_bytes: 8 << 20,
        }
    }
}

/// Purchase-velocity heuristics: too many store purchases / too much spend / too many IAP
/// receipts from one account inside `window_secs`. `action = "flag"` records a fraud flag and
/// emits `economy.fraud_flag`; `"block"` additionally rejects the purchase with 429.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FraudConfig {
    pub enabled: bool,
    pub window_secs: u64,
    /// Store purchases per window (0 = no limit).
    pub max_purchases: u32,
    /// Spend per currency per window, e.g. `{ gold = 100000 }`.
    pub max_spend: std::collections::HashMap<String, i64>,
    /// IAP receipts redeemed per window (0 = no limit).
    pub max_iap: u32,
    pub action: String,
}

impl Default for FraudConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            window_secs: 3600,
            max_purchases: 0,
            max_spend: Default::default(),
            max_iap: 0,
            action: "flag".into(),
        }
    }
}

/// `[economy]` — catalog change workflow and localisation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EconomyConfig {
    /// Changesets must be approved by a different operator key before they can be published.
    pub require_review: bool,
    /// Locale clients get when they ask for none / the string is missing in theirs.
    pub default_locale: String,
    /// Locales every touched item / store entry must have strings for before a changeset
    /// validates (empty = no requirement).
    pub required_locales: Vec<String>,
}

impl Default for EconomyConfig {
    fn default() -> Self {
        Self {
            require_review: false,
            default_locale: "en".into(),
            required_locales: Vec::new(),
        }
    }
}
