//! Limited grants (plan 5.11): per-account caps on server-side grants of a currency, an item or
//! a reward table within a sliding window. Every server grant path (HTTP server routes and the
//! module host ops) calls `check` before and `record` after; a cap is a hard `ResourceExhausted`.
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub const KINDS: &[&str] = &["currency", "item", "reward_table"];

/// Reject the grant if `amount` more of `kind:key` would exceed the account's cap in the window.
pub async fn check(
    db: &PgPool,
    account: AccountId,
    kind: &str,
    key: &str,
    amount: i64,
) -> Result<()> {
    if amount <= 0 {
        return Ok(());
    }
    let limit: Option<(i32, i64)> = sqlx::query_as(
        "select window_secs, max_per_account from musubi.grant_limits where kind = $1 and key = $2",
    )
    .bind(kind)
    .bind(key)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?;
    let Some((window, max)) = limit else {
        return Ok(());
    };
    let used: i64 = sqlx::query_scalar(
        "select coalesce(sum(amount), 0) from musubi.grant_usage
         where account_id = $1 and kind = $2 and key = $3 and at > now() - make_interval(secs => $4)",
    )
    .bind(account.0)
    .bind(kind)
    .bind(key)
    .bind(window as f64)
    .fetch_one(db)
    .await
    .map_err(Error::internal)?;
    if used + amount > max {
        return Err(Error::ResourceExhausted(format!(
            "grant limit for {kind} {key}: {max} per {window}s (used {used}, asked {amount})"
        )));
    }
    Ok(())
}

pub async fn record(
    db: &PgPool,
    account: AccountId,
    kind: &str,
    key: &str,
    amount: i64,
) -> Result<()> {
    if amount <= 0 {
        return Ok(());
    }
    // only bookkeep what is actually limited, so the table stays small
    let limited: bool = sqlx::query_scalar(
        "select exists(select 1 from musubi.grant_limits where kind = $1 and key = $2)",
    )
    .bind(kind)
    .bind(key)
    .fetch_one(db)
    .await
    .map_err(Error::internal)?;
    if !limited {
        return Ok(());
    }
    sqlx::query("insert into musubi.grant_usage (id, account_id, kind, key, amount) values ($1, $2, $3, $4, $5)")
        .bind(Uuid::now_v7())
        .bind(account.0)
        .bind(kind)
        .bind(key)
        .bind(amount)
        .execute(db)
        .await
        .map_err(Error::internal)?;
    Ok(())
}

pub async fn list(db: &PgPool) -> Result<Value> {
    let rows: Vec<(String, String, i32, i64)> = sqlx::query_as(
        "select kind, key, window_secs, max_per_account from musubi.grant_limits order by kind, key",
    )
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(json!(rows
        .into_iter()
        .map(|(k, key, w, m)| json!({ "kind": k, "key": key, "window_secs": w, "max_per_account": m }))
        .collect::<Vec<_>>()))
}

pub async fn upsert(db: &PgPool, kind: &str, key: &str, window_secs: i32, max: i64) -> Result<()> {
    if !KINDS.contains(&kind) {
        return Err(Error::InvalidArgument(format!(
            "kind must be one of {KINDS:?}"
        )));
    }
    if key.is_empty() || key.len() > 128 || window_secs <= 0 || max <= 0 {
        return Err(Error::InvalidArgument(
            "key, window_secs > 0 and max_per_account > 0 required".into(),
        ));
    }
    sqlx::query(
        "insert into musubi.grant_limits (kind, key, window_secs, max_per_account) values ($1, $2, $3, $4)
         on conflict (kind, key) do update set window_secs = excluded.window_secs, max_per_account = excluded.max_per_account",
    )
    .bind(kind)
    .bind(key)
    .bind(window_secs)
    .bind(max)
    .execute(db)
    .await
    .map_err(Error::internal)?;
    Ok(())
}

pub async fn delete(db: &PgPool, kind: &str, key: &str) -> Result<bool> {
    let n = sqlx::query("delete from musubi.grant_limits where kind = $1 and key = $2")
        .bind(kind)
        .bind(key)
        .execute(db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(n > 0)
}

/// Recent limited grants of one account (moderator view).
pub async fn usage(db: &PgPool, account: AccountId) -> Result<Value> {
    let rows: Vec<(String, String, i64, time::OffsetDateTime)> = sqlx::query_as(
        "select kind, key, amount, at from musubi.grant_usage where account_id = $1 order by at desc limit 200",
    )
    .bind(account.0)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(json!(rows
        .into_iter()
        .map(
            |(k, key, a, at)| json!({ "kind": k, "key": key, "amount": a,
            "at": at.format(&time::format_description::well_known::Rfc3339).ok() })
        )
        .collect::<Vec<_>>()))
}
