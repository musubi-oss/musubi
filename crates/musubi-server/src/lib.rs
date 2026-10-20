//! Musubi node: wires config, database, services and HTTP/realtime listeners together.

pub mod achievement_sync;
pub mod analytics;
pub mod anticheat;
pub mod app;
pub mod async_mm;
pub mod audit;
pub mod auth_providers;
pub mod automod;
pub mod bans;
pub mod blobstore;
pub mod compliance;
pub mod db;
pub mod deletion_ledger;
pub mod dev;
pub mod edge;
pub mod events;
pub mod fanout;
pub mod fleet_metrics;
pub mod fraud;
pub mod game_sessions;
pub mod grant_limits;
pub mod hooks;
pub mod http;
pub mod idempotency;
pub mod idl;
pub mod jwt;
pub mod keys;
pub mod legal;
pub mod limits;
pub mod liveops;
pub mod loadtest;
pub mod lobby;
pub mod logins;
pub mod metrics;
pub mod module_env;
pub mod modules;
pub mod nakama;
pub mod notify;
pub mod openapi;
pub mod platform_friends;
pub mod playfab;
pub mod profile;
pub mod push;
pub mod realtime;
pub mod regions;
pub mod registry;
pub mod retention;
pub mod runtime_host;
pub mod schedules;
pub mod scopes;
pub mod security;
pub mod steam_ticket;
pub mod system_logs;
pub mod telemetry;
pub mod translate;
pub mod turns;
pub mod workflows;

pub use app::run;
pub use musubi_core::config::Config;

/// Install the global tracing subscriber from `[log]`.
pub fn init_tracing(cfg: &Config) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cfg.log.filter.clone()));
    let reg = tracing_subscriber::registry()
        .with(filter)
        .with(crate::system_logs::layer(cfg.log.ring_lines));
    if cfg.log.json {
        let reg = reg.with(fmt::layer().json());
        match crate::telemetry::otlp_layer(&cfg.tracing) {
            Some((otel, _provider)) => reg.with(otel).init(),
            None => reg.init(),
        }
    } else {
        let reg = reg.with(fmt::layer());
        match crate::telemetry::otlp_layer(&cfg.tracing) {
            Some((otel, _provider)) => reg.with(otel).init(),
            None => reg.init(),
        }
    }
}
