//! `musubi export` / `musubi import`: move game data between environments as JSONL
//! (`{"table": ..., "row": {...}}` per line). Tables come from the `musubi` schema itself, ordered
//! by foreign keys, so new tables are covered without touching this file. Import is idempotent
//! (`on conflict do nothing`) unless `--replace`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};

use serde_json::{json, Value};
use sqlx::PgPool;

/// Runtime/ops state that is meaningless in another environment.
pub const TRANSIENT: &[&str] = &[
    "_sqlx_migrations",
    "nodes",
    "leases",
    "sessions",
    "match_snapshots",
    "match_journal",
    "world_journal",
    "events",
    "webhook_deliveries",
    "audit_log",
    "server_keys",
    "game_servers",
];

/// Tables of the `musubi` schema in dependency order (referenced tables first).
pub async fn tables(db: &PgPool, only: Option<&[String]>) -> anyhow::Result<Vec<String>> {
    let names: Vec<String> = sqlx::query_scalar(
        "select c.relname::text from pg_class c join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'musubi' and c.relkind = 'r' order by c.relname",
    )
    .fetch_all(db)
    .await?;
    let edges: Vec<(String, String)> = sqlx::query_as(
        "select c.relname::text, r.relname::text from pg_constraint f
         join pg_class c on c.oid = f.conrelid join pg_class r on r.oid = f.confrelid
         join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'musubi' and f.contype = 'f' and c.oid <> r.oid",
    )
    .fetch_all(db)
    .await?;
    let wanted: BTreeSet<String> = match only {
        Some(list) => list.iter().cloned().collect(),
        None => names
            .iter()
            .filter(|n| !TRANSIENT.contains(&n.as_str()))
            .cloned()
            .collect(),
    };
    for w in &wanted {
        anyhow::ensure!(names.contains(w), "unknown table musubi.{w}");
    }
    let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for n in &wanted {
        deps.entry(n.clone()).or_default();
    }
    for (child, parent) in edges {
        if wanted.contains(&child) && wanted.contains(&parent) {
            deps.get_mut(&child).unwrap().insert(parent);
        }
    }
    let mut out = Vec::new();
    let mut done: BTreeSet<String> = BTreeSet::new();
    while out.len() < wanted.len() {
        let ready: Vec<String> = deps
            .iter()
            .filter(|(n, d)| !done.contains(*n) && d.iter().all(|p| done.contains(p)))
            .map(|(n, _)| n.clone())
            .collect();
        anyhow::ensure!(
            !ready.is_empty(),
            "foreign-key cycle among {:?}",
            deps.keys()
        );
        for n in ready {
            done.insert(n.clone());
            out.push(n);
        }
    }
    Ok(out)
}

pub async fn export(
    db: &PgPool,
    out: &mut dyn Write,
    only: Option<&[String]>,
) -> anyhow::Result<BTreeMap<String, u64>> {
    let mut counts = BTreeMap::new();
    for t in tables(db, only).await? {
        let rows: Vec<String> =
            sqlx::query_scalar(&format!("select row_to_json(t)::text from musubi.{t} t"))
                .fetch_all(db)
                .await?;
        for r in &rows {
            let row: Value = serde_json::from_str(r)?;
            writeln!(out, "{}", json!({ "table": t, "row": row }))?;
        }
        counts.insert(t, rows.len() as u64);
    }
    Ok(counts)
}

pub async fn import(
    db: &PgPool,
    input: &mut dyn BufRead,
    replace: bool,
) -> anyhow::Result<BTreeMap<String, (u64, u64)>> {
    let known = tables(db, None).await?;
    let all: Vec<String> = sqlx::query_scalar(
        "select c.relname::text from pg_class c join pg_namespace n on n.oid = c.relnamespace
         where n.nspname = 'musubi' and c.relkind = 'r'",
    )
    .fetch_all(db)
    .await?;
    let _ = known;
    let mut by_table: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&line)?;
        let t = v["table"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("line without table: {line}"))?
            .to_string();
        anyhow::ensure!(all.contains(&t), "unknown table musubi.{t}");
        by_table.entry(t).or_default().push(v["row"].clone());
    }
    let present: Vec<String> = by_table.keys().cloned().collect();
    let order = tables(db, Some(&present)).await?;
    let mut counts = BTreeMap::new();
    for t in order {
        let rows = by_table.remove(&t).unwrap_or_default();
        let pk: Vec<String> = sqlx::query_scalar(
            "select a.attname::text from pg_index i join pg_attribute a on a.attrelid = i.indrelid and a.attnum = any(i.indkey)
             where i.indrelid = ('musubi.' || $1)::regclass and i.indisprimary",
        )
        .bind(&t)
        .fetch_all(db)
        .await?;
        let cols: Vec<String> = sqlx::query_scalar(
            "select attname::text from pg_attribute where attrelid = ('musubi.' || $1)::regclass and attnum > 0 and not attisdropped order by attnum",
        )
        .bind(&t)
        .fetch_all(db)
        .await?;
        let conflict = if pk.is_empty() {
            String::new()
        } else if replace {
            let sets: Vec<String> = cols
                .iter()
                .filter(|c| !pk.contains(c))
                .map(|c| format!("{c} = excluded.{c}"))
                .collect();
            if sets.is_empty() {
                format!("on conflict ({}) do nothing", pk.join(", "))
            } else {
                format!(
                    "on conflict ({}) do update set {}",
                    pk.join(", "),
                    sets.join(", ")
                )
            }
        } else {
            format!("on conflict ({}) do nothing", pk.join(", "))
        };
        let sql = format!(
            "insert into musubi.{t} select * from json_populate_record(null::musubi.{t}, $1::json) {conflict}"
        );
        let mut tx = db.begin().await?;
        let (mut written, mut skipped) = (0u64, 0u64);
        for row in rows {
            let n = sqlx::query(&sql)
                .bind(row.to_string())
                .execute(&mut *tx)
                .await?
                .rows_affected();
            if n > 0 {
                written += 1;
            } else {
                skipped += 1;
            }
        }
        tx.commit().await?;
        counts.insert(t, (written, skipped));
    }
    Ok(counts)
}
