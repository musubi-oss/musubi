//! Operator log tail: `GET /v1/admin/logs` reads this node's in-memory ring (see
//! `crate::system_logs`); `?node=<id>|all` asks other cluster nodes over the bus and merges.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::Error;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;
use crate::system_logs::{self, Line};

pub const REQUEST_TOPIC: &str = "admin.logs.request";
pub const REPLY_TOPIC: &str = "admin.logs.reply";

#[derive(Deserialize, Default)]
#[serde(default)]
struct Q {
    level: Option<String>,
    target: Option<String>,
    q: Option<String>,
    /// RFC 3339.
    since: Option<String>,
    before: Option<u64>,
    limit: Option<usize>,
    /// `all` or a node id; absent = this node only.
    node: Option<String>,
    /// How long to wait for remote nodes (100..=10000, default 2000).
    wait_ms: Option<u64>,
}

impl Q {
    fn inner(&self) -> Result<system_logs::Query, Error> {
        let since = match self.since.as_deref().filter(|s| !s.is_empty()) {
            None => None,
            Some(s) => Some(
                time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                    .map_err(|_| Error::InvalidArgument("since must be RFC 3339".into()))?,
            ),
        };
        Ok(system_logs::Query {
            level: self.level.clone(),
            target: self.target.clone(),
            q: self.q.clone(),
            since,
            before: self.before,
            limit: self.limit,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct Request {
    id: Uuid,
    target: Option<Uuid>,
    reply_to: Uuid,
    q: system_logs::Query,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    id: Uuid,
    node: Uuid,
    node_name: String,
    lines: Vec<Line>,
    held: usize,
    capacity: usize,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/logs", get(logs))
        .route("/v1/admin/logs/nodes", get(nodes))
}

fn local(s: &AppState, q: &system_logs::Query) -> Result<Value, ApiError> {
    let lines = system_logs::query(q).map_err(Error::InvalidArgument)?;
    let (held, seq) = system_logs::stats();
    let (id, name) = s
        .cluster
        .as_ref()
        .map(|c| (c.node_id().to_string(), c.me().name.clone()))
        .unwrap_or_else(|| ("local".into(), s.cfg.node.name.clone()));
    Ok(json!({
        "node": id, "node_name": name, "lines": lines, "held": held, "last_seq": seq,
        "capacity": system_logs::capacity(),
    }))
}

async fn logs(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<Q>,
) -> Result<Json<Value>, ApiError> {
    let inner = q.inner()?;
    system_logs::validate(&inner).map_err(Error::InvalidArgument)?;
    let Some(node) = q.node.as_deref().filter(|n| !n.is_empty()) else {
        return Ok(Json(local(&s, &inner)?));
    };
    let wait = q.wait_ms.unwrap_or(2000);
    if !(100..=10_000).contains(&wait) {
        return Err(Error::InvalidArgument("wait_ms must be 100..=10000".into()).into());
    }
    let Some(c) = s.cluster.clone() else {
        if node == "all" {
            return Ok(Json(
                json!({ "nodes": [local(&s, &inner)?], "missing": [] }),
            ));
        }
        return Err(Error::NotFound(format!("node {node}")).into());
    };
    let me = c.node_id().0;
    let target = if node == "all" {
        None
    } else {
        Some(
            Uuid::parse_str(node)
                .map_err(|_| Error::InvalidArgument("node must be a uuid or `all`".into()))?,
        )
    };
    if target == Some(me) {
        return Ok(Json(local(&s, &inner)?));
    }
    let known: Vec<musubi_cluster::NodeInfo> = c.nodes().await.map_err(Error::internal)?;
    if let Some(t) = target {
        if !known.iter().any(|n| n.id.0 == t) {
            return Err(Error::NotFound(format!("node {t}")).into());
        }
    }
    let mut expect: Vec<Uuid> = known
        .iter()
        .map(|n| n.id.0)
        .filter(|id| *id != me && target.is_none_or(|t| t == *id))
        .collect();
    let req = Request {
        id: Uuid::now_v7(),
        target,
        reply_to: me,
        q: inner.clone(),
    };
    let mut rx = c.subscribe();
    c.publish(REQUEST_TOPIC, &req)
        .await
        .map_err(Error::internal)?;
    let mut out: Vec<Value> = Vec::new();
    if target.is_none() {
        out.push(local(&s, &inner)?);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait);
    while !expect.is_empty() {
        let m = match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(m)) => m,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            _ => break,
        };
        if m.topic != REPLY_TOPIC {
            continue;
        }
        let Ok(r) = m.decode::<Reply>() else {
            continue;
        };
        if r.id != req.id {
            continue;
        }
        expect.retain(|id| *id != r.node);
        out.push(json!({
            "node": r.node, "node_name": r.node_name, "lines": r.lines,
            "held": r.held, "capacity": r.capacity,
        }));
    }
    if target.is_some() {
        return match out.pop() {
            Some(v) => Ok(Json(v)),
            None => Err(
                Error::Unavailable(format!("node {node} did not answer within {wait} ms")).into(),
            ),
        };
    }
    Ok(Json(json!({ "nodes": out, "missing": expect })))
}

async fn nodes(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let Some(c) = s.cluster.clone() else {
        return Ok(Json(
            json!({ "nodes": [{ "id": "local", "name": s.cfg.node.name, "me": true }] }),
        ));
    };
    let me = c.node_id().0;
    let nodes: Vec<Value> = c
        .nodes()
        .await
        .map_err(Error::internal)?
        .into_iter()
        .map(|n| {
            json!({ "id": n.id, "name": n.name, "region": n.region, "roles": n.roles,
                    "version": n.version, "started_at": n.started_at, "me": n.id.0 == me })
        })
        .collect();
    Ok(Json(json!({ "nodes": nodes })))
}

/// Answer other nodes' log requests with this node's ring.
pub fn start_sync(s: AppState) {
    let Some(c) = s.cluster.clone() else {
        return;
    };
    let mut rx = c.subscribe();
    let me = c.node_id().0;
    let name = c.me().name.clone();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) if m.topic == REQUEST_TOPIC && m.origin.0 != me => {
                    let Ok(r) = m.decode::<Request>() else {
                        continue;
                    };
                    if r.target.is_some_and(|t| t != me) {
                        continue;
                    }
                    let lines = system_logs::query(&r.q).unwrap_or_default();
                    let (held, _) = system_logs::stats();
                    let reply = Reply {
                        id: r.id,
                        node: me,
                        node_name: name.clone(),
                        lines,
                        held,
                        capacity: system_logs::capacity(),
                    };
                    let _ = c.publish(REPLY_TOPIC, &reply).await;
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
