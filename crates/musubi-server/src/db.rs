use musubi_core::config::DatabaseConfig;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tracing::info;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(cfg: &DatabaseConfig) -> anyhow::Result<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        // idle connections go back to the server: processes that stop using the pool (drained
        // nodes, finished tests) should not pin their slots until exit
        .idle_timeout(std::time::Duration::from_secs(10))
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&cfg.url)
        .await?;
    Ok(pool)
}

pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    MIGRATOR.run(pool).await?;
    info!("migrations applied");
    Ok(())
}
