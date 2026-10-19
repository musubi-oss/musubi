//! QA clocks (Satori parity "bind identity to a simulated date"): a tester's account is pinned to
//! a fixed instant (`at`, the clock stands still — reproducible screenshots of a future event) or
//! shifted by `offset_secs` (time keeps flowing from a future/past point). Live-ops reads for that
//! account — live events and their phases, flag rule windows, storefront offers, news — and
//! `GET /v1/time` use the simulated clock; everything else (tokens, timers, billing) stays real.
//! Clocks expire by themselves (`expires_in_secs`, default 24 h) so a forgotten tester never ships
//! with a bent clock. Resolution is cached 15 s per account in-process.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

const CACHE_SECS: u64 = 15;
const MAX_EXPIRES_SECS: i64 = 30 * 86_400;
const MAX_SHIFT_SECS: i64 = 20 * 366 * 86_400;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/time", get(server_time))
        .route(
            "/v1/server/accounts/{id}/clock",
            get(get_clock).put(put_clock).delete(delete_clock),
        )
        .route("/v1/server/clocks", get(list_clocks))
}

#[derive(Clone, Debug)]
struct Clock {
    mode: String,
    at: Option<i64>,
    offset_secs: Option<i64>,
    reason: Option<String>,
    expires_at: Option<i64>,
    created_at: i64,
}

type Row = (
    Uuid,
    String,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<OffsetDateTime>,
    OffsetDateTime,
);

const COLS: &str = "account_id, mode, at, offset_secs, reason, expires_at, created_at";

fn clock_of(r: &Row) -> Clock {
    Clock {
        mode: r.1.clone(),
        at: r.2,
        offset_secs: r.3,
        reason: r.4.clone(),
        expires_at: r.5.map(|t| t.unix_timestamp()),
        created_at: r.6.unix_timestamp(),
    }
}

fn real_now() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

impl Clock {
    fn live(&self, real: i64) -> bool {
        self.expires_at.is_none_or(|e| e > real)
    }
    fn now(&self, real: i64) -> i64 {
        match self.mode.as_str() {
            "fixed" => self.at.unwrap_or(real),
            _ => real + self.offset_secs.unwrap_or(0),
        }
    }
    fn doc(&self, account: Uuid, real: i64) -> Value {
        json!({
            "account_id": account, "mode": self.mode, "at": self.at,
            "offset_secs": self.offset_secs, "reason": self.reason,
            "expires_at": self.expires_at, "created_at": self.created_at,
            "now": self.now(real), "real_now": real, "active": self.live(real),
        })
    }
}

type Cache = RwLock<HashMap<Uuid, (Instant, Option<Clock>)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn forget(account: Uuid) {
    cache().write().unwrap().remove(&account);
}

async fn load(s: &AppState, account: Uuid) -> Result<Option<Clock>> {
    if let Some((at, c)) = cache().read().unwrap().get(&account) {
        if at.elapsed().as_secs() < CACHE_SECS {
            return Ok(c.clone());
        }
    }
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.account_clocks where account_id = $1"
    ))
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let c = row.as_ref().map(clock_of);
    cache()
        .write()
        .unwrap()
        .insert(account, (Instant::now(), c.clone()));
    Ok(c)
}

/// Unix seconds "now" as seen by this account: the simulated clock when a live QA clock is
/// bound, the real clock otherwise. Never fails the caller: a storage hiccup falls back to real.
pub async fn now_for(s: &AppState, account: AccountId) -> i64 {
    let real = real_now();
    match load(s, account.0).await {
        Ok(Some(c)) if c.live(real) => c.now(real),
        Ok(_) => real,
        Err(e) => {
            tracing::warn!(error = %e, "qa clock lookup failed; using real time");
            real
        }
    }
}

/// `GET /v1/time` — authoritative server clock for clients (countdowns, offline-progress checks);
/// with a bearer token of an account under a QA clock it reports the simulated time too.
async fn server_time(State(s): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let now = OffsetDateTime::now_utc();
    let mut v = json!({
        "now": now.unix_timestamp(),
        "now_ms": (now.unix_timestamp_nanos() / 1_000_000) as i64,
        "iso": now.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
    });
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.strip_prefix("Bearer ")
                .or_else(|| h.strip_prefix("bearer "))
        });
    if let Some(auth) = token.and_then(|t| s.auth.verify_access(t).ok()) {
        let real = now.unix_timestamp();
        if let Ok(Some(c)) = load(&s, auth.account_id.0).await {
            if c.live(real) {
                v["simulated"] = json!(true);
                v["real_now"] = json!(real);
                v["now"] = json!(c.now(real));
                v["now_ms"] = json!(c.now(real) * 1000 + (now.millisecond() as i64));
                v["iso"] = json!(OffsetDateTime::from_unix_timestamp(c.now(real))
                    .ok()
                    .and_then(|t| t
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok())
                    .unwrap_or_default());
            }
        }
    }
    Json(v)
}

fn account(id: &str) -> Result<Uuid> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("account id".into()))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PutClock {
    at: Option<i64>,
    offset_secs: Option<i64>,
    expires_in_secs: Option<i64>,
    reason: Option<String>,
}

async fn put_clock(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<PutClock>,
) -> Result<Json<Value>, ApiError> {
    let account = account(&id)?;
    let real = real_now();
    let (mode, at, offset) = match (b.at, b.offset_secs) {
        (Some(_), Some(_)) => {
            return Err(Error::InvalidArgument("at and offset_secs are exclusive".into()).into())
        }
        (Some(at), None) => {
            if (at - real).abs() > MAX_SHIFT_SECS || at < 0 {
                return Err(
                    Error::InvalidArgument("at: within 20 years of the real clock".into()).into(),
                );
            }
            ("fixed", Some(at), None)
        }
        (None, Some(o)) => {
            if o == 0 || o.abs() > MAX_SHIFT_SECS {
                return Err(Error::InvalidArgument(
                    "offset_secs: non-zero, within 20 years".into(),
                )
                .into());
            }
            ("offset", None, Some(o))
        }
        (None, None) => {
            return Err(Error::InvalidArgument("at or offset_secs is required".into()).into())
        }
    };
    let ttl = b.expires_in_secs.unwrap_or(86_400);
    if !(1..=MAX_EXPIRES_SECS).contains(&ttl) {
        return Err(
            Error::InvalidArgument(format!("expires_in_secs: 1..={MAX_EXPIRES_SECS}")).into(),
        );
    }
    if b.reason.as_ref().is_some_and(|r| r.len() > 200) {
        return Err(Error::InvalidArgument("reason: at most 200 chars".into()).into());
    }
    let exists: Option<i32> =
        sqlx::query_scalar("select 1 from musubi.accounts where id = $1 and deleted_at is null")
            .bind(account)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    if exists.is_none() {
        return Err(Error::NotFound("account".into()).into());
    }
    let expires = OffsetDateTime::from_unix_timestamp(real + ttl).map_err(Error::internal)?;
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.account_clocks (account_id, mode, at, offset_secs, reason, expires_at)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (account_id) do update set mode = excluded.mode, at = excluded.at,
             offset_secs = excluded.offset_secs, reason = excluded.reason,
             expires_at = excluded.expires_at, updated_at = now()
         returning {COLS}"
    ))
    .bind(account)
    .bind(mode)
    .bind(at)
    .bind(offset)
    .bind(&b.reason)
    .bind(expires)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    forget(account);
    let c = clock_of(&row);
    s.events
        .emit(
            "qa.clock_set",
            Some(AccountId(account)),
            json!({ "mode": mode, "now": c.now(real), "expires_at": c.expires_at, "reason": b.reason }),
        )
        .await;
    Ok(Json(c.doc(account, real)))
}

async fn get_clock(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let account = account(&id)?;
    forget(account);
    match load(&s, account).await? {
        Some(c) => Ok(Json(c.doc(account, real_now()))),
        None => Err(Error::NotFound("clock".into()).into()),
    }
}

async fn delete_clock(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let account = account(&id)?;
    let n = sqlx::query("delete from musubi.account_clocks where account_id = $1")
        .bind(account)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    forget(account);
    if n > 0 {
        s.events
            .emit("qa.clock_cleared", Some(AccountId(account)), json!({}))
            .await;
    }
    Ok(Json(json!({ "removed": n > 0 })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    include_expired: bool,
    limit: Option<i64>,
}

async fn list_clocks(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.account_clocks
         where $1 or expires_at is null or expires_at > now()
         order by created_at desc limit $2"
    ))
    .bind(q.include_expired)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let real = real_now();
    let clocks: Vec<Value> = rows.iter().map(|r| clock_of(r).doc(r.0, real)).collect();
    Ok(Json(json!({ "clocks": clocks, "real_now": real })))
}
