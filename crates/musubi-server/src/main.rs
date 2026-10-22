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

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let cfg = Config::load(args.config.as_deref())?;
    // Worker pools sized to the cores this process may actually use (affinity mask, cgroup
    // quota), not the machine's count: a node pinned to 6 of 8 cores used to run 8 runtime
    // workers plus an 8-thread rayon pool on top of its per-world tick threads and the tick
    // threads missed their 33 ms deadlines (stand: 25 ticks/s instead of 30).
    let workers = cfg
        .node
        .worker_threads
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
        });
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .thread_name(|i| format!("musubi-par-{i}"))
        .build_global()
        .ok();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()?;
    rt.block_on(async {
        musubi_server::init_tracing(&cfg);
        tracing::info!(workers, "worker pools sized");
        musubi_server::run(cfg).await
    })
}
