//! `musubi doctor`: static config checks plus what the server would hit at startup.

use musubi_core::config::{Config, Severity};

fn line(sev: Severity, key: &str, msg: &str) {
    let tag = match sev {
        Severity::Error => "ERROR",
        Severity::Warning => "warn ",
    };
    println!("{tag}  {key:<28} {msg}");
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut errors = 0usize;
    for p in cfg.validate() {
        if p.severity == Severity::Error {
            errors += 1;
        }
        line(p.severity, &p.key, &p.message);
    }
    // data dir
    if !cfg.node.data_dir.is_empty() {
        let dir = std::path::Path::new(&cfg.node.data_dir);
        match std::fs::create_dir_all(dir).and_then(|_| std::fs::write(dir.join(".doctor"), b"ok"))
        {
            Ok(()) => {
                let _ = std::fs::remove_file(dir.join(".doctor"));
                println!(
                    "ok     node.data_dir                {} writable",
                    dir.display()
                );
            }
            Err(e) => {
                errors += 1;
                line(
                    Severity::Error,
                    "node.data_dir",
                    &format!("{}: {e}", dir.display()),
                );
            }
        }
    }
    // database + migrations
    match musubi_server::db::connect(&cfg.database).await {
        Ok(pool) => {
            let applied: i64 =
                sqlx::query_scalar("select count(*) from _sqlx_migrations where success")
                    .fetch_one(&pool)
                    .await
                    .unwrap_or(0);
            let total = musubi_server::db::MIGRATOR.migrations.len() as i64;
            if applied >= total {
                println!(
                    "ok     database                     reachable, {total} migrations applied"
                );
            } else if cfg.database.run_migrations {
                println!(
                    "ok     database                     reachable, {} pending (applied on start)",
                    total - applied
                );
            } else {
                errors += 1;
                line(
                    Severity::Error,
                    "database",
                    &format!(
                        "{} pending migrations and run_migrations = false",
                        total - applied
                    ),
                );
            }
        }
        Err(e) => {
            errors += 1;
            line(
                Severity::Error,
                "database.url",
                &format!("cannot connect: {e}"),
            );
        }
    }
    // ports: a bound port is fine (a server is running), a parse error is not
    for (key, addr) in [
        ("http.listen", cfg.http.listen.to_string()),
        ("node.bus_listen", cfg.node.bus_listen.clone()),
    ] {
        if addr.is_empty() {
            continue;
        }
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(_) => println!("ok     {key:<28} {addr} free"),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                println!("info   {key:<28} {addr} in use (server already running?)")
            }
            Err(e) => {
                errors += 1;
                line(Severity::Error, key, &format!("{addr}: {e}"));
            }
        }
    }
    if errors > 0 {
        anyhow::bail!("{errors} problem(s) would stop musubi-server");
    }
    println!("all good");
    Ok(())
}
