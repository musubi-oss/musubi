//! Player-facing server browser (P1-18): browsable dedicated servers with filters, join through a
//! shared allocation, favourites, join history and player reputation.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use musubi_fleet::{AllocSelector, AllocateRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ApiError, Bearer};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/servers", get(list))
        .route("/v1/servers/favorites", get(favorites))
        .route(
            "/v1/servers/favorites/{key}",
            axum::routing::put(add_favorite).delete(remove_favorite),
        )
        .route("/v1/servers/history", get(history))
        .route("/v1/servers/{id}", get(get_one))
        .route("/v1/servers/{id}/join", post(join))
        .route("/v1/servers/{id}/rate", post(rate))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    region: Option<String>,
    /// `key=value,key2=value2` over the server's labels (map, mode, …).
    labels: Option<String>,
    /// Case-insensitive substring of the server name.
    q: Option<String>,
    min_free_slots: Option<i32>,
    not_full: Option<bool>,
    not_empty: Option<bool>,
    build_version: Option<String>,
    /// `players|-players|age|-age|rating|name` (default `-players`).
    sort: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    fleet_id: Uuid,
    fleet: String,
    region: String,
    state: String,
    address: String,
    port: i32,
    labels: Value,
    players: i32,
    capacity: i32,
    counters: Value,
    build_version: Option<String>,
    created_at: OffsetDateTime,
    rating_avg: Option<f64>,
    rating_votes: i64,
}

const BROWSABLE: &str = "g.state in ('ready', 'allocated', 'in_game') and g.address <> '' \
    and (coalesce((f.policy->>'browsable')::boolean, false) or g.labels->>'public' in ('true', '1'))";

const SELECT: &str = "select g.id, g.fleet_id, f.name as fleet, f.region, g.state, g.address, g.port, g.labels, g.players, g.capacity, \
    g.counters, g.build_version, g.created_at, r.avg as rating_avg, coalesce(r.votes, 0) as rating_votes \
    from musubi.game_servers g join musubi.fleets f on f.id = g.fleet_id \
    left join lateral (select avg(score)::float8 as avg, count(*) as votes from musubi.server_ratings sr where sr.key = g.address || ':' || g.port) r on true \
    where ";

/// Deep offset paging is O(offset); the browser shows at most this far.
const MAX_OFFSET: i64 = 10_000;

/// Escape a user string for use inside a `like … escape '\\'` pattern.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn key_of(r: &Row) -> String {
    format!("{}:{}", r.address, r.port)
}

fn name_of(r: &Row) -> String {
    r.labels["name"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| r.fleet.clone())
}

fn view(r: &Row, favorite: bool, last_joined: Option<&OffsetDateTime>) -> Value {
    let now = OffsetDateTime::now_utc();
    json!({
        "id": r.id,
        "key": key_of(r),
        "name": name_of(r),
        "fleet": r.fleet,
        "fleet_id": r.fleet_id,
        "region": r.region,
        "state": r.state,
        "address": r.address,
        "port": r.port,
        "labels": r.labels,
        "map": r.labels["map"],
        "mode": r.labels["mode"],
        "players": r.players,
        "capacity": r.capacity,
        "free_slots": if r.capacity > 0 { (r.capacity - r.players).max(0) } else { 0 },
        "full": r.capacity > 0 && r.players >= r.capacity,
        "counters": r.counters,
        "build_version": r.build_version,
        "uptime_secs": (now - r.created_at).whole_seconds(),
        "rating": { "avg": r.rating_avg, "votes": r.rating_votes },
        "favorite": favorite,
        "last_joined_at": last_joined.map(|t| t.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()),
    })
}

async fn my_keys(
    s: &AppState,
    me: musubi_core::AccountId,
) -> Result<
    (
        Vec<String>,
        std::collections::HashMap<String, OffsetDateTime>,
    ),
    ApiError,
> {
    let favs: Vec<String> =
        sqlx::query_scalar("select key from musubi.server_favorites where account_id = $1")
            .bind(me.0)
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    let hist: Vec<(String, OffsetDateTime)> = sqlx::query_as(
        "select key, last_joined_at from musubi.server_history where account_id = $1",
    )
    .bind(me.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok((favs, hist.into_iter().collect()))
}

/// `GET /v1/servers?region&labels=map=dust&q&min_free_slots&not_full&not_empty&build_version&sort&limit&offset`
/// — browsable servers (fleet `policy.browsable` or label `public=true`) with slots, labels,
/// counters, uptime, reputation and the caller's favourite / last-joined marks.
async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut qb = sqlx::QueryBuilder::new(SELECT);
    qb.push(BROWSABLE);
    if let Some(r) = q.region.clone() {
        qb.push(" and f.region = ").push_bind(r);
    }
    if let Some(l) = &q.labels {
        let mut m = serde_json::Map::new();
        for pair in l.split(',').filter(|p| !p.trim().is_empty()) {
            let (k, v) = pair.split_once('=').ok_or_else(|| {
                Error::InvalidArgument(format!("labels: expected key=value, got {pair:?}"))
            })?;
            m.insert(k.trim().to_string(), Value::String(v.trim().to_string()));
        }
        qb.push(" and g.labels @> ").push_bind(Value::Object(m));
    }
    if let Some(t) = q.q.clone() {
        qb.push(" and coalesce(g.labels->>'name', f.name) ilike ")
            .push_bind(format!("%{}%", like_escape(&t)))
            .push(" escape '\\'");
    }
    if let Some(n) = q.min_free_slots {
        qb.push(" and g.capacity - g.players >= ").push_bind(n);
    }
    if q.not_full == Some(true) {
        qb.push(" and (g.capacity = 0 or g.players < g.capacity)");
    }
    if q.not_empty == Some(true) {
        qb.push(" and g.players > 0");
    }
    if let Some(b) = q.build_version.clone() {
        qb.push(" and g.build_version = ").push_bind(b);
    }
    let order = match q.sort.as_deref().unwrap_or("-players") {
        "players" => "g.players asc, g.created_at",
        "-players" => "g.players desc, g.created_at",
        "age" => "g.created_at desc",
        "-age" => "g.created_at asc",
        "rating" => "r.avg desc nulls last, r.votes desc, g.players desc",
        "name" => "coalesce(g.labels->>'name', f.name) asc",
        other => {
            return Err(Error::InvalidArgument(format!("bad sort {other:?}")).into());
        }
    };
    qb.push(format!(" order by {order}, g.id"));
    qb.push(" limit ")
        .push_bind(q.limit.unwrap_or(50).clamp(1, 200));
    qb.push(" offset ")
        .push_bind(q.offset.unwrap_or(0).clamp(0, MAX_OFFSET));
    let rows: Vec<Row> = qb
        .build_query_as()
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    let (favs, hist) = my_keys(&s, me.account_id).await?;
    let servers: Vec<Value> = rows
        .iter()
        .map(|r| {
            let k = key_of(r);
            view(r, favs.contains(&k), hist.get(&k))
        })
        .collect();
    Ok(Json(json!({ "servers": servers, "count": servers.len() })))
}

async fn fetch_one(s: &AppState, id: Uuid) -> Result<Row, ApiError> {
    let mut qb = sqlx::QueryBuilder::new(SELECT);
    qb.push(BROWSABLE).push(" and g.id = ").push_bind(id);
    qb.build_query_as::<Row>()
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("server".into()).into())
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let r = fetch_one(&s, id).await?;
    let (favs, hist) = my_keys(&s, me.account_id).await?;
    let k = key_of(&r);
    Ok(Json(view(&r, favs.contains(&k), hist.get(&k))))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct JoinReq {
    payload: Value,
}

/// `POST /v1/servers/{id}/join {payload?}` — connect to a browsable server: a shared allocation
/// on exactly that server (`ready` or already `allocated`), 429 when it is full; records history.
async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(r): Json<JoinReq>,
) -> Result<Json<Value>, ApiError> {
    let row = fetch_one(&s, id).await?;
    if row.capacity > 0 && row.players >= row.capacity {
        return Err(Error::ResourceExhausted("server is full".into()).into());
    }
    let a = s
        .fleets
        .allocate_with(
            row.fleet_id,
            AllocateRequest {
                payload: json!({ "account_id": me.account_id, "browser": true, "payload": r.payload }),
                selector: AllocSelector {
                    server_id: Some(id),
                    states: vec!["ready".into(), "allocated".into(), "in_game".into()],
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await?;
    let key = key_of(&row);
    sqlx::query(
        "insert into musubi.server_history (account_id, key, name, fleet_id, server_id) values ($1, $2, $3, $4, $5)
         on conflict (account_id, key) do update set joins = musubi.server_history.joins + 1, last_joined_at = now(),
             name = excluded.name, server_id = excluded.server_id",
    )
    .bind(me.account_id.0)
    .bind(&key)
    .bind(name_of(&row))
    .bind(row.fleet_id)
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "server_browser.joined",
            Some(me.account_id),
            json!({ "server_id": id, "key": key, "fleet_id": row.fleet_id }),
        )
        .await;
    Ok(Json(json!({
        "server_id": a.server_id, "address": a.address, "port": a.port, "connect_token": a.connect_token,
        "shared": a.backfill, "key": key,
    })))
}

/// `GET /v1/servers/favorites` — saved servers with `online` + the live card when they are up.
async fn favorites(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let favs: Vec<(String, String, OffsetDateTime)> = sqlx::query_as(
        "select key, name, added_at from musubi.server_favorites where account_id = $1 order by added_at desc",
    )
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out = Vec::with_capacity(favs.len());
    for (key, name, added) in favs {
        let live = live_by_key(&s, &key).await?;
        out.push(json!({
            "key": key, "name": name, "online": live.is_some(),
            "added_at": added.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
            "server": live.map(|r| view(&r, true, None)),
        }));
    }
    Ok(Json(json!({ "favorites": out })))
}

async fn live_by_key(s: &AppState, key: &str) -> Result<Option<Row>, ApiError> {
    let Some((addr, port)) = key.rsplit_once(':') else {
        return Ok(None);
    };
    let Ok(port) = port.parse::<i32>() else {
        return Ok(None);
    };
    let mut qb = sqlx::QueryBuilder::new(SELECT);
    qb.push(BROWSABLE)
        .push(" and g.address = ")
        .push_bind(addr.to_string())
        .push(" and g.port = ")
        .push_bind(port)
        .push(" order by g.created_at desc limit 1");
    Ok(qb
        .build_query_as::<Row>()
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FavReq {
    name: Option<String>,
}

/// `PUT /v1/servers/favorites/{key} {name?}` — key is `address:port` (or a server id, resolved).
async fn add_favorite(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
    body: Option<Json<FavReq>>,
) -> Result<Json<Value>, ApiError> {
    let mut name = body.and_then(|b| b.0.name.clone()).unwrap_or_default();
    let key = match Uuid::parse_str(&key) {
        Ok(id) => {
            let r = fetch_one(&s, id).await?;
            if name.is_empty() {
                name = name_of(&r);
            }
            key_of(&r)
        }
        Err(_) => {
            if !key.contains(':') || key.len() > 128 {
                return Err(Error::InvalidArgument(
                    "key must be address:port or a server id".into(),
                )
                .into());
            }
            if name.is_empty() {
                if let Some(r) = live_by_key(&s, &key).await? {
                    name = name_of(&r);
                }
            }
            key
        }
    };
    let n: i64 =
        sqlx::query_scalar("select count(*) from musubi.server_favorites where account_id = $1")
            .bind(me.account_id.0)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if n >= 200 {
        return Err(Error::ResourceExhausted("at most 200 favourites".into()).into());
    }
    sqlx::query(
        "insert into musubi.server_favorites (account_id, key, name) values ($1, $2, $3)
         on conflict (account_id, key) do update set name = case when excluded.name <> '' then excluded.name else musubi.server_favorites.name end",
    )
    .bind(me.account_id.0)
    .bind(&key)
    .bind(name.chars().take(64).collect::<String>())
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "ok": true, "key": key })))
}

async fn remove_favorite(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.server_favorites where account_id = $1 and key = $2")
        .bind(me.account_id.0)
        .bind(&key)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

/// `GET /v1/servers/history` — servers the player joined, most recent first, with `online`.
async fn history(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(String, String, Option<Uuid>, i32, OffsetDateTime)> = sqlx::query_as(
        "select key, name, server_id, joins, last_joined_at from musubi.server_history where account_id = $1
         order by last_joined_at desc limit 100",
    )
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out = Vec::with_capacity(rows.len());
    for (key, name, server_id, joins, at) in rows {
        let live = live_by_key(&s, &key).await?;
        out.push(json!({
            "key": key, "name": name, "server_id": server_id, "joins": joins, "online": live.is_some(),
            "last_joined_at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
            "server": live.map(|r| view(&r, false, Some(&at))),
        }));
    }
    Ok(Json(json!({ "history": out })))
}

#[derive(Deserialize)]
struct RateReq {
    score: i16,
    #[serde(default)]
    tags: Vec<String>,
}

/// `POST /v1/servers/{id}/rate {score: 1..5, tags?}` — one vote per player per server key
/// (re-voting replaces); needs to have joined the server before.
async fn rate(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(r): Json<RateReq>,
) -> Result<Json<Value>, ApiError> {
    if !(1..=5).contains(&r.score) {
        return Err(Error::InvalidArgument("score must be 1..=5".into()).into());
    }
    if r.tags.len() > 8 || r.tags.iter().any(|t| t.is_empty() || t.len() > 32) {
        return Err(Error::InvalidArgument("at most 8 tags of <= 32 chars".into()).into());
    }
    let row = fetch_one(&s, id).await?;
    let key = key_of(&row);
    let joined: Option<i32> = sqlx::query_scalar(
        "select joins from musubi.server_history where account_id = $1 and key = $2",
    )
    .bind(me.account_id.0)
    .bind(&key)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if joined.is_none() {
        return Err(Error::PermissionDenied("join the server before rating it".into()).into());
    }
    sqlx::query(
        "insert into musubi.server_ratings (account_id, key, score, tags) values ($1, $2, $3, $4)
         on conflict (account_id, key) do update set score = excluded.score, tags = excluded.tags, at = now()",
    )
    .bind(me.account_id.0)
    .bind(&key)
    .bind(r.score)
    .bind(&r.tags)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let (avg, votes): (Option<f64>, i64) = sqlx::query_as(
        "select avg(score)::float8, count(*) from musubi.server_ratings where key = $1",
    )
    .bind(&key)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let tags: Vec<(String, i64)> = sqlx::query_as(
        "select t, count(*) from musubi.server_ratings, unnest(tags) t where key = $1 group by t order by 2 desc limit 10",
    )
    .bind(&key)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "key": key, "rating": { "avg": avg, "votes": votes },
        "tags": tags.iter().map(|(t, n)| json!({ "tag": t, "votes": n })).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::like_escape;

    #[test]
    fn like_escape_neutralises_wildcards_and_escape_char() {
        assert_eq!(like_escape("dust"), "dust");
        assert_eq!(like_escape("50%_off"), "50\\%\\_off");
        assert_eq!(like_escape("tail\\"), "tail\\\\");
    }
}
