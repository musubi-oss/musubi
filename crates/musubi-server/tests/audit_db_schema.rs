//! Schema audit regression tests (unit `db-schema`). Run with
//! `MUSUBI_TEST_DATABASE_URL=postgres://...`; skipped otherwise.
//!
//! Migration `0221_db_schema.sql` adds the indexes behind the retention sweeper's purge
//! predicates; these tests pin them (and the migration's idempotency) so a later
//! consolidation cannot silently drop them again.

use musubi_core::config::Config;
use sqlx::PgPool;

async fn db() -> Option<PgPool> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    cfg.database.max_connections = 2;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    musubi_server::db::migrate(&db).await.unwrap();
    Some(db)
}

/// `(index, table, column the predicate filters on, partial-index predicate fragment)`.
const RETENTION_INDEXES: &[(&str, &str, &str, Option<&str>)] = &[
    ("events_created_idx", "events", "created_at", None),
    ("audit_log_at_idx", "audit_log", "at", None),
    ("server_logs_at_idx", "server_logs", "at", None),
    ("server_metrics_at_idx", "server_metrics", "at", None),
    (
        "fleet_events_dispatched_at_idx",
        "fleet_events",
        "at",
        Some("dispatched"),
    ),
    (
        "webhook_deliveries_done_idx",
        "webhook_deliveries",
        "next_at",
        Some("delivered_at IS NOT NULL"),
    ),
    ("sessions_expires_idx", "sessions", "expires_at", None),
    (
        "game_sessions_terminated_idx",
        "game_sessions",
        "terminated_at",
        Some("'terminated'"),
    ),
    (
        "notifications_created_idx",
        "notifications",
        "created_at",
        None,
    ),
    (
        "chat_messages_kind_created_idx",
        "chat_messages",
        "created_at",
        None,
    ),
];

#[tokio::test]
async fn retention_predicates_are_indexed() {
    let Some(db) = db().await else { return };
    for (index, table, column, partial) in RETENTION_INDEXES {
        let def: Option<String> = sqlx::query_scalar(
            "select indexdef from pg_indexes where schemaname = 'musubi' and tablename = $1 and indexname = $2",
        )
        .bind(table)
        .bind(index)
        .fetch_optional(&db)
        .await
        .unwrap();
        let def = def.unwrap_or_else(|| panic!("index musubi.{table}.{index} missing"));
        assert!(
            def.contains(column),
            "{index} does not cover {column}: {def}"
        );
        if let Some(p) = partial {
            assert!(def.contains(p), "{index} is not partial on {p}: {def}");
        }
    }
}

/// Re-applying the audit migration against an already migrated database must be a no-op
/// (every statement is `if not exists`), so a re-run after a partial failure is safe.
#[tokio::test]
async fn audit_migration_is_idempotent() {
    let Some(db) = db().await else { return };
    let sql: String = include_str!("../migrations/0221_db_schema.sql")
        .lines()
        .filter(|l| !l.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n");
    for stmt in sql.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        sqlx::query(stmt).execute(&db).await.unwrap();
    }
    let n: i64 = sqlx::query_scalar(
        "select count(*) from pg_indexes where schemaname = 'musubi' and indexname = any($1)",
    )
    .bind(
        RETENTION_INDEXES
            .iter()
            .map(|(i, ..)| i.to_string())
            .collect::<Vec<_>>(),
    )
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(n as usize, RETENTION_INDEXES.len());
}
