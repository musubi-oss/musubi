//! `musubi` — developer CLI.

use std::path::PathBuf;

mod bench;

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
            if cfg.log.filter == "info" {
                cfg.log.filter = "info,musubi=debug,tower_http=debug".into();
            }
            musubi_server::init_tracing(&cfg);
            musubi_server::run(cfg).await
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
