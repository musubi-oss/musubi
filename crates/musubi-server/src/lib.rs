//! Musubi node: wires config, database, services and HTTP/realtime listeners together.

pub mod app;
pub mod audit;
pub mod db;
pub mod events;
pub mod fanout;
pub mod http;
pub mod limits;
pub mod metrics;
pub mod notify;
pub mod realtime;
pub mod runtime_host;

pub use app::run;
pub use musubi_core::config::Config;

/// Install the global tracing subscriber from `[log]`.
pub fn init_tracing(cfg: &Config) {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(cfg.log.filter.clone()));
    let reg = tracing_subscriber::registry().with(filter);
    if cfg.log.json {
        reg.with(fmt::layer().json()).init();
    } else {
        reg.with(fmt::layer()).init();
    }
}
