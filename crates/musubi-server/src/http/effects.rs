//! Timed status effects (brainCloud parity) on top of the item-effect table `player_effects`:
//! a named buff/debuff/penalty on an account with an optional expiry, a stack count, a JSON
//! `value` (`{"xp_mult": 2}`) and a `source`. Item use applies effects from the catalogue
//! (`items_lifecycle`); here the server grants them directly (`PUT /v1/server/effects/{account}/{key}`
//! with `mode: replace|extend|stack`), finds who has one (`GET /v1/server/effects?key=`), the
//! client reads one (`GET /v1/effects/{key}`), and the leader sweeps expired rows every second
//! emitting `effect.expired` so game servers/modules can react.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

const MAX_DURATION_SECS: i64 = 366 * 86_400;
const MAX_STACKS: i64 = 10_000;
const MAX_VALUE_BYTES: usize = 16 * 1024;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/server/effects/{account_id}/{key}",
            axum::routing::put(server_put),
        )
        .route(
            "/v1/server/effects/{account_id}",
            axum::routing::delete(server_clear),
        )
        .route("/v1/server/effects", get(server_search))
        .route("/v1/server/effects/sweep", post(sweep_now))
        .route("/v1/effects/{key}", get(my_effect))
}

type Row = (
    Uuid,
    String,
    Value,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
    i32,
    Option<String>,
);

const COLS: &str = "account_id, key, value, item_id, applied_at, expires_at, stacks, source";

fn doc(
    (account_id, key, value, item_id, applied_at, expires_at, stacks, source): &Row,
    now: OffsetDateTime,
) -> Value {
    json!({
        "account_id": account_id, "key": key, "value": value, "item_id": item_id,
        "stacks": stacks, "source": source,
        "applied_at": applied_at.unix_timestamp(),
        "expires_at": expires_at.map(|t| t.unix_timestamp()),
        "seconds_left": expires_at.map(|t| (t - now).whole_seconds().max(0)),
        "active": expires_at.is_none_or(|t| t > now),
    })
}

fn valid_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.len() > 64
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
    {
        return Err(Error::InvalidArgument(
            "effect key: [A-Za-z0-9_.:-]{1,64}".into(),
        ));
    }
    Ok(())
}

async fn load(s: &AppState, account: AccountId, key: &str) -> Result<Option<Row>> {
    sqlx::query_as::<_, Row>(&format!(
        "select {COLS} from musubi.player_effects where account_id = $1 and key = $2"
    ))
    .bind(account.0)
    .bind(key)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PutEffect {
    /// Seconds from now (added to the current expiry with `mode: extend`); omit both for permanent.
    duration_secs: Option<i64>,
    /// Absolute unix-seconds expiry (alternative to `duration_secs`).
    expires_at: Option<i64>,
    value: Option<Value>,
    stacks: Option<i64>,
    max_stacks: Option<i64>,
    source: Option<String>,
    mode: Option<String>,
}

async fn server_put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((account_id, key)): Path<(Uuid, String)>,
    Json(r): Json<PutEffect>,
) -> Result<Json<Value>, ApiError> {
    let account = AccountId(account_id);
    valid_key(&key)?;
    let mode = r.mode.as_deref().unwrap_or("replace");
    if !matches!(mode, "replace" | "extend" | "stack") {
        return Err(Error::InvalidArgument("mode: replace | extend | stack".into()).into());
    }
    if r.duration_secs.is_some() && r.expires_at.is_some() {
        return Err(
            Error::InvalidArgument("give duration_secs or expires_at, not both".into()).into(),
        );
    }
    if r.duration_secs
        .is_some_and(|d| d <= 0 || d > MAX_DURATION_SECS)
    {
        return Err(Error::InvalidArgument("duration_secs: 1..=366 days".into()).into());
    }
    if r.stacks.is_some_and(|n| n <= 0 || n > MAX_STACKS)
        || r.max_stacks.is_some_and(|n| n <= 0 || n > MAX_STACKS)
    {
        return Err(Error::InvalidArgument("stacks/max_stacks: 1..=10000".into()).into());
    }
    if r.value
        .as_ref()
        .is_some_and(|d| d.to_string().len() > MAX_VALUE_BYTES)
    {
        return Err(Error::InvalidArgument("value: at most 16 KiB".into()).into());
    }
    if r.source.as_ref().is_some_and(|x| x.len() > 128) {
        return Err(Error::InvalidArgument("source: at most 128 chars".into()).into());
    }
    let now = OffsetDateTime::now_utc();
    let now_s = now.unix_timestamp();
    if r.expires_at
        .is_some_and(|e| e <= now_s || e > now_s + MAX_DURATION_SECS)
    {
        return Err(
            Error::InvalidArgument("expires_at: in the future, at most 366 days".into()).into(),
        );
    }
    let at = |secs: i64| OffsetDateTime::from_unix_timestamp(secs).unwrap_or(now);
    let existing = load(&s, account, &key).await?;
    let live = existing.as_ref().filter(|e| e.5.is_none_or(|x| x > now));
    let fresh = || match (r.duration_secs, r.expires_at) {
        (Some(d), _) => Some(now + time::Duration::seconds(d)),
        (None, Some(e)) => Some(at(e)),
        (None, None) => None,
    };
    let (stacks, expires_at, value, source, item_id, applied_at, event) = match (mode, live) {
        ("extend", Some(e)) => {
            let exp = match (r.duration_secs, r.expires_at) {
                (Some(d), _) => Some(e.5.map_or(now, |x| x.max(now)) + time::Duration::seconds(d))
                    .map(|x| x.min(now + time::Duration::seconds(MAX_DURATION_SECS))),
                (None, Some(x)) => Some(at(x)),
                (None, None) => None,
            };
            (
                r.stacks.map(|n| n as i32).unwrap_or(e.6),
                exp,
                r.value.clone().unwrap_or_else(|| e.2.clone()),
                r.source.clone().or_else(|| e.7.clone()),
                e.3.clone(),
                e.4,
                "effect.extended",
            )
        }
        ("stack", Some(e)) => {
            let cap = r.max_stacks.unwrap_or(MAX_STACKS);
            let n = (e.6 as i64 + r.stacks.unwrap_or(1)).clamp(1, cap) as i32;
            (
                n,
                if r.duration_secs.is_some() || r.expires_at.is_some() {
                    fresh()
                } else {
                    e.5
                },
                r.value.clone().unwrap_or_else(|| e.2.clone()),
                r.source.clone().or_else(|| e.7.clone()),
                e.3.clone(),
                e.4,
                "effect.stacked",
            )
        }
        _ => (
            r.stacks
                .unwrap_or(1)
                .min(r.max_stacks.unwrap_or(MAX_STACKS)) as i32,
            fresh(),
            r.value.clone().unwrap_or(Value::Bool(true)),
            r.source.clone(),
            None,
            now,
            if existing.is_some() {
                "effect.replaced"
            } else {
                "effect.added"
            },
        ),
    };
    let row = sqlx::query_as::<_, Row>(&format!(
        "insert into musubi.player_effects ({COLS}) values ($1, $2, $3, $4, $5, $6, $7, $8) \
         on conflict (account_id, key) do update set value = excluded.value, item_id = excluded.item_id, \
         applied_at = excluded.applied_at, expires_at = excluded.expires_at, stacks = excluded.stacks, \
         source = excluded.source returning {COLS}"
    ))
    .bind(account.0)
    .bind(&key)
    .bind(&value)
    .bind(&item_id)
    .bind(applied_at)
    .bind(expires_at)
    .bind(stacks)
    .bind(&source)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let d = doc(&row, now);
    s.events
        .emit(
            event,
            Some(account),
            json!({ "key": key, "stacks": stacks, "expires_at": d["expires_at"], "source": source }),
        )
        .await;
    Ok(Json(d))
}

async fn server_clear(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(account_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.player_effects where account_id = $1")
        .bind(account_id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n > 0 {
        s.events
            .emit(
                "effect.cleared",
                Some(AccountId(account_id)),
                json!({ "removed": n }),
            )
            .await;
    }
    Ok(Json(json!({ "removed": n })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SearchQ {
    key: Option<String>,
    include_expired: bool,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Who has effect `key` (ops: "everyone with `xp_boost`", "all shadow-muted").
async fn server_search(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<SearchQ>,
) -> Result<Json<Value>, ApiError> {
    let Some(key) = q.key else {
        return Err(Error::InvalidArgument("key is required".into()).into());
    };
    valid_key(&key)?;
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query_as::<_, Row>(&format!(
        "select {COLS} from musubi.player_effects where key = $1 \
         and ($2 or expires_at is null or expires_at > $3) order by account_id limit $4 offset $5"
    ))
    .bind(&key)
    .bind(q.include_expired)
    .bind(now)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .bind(q.offset.unwrap_or(0).max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "effects": rows.iter().map(|r| doc(r, now)).collect::<Vec<_>>() }),
    ))
}

/// Remove expired effects and emit `effect.expired` for each; the leader runs this every second.
pub async fn sweep(s: &AppState) -> Result<usize> {
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query_as::<_, Row>(&format!(
        "delete from musubi.player_effects where expires_at is not null and expires_at <= $1 returning {COLS}"
    ))
    .bind(now)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    for r in &rows {
        s.events
            .emit(
                "effect.expired",
                Some(AccountId(r.0)),
                json!({ "key": r.1, "value": r.2, "item_id": r.3, "stacks": r.6, "source": r.7 }),
            )
            .await;
    }
    Ok(rows.len())
}

async fn sweep_now(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "expired": sweep(&s).await? })))
}

async fn my_effect(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let now = OffsetDateTime::now_utc();
    match load(&s, me.account_id, &key).await? {
        Some(r) if r.5.is_none_or(|e| e > now) => Ok(Json(doc(&r, now))),
        _ => Err(Error::NotFound(format!("effect {key}")).into()),
    }
}
