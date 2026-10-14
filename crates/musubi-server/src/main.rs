use std::path::PathBuf;

use clap::Parser;
use musubi_core::config::Config;

mod app;
mod db;
mod http;

#[derive(Parser, Debug)]
#[command(name = "musubi-server", about = "Musubi node")]
struct Args {
    /// Path to a TOML/YAML config file. Env `MUSUBI__*` overrides it.
    #[arg(short, long, env = "MUSUBI_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cfg = Config::load(args.config.as_deref())?;
    init_tracing(&cfg);
    app::run(cfg).await
}

fn init_tracing(cfg: &Config) {
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
