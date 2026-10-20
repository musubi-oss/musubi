//! Tags on content entries (Beamable content tags parity): any row of a content-section table
//! (currencies, items, reward tables, quests, …) can carry free-form tags — `featured`,
//! `season-4`, `halloween`. Tags live in `musubi.content_tags`, which is itself a content table, so
//! they travel with bundles; `GET /v1/admin/content?tags=a,b` exports only tagged entries
//! (tag-based ContentQuery) and `?omit_tags=true` strips the tag table from a bundle.
//! Admin: `GET /v1/admin/content/tags?tag&table`, `PUT /v1/admin/content/tags {entries, add,
//! remove, set}`, `DELETE /v1/admin/content/tags/{tag}`. Clients resolve a tag to entry ids with
//! `GET /v1/content/tags/{tag}?table=`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

use crate::app::AppState;
use crate::http::content::{shape, SECTIONS, TAGS_TABLE};
use crate::http::{ApiError, Bearer, ServerKey};

const MAX_ENTRIES: usize = 100;
const MAX_TAGS: usize = 32;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/content/tags", get(list).put(update))
        .route(
            "/v1/admin/content/tags/{tag}",
            axum::routing::delete(drop_tag),
        )
        .route("/v1/content/tags/{tag}", get(client_lookup))
}

/// Tables whose rows may carry tags: the `content` section minus the tag table itself.
pub(crate) fn taggable(table: &str) -> bool {
    table != TAGS_TABLE
        && SECTIONS
            .iter()
            .find(|(n, _)| *n == "content")
            .is_some_and(|(_, t)| t.contains(&table))
}

pub(crate) fn valid_tag(t: &str) -> Result<()> {
    if t.is_empty()
        || t.len() > 32
        || !t.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | ':')
        })
    {
        return Err(Error::InvalidArgument(format!(
            "tag {t:?} must be 1..=32 of [a-z0-9_.:-]"
        )));
    }
    Ok(())
}

fn valid_table(t: &str) -> Result<()> {
    if !taggable(t) {
        return Err(Error::InvalidArgument(format!(
            "table {t:?} is not a taggable content table"
        )));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
struct ListQ {
    tag: Option<String>,
    table: Option<String>,
}

type Row = (String, String, String);

fn grouped(rows: Vec<Row>) -> Value {
    let mut by: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for (t, k, tag) in rows {
        by.entry((t, k)).or_default().push(tag);
    }
    json!({
        "entries": by
            .into_iter()
            .map(|((table, id), tags)| json!({"table": table, "id": id, "tags": tags}))
            .collect::<Vec<_>>()
    })
}

async fn fetch(s: &AppState, tag: Option<&str>, table: Option<&str>) -> Result<Vec<Row>> {
    if let Some(t) = table {
        valid_table(t)?;
    }
    if let Some(t) = tag {
        valid_tag(t)?;
    }
    sqlx::query_as(
        "select table_name, entry_key, tag from musubi.content_tags
         where ($1::text is null or table_name = $1) and ($2::text is null or entry_key in
               (select entry_key from musubi.content_tags where tag = $2 and table_name = musubi.content_tags.table_name))
         order by table_name, entry_key, tag limit 10000",
    )
    .bind(table)
    .bind(tag)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)
}

async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(grouped(
        fetch(&s, q.tag.as_deref(), q.table.as_deref()).await?,
    )))
}

#[derive(Deserialize, Default)]
struct LookupQ {
    table: Option<String>,
}

/// Client view: which entries carry a tag (ids only — the entries themselves come from the
/// catalog endpoints, so hidden/disabled content stays hidden).
async fn client_lookup(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(tag): Path<String>,
    Query(q): Query<LookupQ>,
) -> Result<Json<Value>, ApiError> {
    valid_tag(&tag)?;
    if let Some(t) = &q.table {
        valid_table(t)?;
    }
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select table_name, entry_key from musubi.content_tags
         where tag = $1 and ($2::text is null or table_name = $2) order by table_name, entry_key limit 10000",
    )
    .bind(&tag)
    .bind(&q.table)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "tag": tag,
        "entries": rows.iter().map(|(t, k)| json!({"table": t, "id": k})).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct EntryRef {
    table: String,
    id: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UpdateBody {
    entries: Vec<EntryRef>,
    add: Vec<String>,
    remove: Vec<String>,
    /// Replace the whole tag set of each entry (exclusive with add/remove).
    set: Option<Vec<String>>,
}

async fn entry_exists(s: &AppState, table: &str, id: &str) -> Result<bool> {
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let sh = shape(&mut tx, table).await?;
    let found = if sh.pk.len() == 1 {
        let sql = format!(
            "select exists(select 1 from musubi.\"{table}\" where \"{}\"::text = $1)",
            sh.pk[0].replace('"', "\"\"")
        );
        sqlx::query_scalar::<_, bool>(&sql)
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?
    } else {
        // composite key: `a|b` in export key order
        let parts: Vec<&str> = id.split('|').collect();
        if parts.len() != sh.pk.len() {
            return Ok(false);
        }
        let cond = sh
            .pk
            .iter()
            .enumerate()
            .map(|(i, c)| format!("\"{}\"::text = ${}", c.replace('"', "\"\""), i + 1))
            .collect::<Vec<_>>()
            .join(" and ");
        let sql = format!("select exists(select 1 from musubi.\"{table}\" where {cond})");
        let mut q = sqlx::query_scalar::<_, bool>(&sql);
        for p in &parts {
            q = q.bind(p.trim_matches('"'));
        }
        q.fetch_one(&mut *tx).await.map_err(Error::internal)?
    };
    tx.rollback().await.map_err(Error::internal)?;
    Ok(found)
}

async fn update(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<UpdateBody>,
) -> Result<Json<Value>, ApiError> {
    if b.entries.is_empty() || b.entries.len() > MAX_ENTRIES {
        return Err(Error::InvalidArgument(format!("entries must hold 1..={MAX_ENTRIES}")).into());
    }
    if b.set.is_some() && (!b.add.is_empty() || !b.remove.is_empty()) {
        return Err(Error::InvalidArgument("set is exclusive with add/remove".into()).into());
    }
    if b.set.is_none() && b.add.is_empty() && b.remove.is_empty() {
        return Err(Error::InvalidArgument("nothing to do: give add, remove or set".into()).into());
    }
    for t in b.add.iter().chain(&b.remove).chain(b.set.iter().flatten()) {
        valid_tag(t)?;
    }
    if b.add.len() > MAX_TAGS || b.set.as_ref().is_some_and(|v| v.len() > MAX_TAGS) {
        return Err(Error::InvalidArgument(format!("at most {MAX_TAGS} tags per entry")).into());
    }
    for e in &b.entries {
        valid_table(&e.table)?;
        if e.id.is_empty() || e.id.len() > 256 {
            return Err(Error::InvalidArgument("entry id must be 1..=256 chars".into()).into());
        }
        if !entry_exists(&s, &e.table, &e.id).await? {
            return Err(Error::NotFound(format!("{} {:?}", e.table, e.id)).into());
        }
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for e in &b.entries {
        if let Some(set) = &b.set {
            sqlx::query("delete from musubi.content_tags where table_name = $1 and entry_key = $2")
                .bind(&e.table)
                .bind(&e.id)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            for t in set {
                sqlx::query("insert into musubi.content_tags (table_name, entry_key, tag) values ($1, $2, $3) on conflict do nothing")
                    .bind(&e.table).bind(&e.id).bind(t)
                    .execute(&mut *tx).await.map_err(Error::internal)?;
            }
        } else {
            for t in &b.add {
                sqlx::query("insert into musubi.content_tags (table_name, entry_key, tag) values ($1, $2, $3) on conflict do nothing")
                    .bind(&e.table).bind(&e.id).bind(t)
                    .execute(&mut *tx).await.map_err(Error::internal)?;
            }
            if !b.remove.is_empty() {
                sqlx::query("delete from musubi.content_tags where table_name = $1 and entry_key = $2 and tag = any($3)")
                    .bind(&e.table).bind(&e.id).bind(&b.remove)
                    .execute(&mut *tx).await.map_err(Error::internal)?;
            }
        }
    }
    let keys: Vec<String> = b.entries.iter().map(|e| e.id.clone()).collect();
    let tables: Vec<String> = b.entries.iter().map(|e| e.table.clone()).collect();
    let rows: Vec<Row> = sqlx::query_as(
        "select table_name, entry_key, tag from musubi.content_tags
         where (table_name, entry_key) in (select * from unnest($1::text[], $2::text[]))
         order by table_name, entry_key, tag",
    )
    .bind(&tables)
    .bind(&keys)
    .fetch_all(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    let mut out = grouped(rows);
    // entries left without tags still appear, with an empty list
    let have: Vec<(String, String)> = out["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["table"].as_str().unwrap().into(),
                e["id"].as_str().unwrap().into(),
            )
        })
        .collect();
    for e in &b.entries {
        if !have.contains(&(e.table.clone(), e.id.clone())) {
            out["entries"]
                .as_array_mut()
                .unwrap()
                .push(json!({"table": e.table, "id": e.id, "tags": []}));
        }
    }
    s.events
        .emit(
            "content.tags_updated",
            None,
            json!({"entries": b.entries.len(), "add": b.add, "remove": b.remove, "set": b.set}),
        )
        .await;
    Ok(Json(out))
}

async fn drop_tag(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(tag): Path<String>,
) -> Result<Json<Value>, ApiError> {
    valid_tag(&tag)?;
    let n = sqlx::query("delete from musubi.content_tags where tag = $1")
        .bind(&tag)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("tag {tag}")).into());
    }
    s.events
        .emit(
            "content.tags_updated",
            None,
            json!({"dropped": tag, "entries": n}),
        )
        .await;
    Ok(Json(json!({"removed": n})))
}
