//! Session baggage (Pragma parity): small key/value entries attached to a signed-in session
//! (its refresh family, so they survive token rotation). Three scopes: `social` — friends can
//! read it (`GET /v1/accounts/{id}/baggage`); `game` — game servers read it
//! (`GET /v1/server/accounts/{id}/baggage`); `service` — only the server sets it and it is
//! stamped as `baggage` onto every account-bound bus event, so logs / analytics / LiveOps carry
//! e.g. the build hash or A/B cohort of that session. Limits per family: key ≤ 64 chars, entry
//! value ≤ 1 KiB, 32 entries per scope, 16 KiB in total.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;

pub const SCOPES: [&str; 3] = ["social", "game", "service"];
pub const KEY_MAX: usize = 64;
pub const ENTRY_MAX_BYTES: usize = 1024;
pub const ENTRIES_PER_SCOPE: usize = 32;
pub const TOTAL_MAX_BYTES: usize = 16 * 1024;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/session/baggage", get(mine))
        .route(
            "/v1/session/baggage/{scope}",
            axum::routing::put(set_mine).delete(clear_mine),
        )
        .route("/v1/accounts/{id}/baggage", get(of_friend))
        .route("/v1/server/accounts/{id}/baggage", get(of_account))
        .route(
            "/v1/server/accounts/{id}/baggage/{scope}",
            axum::routing::put(set_account).delete(clear_account),
        )
}

#[derive(Deserialize, Default)]
struct SetBody {
    #[serde(default)]
    entries: Map<String, Value>,
}

fn scope_ok(scope: &str) -> Result<()> {
    if SCOPES.contains(&scope) {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "scope must be social | game | service".into(),
        ))
    }
}

async fn family_of(db: &PgPool, session: Uuid) -> Result<Uuid> {
    sqlx::query_scalar("select family from musubi.sessions where id = $1")
        .bind(session)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("session".into()))
}

/// Live refresh families of an account (one per signed-in device).
async fn live_families(db: &PgPool, account: AccountId) -> Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "select distinct family from musubi.sessions
         where account_id = $1 and revoked_at is null and expires_at > now()",
    )
    .bind(account.0)
    .fetch_all(db)
    .await
    .map_err(Error::internal)
}

type Row = (Uuid, String, String, Value, String, time::OffsetDateTime);

async fn rows(db: &PgPool, families: &[Uuid], scope: Option<&str>) -> Result<Vec<Row>> {
    sqlx::query_as(
        "select family, scope, key, value, set_by, updated_at from musubi.session_baggage
         where family = any($1) and ($2::text is null or scope = $2)
         order by updated_at",
    )
    .bind(families)
    .bind(scope)
    .fetch_all(db)
    .await
    .map_err(Error::internal)
}

/// `{scope: {key: value}}`; across several families the latest write of a key wins.
fn fold(rows: &[Row]) -> Value {
    let mut out: BTreeMap<&str, Map<String, Value>> = BTreeMap::new();
    for s in SCOPES {
        out.insert(s, Map::new());
    }
    for r in rows {
        out.entry(r.1.as_str())
            .or_default()
            .insert(r.2.clone(), r.3.clone());
    }
    Value::Object(
        out.into_iter()
            .map(|(k, v)| (k.to_string(), Value::Object(v)))
            .collect(),
    )
}

/// Merge `entries` into one family's scope (`null` removes) under the Pragma-style limits.
async fn apply(
    s: &AppState,
    family: Uuid,
    scope: &str,
    entries: &Map<String, Value>,
    set_by: &str,
) -> Result<Value> {
    for (k, v) in entries {
        if k.is_empty() || k.len() > KEY_MAX {
            return Err(Error::InvalidArgument(format!(
                "baggage key must be 1..={KEY_MAX} chars"
            )));
        }
        if !v.is_null() && v.to_string().len() > ENTRY_MAX_BYTES {
            return Err(Error::InvalidArgument(format!(
                "baggage_entry_limit_exceeded: `{k}` is over {ENTRY_MAX_BYTES} bytes"
            )));
        }
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let current: Vec<Row> = sqlx::query_as(
        "select family, scope, key, value, set_by, updated_at from musubi.session_baggage
         where family = $1 for update",
    )
    .bind(family)
    .fetch_all(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let mut next: BTreeMap<(String, String), Value> = current
        .iter()
        .map(|r| ((r.1.clone(), r.2.clone()), r.3.clone()))
        .collect();
    for (k, v) in entries {
        if v.is_null() {
            next.remove(&(scope.to_string(), k.clone()));
        } else {
            next.insert((scope.to_string(), k.clone()), v.clone());
        }
    }
    let per_scope = next.keys().filter(|(sc, _)| sc == scope).count();
    if per_scope > ENTRIES_PER_SCOPE {
        return Err(Error::InvalidArgument(format!(
            "baggage_entry_limit_exceeded: more than {ENTRIES_PER_SCOPE} entries in `{scope}`"
        )));
    }
    let total: usize = next
        .iter()
        .map(|((_, k), v)| k.len() + v.to_string().len())
        .sum();
    if total > TOTAL_MAX_BYTES {
        return Err(Error::InvalidArgument(format!(
            "baggage_total_limit_exceeded: {total} bytes over the {TOTAL_MAX_BYTES} byte budget"
        )));
    }
    for (k, v) in entries {
        if v.is_null() {
            sqlx::query(
                "delete from musubi.session_baggage where family = $1 and scope = $2 and key = $3",
            )
            .bind(family)
            .bind(scope)
            .bind(k)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        } else {
            sqlx::query(
                "insert into musubi.session_baggage (family, scope, key, value, set_by)
                 values ($1, $2, $3, $4, $5)
                 on conflict (family, scope, key) do update
                   set value = excluded.value, set_by = excluded.set_by, updated_at = now()",
            )
            .bind(family)
            .bind(scope)
            .bind(k)
            .bind(v)
            .bind(set_by)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
    }
    tx.commit().await.map_err(Error::internal)?;
    let scoped: Map<String, Value> = next
        .into_iter()
        .filter(|((sc, _), _)| sc == scope)
        .map(|((_, k), v)| (k, v))
        .collect();
    Ok(Value::Object(scoped))
}

async fn clear(db: &PgPool, families: &[Uuid], scope: &str) -> Result<u64> {
    Ok(
        sqlx::query("delete from musubi.session_baggage where family = any($1) and scope = $2")
            .bind(families)
            .bind(scope)
            .execute(db)
            .await
            .map_err(Error::internal)?
            .rows_affected(),
    )
}

/// `service` entries of the account's live sessions, latest write per key — stamped onto events.
pub async fn service_context(db: &PgPool, account: AccountId) -> Result<Map<String, Value>> {
    let rows: Vec<(String, Value)> = sqlx::query_as(
        "select distinct on (b.key) b.key, b.value from musubi.session_baggage b
         join musubi.sessions s on s.family = b.family
         where s.account_id = $1 and s.revoked_at is null and s.expires_at > now()
           and b.scope = 'service'
         order by b.key, b.updated_at desc",
    )
    .bind(account.0)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(rows.into_iter().collect())
}

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let family = family_of(&s.db, me.session_id.0).await?;
    let r = rows(&s.db, &[family], None).await?;
    Ok(Json(json!({
        "session_family": family, "baggage": fold(&r),
        "limits": { "key_max": KEY_MAX, "entry_max_bytes": ENTRY_MAX_BYTES,
                    "entries_per_scope": ENTRIES_PER_SCOPE, "total_max_bytes": TOTAL_MAX_BYTES },
    })))
}

async fn set_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(scope): Path<String>,
    body: Option<Json<SetBody>>,
) -> Result<Json<Value>, ApiError> {
    scope_ok(&scope)?;
    if scope == "service" {
        return Err(
            Error::PermissionDenied("service baggage is set by the server only".into()).into(),
        );
    }
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let family = family_of(&s.db, me.session_id.0).await?;
    let v = apply(&s, family, &scope, &b.entries, "client").await?;
    Ok(Json(json!({ "scope": scope, "entries": v })))
}

async fn clear_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(scope): Path<String>,
) -> Result<Json<Value>, ApiError> {
    scope_ok(&scope)?;
    if scope == "service" {
        return Err(
            Error::PermissionDenied("service baggage is set by the server only".into()).into(),
        );
    }
    let family = family_of(&s.db, me.session_id.0).await?;
    let n = clear(&s.db, &[family], &scope).await?;
    Ok(Json(json!({ "scope": scope, "removed": n })))
}

/// `GET /v1/accounts/{id}/baggage` — a friend's `social` baggage (mutual friends only).
async fn of_friend(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let friends: bool = sqlx::query_scalar(
        "select exists(select 1 from musubi.friends
                       where user_id = $1 and friend_id = $2 and state = 0)",
    )
    .bind(me.account_id.0)
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if !friends && me.account_id.0 != id {
        return Err(Error::PermissionDenied("not friends".into()).into());
    }
    let fams = live_families(&s.db, AccountId(id)).await?;
    let r = rows(&s.db, &fams, Some("social")).await?;
    Ok(Json(json!({
        "account_id": id, "online": !fams.is_empty(), "social": fold(&r)["social"],
    })))
}

/// `GET /v1/server/accounts/{id}/baggage` — every scope of every live session, folded plus
/// per-session.
async fn of_account(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.accounts.get(AccountId(id)).await?;
    let fams = live_families(&s.db, AccountId(id)).await?;
    let r = rows(&s.db, &fams, None).await?;
    let sessions: Vec<Value> = fams
        .iter()
        .map(|f| {
            let own: Vec<Row> = r.iter().filter(|x| x.0 == *f).cloned().collect();
            json!({ "session_family": f, "baggage": fold(&own) })
        })
        .collect();
    Ok(Json(json!({
        "account_id": id, "live_sessions": fams.len(), "baggage": fold(&r), "sessions": sessions,
    })))
}

/// `PUT /v1/server/accounts/{id}/baggage/{scope} {entries}` — set on every live session of the
/// account (any scope, including `service`).
async fn set_account(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, scope)): Path<(Uuid, String)>,
    body: Option<Json<SetBody>>,
) -> Result<Json<Value>, ApiError> {
    scope_ok(&scope)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    s.accounts.get(AccountId(id)).await?;
    let fams = live_families(&s.db, AccountId(id)).await?;
    if fams.is_empty() {
        return Err(Error::NotFound("live session".into()).into());
    }
    let mut last = Value::Object(Map::new());
    for f in &fams {
        last = apply(&s, *f, &scope, &b.entries, "server").await?;
    }
    if scope == "service" {
        s.events.invalidate_baggage(AccountId(id));
    }
    Ok(Json(
        json!({ "scope": scope, "sessions": fams.len(), "entries": last }),
    ))
}

async fn clear_account(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, scope)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    scope_ok(&scope)?;
    let fams = live_families(&s.db, AccountId(id)).await?;
    let n = clear(&s.db, &fams, &scope).await?;
    if scope == "service" {
        s.events.invalidate_baggage(AccountId(id));
    }
    Ok(Json(
        json!({ "scope": scope, "sessions": fams.len(), "removed": n }),
    ))
}
