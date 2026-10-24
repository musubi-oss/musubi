//! `GET /v1/admin/search?q=&limit=` — one query for the console's global search: accounts
//! (username / e-mail / id prefix), worlds and matches by name, cluster nodes, operator keys, and
//! when `q` is a UUID, the entity with that id in any world hosted on this node.
use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};
use crate::realtime::world::QueryKind;

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/admin/search", get(search))
}

#[derive(Deserialize)]
struct Q {
    q: String,
    limit: Option<i64>,
}

async fn search(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<Q>,
) -> Result<Json<Value>, ApiError> {
    let needle = q.q.trim();
    let limit = q.limit.unwrap_or(8).clamp(1, 50);
    if needle.len() < 2 {
        return Ok(Json(
            json!({ "accounts": [], "worlds": [], "matches": [], "nodes": [], "keys": [] }),
        ));
    }
    let like = format!("%{}%", needle.replace('%', "\\%").replace('_', "\\_"));
    let lower = needle.to_lowercase();

    let accounts = s
        .accounts
        .search_by(Some(needle), None, false, limit, None)
        .await?;

    let worlds: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "select name, node_id from musubi.worlds where cluster_group = $1 and name ilike $2 order by name limit $3",
    )
    .bind(&s.cfg.node.cluster_group)
    .bind(&like)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let worlds: Vec<Value> = worlds
        .into_iter()
        .map(|(name, node_id)| json!({ "name": name, "node_id": node_id }))
        .collect();

    let matches: Vec<(Uuid, String, String, i64, Option<Uuid>)> = sqlx::query_as(
        "select match_id, module, label, tick, node_id from musubi.match_snapshots
         where cluster_group = $1 and (label ilike $2 or module ilike $2 or match_id::text like $3)
         order by updated_at desc limit $4",
    )
    .bind(&s.cfg.node.cluster_group)
    .bind(&like)
    .bind(format!("{lower}%"))
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let matches: Vec<Value> = matches
        .into_iter()
        .map(|(match_id, module, label, tick, node_id)| {
            json!({ "match_id": match_id, "module": module, "label": label, "tick": tick, "node_id": node_id })
        })
        .collect();

    let nodes: Vec<Value> = match &s.cluster {
        Some(c) => c
            .nodes()
            .await?
            .into_iter()
            .filter(|n| {
                n.name.to_lowercase().contains(&lower)
                    || n.id.0.to_string().starts_with(&lower)
                    || n.region.to_lowercase() == lower
            })
            .take(limit as usize)
            .map(|n| json!({ "id": n.id, "name": n.name, "region": n.region, "alive": n.alive }))
            .collect(),
        None => vec![],
    };

    let keys: Vec<Value> = crate::keys::ServerKeys::list(&s.db)
        .await?
        .into_iter()
        .filter(|k| {
            k.get("name")
                .and_then(Value::as_str)
                .map(|n| n.to_lowercase().contains(&lower))
                .unwrap_or(false)
                || k.get("id")
                    .and_then(Value::as_str)
                    .map(|i| i.starts_with(&lower))
                    .unwrap_or(false)
        })
        .take(limit as usize)
        .collect();

    let entity = match Uuid::parse_str(needle) {
        Ok(id) => {
            let mut found = None;
            for w in s.worlds.list() {
                if let Ok(v) = s
                    .worlds
                    .query(&w, QueryKind::Entity { id }, &s.fanout)
                    .await
                {
                    let hit = v.get("entity").map(|e| !e.is_null()).unwrap_or(false)
                        || v.get("records")
                            .and_then(Value::as_array)
                            .map(|r| !r.is_empty())
                            .unwrap_or(false);
                    if hit {
                        found = Some(json!({ "id": id, "world": w }));
                        break;
                    }
                }
            }
            found
        }
        Err(_) => None,
    };

    Ok(Json(json!({
        "accounts": accounts, "worlds": worlds, "matches": matches, "nodes": nodes, "keys": keys, "entity": entity,
    })))
}
