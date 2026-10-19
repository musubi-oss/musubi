//! Per-account security timeline (competitive P0-27): the `account.*` events the bus already
//! records (`musubi.events`), with secrets (magic-link tokens, e-mail codes) stripped.

use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Deserialize, Default)]
pub struct TimelineQ {
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Unix seconds; only events strictly before this instant.
    pub before: Option<i64>,
    /// Comma-separated kinds (`account.authenticated,account.banned`); default: every `account.*`.
    pub kinds: Option<String>,
}

fn default_limit() -> i64 {
    50
}

/// Payload keys never shown back (they are one-time secrets delivered out of band).
const SECRET_KEYS: &[&str] = &["token", "code", "refresh_token", "access_token", "password"];

pub async fn timeline(db: &PgPool, account: Uuid, q: &TimelineQ) -> Result<Value> {
    let kinds: Option<Vec<String>> = q.kinds.as_deref().map(|k| {
        k.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });
    let before = q
        .before
        .map(|b| {
            OffsetDateTime::from_unix_timestamp(b)
                .map_err(|_| Error::InvalidArgument("before".into()))
        })
        .transpose()?;
    type Row = (Uuid, String, Value, OffsetDateTime);
    let rows: Vec<Row> = sqlx::query_as(
        "select id, kind, payload, created_at from musubi.events
         where account_id = $1 and kind like 'account.%'
           and ($2::text[] is null or kind = any($2))
           and ($3::timestamptz is null or created_at < $3)
         order by created_at desc limit $4",
    )
    .bind(account)
    .bind(kinds.as_deref())
    .bind(before)
    .bind(q.limit.clamp(1, 500))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let events: Vec<Value> = rows
        .into_iter()
        .map(|(id, kind, mut payload, at)| {
            if let Some(o) = payload.as_object_mut() {
                for k in SECRET_KEYS {
                    o.remove(*k);
                }
            }
            json!({ "id": id, "kind": kind, "payload": payload, "at": at.unix_timestamp() })
        })
        .collect();
    let next = events.last().and_then(|e| e["at"].as_i64());
    Ok(json!({ "events": events, "next_before": next }))
}
