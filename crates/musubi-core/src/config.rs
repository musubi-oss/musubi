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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeConfig {
    /// Stable name; defaults to the hostname.
    pub name: String,
    pub roles: Vec<Role>,
    pub region: String,
    pub data_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    pub listen: SocketAddr,
    pub max_body_bytes: usize,
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
            data_dir: "./data".into(),
        }
    }
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:7350".parse().unwrap(),
            max_body_bytes: 1 << 20,
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
