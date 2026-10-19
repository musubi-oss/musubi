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
/// Bundle sections, each a list of tables in dependency order (parents first).
pub const SECTIONS: &[(&str, &[&str])] = &[
    (
        "content",
        &[
            "currencies",
            "items",
            "reward_tables",
            "progression_tracks",
            "quests",
            "store_items",
            "store_coupons",
            "economy_strings",
            "rating_queues",
            "achievements",
            "crafting_recipes",
            "seasons",
            "streaks",
            "energies",
            "entitlements",
            "iap_products",
            "leaderboards",
        ],
    ),
    (
        "config",
        &[
            "storage_schemas",
            "grant_limits",
            "webhooks",
            "chat_filters",
        ],
    ),
];

fn all_tables() -> Vec<&'static str> {
    SECTIONS
        .iter()
        .flat_map(|(_, t)| t.iter().copied())
        .collect()
}

pub(crate) fn section_of(table: &str) -> Option<&'static str> {
    SECTIONS
        .iter()
        .find(|(_, t)| t.contains(&table))
        .map(|(n, _)| *n)
}

/// Every known section name (for "promote everything").
pub(crate) fn all_sections() -> Vec<String> {
    SECTIONS.iter().map(|(n, _)| n.to_string()).collect()
}

fn tables_of(sections: &[String]) -> Result<Vec<&'static str>, Error> {
    for sc in sections {
        if !SECTIONS.iter().any(|(n, _)| n == sc) {
            return Err(Error::InvalidArgument(format!(
                "unknown section {sc} (known: {})",
                SECTIONS
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    Ok(SECTIONS
        .iter()
        .filter(|(n, _)| sections.iter().any(|sc| sc == n))
        .flat_map(|(_, t)| t.iter().copied())
        .collect())
}

/// The sections a bundle covers: its `sections` field, else inferred from its tables.
fn sections_of(bundle: &Value) -> Vec<String> {
    if let Some(a) = bundle.get("sections").and_then(Value::as_array) {
        let v: Vec<String> = a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if !v.is_empty() {
            return v;
        }
    }
    let mut v: Vec<String> = Vec::new();
    if let Some(t) = bundle.get("tables").and_then(Value::as_object) {
        for name in t.keys() {
            if let Some(sc) = section_of(name) {
                if !v.iter().any(|x| x == sc) {
                    v.push(sc.to_string());
                }
            }
        }
    }
    if v.is_empty() {
        v.push("content".into());
    }
    v
}
/// Bookkeeping columns that are neither exported nor compared.
const SKIP_COLS: &[&str] = &["created_at", "updated_at"];
pub const FORMAT: u64 = 1;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/content", get(export))
        .route(
            "/v1/admin/content/import",
            post(import).layer(axum::extract::DefaultBodyLimit::max(
                CONTENT_IMPORT_MAX_BYTES,
            )),
        )
        .route("/v1/admin/content/versions", get(versions))
        .route("/v1/admin/content/versions/{id}", get(version))
        .route("/v1/admin/content/versions/{id}/rollback", post(rollback))
}

pub(crate) type Tx<'a> = Transaction<'a, Postgres>;

/// Whole-catalog bundles (promotion between environments) are far bigger than ordinary
/// request bodies.
pub(crate) const CONTENT_IMPORT_MAX_BYTES: usize = 256 * 1024 * 1024;
/// id, created_at, source, note, counts, environment, sections
type VersionRow = (i64, String, String, String, Value, String, Vec<String>);
/// created_at, source, note, counts, bundle, environment, sections
type VersionFull = (String, String, String, Value, Value, String, Vec<String>);

pub(crate) struct Shape {
    pub(crate) cols: Vec<String>,
    pub(crate) pk: Vec<String>,
}

fn q(c: &str) -> String {
    format!("\"{}\"", c.replace('"', "\"\""))
}

pub(crate) async fn shape(tx: &mut Tx<'_>, table: &str) -> Result<Shape, Error> {
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

pub(crate) async fn rows(tx: &mut Tx<'_>, table: &str, sh: &Shape) -> Result<Vec<Value>, Error> {
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

pub(crate) async fn export_bundle(
    tx: &mut Tx<'_>,
    sections: &[String],
    environment: &str,
) -> Result<Value, Error> {
    let mut tables = Map::new();
    for t in tables_of(sections)? {
        let sh = shape(tx, t).await?;
        tables.insert(t.to_string(), Value::Array(rows(tx, t, &sh).await?));
    }
    let (at,): (String,) = sqlx::query_as("select now()::text")
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
    Ok(json!({
        "format": FORMAT, "exported_at": at, "environment": environment,
        "sections": sections, "tables": tables,
    }))
}

pub(crate) fn key_of(row: &Value, pk: &[String]) -> String {
    pk.iter()
        .map(|c| row.get(c).map(|v| v.to_string()).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("|")
}

#[derive(Default, Serialize)]
pub(crate) struct TableDiff {
    pub(crate) added: Vec<Value>,
    pub(crate) changed: Vec<Value>,
    pub(crate) removed: Vec<Value>,
}

/// A wanted row may omit columns (defaults apply, existing values stay): only the keys it carries
/// are compared.
pub(crate) fn diff(current: &[Value], wanted: &[Value], pk: &[String]) -> TableDiff {
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
    let all = all_tables();
    for name in tables.keys() {
        if !all.contains(&name.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "unknown content table {name} (known: {})",
                all.join(", ")
            )));
        }
    }
    let mut diffs = Map::new();
    let mut shapes = BTreeMap::new();
    let mut removed: Vec<(&str, Vec<Value>)> = Vec::new();
    for t in &all {
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

pub(crate) async fn run_import(
    s: &AppState,
    bundle: &Value,
    dry_run: bool,
    prune: bool,
    note: &str,
    source: &str,
    target_environment: Option<&str>,
) -> Result<Value, Error> {
    let env = s.cfg.node.environment.as_str();
    if let Some(t) = target_environment.filter(|t| !t.is_empty()) {
        if t != env {
            return Err(Error::Conflict(format!(
                "this cluster is the {env} environment, the import targets {t}"
            )));
        }
    }
    let sections = sections_of(bundle);
    let db = &s.db;
    let mut tx = db.begin().await.map_err(Error::internal)?;
    // one consistent view for before/diff/after: rows other requests commit meanwhile must not
    // show up as changes of this import
    sqlx::query("set transaction isolation level repeatable read")
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    let before = export_bundle(&mut tx, &sections, env).await?;
    let diff = apply(&mut tx, bundle, prune).await?;
    let after = export_bundle(&mut tx, &sections, env).await?;
    // what the bundle itself changed (not what else moved in those tables)
    let changed = diff.values().any(|d| {
        ["added", "changed", "removed"]
            .iter()
            .any(|k| d[k].as_u64().unwrap_or(0) > 0)
    });
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
                "insert into musubi.content_versions (source, note, counts, bundle, environment, sections)
                 values ('before', $1, $2, $3, $4, $5)",
            )
            .bind(format!("state before {source}"))
            .bind(counts(&before))
            .bind(&before)
            .bind(env)
            .bind(&sections)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        let (id,): (i64,) = sqlx::query_as(
            "insert into musubi.content_versions (source, note, counts, bundle, environment, sections)
             values ($1, $2, $3, $4, $5, $6) returning id",
        )
        .bind(source)
        .bind(note)
        .bind(counts(&after))
        .bind(&after)
        .bind(env)
        .bind(&sections)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        version = json!(id);
    }
    tx.commit().await.map_err(Error::internal)?;
    if changed {
        after_import(s, &after).await;
    }
    Ok(
        json!({"applied": changed, "dry_run": false, "changes": changed, "version": version, "diff": diff}),
    )
}

/// Config tables with in-memory state: collection schemas re-create their expression indexes
/// and refresh the cache (chat filters re-read on their own cache TTL).
async fn after_import(s: &AppState, after: &Value) {
    if let Some(rows) = after["tables"]["storage_schemas"].as_array() {
        for r in rows {
            match serde_json::from_value::<musubi_storage::CollectionSchema>(r.clone()) {
                Ok(sc) => {
                    if let Err(e) = s.storage.put_schema(sc).await {
                        tracing::warn!(error = %e, "imported collection schema: indexes not applied");
                    }
                }
                Err(e) => tracing::warn!(error = %e, "imported collection schema row"),
            }
        }
        if let Err(e) = s.storage.load_schemas().await {
            tracing::warn!(error = %e, "collection schemas reload after import");
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ExportQuery {
    /// Comma-separated sections (`content`, `config`); default `content`.
    sections: String,
}

async fn export(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ExportQuery>,
) -> Result<Json<Value>, ApiError> {
    let sections: Vec<String> = if q.sections.trim().is_empty() {
        vec!["content".into()]
    } else {
        q.sections
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    };
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let b = export_bundle(&mut tx, &sections, &s.cfg.node.environment).await?;
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
    /// Environment this import is meant for; refused (409) when this cluster is another one.
    #[serde(default)]
    environment: Option<String>,
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
        run_import(
            &s,
            &b.bundle,
            b.dry_run,
            b.prune,
            &b.note,
            "import",
            b.environment.as_deref(),
        )
        .await?,
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
    let rows: Vec<VersionRow> = sqlx::query_as(
        "select id, created_at::text, source, note, counts, environment, sections
         from musubi.content_versions order by id desc limit $1",
    )
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({"versions": rows.into_iter().map(|(id, at, source, note, counts, env, sections)| {
        json!({"id": id, "created_at": at, "source": source, "note": note, "counts": counts,
            "environment": env, "sections": sections})
    }).collect::<Vec<_>>()}),
    ))
}

async fn load_version(db: &PgPool, id: i64) -> Result<Value, Error> {
    let row: Option<VersionFull> = sqlx::query_as(
        "select created_at::text, source, note, counts, bundle, environment, sections
         from musubi.content_versions where id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?;
    let (at, source, note, counts, bundle, env, sections) =
        row.ok_or_else(|| Error::NotFound(format!("content version {id}")))?;
    Ok(json!({
        "id": id, "created_at": at, "source": source, "note": note, "counts": counts,
        "environment": env, "sections": sections, "bundle": bundle,
    }))
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
        run_import(
            &s,
            &v["bundle"],
            b.dry_run,
            b.prune,
            &note,
            "rollback",
            None,
        )
        .await?,
    ))
}
