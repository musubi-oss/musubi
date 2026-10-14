use std::sync::Arc;

use musubi_core::config::Config;
use sqlx::PgPool;
use tokio::signal;
use tracing::info;

use crate::{db, http};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub started_at: std::time::Instant,
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    info!(node = %cfg.node.name, roles = ?cfg.node.roles, "starting");
    let db = db::connect(&cfg.database).await?;
    if cfg.database.run_migrations {
        db::migrate(&db).await?;
    }
    let state = AppState {
        cfg: Arc::new(cfg),
        db,
        started_at: std::time::Instant::now(),
    };
    let listener = tokio::net::TcpListener::bind(state.cfg.http.listen).await?;
    info!(addr = %state.cfg.http.listen, "http listening");
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    info!("stopped");
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    info!("shutdown signal received");
}
