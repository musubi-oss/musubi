//! Player statistics (5.6): server-defined typed counters (`sum` / `max` / `min` / `last`) per
//! account, sliced by an optional scope (mode, season, map …). Every definition has a visibility:
//! `public` (anyone), `friends` (the owner's friends), `private` (owner and server only).

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/stats/defs", get(defs).put(put_defs))
        .route("/v1/server/stats/update", post(update))
        .route("/v1/server/stats/top", get(top))
        .route("/v1/server/stats/{account_id}", get(server_read))
        .route("/v1/stats", get(mine))
        .route("/v1/stats/{account_id}", get(theirs))
}

const KINDS: [&str; 4] = ["sum", "max", "min", "last"];
const VISIBILITY: [&str; 3] = ["public", "friends", "private"];

#[derive(Deserialize)]
struct Def {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default = "sum")]
    kind: String,
    #[serde(default = "public")]
    visibility: String,
}

fn sum() -> String {
    "sum".into()
}
fn public() -> String {
    "public".into()
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
}

async fn defs(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "defs": load_defs(&s.db).await? })))
}

async fn load_defs(db: &PgPool) -> Result<Vec<Value>> {
    let rows: Vec<(String, String, String, String)> =
        sqlx::query_as("select id, name, kind, visibility from musubi.stat_defs order by id")
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(id, name, kind, visibility)| {
            json!({ "id": id, "name": name, "kind": kind, "visibility": visibility })
        })
        .collect())
}

#[derive(Deserialize)]
struct PutDefs {
    defs: Vec<Def>,
    /// Drop definitions (and their values) not in this list.
    #[serde(default)]
    replace: bool,
}

async fn put_defs(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<PutDefs>,
) -> Result<Json<Value>, ApiError> {
    for d in &r.defs {
        if !valid_id(&d.id) {
            return Err(Error::InvalidArgument(format!("stat id {:?}", d.id)).into());
        }
        if !KINDS.contains(&d.kind.as_str()) {
            return Err(Error::InvalidArgument("kind must be sum|max|min|last".into()).into());
        }
        if !VISIBILITY.contains(&d.visibility.as_str()) {
            return Err(
                Error::InvalidArgument("visibility must be public|friends|private".into()).into(),
            );
        }
        if d.name.chars().count() > 128 {
            return Err(Error::InvalidArgument("name longer than 128 chars".into()).into());
        }
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for d in &r.defs {
        sqlx::query(
            "insert into musubi.stat_defs (id, name, kind, visibility) values ($1, $2, $3, $4)
             on conflict (id) do update set name = excluded.name, kind = excluded.kind,
                 visibility = excluded.visibility, updated_at = now()",
        )
        .bind(&d.id)
        .bind(&d.name)
        .bind(&d.kind)
        .bind(&d.visibility)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    if r.replace {
        let keep: Vec<&str> = r.defs.iter().map(|d| d.id.as_str()).collect();
        sqlx::query("delete from musubi.stat_defs where id <> all($1)")
            .bind(&keep)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(Json(json!({ "defs": load_defs(&s.db).await? })))
}

#[derive(Deserialize)]
struct UpdateReq {
    account_id: Uuid,
    #[serde(default)]
    scope: String,
    /// `{stat_id: number}`; the number is folded in according to the definition's kind.
    stats: BTreeMap<String, f64>,
}

/// Fold `stats` into `account`'s counters under `scope` (and, when `scope` is not the global
/// slice, into the global slice too). Unknown stat ids are reported, not applied.
pub async fn apply(
    db: &PgPool,
    account: AccountId,
    scope: &str,
    stats: &BTreeMap<String, f64>,
) -> Result<Value> {
    if scope.len() > 64 {
        return Err(Error::InvalidArgument("scope longer than 64 chars".into()));
    }
    let mut applied = serde_json::Map::new();
    let mut unknown = Vec::new();
    let mut tx = db.begin().await.map_err(Error::internal)?;
    for (id, v) in stats {
        if !v.is_finite() {
            return Err(Error::InvalidArgument(format!(
                "stat {id}: not a finite number"
            )));
        }
        let kind: Option<String> =
            sqlx::query_scalar("select kind from musubi.stat_defs where id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some(kind) = kind else {
            unknown.push(id.clone());
            continue;
        };
        let fold = match kind.as_str() {
            "max" => "greatest(musubi.player_stats.value, excluded.value)",
            "min" => "least(musubi.player_stats.value, excluded.value)",
            "last" => "excluded.value",
            _ => "musubi.player_stats.value + excluded.value",
        };
        let sql = format!(
            "insert into musubi.player_stats (account_id, stat_id, scope, value, samples)
             values ($1, $2, $3, $4, 1)
             on conflict (account_id, stat_id, scope) do update
               set value = {fold}, samples = musubi.player_stats.samples + 1, updated_at = now()
             returning value, samples"
        );
        let mut scopes = vec![scope.to_string()];
        if !scope.is_empty() {
            scopes.push(String::new());
        }
        let mut out = json!({});
        for sc in &scopes {
            let (value, samples): (f64, i64) = sqlx::query_as(&sql)
                .bind(account.0)
                .bind(id)
                .bind(sc)
                .bind(v)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
            let key = if sc.is_empty() { "all" } else { "scope" };
            out[key] = json!({ "value": value, "samples": samples });
        }
        applied.insert(id.clone(), out);
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(json!({ "applied": applied, "unknown": unknown }))
}

async fn update(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<UpdateReq>,
) -> Result<Json<Value>, ApiError> {
    if r.stats.is_empty() || r.stats.len() > 64 {
        return Err(Error::InvalidArgument("1..=64 stats per update".into()).into());
    }
    Ok(Json(
        apply(&s.db, AccountId(r.account_id), &r.scope, &r.stats).await?,
    ))
}

#[derive(Deserialize, Default)]
struct ReadQuery {
    /// Only this slice; absent = every slice the account has.
    scope: Option<String>,
}

/// `visible`: which visibilities to include.
async fn read(
    db: &PgPool,
    account: AccountId,
    scope: Option<&str>,
    visible: &[&str],
) -> Result<Value> {
    let rows: Vec<(String, String, String, String, f64, i64)> = sqlx::query_as(
        "select p.stat_id, d.kind, d.visibility, p.scope, p.value, p.samples
         from musubi.player_stats p join musubi.stat_defs d on d.id = p.stat_id
         where p.account_id = $1 and ($2::text is null or p.scope = $2) and d.visibility = any($3)
         order by p.stat_id, p.scope",
    )
    .bind(account.0)
    .bind(scope)
    .bind(visible)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let mut stats = serde_json::Map::new();
    for (id, kind, visibility, sc, value, samples) in rows {
        let e = stats
            .entry(id)
            .or_insert_with(|| json!({ "kind": kind, "visibility": visibility, "scopes": {} }));
        let key = if sc.is_empty() { "" } else { sc.as_str() };
        e["scopes"][key] = json!({ "value": value, "samples": samples });
    }
    Ok(json!({ "account_id": account, "stats": stats }))
}

async fn server_read(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(account_id): Path<Uuid>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        read(
            &s.db,
            AccountId(account_id),
            q.scope.as_deref(),
            &VISIBILITY,
        )
        .await?,
    ))
}

async fn mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        read(&s.db, me.account_id, q.scope.as_deref(), &VISIBILITY).await?,
    ))
}

/// Another player's stats: public ones, plus `friends` ones when the viewer is a friend.
async fn theirs(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(account_id): Path<Uuid>,
    Query(q): Query<ReadQuery>,
) -> Result<Json<Value>, ApiError> {
    let target = AccountId(account_id);
    if target == me.account_id {
        return Ok(Json(
            read(&s.db, target, q.scope.as_deref(), &VISIBILITY).await?,
        ));
    }
    if s.social.is_blocked(target, me.account_id).await? {
        return Err(Error::NotFound("account".into()).into());
    }
    let friend: Option<i16> = sqlx::query_scalar(
        "select state from musubi.friends where user_id = $1 and friend_id = $2 and state = 0",
    )
    .bind(target.0)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let visible: &[&str] = if friend.is_some() {
        &["public", "friends"]
    } else {
        &["public"]
    };
    Ok(Json(
        read(&s.db, target, q.scope.as_deref(), visible).await?,
    ))
}

#[derive(Deserialize)]
struct TopQuery {
    stat: String,
    #[serde(default)]
    scope: String,
    #[serde(default = "fifty")]
    limit: i64,
}

fn fifty() -> i64 {
    50
}

/// Highest (or, for `min` stats, lowest) values of one stat in one slice.
async fn top(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<TopQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.clamp(1, 500);
    let kind: Option<String> =
        sqlx::query_scalar("select kind from musubi.stat_defs where id = $1")
            .bind(&q.stat)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some(kind) = kind else {
        return Err(Error::NotFound(format!("stat {}", q.stat)).into());
    };
    let order = if kind == "min" { "asc" } else { "desc" };
    let rows: Vec<(Uuid, f64, i64)> = sqlx::query_as(&format!(
        "select account_id, value, samples from musubi.player_stats
         where stat_id = $1 and scope = $2 order by value {order}, account_id limit $3"
    ))
    .bind(&q.stat)
    .bind(&q.scope)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let entries: Vec<Value> = rows
        .into_iter()
        .enumerate()
        .map(|(i, (a, v, n))| json!({ "rank": i + 1, "account_id": a, "value": v, "samples": n }))
        .collect();
    Ok(Json(
        json!({ "stat": q.stat, "scope": q.scope, "kind": kind, "entries": entries }),
    ))
}
