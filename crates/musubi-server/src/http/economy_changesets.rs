//! Economy configuration workflow (P1-48): catalog edits (items, store, currencies, reward
//! tables, coupons, strings…) are staged in a *changeset* — a partial content bundle overlaid on
//! the live tables — reviewed as a diff, validated (references, prices, locales, images) and
//! published atomically as one content version (so `/v1/admin/content/versions/{id}/rollback`
//! undoes it). `[economy] require_review` makes publishing a four-eyes step. Per-locale catalog
//! strings live in `musubi.economy_strings` and are merged into `/v1/store?locale=`.

use std::collections::{BTreeMap, HashSet};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::content::{self, key_of, rows, shape};
use crate::http::{ApiError, ServerKey};
use crate::keys::Caller;

pub const STATES: &[&str] = &[
    "draft",
    "in_review",
    "approved",
    "scheduled",
    "published",
    "rejected",
    "cancelled",
];
pub const STRING_KINDS: &[&str] = &["item", "store_item", "currency", "reward_table"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/economy/changesets", get(list).post(create))
        .route(
            "/v1/server/economy/changesets/{id}",
            get(get_one).delete(delete),
        )
        .route(
            "/v1/server/economy/changesets/{id}/changes",
            put(put_changes),
        )
        .route(
            "/v1/server/economy/changesets/{id}/changes/{table}/{key}",
            axum::routing::delete(drop_change),
        )
        .route("/v1/server/economy/changesets/{id}/diff", get(diff_route))
        .route(
            "/v1/server/economy/changesets/{id}/validate",
            get(validate_route),
        )
        .route("/v1/server/economy/changesets/{id}/preview", get(preview))
        .route("/v1/server/economy/changesets/{id}/submit", post(submit))
        .route("/v1/server/economy/changesets/{id}/approve", post(approve))
        .route("/v1/server/economy/changesets/{id}/reject", post(reject))
        .route("/v1/server/economy/changesets/{id}/publish", post(publish))
        .route("/v1/server/economy/changesets/{id}/cancel", post(cancel))
        .route("/v1/server/economy/strings", get(strings_list))
        .route(
            "/v1/server/economy/strings/csv",
            get(strings_csv_export).post(strings_csv_import),
        )
        .route(
            "/v1/server/economy/strings/{kind}/{id}",
            put(strings_put).delete(strings_delete_all),
        )
        .route(
            "/v1/server/economy/strings/{kind}/{id}/{locale}",
            axum::routing::delete(strings_delete),
        )
        .route("/v1/economy/strings", get(player_strings))
}

fn parse(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| Error::InvalidArgument("bad changeset id".into()).into())
}

#[derive(sqlx::FromRow, Clone)]
struct Row {
    id: Uuid,
    name: String,
    note: String,
    state: String,
    author: String,
    reviewer: String,
    review_note: String,
    changes: Value,
    schedule_at: Option<OffsetDateTime>,
    published_version: Option<i64>,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    published_at: Option<OffsetDateTime>,
}

const COLS: &str = "id, name, note, state, author, reviewer, review_note, changes, schedule_at, published_version, created_at, updated_at, published_at";

fn ts(t: Option<OffsetDateTime>) -> Value {
    t.map(|t| json!(t.unix_timestamp())).unwrap_or(Value::Null)
}

fn summary(r: &Row) -> Value {
    let mut counts = Map::new();
    if let Some(t) = r.changes.as_object() {
        for (table, ch) in t {
            let m = ch.as_object().cloned().unwrap_or_default();
            let deletes = m.values().filter(|v| v.is_null()).count();
            counts.insert(
                table.clone(),
                json!({ "upserts": m.len() - deletes, "deletes": deletes }),
            );
        }
    }
    json!({
        "id": r.id, "name": r.name, "note": r.note, "state": r.state, "author": r.author,
        "reviewer": r.reviewer, "review_note": r.review_note, "counts": counts,
        "schedule_at": ts(r.schedule_at), "published_version": r.published_version,
        "created_at": r.created_at.unix_timestamp(), "updated_at": r.updated_at.unix_timestamp(),
        "published_at": ts(r.published_at),
    })
}

async fn load(db: &PgPool, id: Uuid) -> Result<Row, Error> {
    sqlx::query_as::<_, Row>(&format!(
        "select {COLS} from musubi.economy_changesets where id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("changeset {id}")))
}

async fn set_state(
    db: &PgPool,
    id: Uuid,
    state: &str,
    reviewer: Option<&str>,
    review_note: Option<&str>,
    schedule_at: Option<Option<OffsetDateTime>>,
) -> Result<Row, Error> {
    sqlx::query(
        "update musubi.economy_changesets set state = $2, reviewer = coalesce($3, reviewer),
             review_note = coalesce($4, review_note), schedule_at = case when $5 then $6 else schedule_at end,
             updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(state)
    .bind(reviewer)
    .bind(review_note)
    .bind(schedule_at.is_some())
    .bind(schedule_at.flatten())
    .execute(db)
    .await
    .map_err(Error::internal)?;
    load(db, id).await
}

fn editable_table(t: &str) -> Result<(), Error> {
    match content::section_of(t) {
        Some("content") => Ok(()),
        _ => Err(Error::InvalidArgument(format!(
            "table {t}: not a content table (see GET /v1/admin/content)"
        ))),
    }
}

/// Live rows of `table` with the changeset's upserts/deletes applied; `Some(shape)` when the
/// caller needs keys.
async fn wanted(
    s: &AppState,
    changes: &Value,
    table: &str,
) -> Result<(Vec<Value>, content::Shape), Error> {
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let sh = shape(&mut tx, table).await?;
    let live = rows(&mut tx, table, &sh).await?;
    tx.rollback().await.map_err(Error::internal)?;
    let mut by_key: BTreeMap<String, Value> =
        live.into_iter().map(|r| (key_of(&r, &sh.pk), r)).collect();
    if let Some(ch) = changes.get(table).and_then(Value::as_object) {
        for (key, v) in ch {
            match v {
                Value::Null => {
                    by_key.remove(key);
                }
                Value::Object(patch) => {
                    let mut row = by_key.remove(key).unwrap_or_else(|| json!({}));
                    for (k, val) in patch {
                        row[k] = val.clone();
                    }
                    by_key.insert(key.clone(), row);
                }
                _ => {}
            }
        }
    }
    Ok((by_key.into_values().collect(), sh))
}

/// The bundle a changeset publishes: every touched table in full (live + changes), so an
/// import with `prune` applies exactly the upserts and deletes.
async fn bundle_of(s: &AppState, r: &Row) -> Result<Value, Error> {
    let mut tables = Map::new();
    if let Some(t) = r.changes.as_object() {
        for table in t.keys() {
            let (w, _) = wanted(s, &r.changes, table).await?;
            tables.insert(table.clone(), Value::Array(w));
        }
    }
    Ok(json!({ "format": content::FORMAT, "sections": ["content"], "tables": tables }))
}

fn image_ok(v: &Value) -> bool {
    match v {
        Value::String(u) => u.starts_with("https://") || u.starts_with("/v1/files/"),
        Value::Null => true,
        _ => false,
    }
}

fn locale_ok(l: &str) -> bool {
    (2..=16).contains(&l.len())
        && l.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Referential and content checks over the would-be state. Empty = publishable.
async fn validate(s: &AppState, r: &Row) -> Result<Vec<Value>, Error> {
    let mut problems = Vec::new();
    let touched: Vec<String> = r
        .changes
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    if touched.is_empty() {
        problems.push(json!({ "table": "", "key": "", "problem": "changeset is empty" }));
        return Ok(problems);
    }
    let (items, _) = wanted(s, &r.changes, "items").await?;
    let (currencies, _) = wanted(s, &r.changes, "currencies").await?;
    let (strings, _) = wanted(s, &r.changes, "economy_strings").await?;
    let item_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| i["id"].as_str().map(str::to_string))
        .collect();
    let currency_ids: HashSet<String> = currencies
        .iter()
        .filter_map(|c| c["code"].as_str().map(str::to_string))
        .collect();
    let have_locales = |kind: &str, id: &str| -> HashSet<String> {
        strings
            .iter()
            .filter(|x| x["kind"] == kind && x["id"] == id)
            .filter_map(|x| x["locale"].as_str().map(str::to_string))
            .collect()
    };
    let required = &s.cfg.economy.required_locales;
    let mut push = |table: &str, key: &str, p: String| {
        problems.push(json!({ "table": table, "key": key, "problem": p }));
    };
    for (table, ch) in r.changes.as_object().unwrap() {
        let Some(ch) = ch.as_object() else { continue };
        let (w, sh) = wanted(s, &r.changes, table).await?;
        let by_key: BTreeMap<String, &Value> = w.iter().map(|x| (key_of(x, &sh.pk), x)).collect();
        for (key, patch) in ch {
            if patch.is_null() {
                continue;
            }
            let Some(row) = by_key.get(key) else { continue };
            for img in row["images"]
                .as_object()
                .map(|m| m.values().collect::<Vec<_>>())
                .unwrap_or_default()
            {
                if !image_ok(img) {
                    push(table, key, "images: https:// url or /v1/files/ path".into());
                }
            }
            match table.as_str() {
                "items" => {
                    if row["name"].as_str().unwrap_or("").is_empty() {
                        push(table, key, "name required".into());
                    }
                    if let Some(st) = row.get("status").and_then(Value::as_str) {
                        if !["draft", "published", "retired"].contains(&st) {
                            push(table, key, format!("status {st}: draft|published|retired"));
                        }
                    }
                    if !required.is_empty() {
                        let have = have_locales("item", row["id"].as_str().unwrap_or(""));
                        for l in required.iter().filter(|l| !have.contains(*l)) {
                            push(table, key, format!("missing strings for locale {l}"));
                        }
                    }
                }
                "store_items" => {
                    let item = row["item_id"].as_str().unwrap_or("");
                    if !item_ids.contains(item) {
                        push(table, key, format!("item_id {item}: unknown item"));
                    } else if items
                        .iter()
                        .any(|i| i["id"] == item && i["status"] == "retired")
                    {
                        push(table, key, format!("item_id {item}: item is retired"));
                    }
                    match row["prices"].as_object() {
                        Some(p) if !p.is_empty() => {
                            for (cur, amt) in p {
                                if !currency_ids.contains(cur) {
                                    push(table, key, format!("prices.{cur}: unknown currency"));
                                }
                                if amt.as_f64().is_none_or(|a| a < 0.0) {
                                    push(table, key, format!("prices.{cur}: amount >= 0"));
                                }
                            }
                        }
                        _ => push(table, key, "prices: at least one currency".into()),
                    }
                    if row
                        .get("quantity")
                        .and_then(Value::as_i64)
                        .is_some_and(|q| q < 1)
                    {
                        push(table, key, "quantity >= 1".into());
                    }
                    if !required.is_empty() {
                        let have = have_locales("store_item", row["id"].as_str().unwrap_or(""));
                        for l in required.iter().filter(|l| !have.contains(*l)) {
                            push(table, key, format!("missing strings for locale {l}"));
                        }
                    }
                }
                "economy_strings" => {
                    let kind = row["kind"].as_str().unwrap_or("");
                    if !STRING_KINDS.contains(&kind) {
                        push(
                            table,
                            key,
                            format!("kind {kind}: {}", STRING_KINDS.join("|")),
                        );
                    }
                    if !locale_ok(row["locale"].as_str().unwrap_or("")) {
                        push(table, key, "locale: 2..=16 chars [a-z0-9-]".into());
                    }
                    if row["name"].as_str().is_some_and(|n| n.len() > 128) {
                        push(table, key, "name: up to 128 chars".into());
                    }
                    if kind == "item" && !item_ids.contains(row["id"].as_str().unwrap_or("")) {
                        push(table, key, "id: unknown item".into());
                    }
                }
                _ => {}
            }
        }
    }
    Ok(problems)
}

async fn emit(s: &AppState, kind: &str, r: &Row, extra: Value) {
    let mut data = summary(r);
    if let (Some(d), Some(e)) = (data.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            d.insert(k.clone(), v.clone());
        }
    }
    s.events.emit(kind, None, data).await;
}

/// Apply the changeset as one content import (its own content version).
async fn do_publish(s: &AppState, r: &Row, by: &str) -> Result<Value, Error> {
    let problems = validate(s, r).await?;
    if !problems.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "changeset does not validate: {}",
            serde_json::to_string(&problems).unwrap_or_default()
        )));
    }
    let bundle = bundle_of(s, r).await?;
    let note = if r.note.is_empty() {
        format!("changeset {} ({})", r.name, r.id)
    } else {
        format!("changeset {}: {}", r.name, r.note)
    };
    let res = content::run_import(s, &bundle, false, true, &note, "changeset", None).await?;
    let version = res["version"].as_i64();
    sqlx::query(
        "update musubi.economy_changesets set state = 'published', published_version = $2,
             published_at = now(), reviewer = case when reviewer = '' then $3 else reviewer end, updated_at = now() where id = $1",
    )
    .bind(r.id)
    .bind(version)
    .bind(by)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let r = load(&s.db, r.id).await?;
    emit(
        s,
        "economy.changeset_published",
        &r,
        json!({ "by": by, "diff": res["diff"] }),
    )
    .await;
    Ok(
        json!({ "changeset": summary(&r), "version": version, "diff": res["diff"], "applied": res["applied"] }),
    )
}

/// Scheduled changesets whose time has come (called from the periodic job loop).
pub async fn run_due(s: &AppState) -> musubi_core::Result<usize> {
    let due: Vec<Row> = sqlx::query_as::<_, Row>(&format!(
        "select {COLS} from musubi.economy_changesets where state = 'scheduled' and schedule_at <= now() order by schedule_at limit 10"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut n = 0;
    for r in due {
        match do_publish(s, &r, "schedule").await {
            Ok(_) => n += 1,
            Err(e) => {
                tracing::warn!(changeset = %r.id, error = %e, "scheduled changeset failed; back to approved");
                let _ = set_state(
                    &s.db,
                    r.id,
                    "approved",
                    None,
                    Some(&format!("schedule failed: {e}")),
                    Some(None),
                )
                .await;
            }
        }
    }
    Ok(n)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    state: Option<String>,
    limit: Option<i64>,
}

async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as::<_, Row>(&format!(
        "select {COLS} from musubi.economy_changesets where ($1::text is null or state = $1) order by created_at desc limit $2"
    ))
    .bind(&q.state)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "changesets": rows.iter().map(summary).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CreateBody {
    name: String,
    note: String,
    /// Optional initial changes, same shape as `PUT …/changes`.
    changes: Vec<Change>,
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Json(b): Json<CreateBody>,
) -> Result<Json<Value>, ApiError> {
    if b.name.is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..=128 chars".into()).into());
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.economy_changesets (id, name, note, author) values ($1, $2, $3, $4)",
    )
    .bind(id)
    .bind(&b.name)
    .bind(&b.note)
    .bind(&caller.name)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if !b.changes.is_empty() {
        apply_changes(&s, id, b.changes).await?;
    }
    Ok(Json(summary(&load(&s.db, id).await?)))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    let mut v = summary(&r);
    v["changes"] = r.changes.clone();
    Ok(Json(v))
}

async fn delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    if !["draft", "rejected", "cancelled"].contains(&r.state.as_str()) {
        return Err(Error::Conflict(format!(
            "changeset is {}; only draft/rejected/cancelled ones can be deleted",
            r.state
        ))
        .into());
    }
    sqlx::query("delete from musubi.economy_changesets where id = $1")
        .bind(r.id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Clone)]
struct Change {
    table: String,
    /// `upsert` (default; a partial row merges into the live one) or `delete`.
    #[serde(default)]
    op: Option<String>,
    /// Row (upsert) or primary key values (delete).
    row: Value,
}

#[derive(Deserialize)]
struct ChangesBody {
    changes: Vec<Change>,
}

async fn apply_changes(s: &AppState, id: Uuid, changes: Vec<Change>) -> Result<Row, Error> {
    let r = load(&s.db, id).await?;
    if !["draft", "rejected"].contains(&r.state.as_str()) {
        return Err(Error::Conflict(format!(
            "changeset is {}; edit a draft (reopen a rejected one by editing it)",
            r.state
        )));
    }
    if changes.len() > 2000 {
        return Err(Error::InvalidArgument(
            "changes: up to 2000 per call".into(),
        ));
    }
    let mut all = r.changes.as_object().cloned().unwrap_or_default();
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for c in changes {
        editable_table(&c.table)?;
        let row = c
            .row
            .as_object()
            .ok_or_else(|| Error::InvalidArgument(format!("{}: row must be an object", c.table)))?;
        let sh = shape(&mut tx, &c.table).await?;
        for pk in &sh.pk {
            if !row.contains_key(pk) {
                return Err(Error::InvalidArgument(format!(
                    "{}: row needs primary key {pk}",
                    c.table
                )));
            }
        }
        for col in row.keys() {
            if !sh.cols.contains(col) {
                return Err(Error::InvalidArgument(format!(
                    "{}: unknown column {col}",
                    c.table
                )));
            }
        }
        let key = key_of(&c.row, &sh.pk);
        let entry = all.entry(c.table.clone()).or_insert_with(|| json!({}));
        match c.op.as_deref().unwrap_or("upsert") {
            "upsert" => {
                let mut merged = entry[&key].as_object().cloned().unwrap_or_default();
                for (k, v) in row {
                    merged.insert(k.clone(), v.clone());
                }
                entry[&key] = Value::Object(merged);
            }
            "delete" => entry[&key] = Value::Null,
            o => return Err(Error::InvalidArgument(format!("op {o}: upsert|delete"))),
        }
    }
    tx.rollback().await.map_err(Error::internal)?;
    sqlx::query(
        "update musubi.economy_changesets set changes = $2, state = 'draft', updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(Value::Object(all))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    load(&s.db, id).await
}

/// `PUT …/{id}/changes {changes: [{table, op?, row}]}` — stage rows (merged into earlier stages).
async fn put_changes(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<ChangesBody>,
) -> Result<Json<Value>, ApiError> {
    let r = apply_changes(&s, parse(&id)?, b.changes).await?;
    let mut v = summary(&r);
    v["changes"] = r.changes.clone();
    Ok(Json(v))
}

async fn drop_change(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, table, key)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let id = parse(&id)?;
    let r = load(&s.db, id).await?;
    if !["draft", "rejected"].contains(&r.state.as_str()) {
        return Err(Error::Conflict(format!("changeset is {}", r.state)).into());
    }
    let mut all = r.changes.as_object().cloned().unwrap_or_default();
    // keys are `"<pk>"` (json-encoded) — accept the bare value too
    let quoted = format!("\"{key}\"");
    let removed = all
        .get_mut(&table)
        .and_then(Value::as_object_mut)
        .map(|m| m.remove(&key).or_else(|| m.remove(&quoted)).is_some())
        .unwrap_or(false);
    if !removed {
        return Err(Error::NotFound(format!("{table}/{key} is not staged")).into());
    }
    if all[&table].as_object().is_some_and(|m| m.is_empty()) {
        all.remove(&table);
    }
    sqlx::query(
        "update musubi.economy_changesets set changes = $2, updated_at = now() where id = $1",
    )
    .bind(id)
    .bind(Value::Object(all))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(summary(&load(&s.db, id).await?)))
}

/// What publishing would change, as a dry-run content import.
async fn diff_route(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    let bundle = bundle_of(&s, &r).await?;
    let res = content::run_import(&s, &bundle, true, true, "", "changeset", None).await?;
    Ok(Json(
        json!({ "changeset": summary(&r), "diff": res["diff"], "changes": res["changes"] }),
    ))
}

async fn validate_route(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    let problems = validate(&s, &r).await?;
    Ok(Json(
        json!({ "ok": problems.is_empty(), "problems": problems }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PreviewQ {
    table: String,
    locale: Option<String>,
}

/// `GET …/{id}/preview?table=store_items&locale=de` — the table as it would be after
/// publishing, with strings of the changeset (and live ones) merged in.
async fn preview(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<PreviewQ>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    if q.table.is_empty() {
        return Err(Error::InvalidArgument("table required".into()).into());
    }
    editable_table(&q.table)?;
    let (mut w, _) = wanted(&s, &r.changes, &q.table).await?;
    let kind = match q.table.as_str() {
        "items" => Some("item"),
        "store_items" => Some("store_item"),
        "currencies" => Some("currency"),
        "reward_tables" => Some("reward_table"),
        _ => None,
    };
    if let Some(kind) = kind {
        let (strings, _) = wanted(&s, &r.changes, "economy_strings").await?;
        let locale = q
            .locale
            .clone()
            .unwrap_or_else(|| s.cfg.economy.default_locale.clone());
        merge_strings(
            &mut w,
            "id",
            kind,
            &locale,
            &s.cfg.economy.default_locale,
            &strings,
        );
    }
    Ok(Json(
        json!({ "table": q.table, "rows": w, "count": w.len() }),
    ))
}

async fn submit(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    if !["draft", "rejected"].contains(&r.state.as_str()) {
        return Err(Error::Conflict(format!("changeset is {}", r.state)).into());
    }
    let problems = validate(&s, &r).await?;
    if !problems.is_empty() {
        return Err(Error::InvalidArgument(format!(
            "changeset does not validate: {}",
            serde_json::to_string(&problems).unwrap_or_default()
        ))
        .into());
    }
    let r = set_state(&s.db, r.id, "in_review", None, Some(""), None).await?;
    emit(
        &s,
        "economy.changeset_submitted",
        &r,
        json!({ "by": caller.name }),
    )
    .await;
    Ok(Json(summary(&r)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewBody {
    note: String,
}

async fn approve(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<ReviewBody>>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    if r.state != "in_review" {
        return Err(
            Error::Conflict(format!("changeset is {}, expected in_review", r.state)).into(),
        );
    }
    if s.cfg.economy.require_review && caller.name == r.author {
        return Err(Error::PermissionDenied(
            "four-eyes: the author cannot approve their own changeset".into(),
        )
        .into());
    }
    let note = body.map(|b| b.0.note).unwrap_or_default();
    let r = set_state(
        &s.db,
        r.id,
        "approved",
        Some(&caller.name),
        Some(&note),
        None,
    )
    .await?;
    emit(
        &s,
        "economy.changeset_approved",
        &r,
        json!({ "by": caller.name }),
    )
    .await;
    Ok(Json(summary(&r)))
}

async fn reject(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<ReviewBody>>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    if !["in_review", "approved", "scheduled"].contains(&r.state.as_str()) {
        return Err(Error::Conflict(format!("changeset is {}", r.state)).into());
    }
    let note = body.map(|b| b.0.note).unwrap_or_default();
    let r = set_state(
        &s.db,
        r.id,
        "rejected",
        Some(&caller.name),
        Some(&note),
        Some(None),
    )
    .await?;
    emit(
        &s,
        "economy.changeset_rejected",
        &r,
        json!({ "by": caller.name }),
    )
    .await;
    Ok(Json(summary(&r)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PublishBody {
    /// Unix seconds; in the future → state `scheduled`, published by the job loop.
    schedule_at: Option<i64>,
}

async fn publish(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
    body: Option<Json<PublishBody>>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    let ok_states: &[&str] = if s.cfg.economy.require_review {
        &["approved", "scheduled"]
    } else {
        &["draft", "in_review", "approved", "scheduled"]
    };
    if !ok_states.contains(&r.state.as_str()) {
        return Err(Error::Conflict(format!(
            "changeset is {}; publishable states: {}",
            r.state,
            ok_states.join("|")
        ))
        .into());
    }
    if s.cfg.economy.require_review
        && r.state == "approved"
        && caller.name == r.author
        && r.reviewer.is_empty()
    {
        return Err(Error::PermissionDenied("four-eyes: approval missing".into()).into());
    }
    let when = body.and_then(|b| b.0.schedule_at);
    let now = OffsetDateTime::now_utc().unix_timestamp();
    if let Some(t) = when.filter(|t| *t > now) {
        if t - now > 365 * 86_400 {
            return Err(Error::InvalidArgument("schedule_at: within a year".into()).into());
        }
        let problems = validate(&s, &r).await?;
        if !problems.is_empty() {
            return Err(Error::InvalidArgument(format!(
                "changeset does not validate: {}",
                serde_json::to_string(&problems).unwrap_or_default()
            ))
            .into());
        }
        let at = OffsetDateTime::from_unix_timestamp(t)
            .map_err(|_| Error::InvalidArgument("schedule_at".into()))?;
        let r = set_state(&s.db, r.id, "scheduled", None, None, Some(Some(at))).await?;
        emit(
            &s,
            "economy.changeset_scheduled",
            &r,
            json!({ "by": caller.name, "schedule_at": t }),
        )
        .await;
        return Ok(Json(summary(&r)));
    }
    Ok(Json(do_publish(&s, &r, &caller.name).await?))
}

async fn cancel(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = load(&s.db, parse(&id)?).await?;
    if r.state == "published" {
        return Err(Error::Conflict(
            "already published; roll the content version back instead".into(),
        )
        .into());
    }
    let r = set_state(&s.db, r.id, "cancelled", None, None, Some(None)).await?;
    emit(
        &s,
        "economy.changeset_cancelled",
        &r,
        json!({ "by": caller.name }),
    )
    .await;
    Ok(Json(summary(&r)))
}

// ---- strings -------------------------------------------------------------------------------

type StringRow = (String, String, String, String, String, Value);

fn string_json(r: &StringRow) -> Value {
    json!({ "kind": r.0, "id": r.1, "locale": r.2, "name": r.3, "description": r.4, "extra": r.5 })
}

/// Set `name`/`description`/`strings` on rows from (changeset or live) string rows, locale
/// first, default locale as fallback, the row's own `name` last.
fn merge_strings(
    rows: &mut [Value],
    id_field: &str,
    kind: &str,
    locale: &str,
    default_locale: &str,
    strings: &[Value],
) {
    for row in rows.iter_mut() {
        let id = row[id_field].as_str().unwrap_or("").to_string();
        let pick = |l: &str| {
            strings
                .iter()
                .find(|x| x["kind"] == kind && x["id"] == id && x["locale"] == l)
        };
        let (loc, def) = (pick(locale), pick(default_locale));
        let field = |f: &str| -> Option<String> {
            loc.and_then(|h| h[f].as_str().filter(|x| !x.is_empty()).map(str::to_string))
                .or_else(|| {
                    def.and_then(|h| h[f].as_str().filter(|x| !x.is_empty()).map(str::to_string))
                })
        };
        let Some(used) = loc.or(def) else { continue };
        if let Some(n) = field("name") {
            row["name"] = json!(n);
        }
        if let Some(d) = field("description") {
            row["description"] = json!(d);
        }
        row["strings"] = json!({ "locale": used["locale"], "extra": used["extra"] });
    }
}

async fn load_strings(
    db: &PgPool,
    kind: &str,
    ids: &[String],
    locales: &[String],
) -> Result<Vec<Value>, Error> {
    let rows: Vec<StringRow> = sqlx::query_as(
        "select kind, id, locale, name, description, extra from musubi.economy_strings
         where kind = $1 and id = any($2) and locale = any($3)",
    )
    .bind(kind)
    .bind(ids)
    .bind(locales)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(rows.iter().map(string_json).collect())
}

/// Localise `rows` (items / store entries) for a player: `locale` first, default locale second.
pub async fn localize(
    s: &AppState,
    kind: &str,
    id_field: &str,
    locale: Option<&str>,
    rows: &mut [Value],
) -> Result<(), Error> {
    let default = s.cfg.economy.default_locale.clone();
    let locale = locale
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| locale_ok(l))
        .unwrap_or_else(|| default.clone());
    let ids: Vec<String> = rows
        .iter()
        .filter_map(|r| r[id_field].as_str().map(str::to_string))
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let strings = load_strings(&s.db, kind, &ids, &[locale.clone(), default.clone()]).await?;
    merge_strings(rows, id_field, kind, &locale, &default, &strings);
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StringsQ {
    kind: Option<String>,
    id: Option<String>,
    locale: Option<String>,
    ids: Option<String>,
}

async fn strings_list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<StringsQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<StringRow> = sqlx::query_as(
        "select kind, id, locale, name, description, extra from musubi.economy_strings
         where ($1::text is null or kind = $1) and ($2::text is null or id = $2) and ($3::text is null or locale = $3)
         order by kind, id, locale limit 5000",
    )
    .bind(&q.kind)
    .bind(&q.id)
    .bind(&q.locale)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "strings": rows.iter().map(string_json).collect::<Vec<_>>() }),
    ))
}

/// `GET /v1/economy/strings?kind=item&ids=a,b&locale=de` — clients fetch catalog strings
/// (locale, falling back to the default locale per id).
async fn player_strings(
    State(s): State<AppState>,
    _me: crate::http::Bearer,
    Query(q): Query<StringsQ>,
) -> Result<Json<Value>, ApiError> {
    let kind = q.kind.clone().unwrap_or_else(|| "item".into());
    if !STRING_KINDS.contains(&kind.as_str()) {
        return Err(Error::InvalidArgument(format!("kind: {}", STRING_KINDS.join("|"))).into());
    }
    let ids: Vec<String> = q
        .ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .take(200)
        .collect();
    if ids.is_empty() {
        return Err(Error::InvalidArgument("ids required (up to 200)".into()).into());
    }
    let mut rows: Vec<Value> = ids.iter().map(|i| json!({ "id": i })).collect();
    localize(&s, &kind, "id", q.locale.as_deref(), &mut rows).await?;
    Ok(Json(json!({ "kind": kind, "strings": rows })))
}

#[derive(Deserialize)]
struct StringsPut {
    /// `{ "de": {name, description, extra?}, "fr": {…} }`
    locales: Map<String, Value>,
}

async fn strings_put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((kind, id)): Path<(String, String)>,
    Json(b): Json<StringsPut>,
) -> Result<Json<Value>, ApiError> {
    if !STRING_KINDS.contains(&kind.as_str()) {
        return Err(Error::InvalidArgument(format!("kind: {}", STRING_KINDS.join("|"))).into());
    }
    if b.locales.is_empty() || b.locales.len() > 64 {
        return Err(Error::InvalidArgument("locales: 1..=64 entries".into()).into());
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for (locale, v) in &b.locales {
        if !locale_ok(locale) {
            return Err(
                Error::InvalidArgument(format!("locale {locale}: 2..=16 chars [a-z0-9-]")).into(),
            );
        }
        let name = v["name"].as_str().unwrap_or("");
        let desc = v["description"].as_str().unwrap_or("");
        if name.len() > 128 || desc.len() > 4096 {
            return Err(
                Error::InvalidArgument("name <= 128, description <= 4096 chars".into()).into(),
            );
        }
        let extra = v.get("extra").cloned().unwrap_or_else(|| json!({}));
        if !extra.is_object() {
            return Err(Error::InvalidArgument("extra: object".into()).into());
        }
        sqlx::query(
            "insert into musubi.economy_strings (kind, id, locale, name, description, extra) values ($1, $2, $3, $4, $5, $6)
             on conflict (kind, id, locale) do update set name = excluded.name, description = excluded.description, extra = excluded.extra, updated_at = now()",
        )
        .bind(&kind)
        .bind(&id)
        .bind(locale)
        .bind(name)
        .bind(desc)
        .bind(&extra)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    let locales: Vec<String> = b.locales.keys().cloned().collect();
    let rows = load_strings(&s.db, &kind, std::slice::from_ref(&id), &locales).await?;
    Ok(Json(json!({ "kind": kind, "id": id, "strings": rows })))
}

async fn strings_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((kind, id, locale)): Path<(String, String, String)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query(
        "delete from musubi.economy_strings where kind = $1 and id = $2 and locale = $3",
    )
    .bind(&kind)
    .bind(&id)
    .bind(&locale)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("{kind}/{id}/{locale}")).into());
    }
    Ok(Json(json!({ "ok": true })))
}

async fn strings_delete_all(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((kind, id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.economy_strings where kind = $1 and id = $2")
        .bind(&kind)
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "ok": true, "deleted": n })))
}

fn csv_field(v: &str) -> String {
    if v.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// Minimal RFC 4180 reader (quotes, doubled quotes, newlines inside quotes).
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if row.iter().any(|f| !f.is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// `GET /v1/server/economy/strings/csv?kind&locale` — `kind,id,locale,name,description` for
/// translators.
async fn strings_csv_export(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<StringsQ>,
) -> Result<axum::response::Response, ApiError> {
    let rows: Vec<StringRow> = sqlx::query_as(
        "select kind, id, locale, name, description, extra from musubi.economy_strings
         where ($1::text is null or kind = $1) and ($2::text is null or locale = $2) order by kind, id, locale",
    )
    .bind(&q.kind)
    .bind(&q.locale)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out = String::from("kind,id,locale,name,description\n");
    for r in &rows {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            csv_field(&r.0),
            csv_field(&r.1),
            csv_field(&r.2),
            csv_field(&r.3),
            csv_field(&r.4)
        ));
    }
    Ok(axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .body(axum::body::Body::from(out))
        .unwrap())
}

/// `POST /v1/server/economy/strings/csv` (text/csv body, header `kind,id,locale,name,description`,
/// `?changeset=<id>` stages the rows instead of writing them live).
async fn strings_csv_import(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<CsvImportQ>,
    body: String,
) -> Result<Json<Value>, ApiError> {
    let rows = parse_csv(&body);
    let Some((header, data)) = rows.split_first() else {
        return Err(Error::InvalidArgument("empty csv".into()).into());
    };
    let col = |n: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(n));
    let (Some(ck), Some(ci), Some(cl), Some(cn)) =
        (col("kind"), col("id"), col("locale"), col("name"))
    else {
        return Err(Error::InvalidArgument(
            "header must contain kind,id,locale,name[,description]".into(),
        )
        .into());
    };
    let cd = col("description");
    if data.len() > 20_000 {
        return Err(Error::InvalidArgument("up to 20000 rows per import".into()).into());
    }
    let mut staged = Vec::new();
    let mut errors = Vec::new();
    for (i, r) in data.iter().enumerate() {
        let get = |c: usize| r.get(c).map(|x| x.trim().to_string()).unwrap_or_default();
        let (kind, id, locale, name) = (get(ck), get(ci), get(cl), get(cn));
        let desc = cd.map(get).unwrap_or_default();
        if !STRING_KINDS.contains(&kind.as_str())
            || id.is_empty()
            || !locale_ok(&locale)
            || name.len() > 128
        {
            errors.push(json!({ "line": i + 2, "problem": "kind/id/locale/name invalid" }));
            continue;
        }
        staged.push(
            json!({ "kind": kind, "id": id, "locale": locale, "name": name, "description": desc }),
        );
    }
    let n = staged.len();
    if let Some(cs) = q.changeset {
        let changes = staged
            .into_iter()
            .map(|row| Change {
                table: "economy_strings".into(),
                op: None,
                row,
            })
            .collect();
        apply_changes(&s, parse(&cs)?, changes).await?;
        return Ok(Json(
            json!({ "staged": n, "changeset": cs, "errors": errors }),
        ));
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for r in &staged {
        sqlx::query(
            "insert into musubi.economy_strings (kind, id, locale, name, description) values ($1, $2, $3, $4, $5)
             on conflict (kind, id, locale) do update set name = excluded.name, description = excluded.description, updated_at = now()",
        )
        .bind(r["kind"].as_str())
        .bind(r["id"].as_str())
        .bind(r["locale"].as_str())
        .bind(r["name"].as_str())
        .bind(r["description"].as_str())
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(Json(json!({ "imported": n, "errors": errors })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CsvImportQ {
    changeset: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip() {
        let text = "kind,id,locale,name,description\nitem,sword,de,Schwert,\"Ein, scharfes\n\"\"Ding\"\"\"\n\nitem,sword,en,Sword,\n";
        let rows = parse_csv(text);
        assert_eq!(rows.len(), 3);
        assert_eq!(
            rows[1],
            vec!["item", "sword", "de", "Schwert", "Ein, scharfes\n\"Ding\""]
        );
        assert_eq!(rows[2], vec!["item", "sword", "en", "Sword", ""]);
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn locale_and_image_rules() {
        assert!(locale_ok("de") && locale_ok("pt-br") && locale_ok("zh-hans"));
        assert!(!locale_ok("D") && !locale_ok("en_US") && !locale_ok(""));
        assert!(image_ok(&json!("https://cdn.example/x.png")));
        assert!(image_ok(&json!("/v1/files/icons/x.png")));
        assert!(!image_ok(&json!("http://cdn.example/x.png")));
        assert!(!image_ok(&json!(3)));
    }

    #[test]
    fn strings_merge_with_fallback() {
        let strings = vec![
            json!({"kind": "item", "id": "sword", "locale": "en", "name": "Sword", "description": "Sharp", "extra": {}}),
            json!({"kind": "item", "id": "sword", "locale": "de", "name": "Schwert", "description": "", "extra": {"flavor": "x"}}),
        ];
        let mut rows = vec![
            json!({"id": "sword", "name": "sword_raw"}),
            json!({"id": "shield", "name": "shield_raw"}),
        ];
        merge_strings(&mut rows, "id", "item", "de", "en", &strings);
        assert_eq!(rows[0]["name"], "Schwert");
        assert_eq!(
            rows[0]["description"], "Sharp",
            "empty de description falls back per field"
        );
        assert_eq!(rows[0]["strings"]["locale"], "de");
        assert_eq!(rows[1]["name"], "shield_raw");
        let mut rows = vec![json!({"id": "sword"})];
        merge_strings(&mut rows, "id", "item", "fr", "en", &strings);
        assert_eq!(rows[0]["name"], "Sword", "falls back to the default locale");
    }
}
