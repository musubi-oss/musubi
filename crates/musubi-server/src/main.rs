use std::path::PathBuf;

use clap::Parser;
use musubi_server::Config;

#[derive(Parser, Debug)]
#[command(name = "musubi-server", about = "Musubi node")]
struct Args {
    /// Path to a TOML/YAML config file. Env `MUSUBI__*` overrides it.
    #[arg(short, long, env = "MUSUBI_CONFIG")]
    config: Option<PathBuf>,
}

/// glibc malloc serialises the runtime workers and the per-world tick threads on its arena
/// locks under load (10k sessions: most worker samples sat in realloc/free waiting for one)
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cfg = Config::load(args.config.as_deref())?;
    musubi_server::init_tracing(&cfg);
    musubi_server::run(cfg).await
}
