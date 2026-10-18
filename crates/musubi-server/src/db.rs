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

/// Apply pending migrations. The role is usually called `musubi` like the schema, and with
/// Postgres' default `search_path` (`"$user", public`) an unqualified `_sqlx_migrations`
/// resolves to `musubi._sqlx_migrations` as soon as the schema exists — a second process would
/// then start from an empty ledger and re-run every migration against a filled database. The
/// ledger is pinned to `public`; a stray `musubi._sqlx_migrations` is merged into it.
pub async fn migrate(pool: &PgPool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    sqlx::query("set search_path to public")
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "do $$ begin
           if to_regclass('musubi._sqlx_migrations') is not null then
             if to_regclass('public._sqlx_migrations') is null then
               alter table musubi._sqlx_migrations set schema public;
             else
               insert into public._sqlx_migrations
                 select m.* from musubi._sqlx_migrations m
                  where not exists (select 1 from public._sqlx_migrations p where p.version = m.version);
               drop table musubi._sqlx_migrations;
             end if;
           end if;
         end $$",
    )
    .execute(&mut *conn)
    .await?;
    MIGRATOR.run(&mut *conn).await?;
    info!("migrations applied");
    Ok(())
}
