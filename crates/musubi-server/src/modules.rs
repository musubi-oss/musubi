//! Hot-loadable WASM modules: stored in `musubi.modules`, loaded by every node at startup and
//! re-loaded cluster-wide on `runtime.reload`. `runtime.modules_dir` modules stay file-based.

use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;

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
                                tracing::info!(module = %r.name, ?ids, "module reloaded (cluster)")
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
