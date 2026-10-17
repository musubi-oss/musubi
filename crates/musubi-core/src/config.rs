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
}

impl Role {
    pub const ALL: [Role; 6] = [
        Role::Edge,
        Role::Api,
        Role::Realtime,
        Role::Sim,
        Role::Orchestrator,
        Role::Control,
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
    pub zanshin: ZanshinConfig,
    pub aurix: AurixConfig,
    pub iap: IapConfig,
    pub market: MarketConfig,
    pub fleet: FleetConfig,
    pub limits: LimitsConfig,
    pub metrics: MetricsConfig,
    pub realtime: RealtimeConfig,
    pub retention: RetentionConfig,
    pub matchmaker: MatchmakerConfig,
    pub social: SocialConfig,
    pub tracing: TracingConfig,
    pub push: PushConfig,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LogConfig {
    pub filter: String,
    pub json: bool,
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
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            filter: "info".into(),
            json: false,
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
        b.build()?.try_deserialize()
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
    /// Cron-driven calls into module exports named `schedule_<name>` (one runner cluster-wide).
    pub schedules: Vec<ScheduleConfig>,
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
        }
    }
}

/// QUIC realtime transport (same envelopes as WebSocket).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuicConfig {
    /// UDP listen address; unset = QUIC disabled.
    pub listen: Option<SocketAddr>,
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
        }
    }
}

/// Dedicated-server orchestration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FleetConfig {
    pub kubernetes: KubernetesConfig,
    pub firecracker: FirecrackerConfig,
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
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IapConfig {
    pub apple: AppleIapConfig,
    pub google: GoogleIapConfig,
    pub steam: SteamIapConfig,
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

/// Retention windows in days for time-series tables (0 = keep forever); see `retention.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetentionConfig {
    pub interval_secs: u64,
    pub events_days: u64,
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
    /// World history snapshots (the time machine's reach).
    pub world_history_days: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            interval_secs: 600,
            events_days: 30,
            deliveries_days: 7,
            audit_days: 90,
            notifications_days: 30,
            chat_room_days: 90,
            chat_group_days: 0,
            chat_direct_days: 0,
            chat_days: 0,
            sessions_days: 7,
            world_history_days: 7,
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
}

impl Default for SocialConfig {
    fn default() -> Self {
        Self {
            steam: SteamLoginConfig::default(),
            google: Self::google_defaults(),
            apple: Self::apple_defaults(),
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
}

impl Default for SteamLoginConfig {
    fn default() -> Self {
        Self {
            url: "https://partner.steam-api.com".into(),
            web_api_key: String::new(),
            app_id: 0,
        }
    }
}

/// OpenID Connect ID-token verification against a JWKS. `client_ids` empty = audience not checked
/// (set it in production).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct OidcLoginConfig {
    pub jwks_url: String,
    pub issuers: Vec<String>,
    pub client_ids: Vec<String>,
}

impl SocialConfig {
    pub fn google_defaults() -> OidcLoginConfig {
        OidcLoginConfig {
            jwks_url: "https://www.googleapis.com/oauth2/v3/certs".into(),
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
            issuers: vec!["https://appleid.apple.com".into()],
            client_ids: vec![],
        }
    }
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
