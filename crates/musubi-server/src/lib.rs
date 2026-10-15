//! Musubi node: wires config, database, services and HTTP/realtime listeners together.

pub mod app;
pub mod audit;
pub mod bans;
pub mod db;
pub mod events;
pub mod fanout;
pub mod http;
pub mod jwt;
pub mod keys;
pub mod limits;
pub mod logins;
pub mod metrics;
pub mod modules;
pub mod notify;
pub mod openapi;
pub mod realtime;
pub mod retention;
pub mod runtime_host;
pub mod telemetry;

pub use app::run;
pub use musubi_core::config::Config;

/// Install the global tracing subscriber from `[log]`.
pub fn init_tracing(cfg: &Config) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cfg.log.filter.clone()));
    let reg = tracing_subscriber::registry().with(filter);
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
