//! Limited grants (plan 5.11): per-account caps on server-side grants of a currency, an item or
//! a reward table within a sliding window. Every server grant path (HTTP server routes and the
//! module host ops) calls `check` before the grant (it books the amount atomically) and `release`
//! when the grant then fails; a cap is a hard `ResourceExhausted`.
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub const KINDS: &[&str] = &["currency", "item", "reward_table"];

/// A grant admitted by [`check`]: the usage row it booked, or nothing when `kind:key` is not
/// limited. Hand it to [`release`] when the grant itself failed so the budget is given back.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reservation {
    id: Option<Uuid>,
}

/// Reject the grant if `amount` more of `kind:key` would exceed the account's cap in the window;
/// otherwise book `amount` atomically (per-account advisory lock + sum + insert in one
/// transaction), so concurrent grants cannot each pass the check and together overshoot the cap.
pub async fn check(
    db: &PgPool,
    account: AccountId,
    kind: &str,
    key: &str,
    amount: i64,
) -> Result<Reservation> {
    if amount <= 0 {
        return Ok(Reservation::default());
    }
    let mut tx = db.begin().await.map_err(Error::internal)?;
    let limit: Option<(i32, i64)> = sqlx::query_as(
        "select window_secs, max_per_account from musubi.grant_limits where kind = $1 and key = $2",
    )
    .bind(kind)
    .bind(key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let Some((window, max)) = limit else {
        return Ok(Reservation::default());
    };
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("grant_limit:{kind}:{key}:{}", account.0))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    let used: i64 = sqlx::query_scalar(
        "select coalesce(sum(amount), 0)::bigint from musubi.grant_usage
         where account_id = $1 and kind = $2 and key = $3 and at > now() - make_interval(secs => $4)",
    )
    .bind(account.0)
    .bind(kind)
    .bind(key)
    .bind(window as f64)
    .fetch_one(&mut *tx)
    .await
    .map_err(Error::internal)?;
    if used.checked_add(amount).is_none_or(|total| total > max) {
        return Err(Error::ResourceExhausted(format!(
            "grant limit for {kind} {key}: {max} per {window}s (used {used}, asked {amount})"
        )));
    }
    let id = Uuid::now_v7();
    sqlx::query("insert into musubi.grant_usage (id, account_id, kind, key, amount) values ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(account.0)
        .bind(kind)
        .bind(key)
        .bind(amount)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    Ok(Reservation { id: Some(id) })
}

/// Give a booked grant back (the grant failed or was an idempotent replay).
pub async fn release(db: &PgPool, r: &Reservation) -> Result<()> {
    let Some(id) = r.id else { return Ok(()) };
    sqlx::query("delete from musubi.grant_usage where id = $1")
        .bind(id)
        .execute(db)
        .await
        .map_err(Error::internal)?;
    Ok(())
}

/// Kept for the existing call sites: usage is booked by [`check`] itself now, so a separate
/// record step would double-count.
pub async fn record(
    _db: &PgPool,
    _account: AccountId,
    _kind: &str,
    _key: &str,
    _amount: i64,
) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> Option<PgPool> {
        let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(8)
            .connect(&url)
            .await
            .unwrap();
        crate::db::migrate(&db).await.unwrap();
        Some(db)
    }

    /// 20 concurrent grants of 1 against a cap of 5: exactly 5 may pass (the old
    /// check-then-record pair let all of them through).
    #[tokio::test]
    async fn concurrent_checks_never_overshoot_the_cap() {
        let Some(db) = pool().await else { return };
        let key = format!("audit-{}", Uuid::now_v7().simple());
        upsert(&db, "currency", &key, 3600, 5).await.unwrap();
        let acc = AccountId(Uuid::now_v7());
        let mut tasks = Vec::new();
        for _ in 0..20 {
            let (db, key) = (db.clone(), key.clone());
            tasks.push(tokio::spawn(async move {
                check(&db, acc, "currency", &key, 1).await
            }));
        }
        let mut ok = 0;
        let mut exhausted = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => ok += 1,
                Err(Error::ResourceExhausted(_)) => exhausted += 1,
                Err(e) => panic!("unexpected {e}"),
            }
        }
        assert_eq!((ok, exhausted), (5, 15));
        let used: i64 = sqlx::query_scalar(
            "select coalesce(sum(amount), 0)::bigint from musubi.grant_usage where account_id = $1 and key = $2",
        )
        .bind(acc.0)
        .bind(&key)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(used, 5);
        delete(&db, "currency", &key).await.unwrap();
    }

    #[tokio::test]
    async fn release_gives_the_budget_back_and_overflow_is_rejected() {
        let Some(db) = pool().await else { return };
        let key = format!("audit-{}", Uuid::now_v7().simple());
        upsert(&db, "item", &key, 3600, 2).await.unwrap();
        let acc = AccountId(Uuid::now_v7());
        let r = check(&db, acc, "item", &key, 2).await.unwrap();
        assert!(matches!(
            check(&db, acc, "item", &key, 1).await,
            Err(Error::ResourceExhausted(_))
        ));
        release(&db, &r).await.unwrap();
        check(&db, acc, "item", &key, 2).await.unwrap();
        // unlimited keys book nothing and release is a no-op
        let none = check(&db, acc, "item", "audit-unlimited", 1).await.unwrap();
        assert!(none.id.is_none());
        release(&db, &none).await.unwrap();
        // used + amount must not wrap around into a pass
        upsert(&db, "item", &key, 3600, i64::MAX).await.unwrap();
        let acc2 = AccountId(Uuid::now_v7());
        check(&db, acc2, "item", &key, i64::MAX - 1).await.unwrap();
        assert!(matches!(
            check(&db, acc2, "item", &key, i64::MAX).await,
            Err(Error::ResourceExhausted(_))
        ));
        delete(&db, "item", &key).await.unwrap();
    }
}
