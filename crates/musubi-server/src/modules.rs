//! Hot-loadable WASM modules: stored in `musubi.modules`, loaded by every node at startup and
//! re-loaded cluster-wide on `runtime.reload`. `runtime.modules_dir` modules stay file-based.

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::app::AppState;
use crate::realtime::MatchRegistry;

pub const RELOAD_TOPIC: &str = "runtime.reload";
pub const MAX_MODULE_BYTES: usize = 64 << 20;

#[derive(Serialize, Deserialize)]
struct Reload {
    name: String,
    deleted: bool,
}

pub fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "module name: 1-64 chars of [A-Za-z0-9_-]".into(),
        ))
    }
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Load every stored module into `runtime` (startup). Bad modules are logged and skipped so one
/// broken upload cannot keep a node down.
pub async fn load_all(
    db: &PgPool,
    runtime: &std::sync::Arc<musubi_runtime::Runtime>,
    matches: &MatchRegistry,
) -> Result<usize> {
    let rows: Vec<(String, Vec<u8>)> =
        sqlx::query_as("select name, bytes from musubi.modules order by name")
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
    let mut n = 0;
    for (name, bytes) in rows {
        match runtime.load_bytes(&name, &bytes) {
            Ok(_) => n += 1,
            Err(e) => tracing::warn!(module = %name, error = %e, "stored module failed to load"),
        }
    }
    if n > 0 {
        matches.register_wasm(runtime);
    }
    Ok(n)
}

async fn load_one(s: &AppState, name: &str) -> Result<Option<Vec<String>>> {
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("select bytes from musubi.modules where name = $1")
            .bind(name)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((bytes,)) = row else {
        return Ok(None);
    };
    let ids = s.runtime.load_bytes(name, &bytes)?;
    s.matches.register_wasm(&s.runtime);
    Ok(Some(ids))
}

/// Validate (by loading locally), store, and tell the other nodes. Returns the RPC ids.
pub async fn put(s: &AppState, name: &str, bytes: &[u8]) -> Result<Value> {
    valid_name(name)?;
    if bytes.is_empty() {
        return Err(Error::InvalidArgument("empty module".into()));
    }
    let ids = s.runtime.load_bytes(name, bytes)?;
    s.matches.register_wasm(&s.runtime);
    let sha = digest(bytes);
    sqlx::query(
        "insert into musubi.modules (name, bytes, sha256, size, updated_at) values ($1, $2, $3, $4, now())
         on conflict (name) do update set bytes = excluded.bytes, sha256 = excluded.sha256, size = excluded.size, updated_at = now()",
    )
    .bind(name)
    .bind(bytes)
    .bind(&sha)
    .bind(bytes.len() as i32)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    record_version(s, name, bytes, &sha, "active", 0, &[], "").await?;
    broadcast(s, name, false).await;
    s.events
        .emit(
            "module.updated",
            None,
            json!({ "name": name, "sha256": sha, "size": bytes.len(), "rpcs": ids }),
        )
        .await;
    Ok(json!({ "name": name, "sha256": sha, "size": bytes.len(), "rpcs": ids }))
}

pub async fn delete(s: &AppState, name: &str) -> Result<bool> {
    valid_name(name)?;
    let r = sqlx::query("delete from musubi.modules where name = $1")
        .bind(name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    let unloaded = s.runtime.unload(name);
    if r.rows_affected() == 0 && !unloaded {
        return Ok(false);
    }
    broadcast(s, name, true).await;
    s.events
        .emit("module.deleted", None, json!({ "name": name }))
        .await;
    Ok(true)
}

pub async fn list(s: &AppState) -> Result<Value> {
    let rows: Vec<(String, String, i32, time::OffsetDateTime)> =
        sqlx::query_as("select name, sha256, size, updated_at from musubi.modules order by name")
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    let loaded = s.runtime.module_names();
    let stored: Vec<Value> = rows
        .into_iter()
        .map(|(name, sha, size, at)| {
            json!({ "name": name, "sha256": sha, "size": size, "updated_at": at.unix_timestamp(), "loaded": loaded.contains(&name) })
        })
        .collect();
    Ok(json!({ "stored": stored, "loaded": loaded, "rpcs": s.runtime.rpc_ids() }))
}

async fn broadcast(s: &AppState, name: &str, deleted: bool) {
    if let Some(c) = &s.cluster {
        if let Err(e) = c
            .publish(
                RELOAD_TOPIC,
                &Reload {
                    name: name.to_string(),
                    deleted,
                },
            )
            .await
        {
            tracing::warn!(error = %e, module = name, "module reload broadcast failed");
        }
    }
}

/// Follow other nodes' uploads/deletes.
pub fn start_sync(s: AppState) {
    let Some(c) = s.cluster.clone() else {
        return;
    };
    let mut rx = c.subscribe();
    let me = c.node_id();
    tokio::spawn(async move {
        if let Err(e) = load_canaries(&s).await {
            tracing::warn!(error = %e, "canary modules not loaded");
        }
        loop {
            match rx.recv().await {
                Ok(m) if m.topic == RELOAD_TOPIC && m.origin != me => {
                    let Ok(r) = serde_json::from_value::<Reload>(m.payload) else {
                        continue;
                    };
                    if r.deleted {
                        s.runtime.unload(&r.name);
                        tracing::info!(module = %r.name, "module unloaded (cluster)");
                    } else {
                        match load_one(&s, &r.name).await {
                            Ok(Some(ids)) => {
                                tracing::info!(module = %r.name, ?ids, "module reloaded (cluster)");
                                if let Err(e) = sync_canary(&s, &r.name).await {
                                    tracing::warn!(module = %r.name, error = %e, "canary sync failed");
                                }
                            }
                            Ok(None) => {
                                tracing::warn!(module = %r.name, "reload for unknown module")
                            }
                            Err(e) => {
                                tracing::warn!(module = %r.name, error = %e, "module reload failed")
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            }
        }
    });
}

#[allow(clippy::too_many_arguments)]
async fn record_version(
    s: &AppState,
    name: &str,
    bytes: &[u8],
    sha: &str,
    state: &str,
    percent: i32,
    accounts: &[Uuid],
    note: &str,
) -> Result<i32> {
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let next: i32 = sqlx::query_scalar(
        "select coalesce(max(version), 0) + 1 from musubi.module_versions where name = $1",
    )
    .bind(name)
    .fetch_one(&mut *tx)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "update musubi.module_versions set state = 'retired' where name = $1 and state = $2",
    )
    .bind(name)
    .bind(state)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.module_versions (id, name, version, bytes, sha256, size, state, canary_percent, canary_accounts, note)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(Uuid::now_v7())
    .bind(name)
    .bind(next)
    .bind(bytes)
    .bind(sha)
    .bind(bytes.len() as i32)
    .bind(state)
    .bind(percent)
    .bind(accounts)
    .bind(note)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    Ok(next)
}

type CanaryRow = (Vec<u8>, i32, Vec<Uuid>, i32, String);

async fn canary_row(s: &AppState, name: &str) -> Result<Option<CanaryRow>> {
    sqlx::query_as(
        "select bytes, canary_percent, canary_accounts, version, sha256 from musubi.module_versions
         where name = $1 and state = 'canary' order by version desc limit 1",
    )
    .bind(name)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

/// Load (or drop) this node's canary build of `name` to match the database.
pub async fn sync_canary(s: &AppState, name: &str) -> Result<()> {
    match canary_row(s, name).await? {
        Some((bytes, percent, accounts, _, _)) => {
            s.runtime.load_shadow(
                name,
                &bytes,
                percent.clamp(0, 100) as u8,
                accounts.into_iter().map(musubi_core::AccountId).collect(),
            )?;
        }
        None => {
            s.runtime.unload_shadow(name);
        }
    }
    Ok(())
}

pub async fn load_canaries(s: &AppState) -> Result<usize> {
    let names: Vec<(String,)> =
        sqlx::query_as("select distinct name from musubi.module_versions where state = 'canary'")
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    let mut n = 0;
    for (name,) in names {
        match sync_canary(s, &name).await {
            Ok(()) => n += 1,
            Err(e) => tracing::warn!(module = %name, error = %e, "canary not loaded"),
        }
    }
    Ok(n)
}

/// Upload a canary build: `percent` of accounts (stable per account) plus the pinned `accounts`
/// run their RPCs on it; everyone else stays on the active build, on every node.
pub async fn put_canary(
    s: &AppState,
    name: &str,
    bytes: &[u8],
    percent: i32,
    accounts: Vec<Uuid>,
    note: &str,
) -> Result<Value> {
    valid_name(name)?;
    if bytes.is_empty() {
        return Err(Error::InvalidArgument("empty module".into()));
    }
    if !(0..=100).contains(&percent) {
        return Err(Error::InvalidArgument("percent must be 0..=100".into()));
    }
    let ids = s.runtime.load_shadow(
        name,
        bytes,
        percent as u8,
        accounts
            .iter()
            .copied()
            .map(musubi_core::AccountId)
            .collect(),
    )?;
    let sha = digest(bytes);
    let version = record_version(s, name, bytes, &sha, "canary", percent, &accounts, note).await?;
    broadcast(s, name, false).await;
    s.events
        .emit(
            "module.canary",
            None,
            json!({ "name": name, "version": version, "sha256": sha, "percent": percent, "accounts": accounts.len() }),
        )
        .await;
    Ok(
        json!({ "name": name, "version": version, "sha256": sha, "size": bytes.len(), "rpcs": ids,
        "percent": percent, "accounts": accounts.len() }),
    )
}

/// Make the canary the active build everywhere.
pub async fn promote_canary(s: &AppState, name: &str) -> Result<Value> {
    valid_name(name)?;
    let Some((bytes, _, _, version, sha)) = canary_row(s, name).await? else {
        return Err(Error::NotFound(format!("module {name} has no canary")));
    };
    let ids = s.runtime.load_bytes(name, &bytes)?;
    s.runtime.unload_shadow(name);
    s.matches.register_wasm(&s.runtime);
    sqlx::query(
        "insert into musubi.modules (name, bytes, sha256, size, updated_at) values ($1, $2, $3, $4, now())
         on conflict (name) do update set bytes = excluded.bytes, sha256 = excluded.sha256, size = excluded.size, updated_at = now()",
    )
    .bind(name)
    .bind(&bytes)
    .bind(&sha)
    .bind(bytes.len() as i32)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "update musubi.module_versions set state = 'retired' where name = $1 and state = 'active'",
    )
    .bind(name)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "update musubi.module_versions set state = 'active' where name = $1 and version = $2",
    )
    .bind(name)
    .bind(version)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    broadcast(s, name, false).await;
    s.events
        .emit(
            "module.promoted",
            None,
            json!({ "name": name, "version": version, "sha256": sha }),
        )
        .await;
    Ok(json!({ "name": name, "version": version, "sha256": sha, "rpcs": ids }))
}

/// Drop the canary: every account goes back to the active build.
pub async fn drop_canary(s: &AppState, name: &str) -> Result<bool> {
    valid_name(name)?;
    let n = sqlx::query(
        "update musubi.module_versions set state = 'retired' where name = $1 and state = 'canary'",
    )
    .bind(name)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    let had = s.runtime.unload_shadow(name);
    if n == 0 && !had {
        return Ok(false);
    }
    broadcast(s, name, false).await;
    s.events
        .emit("module.canary_dropped", None, json!({ "name": name }))
        .await;
    Ok(true)
}

pub async fn versions(s: &AppState, name: &str) -> Result<Value> {
    type Row = (
        i32,
        String,
        i32,
        String,
        i32,
        Vec<Uuid>,
        String,
        time::OffsetDateTime,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select version, sha256, size, state, canary_percent, canary_accounts, note, created_at
         from musubi.module_versions where name = $1 order by version desc",
    )
    .bind(name)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let canary = s.runtime.shadow_info(name);
    Ok(json!({
        "name": name,
        "canary_loaded": canary.map(|(p, n)| json!({ "percent": p, "accounts": n })),
        "versions": rows.into_iter().map(|(v, sha, size, state, pct, accs, note, at)| json!({
            "version": v, "sha256": sha, "size": size, "state": state, "canary_percent": pct,
            "canary_accounts": accs, "note": note,
            "created_at": at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()
        })).collect::<Vec<_>>(),
    }))
}

/// Make an earlier version the active build again (as a new version).
pub async fn rollback(s: &AppState, name: &str, version: i32) -> Result<Value> {
    valid_name(name)?;
    let row: Option<(Vec<u8>,)> =
        sqlx::query_as("select bytes from musubi.module_versions where name = $1 and version = $2")
            .bind(name)
            .bind(version)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((bytes,)) = row else {
        return Err(Error::NotFound(format!("module {name} version {version}")));
    };
    let mut out = put(s, name, &bytes).await?;
    sqlx::query(
        "update musubi.module_versions set note = $3 where name = $1 and version = (select max(version) from musubi.module_versions where name = $2)",
    )
    .bind(name)
    .bind(name)
    .bind(format!("rollback to v{version}"))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    out["rolled_back_to"] = json!(version);
    Ok(out)
}

/// Dev loop (plan 3.6): poll `dir` every `every` and hot-reload modules whose `*.wasm` / `*.wat`
/// changed — edited or new files are loaded under their file stem (a failing load keeps the old
/// version and is logged), removed files are unloaded; match handlers are re-registered.
pub fn start_module_watch(
    runtime: std::sync::Arc<musubi_runtime::Runtime>,
    matches: std::sync::Arc<crate::realtime::MatchRegistry>,
    dir: std::path::PathBuf,
    every: std::time::Duration,
) -> tokio::task::JoinHandle<()> {
    use std::collections::BTreeMap;
    use std::time::SystemTime;
    fn scan(dir: &std::path::Path) -> BTreeMap<String, (std::path::PathBuf, SystemTime, u64)> {
        let mut out = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "wasm" && ext != "wat" {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            let name = p
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("module")
                .to_string();
            out.insert(
                name,
                (
                    p,
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                    meta.len(),
                ),
            );
        }
        out
    }
    tokio::spawn(async move {
        // start empty: everything present is (re)loaded once, so a file written before the
        // first poll is never missed
        let mut seen: BTreeMap<String, (std::path::PathBuf, SystemTime, u64)> = BTreeMap::new();
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            let now = scan(&dir);
            for (name, (path, mtime, len)) in &now {
                let changed = seen
                    .get(name)
                    .is_none_or(|(_, m, l)| m != mtime || l != len);
                if !changed {
                    continue;
                }
                match std::fs::read(path)
                    .map_err(musubi_core::Error::internal)
                    .and_then(|bytes| runtime.load_bytes(name, &bytes))
                {
                    Ok(rpcs) => {
                        matches.register_wasm(&runtime);
                        tracing::info!(module = %name, rpcs = rpcs.len(), "module hot-reloaded");
                    }
                    Err(e) => {
                        tracing::warn!(module = %name, error = %e, "module reload failed; old version kept")
                    }
                }
            }
            for name in seen.keys() {
                if !now.contains_key(name) && runtime.unload(name) {
                    tracing::info!(module = %name, "module unloaded (file removed)");
                }
            }
            seen = now;
        }
    })
}
