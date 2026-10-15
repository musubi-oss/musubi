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
    pub zanshin: ZanshinConfig,
    pub aurix: AurixConfig,
    pub iap: IapConfig,
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

impl Config {
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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            modules_dir: None,
            fuel: 50_000_000,
            max_memory_mb: 64,
            timeout_ms: 2000,
            max_payload_kb: 1024,
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

/// Zanshin LiveOps (flags, experiments, analytics). Unset `url` = disabled.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ZanshinConfig {
    pub url: Option<String>,
    pub server_key: String,
    /// Seconds between analytics event flushes.
    pub flush_secs: u64,
}

/// Aurix voice. Unset `url` = disabled.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AurixConfig {
    pub url: Option<String>,
    pub api_key: String,
    /// Aurix channel type for party / match channels.
    pub party_channel_type: String,
    pub match_channel_type: String,
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
