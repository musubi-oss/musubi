//! Background clean-up of time-series tables, run by the `retention` lease holder every
//! `retention.interval_secs` and on `POST /v1/admin/retention/run`. Every window is in days;
//! `0` keeps rows forever. Journals of matches/worlds that no longer exist are always dropped.

use musubi_core::config::RetentionConfig;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;

/// Batched deletes keep locks short on big tables.
const BATCH: i64 = 5000;

async fn purge(db: &PgPool, table: &str, predicate: &str, days: u64) -> Result<u64> {
    if days == 0 {
        return Ok(0);
    }
    let sql = format!(
        "delete from {table} where ctid in (select ctid from {table} where {predicate} limit {BATCH})"
    );
    let mut total = 0u64;
    loop {
        let n = sqlx::query(&sql)
            .bind(days as f64)
            .execute(db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        total += n;
        if n < BATCH as u64 {
            return Ok(total);
        }
    }
}

/// One pass; returns rows removed per table.
pub async fn run(db: &PgPool, cfg: &RetentionConfig) -> Result<Value> {
    let age = |col: &str| format!("{col} < now() - make_interval(days => $1::int)");
    let events = purge(db, "musubi.events", &age("created_at"), cfg.events_days).await?;
    let server_logs = purge(db, "musubi.server_logs", &age("at"), cfg.server_logs_days).await?;
    let mm_log = purge(
        db,
        "musubi.matchmaker_ticket_log",
        &age("finished_at"),
        cfg.matchmaker_log_days,
    )
    .await?
        + purge(
            db,
            "musubi.matchmaker_matches",
            &age("created_at"),
            cfg.matchmaker_log_days,
        )
        .await?;
    let analytics_events = purge(
        db,
        "musubi.analytics_events",
        &age("at"),
        cfg.analytics_events_days,
    )
    .await?;
    let deliveries = purge(
        db,
        "musubi.webhook_deliveries",
        &format!("(delivered_at is not null or dead) and {}", age("next_at")),
        cfg.deliveries_days,
    )
    .await?;
    let audit = purge(db, "musubi.audit_log", &age("at"), cfg.audit_days).await?;
    let game_sessions = purge(
        db,
        "musubi.game_sessions",
        &format!("state = 'terminated' and {}", age("terminated_at")),
        cfg.game_sessions_days,
    )
    .await?;
    let mut notifications = purge(
        db,
        "musubi.notifications",
        &age("created_at"),
        cfg.notifications_days,
    )
    .await?;
    notifications += sqlx::query("delete from musubi.notifications where expires_at <= now()")
        .execute(db)
        .await
        .map_err(musubi_core::Error::internal)?
        .rows_affected();
    let mut chat = 0u64;
    for (kind, days) in [
        (2, cfg.chat_room_days),
        (4, cfg.chat_group_days),
        (3, cfg.chat_direct_days),
    ] {
        let days = if cfg.chat_days > 0 {
            cfg.chat_days
        } else {
            days
        };
        chat += purge(
            db,
            "musubi.chat_messages",
            &format!("kind = {kind} and {}", age("created_at")),
            days,
        )
        .await?;
    }
    let idempotency = purge(db, "musubi.idempotency", &age("created_at"), 1).await?;
    let resumable = crate::realtime::resume::ResumeStore::purge_expired(db).await?;
    let storage = sqlx::query("delete from musubi.storage_objects where expires_at <= now()")
        .execute(db)
        .await
        .map_err(musubi_core::Error::internal)?
        .rows_affected();
    let sessions = purge(
        db,
        "musubi.sessions",
        &format!(
            "(revoked_at is not null or expires_at < now()) and {}",
            age("expires_at")
        ),
        cfg.sessions_days,
    )
    .await?;
    let world_history = purge(
        db,
        "musubi.world_history",
        &age("taken_at"),
        cfg.world_history_days,
    )
    .await?;
    let replays = purge(
        db,
        "musubi.match_recordings",
        &age("started_at"),
        cfg.replay_days,
    )
    .await?;
    let match_journal = sqlx::query(
        "delete from musubi.match_journal j where not exists (select 1 from musubi.match_snapshots s where s.match_id = j.match_id)",
    )
    .execute(db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    let world_journal = sqlx::query(
        "delete from musubi.world_journal j where not exists (select 1 from musubi.worlds w where w.name = j.name)",
    )
    .execute(db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    Ok(json!({
        "events": events, "server_logs": server_logs, "matchmaker_log": mm_log, "analytics_events": analytics_events, "webhook_deliveries": deliveries, "audit_log": audit, "game_sessions": game_sessions,
        "notifications": notifications, "chat_messages": chat, "sessions": sessions, "idempotency": idempotency, "storage_objects": storage, "resumable_sessions": resumable,
        "match_journal": match_journal, "world_journal": world_journal, "world_history": world_history, "replays": replays,
    }))
}
