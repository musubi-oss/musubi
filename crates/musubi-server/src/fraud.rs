//! Purchase-velocity heuristics (`[fraud]`): counts store purchases, spend per currency and IAP
//! receipts per account inside a sliding window; over the limit = a fraud flag for review
//! (`economy.fraud_flag` event) and, with `action = "block"`, a 429 on the purchase itself.

use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::app::AppState;

async fn count(s: &AppState, sql: &str, me: AccountId, extra: Option<&str>) -> Result<i64> {
    let q = sqlx::query_scalar::<_, i64>(sql)
        .bind(me.0)
        .bind(s.cfg.fraud.window_secs as i64);
    let q = if let Some(e) = extra { q.bind(e) } else { q };
    q.fetch_one(&s.db).await.map_err(Error::internal)
}

const PURCHASES: &str = "select count(*) from musubi.purchases \
    where account_id = $1 and created_at > now() - $2::bigint * interval '1 second'";
const SPEND: &str = "select coalesce(sum(amount), 0)::bigint from musubi.purchases \
    where account_id = $1 and created_at > now() - $2::bigint * interval '1 second' and currency = $3";
const IAP: &str = "select count(*) from musubi.iap_receipts \
    where account_id = $1 and created_at > now() - $2::bigint * interval '1 second'";

/// Before a store purchase in `currency`.
pub async fn check_store(s: &AppState, me: AccountId, currency: &str) -> Result<()> {
    let f = &s.cfg.fraud;
    if !f.enabled {
        return Ok(());
    }
    if f.max_purchases > 0 {
        let n = count(s, PURCHASES, me, None).await?;
        if n >= f.max_purchases as i64 {
            trip(s, me, "purchases", n, f.max_purchases as i64).await?;
        }
    }
    if let Some(limit) = f.max_spend.get(currency) {
        let n = count(s, SPEND, me, Some(currency)).await?;
        if n >= *limit {
            trip(s, me, &format!("spend:{currency}"), n, *limit).await?;
        }
    }
    Ok(())
}

/// Before an IAP receipt is redeemed.
pub async fn check_iap(s: &AppState, me: AccountId) -> Result<()> {
    let f = &s.cfg.fraud;
    if !f.enabled || f.max_iap == 0 {
        return Ok(());
    }
    let n = count(s, IAP, me, None).await?;
    if n >= f.max_iap as i64 {
        trip(s, me, "iap", n, f.max_iap as i64).await?;
    }
    Ok(())
}

async fn trip(s: &AppState, me: AccountId, rule: &str, n: i64, limit: i64) -> Result<()> {
    let block = s.cfg.fraud.action == "block";
    let fresh = sqlx::query(
        "insert into musubi.fraud_flags (id, account_id, rule, count, \"limit\", blocked)
         select $1, $2, $3, $4, $5, $6
          where not exists (select 1 from musubi.fraud_flags
                             where account_id = $2 and rule = $3 and resolved_at is null)",
    )
    .bind(Uuid::now_v7())
    .bind(me.0)
    .bind(rule)
    .bind(n)
    .bind(limit)
    .bind(block)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if fresh > 0 {
        s.events
            .emit(
                "economy.fraud_flag",
                Some(me),
                json!({ "rule": rule, "count": n, "limit": limit, "blocked": block }),
            )
            .await;
    }
    if block {
        return Err(Error::ResourceExhausted(format!(
            "purchase velocity limit ({rule}: {n} in {}s)",
            s.cfg.fraud.window_secs
        )));
    }
    Ok(())
}

fn flag_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "account_id": r.get::<Uuid, _>("account_id"),
        "rule": r.get::<String, _>("rule"),
        "count": r.get::<i64, _>("count"),
        "limit": r.get::<i64, _>("limit"),
        "blocked": r.get::<bool, _>("blocked"),
        "created_at": r.get::<time::OffsetDateTime, _>("created_at").unix_timestamp(),
        "resolved_at": r.get::<Option<time::OffsetDateTime>, _>("resolved_at").map(|t| t.unix_timestamp()),
    })
}

/// Live counters against the configured limits plus the account's flags.
pub async fn status(s: &AppState, me: AccountId) -> Result<Value> {
    let f = &s.cfg.fraud;
    let purchases = count(s, PURCHASES, me, None).await?;
    let iap = count(s, IAP, me, None).await?;
    let mut spend = serde_json::Map::new();
    for (cur, limit) in &f.max_spend {
        let n = count(s, SPEND, me, Some(cur)).await?;
        spend.insert(cur.clone(), json!({ "spent": n, "limit": limit }));
    }
    let flags = sqlx::query(
        "select * from musubi.fraud_flags where account_id = $1 order by created_at desc limit 50",
    )
    .bind(me.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({
        "enabled": f.enabled,
        "window_secs": f.window_secs,
        "action": f.action,
        "purchases": { "count": purchases, "limit": f.max_purchases },
        "iap": { "count": iap, "limit": f.max_iap },
        "spend": spend,
        "flags": flags.iter().map(flag_json).collect::<Vec<_>>(),
    }))
}

pub async fn open_flags(s: &AppState, limit: i64) -> Result<Value> {
    let rows = sqlx::query(
        "select * from musubi.fraud_flags where resolved_at is null order by created_at desc limit $1",
    )
    .bind(limit.clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({ "flags": rows.iter().map(flag_json).collect::<Vec<_>>() }))
}

pub async fn resolve(s: &AppState, id: Uuid) -> Result<Value> {
    let row = sqlx::query(
        "update musubi.fraud_flags set resolved_at = now() where id = $1 and resolved_at is null returning *",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound("fraud flag".into()))?;
    Ok(flag_json(&row))
}
