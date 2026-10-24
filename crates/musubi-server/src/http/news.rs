//! Title news (PlayFab `AddNews` / `AddLocalizedNews` / `GetTitleNews` parity): operator-published
//! items with per-locale copies, audience gating, scheduling and expiry, pins, and per-account
//! read tracking so clients can show an unread badge.

use std::collections::{HashMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/news", get(list).post(create))
        .route(
            "/v1/server/news/{id}",
            get(get_one).put(update).delete(delete_one),
        )
        .route(
            "/v1/server/news/{id}/locales/{locale}",
            axum::routing::put(put_locale).delete(delete_locale),
        )
        .route("/v1/server/news/{id}/stats", get(stats))
        .route("/v1/news", get(feed))
        .route("/v1/news/{id}/read", post(mark_read))
        .route("/v1/news/read_all", post(read_all))
}

fn now_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[derive(sqlx::FromRow)]
struct NewsRow {
    id: Uuid,
    title: String,
    body: String,
    published_at: i64,
    expires_at: Option<i64>,
    audience: Option<String>,
    tags: Value,
    pinned: bool,
    link: Option<String>,
    image: Option<String>,
    metadata: Value,
    enabled: bool,
    created_at: i64,
    updated_at: i64,
}

const COLS: &str = "id, title, body, published_at, expires_at, audience, tags, pinned, link, image, metadata, enabled, extract(epoch from created_at)::bigint as created_at, extract(epoch from updated_at)::bigint as updated_at";

type Locales = HashMap<Uuid, HashMap<String, (String, String)>>;

async fn locales_of(s: &AppState, ids: &[Uuid]) -> Result<Locales> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "select news_id, locale, title, body from musubi.news_locales where news_id = any($1)",
    )
    .bind(ids)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out: Locales = HashMap::new();
    for (id, l, t, b) in rows {
        out.entry(id).or_default().insert(l, (t, b));
    }
    Ok(out)
}

fn doc(r: &NewsRow, locales: Option<&HashMap<String, (String, String)>>) -> Value {
    let locales: serde_json::Map<String, Value> = locales
        .map(|m| {
            m.iter()
                .map(|(l, (t, b))| (l.clone(), json!({ "title": t, "body": b })))
                .collect()
        })
        .unwrap_or_default();
    json!({
        "id": r.id, "title": r.title, "body": r.body, "published_at": r.published_at,
        "expires_at": r.expires_at, "audience": r.audience, "tags": r.tags, "pinned": r.pinned,
        "link": r.link, "image": r.image, "metadata": r.metadata, "enabled": r.enabled,
        "created_at": r.created_at, "updated_at": r.updated_at, "locales": locales,
    })
}

async fn load(s: &AppState, id: Uuid) -> Result<NewsRow> {
    sqlx::query_as::<_, NewsRow>(&format!("select {COLS} from musubi.news where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("news item".into()))
}

async fn full_doc(s: &AppState, id: Uuid) -> Result<Value> {
    let r = load(s, id).await?;
    let l = locales_of(s, &[id]).await?;
    Ok(doc(&r, l.get(&id)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LocaleBody {
    title: String,
    body: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NewsBody {
    title: String,
    body: String,
    /// Unix seconds; defaults to now. Items in the future stay hidden until then.
    published_at: Option<i64>,
    expires_at: Option<i64>,
    audience: Option<String>,
    tags: Vec<String>,
    pinned: bool,
    link: Option<String>,
    image: Option<String>,
    metadata: Value,
    enabled: Option<bool>,
    locales: HashMap<String, LocaleBody>,
}

fn valid_locale(l: &str) -> Result<()> {
    if l.len() < 2
        || l.len() > 16
        || !l
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::InvalidArgument(format!(
            "locale `{l}`: [A-Za-z0-9_-]{{2,16}}"
        )));
    }
    Ok(())
}

fn validate(b: &NewsBody) -> Result<()> {
    if b.title.trim().is_empty() || b.title.len() > 200 {
        return Err(Error::InvalidArgument("title: 1..200 chars".into()));
    }
    if b.body.len() > 20_000 {
        return Err(Error::InvalidArgument("body: at most 20000 chars".into()));
    }
    if b.tags.len() > 16 {
        return Err(Error::InvalidArgument("tags: at most 16".into()));
    }
    for t in &b.tags {
        if t.is_empty()
            || t.len() > 32
            || !t
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(Error::InvalidArgument("tags: [a-z0-9_-]{1,32} each".into()));
        }
    }
    if let (Some(p), Some(e)) = (b.published_at.or(Some(now_s())), b.expires_at) {
        if e <= p {
            return Err(Error::InvalidArgument(
                "expires_at must be after published_at".into(),
            ));
        }
    }
    for s in [&b.link, &b.image].into_iter().flatten() {
        if s.is_empty() || s.len() > 2000 {
            return Err(Error::InvalidArgument("link/image: 1..2000 chars".into()));
        }
    }
    if b.locales.len() > 64 {
        return Err(Error::InvalidArgument("locales: at most 64".into()));
    }
    for (l, c) in &b.locales {
        valid_locale(l)?;
        if c.title.trim().is_empty() || c.title.len() > 200 || c.body.len() > 20_000 {
            return Err(Error::InvalidArgument(format!(
                "locales.{l}: title 1..200, body <= 20000"
            )));
        }
    }
    if !b.metadata.is_null() && !b.metadata.is_object() {
        return Err(Error::InvalidArgument("metadata must be an object".into()));
    }
    Ok(())
}

async fn save(s: &AppState, id: Uuid, b: NewsBody, replace_locales: bool) -> Result<Value> {
    validate(&b)?;
    let published_at = b.published_at.unwrap_or_else(now_s);
    let enabled = b.enabled.unwrap_or(true);
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.news (id, title, body, published_at, expires_at, audience, tags, pinned, link, image, metadata, enabled)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         on conflict (id) do update set title = excluded.title, body = excluded.body,
            published_at = excluded.published_at, expires_at = excluded.expires_at, audience = excluded.audience,
            tags = excluded.tags, pinned = excluded.pinned, link = excluded.link, image = excluded.image,
            metadata = excluded.metadata, enabled = excluded.enabled, updated_at = now()",
    )
    .bind(id)
    .bind(b.title.trim())
    .bind(&b.body)
    .bind(published_at)
    .bind(b.expires_at)
    .bind(&b.audience)
    .bind(json!(b.tags))
    .bind(b.pinned)
    .bind(&b.link)
    .bind(&b.image)
    .bind(if b.metadata.is_null() { json!({}) } else { b.metadata })
    .bind(enabled)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    if replace_locales {
        sqlx::query("delete from musubi.news_locales where news_id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
    }
    for (l, c) in &b.locales {
        sqlx::query(
            "insert into musubi.news_locales (news_id, locale, title, body) values ($1, $2, $3, $4)
             on conflict (news_id, locale) do update set title = excluded.title, body = excluded.body",
        )
        .bind(id)
        .bind(l)
        .bind(c.title.trim())
        .bind(&c.body)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    full_doc(s, id).await
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    limit: Option<i64>,
    offset: Option<i64>,
    tag: Option<String>,
    include_disabled: bool,
}

async fn list(
    State(s): State<AppState>,
    _k: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<NewsRow> = sqlx::query_as::<_, NewsRow>(&format!(
        "select {COLS} from musubi.news
         where ($1::boolean or enabled) and ($2::text is null or tags @> jsonb_build_array($2::text))
         order by pinned desc, published_at desc limit $3 offset $4"
    ))
    .bind(q.include_disabled)
    .bind(&q.tag)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .bind(q.offset.unwrap_or(0).max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let locales = locales_of(&s, &ids).await?;
    Ok(Json(json!({
        "news": rows.iter().map(|r| doc(r, locales.get(&r.id))).collect::<Vec<_>>(),
    })))
}

async fn create(
    State(s): State<AppState>,
    _k: ServerKey,
    Json(b): Json<NewsBody>,
) -> Result<Json<Value>, ApiError> {
    let id = Uuid::now_v7();
    let live = b.enabled.unwrap_or(true) && b.published_at.unwrap_or_else(now_s) <= now_s();
    let v = save(&s, id, b, false).await?;
    s.events
        .emit(
            if live {
                "news.published"
            } else {
                "news.created"
            },
            None,
            json!({ "id": id, "title": v["title"], "published_at": v["published_at"] }),
        )
        .await;
    Ok(Json(v))
}

async fn get_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(full_doc(&s, id).await?))
}

/// `PUT /v1/server/news/{id}` — full replace (locales in the body replace the stored set).
async fn update(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
    Json(b): Json<NewsBody>,
) -> Result<Json<Value>, ApiError> {
    load(&s, id).await?;
    let v = save(&s, id, b, true).await?;
    s.events
        .emit(
            "news.updated",
            None,
            json!({ "id": id, "title": v["title"] }),
        )
        .await;
    Ok(Json(v))
}

async fn delete_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.news where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n > 0 {
        s.events
            .emit("news.deleted", None, json!({ "id": id }))
            .await;
    }
    Ok(Json(json!({ "removed": n > 0 })))
}

async fn put_locale(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, locale)): Path<(Uuid, String)>,
    Json(c): Json<LocaleBody>,
) -> Result<Json<Value>, ApiError> {
    load(&s, id).await?;
    valid_locale(&locale)?;
    if c.title.trim().is_empty() || c.title.len() > 200 || c.body.len() > 20_000 {
        return Err(Error::InvalidArgument("title 1..200, body <= 20000".into()).into());
    }
    sqlx::query(
        "insert into musubi.news_locales (news_id, locale, title, body) values ($1, $2, $3, $4)
         on conflict (news_id, locale) do update set title = excluded.title, body = excluded.body",
    )
    .bind(id)
    .bind(&locale)
    .bind(c.title.trim())
    .bind(&c.body)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(full_doc(&s, id).await?))
}

async fn delete_locale(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((id, locale)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.news_locales where news_id = $1 and locale = $2")
        .bind(id)
        .bind(&locale)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

async fn stats(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    load(&s, id).await?;
    let (reads, first, last): (i64, Option<i64>, Option<i64>) = sqlx::query_as(
        "select count(*), extract(epoch from min(read_at))::bigint, extract(epoch from max(read_at))::bigint
         from musubi.news_reads where news_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "id": id, "reads": reads, "first_read_at": first, "last_read_at": last }),
    ))
}

// ---------------------------------------------------------------- client

/// Items an account may see right now: enabled, published, not expired, audience matches.
async fn visible(s: &AppState, account: AccountId) -> Result<Vec<NewsRow>> {
    let now = crate::http::qa_clock::now_for(s, account).await;
    let rows: Vec<NewsRow> = sqlx::query_as::<_, NewsRow>(&format!(
        "select {COLS} from musubi.news
         where enabled and published_at <= $1 and (expires_at is null or expires_at > $1)
         order by pinned desc, published_at desc limit 500"
    ))
    .bind(now)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    if rows.iter().all(|r| r.audience.is_none()) {
        return Ok(rows);
    }
    let audiences = s.audiences.of(&s.db, &s.economy, account).await?;
    Ok(rows
        .into_iter()
        .filter(|r| r.audience.as_ref().is_none_or(|a| audiences.contains(a)))
        .collect())
}

async fn read_set(s: &AppState, account: AccountId, ids: &[Uuid]) -> Result<HashSet<Uuid>> {
    if ids.is_empty() {
        return Ok(HashSet::new());
    }
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "select news_id from musubi.news_reads where account_id = $1 and news_id = any($2)",
    )
    .bind(account.0)
    .bind(ids)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(rows.into_iter().map(|(i,)| i).collect())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FeedQ {
    count: Option<i64>,
    locale: Option<String>,
    /// Only items published after this Unix time.
    since: Option<i64>,
    unread_only: bool,
    tag: Option<String>,
}

fn pick_locale<'a>(
    want: Option<&str>,
    locales: Option<&'a HashMap<String, (String, String)>>,
) -> Option<(&'a str, &'a (String, String))> {
    let want = want?;
    let m = locales?;
    if let Some((k, v)) = m.get_key_value(want) {
        return Some((k.as_str(), v));
    }
    let base = want.split(['-', '_']).next()?;
    if let Some((k, v)) = m.get_key_value(base) {
        return Some((k.as_str(), v));
    }
    m.iter()
        .find(|(k, _)| k.split(['-', '_']).next() == Some(base))
        .map(|(k, v)| (k.as_str(), v))
}

/// `GET /v1/news?count&locale&since&unread_only&tag` — the feed for the caller, pinned first,
/// localized with fallback (`de-AT` → `de` → base copy), plus the unread count for a badge.
async fn feed(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<FeedQ>,
) -> Result<Json<Value>, ApiError> {
    let rows = visible(&s, me.account_id).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let read = read_set(&s, me.account_id, &ids).await?;
    let unread = rows.iter().filter(|r| !read.contains(&r.id)).count();
    let count = q.count.unwrap_or(20).clamp(1, 100) as usize;
    let selected: Vec<&NewsRow> = rows
        .iter()
        .filter(|r| q.since.is_none_or(|t| r.published_at > t))
        .filter(|r| !q.unread_only || !read.contains(&r.id))
        .filter(|r| {
            q.tag.as_ref().is_none_or(|t| {
                r.tags
                    .as_array()
                    .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(t)))
            })
        })
        .take(count)
        .collect();
    let locales = locales_of(&s, &selected.iter().map(|r| r.id).collect::<Vec<_>>()).await?;
    let items: Vec<Value> = selected
        .iter()
        .map(|r| {
            let (locale, title, body) = match pick_locale(q.locale.as_deref(), locales.get(&r.id)) {
                Some((l, (t, b))) => (Some(l), t.as_str(), b.as_str()),
                None => (None, r.title.as_str(), r.body.as_str()),
            };
            json!({
                "id": r.id, "title": title, "body": body, "locale": locale,
                "published_at": r.published_at, "expires_at": r.expires_at, "tags": r.tags,
                "pinned": r.pinned, "link": r.link, "image": r.image, "metadata": r.metadata,
                "read": read.contains(&r.id),
            })
        })
        .collect();
    Ok(Json(
        json!({ "news": items, "unread": unread, "total": rows.len() }),
    ))
}

async fn mark_read(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    // only what the caller can see: no read marks (or existence oracle) for disabled,
    // unpublished, expired or audience-gated items
    let rows = visible(&s, me.account_id).await?;
    if !rows.iter().any(|r| r.id == id) {
        return Err(Error::NotFound("news item".into()).into());
    }
    sqlx::query(
        "insert into musubi.news_reads (news_id, account_id) values ($1, $2) on conflict do nothing",
    )
    .bind(id)
    .bind(me.account_id.0)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let read = read_set(&s, me.account_id, &ids).await?;
    Ok(Json(
        json!({ "id": id, "read": true, "unread": ids.iter().filter(|i| !read.contains(i)).count() }),
    ))
}

async fn read_all(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows = visible(&s, me.account_id).await?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.id).collect();
    let n = if ids.is_empty() {
        0
    } else {
        sqlx::query(
            "insert into musubi.news_reads (news_id, account_id) select unnest($1::uuid[]), $2 on conflict do nothing",
        )
        .bind(&ids)
        .bind(me.account_id.0)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected()
    };
    Ok(Json(json!({ "read": n, "unread": 0 })))
}
