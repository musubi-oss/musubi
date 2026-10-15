//! `musubi` — developer CLI.

use std::io::Write;
use std::path::PathBuf;

mod bench;
mod doctor;
mod soak;
mod transfer;

use clap::{Parser, Subcommand};
use musubi_core::config::Config;

#[derive(Parser)]
#[command(name = "musubi", version, about = "Musubi developer CLI")]
struct Cli {
    /// Config file (TOML/YAML). Env `MUSUBI__SECTION__KEY` overrides.
    #[arg(short, long, global = true, env = "MUSUBI_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a single all-roles node against a local Postgres, migrating on start.
    Dev {
        /// Postgres URL (default: postgres://musubi:musubi@localhost:5432/musubi).
        #[arg(long, env = "DATABASE_URL")]
        database_url: Option<String>,
        /// HTTP listen address.
        #[arg(long, default_value = "127.0.0.1:7350")]
        listen: String,
    },
    /// Apply pending database migrations and exit.
    Migrate,
    /// Dump game data (every `musubi` table except runtime state, or `--tables`) as JSONL.
    Export {
        /// Output file (default: stdout).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Comma-separated table names to export instead of the default set.
        #[arg(long, value_delimiter = ',')]
        tables: Option<Vec<String>>,
    },
    /// Load a JSONL dump produced by `export`; existing rows are kept unless `--replace`.
    Import {
        /// Input file (default: stdin).
        #[arg(long)]
        input: Option<PathBuf>,
        #[arg(long)]
        replace: bool,
    },
    /// Check the configuration and the environment (secrets, TTLs, database, pending migrations,
    /// data dir, ports) and exit 1 if anything would stop the server.
    Doctor,
    /// Print the effective configuration.
    Config {
        #[arg(long, default_value = "toml")]
        format: String,
    },
    /// Load-test a running server: device auth per client, then http | storage | ws | all loops;
    /// prints p50/p95/p99 latencies, throughput and errors.
    Bench {
        #[arg(long, default_value = "http://127.0.0.1:7350")]
        url: String,
        #[arg(long, default_value_t = 50)]
        clients: usize,
        #[arg(long, default_value_t = 10)]
        duration_secs: u64,
        #[arg(long, default_value = "all")]
        scenario: String,
        #[arg(long)]
        json: bool,
    },
    /// Chaos/soak stand: spawn a local cluster of `musubi-server` processes on one Postgres, put
    /// WS + storage clients on it, SIGKILL or drain random nodes every `kill_every_secs` and
    /// restart them; reports failover and world re-home times, client errors, reconnects and lost
    /// writes.
    Soak {
        /// Postgres the spawned nodes share (needs an empty-ish database you do not care about).
        #[arg(long, env = "MUSUBI_SOAK_DATABASE_URL")]
        database_url: String,
        /// Path to the `musubi-server` binary (default: next to this executable).
        #[arg(long)]
        server_bin: Option<std::path::PathBuf>,
        #[arg(long, default_value_t = 3)]
        nodes: usize,
        #[arg(long, default_value_t = 24)]
        clients: usize,
        #[arg(long, default_value_t = 6)]
        worlds: usize,
        /// authoritative "echo" matches spread across the nodes (a third of the clients play them)
        #[arg(long, default_value_t = 6)]
        matches: usize,
        #[arg(long, default_value_t = 60)]
        duration_secs: u64,
        #[arg(long, default_value_t = 10)]
        kill_every_secs: u64,
        /// kill | drain | mixed
        #[arg(long, default_value = "mixed")]
        mode: String,
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut cfg = Config::load(cli.config.as_deref())?;
    match cli.cmd {
        Cmd::Dev {
            database_url,
            listen,
        } => {
            if let Some(url) = database_url {
                cfg.database.url = url;
            }
            cfg.http.listen = listen.parse()?;
            cfg.database.run_migrations = true;
            cfg.node.allow_insecure_defaults = true;
            if cfg.log.filter == "info" {
                cfg.log.filter = "info,musubi=debug,tower_http=debug".into();
            }
            musubi_server::init_tracing(&cfg);
            musubi_server::run(cfg).await
        }
        Cmd::Doctor => doctor::run(&cfg).await,
        Cmd::Export { out, tables } => {
            let pool = musubi_server::db::connect(&cfg.database).await?;
            let counts = match out {
                Some(p) => {
                    let mut f = std::io::BufWriter::new(std::fs::File::create(&p)?);
                    let c = transfer::export(&pool, &mut f, tables.as_deref()).await?;
                    f.flush()?;
                    c
                }
                None => {
                    let mut o = std::io::stdout().lock();
                    transfer::export(&pool, &mut o, tables.as_deref()).await?
                }
            };
            for (t, n) in &counts {
                eprintln!("{t:<24} {n}");
            }
            eprintln!(
                "exported {} rows from {} tables",
                counts.values().sum::<u64>(),
                counts.len()
            );
            Ok(())
        }
        Cmd::Import { input, replace } => {
            let pool = musubi_server::db::connect(&cfg.database).await?;
            let counts = match input {
                Some(p) => {
                    let mut r = std::io::BufReader::new(std::fs::File::open(&p)?);
                    transfer::import(&pool, &mut r, replace).await?
                }
                None => {
                    let mut r = std::io::stdin().lock();
                    transfer::import(&pool, &mut r, replace).await?
                }
            };
            for (t, (w, s)) in &counts {
                eprintln!("{t:<24} written {w:<8} kept {s}");
            }
            Ok(())
        }
        Cmd::Migrate => {
            musubi_server::init_tracing(&cfg);
            let pool = musubi_server::db::connect(&cfg.database).await?;
            musubi_server::db::migrate(&pool).await
        }
        Cmd::Bench {
            url,
            clients,
            duration_secs,
            scenario,
            json,
        } => {
            bench::run(bench::Opts {
                url,
                clients,
                duration: std::time::Duration::from_secs(duration_secs),
                scenario,
                json,
            })
            .await
        }
        Cmd::Soak {
            database_url,
            server_bin,
            nodes,
            clients,
            worlds,
            matches,
            duration_secs,
            kill_every_secs,
            mode,
            json,
        } => {
            soak::run(soak::Opts {
                database_url,
                server_bin,
                nodes,
                clients,
                worlds,
                matches,
                duration: std::time::Duration::from_secs(duration_secs),
                kill_every: std::time::Duration::from_secs(kill_every_secs),
                mode,
                json,
            })
            .await
        }
        Cmd::Config { format } => {
            let out = match format.as_str() {
                "json" => serde_json::to_string_pretty(&cfg)?,
                _ => toml::to_string_pretty(&cfg)?,
            };
            println!("{out}");
            Ok(())
        }
    }
}
