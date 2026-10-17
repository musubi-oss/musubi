//! Content bundles: the game's definition tables (currencies, items, reward tables, progression
//! tracks, quests, store, coupons, rating queues) as one versioned document. Export from one
//! environment, import into another with a dry-run diff — dev → stage → prod promotion — with a
//! history of applied versions and rollback.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sqlx::{PgPool, Postgres, Transaction};
use std::collections::BTreeMap;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

/// Tables of a bundle, parents before children (foreign keys).
const TABLES: &[&str] = &[
    "currencies",
    "items",
    "reward_tables",
    "progression_tracks",
    "quests",
    "store_items",
    "store_coupons",
    "rating_queues",
];
/// Bookkeeping columns that are neither exported nor compared.
const SKIP_COLS: &[&str] = &["created_at", "updated_at"];
pub const FORMAT: u64 = 1;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/content", get(export))
        .route("/v1/admin/content/import", post(import))
        .route("/v1/admin/content/versions", get(versions))
        .route("/v1/admin/content/versions/{id}", get(version))
        .route("/v1/admin/content/versions/{id}/rollback", post(rollback))
}

type Tx<'a> = Transaction<'a, Postgres>;

struct Shape {
    cols: Vec<String>,
    pk: Vec<String>,
}

fn q(c: &str) -> String {
    format!("\"{}\"", c.replace('"', "\"\""))
}

async fn shape(tx: &mut Tx<'_>, table: &str) -> Result<Shape, Error> {
    let cols: Vec<(String,)> = sqlx::query_as(
        "select column_name::text from information_schema.columns
         where table_schema = 'musubi' and table_name = $1 order by ordinal_position",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await
    .map_err(Error::internal)?;
    let pk: Vec<(String,)> = sqlx::query_as(
        "select a.attname::text from pg_index i
         join pg_attribute a on a.attrelid = i.indrelid and a.attnum = any(i.indkey)
         where i.indrelid = ('musubi.' || $1)::regclass and i.indisprimary order by a.attnum",
    )
    .bind(table)
    .fetch_all(&mut **tx)
    .await
    .map_err(Error::internal)?;
    let cols: Vec<String> = cols
        .into_iter()
        .map(|(c,)| c)
        .filter(|c| !SKIP_COLS.contains(&c.as_str()))
        .collect();
    if cols.is_empty() || pk.is_empty() {
        return Err(Error::internal(format!(
            "content table {table}: no columns or primary key"
        )));
    }
    Ok(Shape {
        cols,
        pk: pk.into_iter().map(|(c,)| c).collect(),
    })
}

async fn rows(tx: &mut Tx<'_>, table: &str, sh: &Shape) -> Result<Vec<Value>, Error> {
    let order = sh.pk.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let sql = format!(
        "select to_jsonb(t) - 'created_at' - 'updated_at' from musubi.{} t order by {order}",
        q(table)
    );
    let v: Vec<(Value,)> = sqlx::query_as(&sql)
        .fetch_all(&mut **tx)
        .await
        .map_err(Error::internal)?;
    Ok(v.into_iter().map(|(r,)| r).collect())
}

async fn export_bundle(tx: &mut Tx<'_>) -> Result<Value, Error> {
    let mut tables = Map::new();
    for t in TABLES {
        let sh = shape(tx, t).await?;
        tables.insert(t.to_string(), Value::Array(rows(tx, t, &sh).await?));
    }
    let (at,): (String,) = sqlx::query_as("select now()::text")
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
    Ok(json!({"format": FORMAT, "exported_at": at, "tables": tables}))
}

fn key_of(row: &Value, pk: &[String]) -> String {
    pk.iter()
        .map(|c| row.get(c).map(|v| v.to_string()).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Default, Serialize)]
struct TableDiff {
    added: Vec<Value>,
    changed: Vec<Value>,
    removed: Vec<Value>,
}

/// A wanted row may omit columns (defaults apply, existing values stay): only the keys it carries
/// are compared.
fn diff(current: &[Value], wanted: &[Value], pk: &[String]) -> TableDiff {
    let cur: BTreeMap<String, &Value> = current.iter().map(|r| (key_of(r, pk), r)).collect();
    let mut d = TableDiff::default();
    let mut seen = BTreeMap::new();
    for w in wanted {
        let k = key_of(w, pk);
        seen.insert(k.clone(), ());
        match cur.get(&k) {
            None => d.added.push(w.clone()),
            Some(c) => {
                let same = w
                    .as_object()
                    .is_some_and(|m| m.iter().all(|(col, v)| c.get(col) == Some(v)));
                if !same {
                    d.changed.push(json!({"key": k, "from": c, "to": w}));
                }
            }
        }
    }
    for (k, c) in &cur {
        if !seen.contains_key(k) {
            d.removed.push((*c).clone());
        }
    }
    d
}

async fn upsert(tx: &mut Tx<'_>, table: &str, sh: &Shape, row: &Value) -> Result<(), Error> {
    let obj = row
        .as_object()
        .ok_or_else(|| Error::InvalidArgument(format!("{table}: every row must be an object")))?;
    for k in obj.keys() {
        if !sh.cols.contains(k) {
            return Err(Error::InvalidArgument(format!(
                "{table}: unknown column {k}"
            )));
        }
    }
    for p in &sh.pk {
        if !obj.contains_key(p) {
            return Err(Error::InvalidArgument(format!(
                "{table}: row without primary key {p}"
            )));
        }
    }
    let cols: Vec<String> = sh
        .cols
        .iter()
        .filter(|c| obj.contains_key(*c))
        .map(|c| q(c))
        .collect();
    let set: Vec<String> = sh
        .cols
        .iter()
        .filter(|c| obj.contains_key(*c) && !sh.pk.contains(c))
        .map(|c| format!("{0} = excluded.{0}", q(c)))
        .collect();
    let conflict = sh.pk.iter().map(|c| q(c)).collect::<Vec<_>>().join(", ");
    let action = if set.is_empty() {
        "nothing".to_string()
    } else {
        format!("update set {}", set.join(", "))
    };
    let list = cols.join(", ");
    let sql = format!(
        "insert into musubi.{t} ({list}) select {list} from jsonb_populate_record(null::musubi.{t}, $1) on conflict ({conflict}) do {action}",
        t = q(table)
    );
    sqlx::query(&sql)
        .bind(row)
        .execute(&mut **tx)
        .await
        .map_err(|e| Error::Conflict(format!("{table} {}: {e}", key_of(row, &sh.pk))))?;
    Ok(())
}

async fn delete(tx: &mut Tx<'_>, table: &str, sh: &Shape, row: &Value) -> Result<(), Error> {
    let cond = sh
        .pk
        .iter()
        .map(|c| format!("x.{0} = r.{0}", q(c)))
        .collect::<Vec<_>>()
        .join(" and ");
    let sql = format!(
        "delete from musubi.{t} x using jsonb_populate_record(null::musubi.{t}, $1) r where {cond}",
        t = q(table)
    );
    sqlx::query(&sql)
        .bind(row)
        .execute(&mut **tx)
        .await
        .map_err(|e| Error::Conflict(format!("{table} {}: {e}", key_of(row, &sh.pk))))?;
    Ok(())
}

/// Apply a bundle inside `tx`: upserts parents first, prunes (rows missing from the bundle)
/// children first. Returns the per-table diff against the state before.
async fn apply(tx: &mut Tx<'_>, bundle: &Value, prune: bool) -> Result<Map<String, Value>, Error> {
    if bundle.get("format").and_then(Value::as_u64) != Some(FORMAT) {
        return Err(Error::InvalidArgument(format!(
            "bundle.format must be {FORMAT}"
        )));
    }
    let tables = bundle
        .get("tables")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::InvalidArgument("bundle.tables: object of table → rows".into()))?;
    for name in tables.keys() {
        if !TABLES.contains(&name.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "unknown content table {name} (known: {})",
                TABLES.join(", ")
            )));
        }
    }
    let mut diffs = Map::new();
    let mut shapes = BTreeMap::new();
    let mut removed: Vec<(&str, Vec<Value>)> = Vec::new();
    for t in TABLES {
        let Some(wanted) = tables.get(*t) else {
            continue;
        };
        let wanted = wanted
            .as_array()
            .ok_or_else(|| Error::InvalidArgument(format!("{t}: array of rows")))?;
        let sh = shape(tx, t).await?;
        let current = rows(tx, t, &sh).await?;
        let d = diff(&current, wanted, &sh.pk);
        for r in wanted {
            upsert(tx, t, &sh, r).await?;
        }
        removed.push((t, d.removed.clone()));
        diffs.insert(
            t.to_string(),
            json!({
                "added": d.added.len(), "changed": d.changed.len(),
                "removed": if prune { d.removed.len() } else { 0 },
                "kept": if prune { 0 } else { d.removed.len() },
                "rows": {"added": d.added, "changed": d.changed, "removed": d.removed},
            }),
        );
        shapes.insert(*t, sh);
    }
    if prune {
        for (t, rows) in removed.iter().rev() {
            for r in rows {
                delete(tx, t, &shapes[t], r).await?;
            }
        }
    }
    Ok(diffs)
}

fn counts(bundle: &Value) -> Value {
    let mut m = Map::new();
    if let Some(t) = bundle["tables"].as_object() {
        for (k, v) in t {
            m.insert(k.clone(), json!(v.as_array().map(|a| a.len()).unwrap_or(0)));
        }
    }
    Value::Object(m)
}

async fn run_import(
    db: &PgPool,
    bundle: &Value,
    dry_run: bool,
    prune: bool,
    note: &str,
    source: &str,
) -> Result<Value, Error> {
    let mut tx = db.begin().await.map_err(Error::internal)?;
    let before = export_bundle(&mut tx).await?;
    let diff = apply(&mut tx, bundle, prune).await?;
    let after = export_bundle(&mut tx).await?;
    let changed = before["tables"] != after["tables"];
    if dry_run {
        tx.rollback().await.map_err(Error::internal)?;
        return Ok(json!({"applied": false, "dry_run": true, "changes": changed, "diff": diff}));
    }
    let mut version = Value::Null;
    if changed {
        let latest: Option<(Value,)> =
            sqlx::query_as("select bundle from musubi.content_versions order by id desc limit 1")
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        if latest.is_none_or(|(b,)| b["tables"] != before["tables"]) {
            sqlx::query(
                "insert into musubi.content_versions (source, note, counts, bundle) values ('before', $1, $2, $3)",
            )
            .bind(format!("state before {source}"))
            .bind(counts(&before))
            .bind(&before)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let (id,): (i64,) = sqlx::query_as(
            "insert into musubi.content_versions (source, note, counts, bundle) values ($1, $2, $3, $4) returning id",
        )
        .bind(source)
        .bind(note)
        .bind(counts(&after))
        .bind(&after)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        version = json!(id);
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(
        json!({"applied": changed, "dry_run": false, "changes": changed, "version": version, "diff": diff}),
    )
}

async fn export(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let b = export_bundle(&mut tx).await?;
    tx.rollback().await.map_err(Error::internal)?;
    Ok(Json(b))
}

#[derive(Deserialize)]
struct ImportBody {
    bundle: Value,
    #[serde(default)]
    dry_run: bool,
    /// Delete rows of the bundled tables that the bundle does not list (default: keep them).
    #[serde(default)]
    prune: bool,
    #[serde(default)]
    note: String,
}

async fn import(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(b): Json<ImportBody>,
) -> Result<Json<Value>, ApiError> {
    if b.note.len() > 500 {
        return Err(Error::InvalidArgument("note: at most 500 chars".into()).into());
    }
    Ok(Json(
        run_import(&s.db, &b.bundle, b.dry_run, b.prune, &b.note, "import").await?,
    ))
}

#[derive(Deserialize)]
struct ListQuery {
    limit: Option<i64>,
}

async fn versions(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(qr): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = qr.limit.unwrap_or(50).clamp(1, 500);
    let rows: Vec<(i64, String, String, String, Value)> = sqlx::query_as(
        "select id, created_at::text, source, note, counts from musubi.content_versions order by id desc limit $1",
    )
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({"versions": rows.into_iter().map(|(id, at, source, note, counts)| {
        json!({"id": id, "created_at": at, "source": source, "note": note, "counts": counts})
    }).collect::<Vec<_>>()}),
    ))
}

async fn load_version(db: &PgPool, id: i64) -> Result<Value, Error> {
    let row: Option<(String, String, String, Value, Value)> = sqlx::query_as(
        "select created_at::text, source, note, counts, bundle from musubi.content_versions where id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?;
    let (at, source, note, counts, bundle) =
        row.ok_or_else(|| Error::NotFound(format!("content version {id}")))?;
    Ok(
        json!({"id": id, "created_at": at, "source": source, "note": note, "counts": counts, "bundle": bundle}),
    )
}

async fn version(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(load_version(&s.db, id).await?))
}

#[derive(Deserialize, Default)]
struct RollbackBody {
    #[serde(default)]
    note: String,
    /// Also delete rows added since that version (default: only restore what it contains).
    #[serde(default)]
    prune: bool,
    #[serde(default)]
    dry_run: bool,
}

async fn rollback(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<RollbackBody>>,
) -> Result<Json<Value>, ApiError> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let v = load_version(&s.db, id).await?;
    let note = if b.note.is_empty() {
        format!("rollback to version {id}")
    } else {
        b.note
    };
    Ok(Json(
        run_import(&s.db, &v["bundle"], b.dry_run, b.prune, &note, "rollback").await?,
    ))
}
