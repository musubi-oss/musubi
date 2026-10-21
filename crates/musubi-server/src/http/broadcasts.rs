//! Broadcast a notification to every connected player (Pragma "notify all active sessions"):
//! `POST /v1/server/notifications/broadcast` fans out in batches of `batch_size` every
//! `batch_interval_ms` so a maintenance warning to 200k sockets does not stall the node; every
//! cluster node runs the same batched pass for its own sessions (bus topic `notify.broadcast`).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use dashmap::DashMap;
use musubi_core::{AccountId, Error};
use musubi_social::notifications::{NewNotification, MAX_BATCH};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use super::{ApiError, ServerKey};
use crate::app::AppState;

pub const TOPIC: &str = "notify.broadcast";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Broadcast {
    pub id: Uuid,
    pub subject: String,
    pub content: Value,
    pub code: i32,
    /// Also store it in every recipient's inbox (default: live delivery only).
    pub persistent: bool,
    pub ttl_secs: Option<u64>,
    /// Recipients per batch, 1..=1000.
    pub batch_size: usize,
    /// Pause between batches, 0..=60000.
    pub batch_interval_ms: u64,
    pub sender_id: Option<Uuid>,
}

impl Default for Broadcast {
    fn default() -> Self {
        Self {
            id: Uuid::nil(),
            subject: String::new(),
            content: json!({}),
            code: 0,
            persistent: false,
            ttl_secs: None,
            batch_size: 200,
            batch_interval_ms: 100,
            sender_id: None,
        }
    }
}

#[derive(Deserialize)]
struct Req {
    #[serde(flatten)]
    b: Broadcast,
    #[serde(default)]
    dry_run: bool,
}

/// Progress of one broadcast on this node.
pub struct Status {
    pub targets: usize,
    pub sent: AtomicUsize,
    pub batches: AtomicUsize,
    pub errors: AtomicUsize,
    pub done: AtomicU64,
    pub started_at: OffsetDateTime,
}

fn statuses() -> &'static DashMap<Uuid, Arc<Status>> {
    static S: OnceLock<DashMap<Uuid, Arc<Status>>> = OnceLock::new();
    S.get_or_init(DashMap::new)
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/notifications/broadcast", post(start))
        .route("/v1/server/notifications/broadcasts/{id}", get(status))
}

fn validate(b: &Broadcast) -> Result<(), Error> {
    if b.subject.trim().is_empty() || b.subject.len() > 256 {
        return Err(Error::InvalidArgument(
            "subject must be 1..=256 chars".into(),
        ));
    }
    if !(1..=1000).contains(&b.batch_size) {
        return Err(Error::InvalidArgument("batch_size must be 1..=1000".into()));
    }
    if b.batch_interval_ms > 60_000 {
        return Err(Error::InvalidArgument(
            "batch_interval_ms must be <= 60000".into(),
        ));
    }
    if b.content.to_string().len() > 16 * 1024 {
        return Err(Error::InvalidArgument("content must be <= 16 KiB".into()));
    }
    Ok(())
}

fn status_json(id: Uuid, st: &Status) -> Value {
    json!({
        "id": id, "targets": st.targets, "sent": st.sent.load(Ordering::Relaxed),
        "batches": st.batches.load(Ordering::Relaxed), "errors": st.errors.load(Ordering::Relaxed),
        "done": st.done.load(Ordering::Relaxed) == 1,
        "started_at": st.started_at.unix_timestamp(),
    })
}

/// Run one node's share: its own live accounts, batched.
pub fn run_local(s: &AppState, b: Broadcast) -> Arc<Status> {
    let ids = s.hub.online_account_ids();
    let st = Arc::new(Status {
        targets: ids.len(),
        sent: AtomicUsize::new(0),
        batches: AtomicUsize::new(0),
        errors: AtomicUsize::new(0),
        done: AtomicU64::new(0),
        started_at: OffsetDateTime::now_utc(),
    });
    statuses()
        .retain(|_, v| (OffsetDateTime::now_utc() - v.started_at) < time::Duration::hours(24));
    statuses().insert(b.id, st.clone());
    let (s, st2) = (s.clone(), st.clone());
    tokio::spawn(async move {
        for (i, chunk) in ids.chunks(b.batch_size).enumerate() {
            if i > 0 && b.batch_interval_ms > 0 {
                tokio::time::sleep(Duration::from_millis(b.batch_interval_ms)).await;
            }
            for sub in chunk.chunks(MAX_BATCH) {
                let batch: Vec<NewNotification> = sub
                    .iter()
                    .map(|a| NewNotification {
                        user_id: *a,
                        sender_id: b.sender_id.map(AccountId),
                        subject: b.subject.clone(),
                        content: b.content.clone(),
                        code: b.code,
                        persistent: b.persistent,
                        ttl_secs: b.ttl_secs,
                    })
                    .collect();
                match s.notify.send(batch).await {
                    Ok(_) => {
                        st2.sent.fetch_add(sub.len(), Ordering::Relaxed);
                    }
                    Err(e) => {
                        st2.errors.fetch_add(sub.len(), Ordering::Relaxed);
                        tracing::warn!(error = %e, broadcast = %b.id, "broadcast batch failed");
                    }
                }
            }
            st2.batches.fetch_add(1, Ordering::Relaxed);
        }
        st2.done.store(1, Ordering::Relaxed);
    });
    st
}

/// `POST /v1/server/notifications/broadcast {subject, content, code, persistent, ttl_secs,
/// batch_size, batch_interval_ms, dry_run}` — to every player connected anywhere in the cluster.
async fn start(
    State(s): State<AppState>,
    _k: ServerKey,
    body: Option<Json<Req>>,
) -> Result<Json<Value>, ApiError> {
    let Json(r) = body.unwrap_or_else(|| {
        Json(Req {
            b: Broadcast::default(),
            dry_run: false,
        })
    });
    let mut b = r.b;
    validate(&b)?;
    b.id = Uuid::now_v7();
    let local = s.hub.online_account_ids().len();
    if r.dry_run {
        return Ok(Json(json!({
            "dry_run": true, "local_targets": local,
            "batches": local.div_ceil(b.batch_size),
            "estimated_secs": (local.div_ceil(b.batch_size).saturating_sub(1) as u64 * b.batch_interval_ms) as f64 / 1000.0,
        })));
    }
    let mut nodes = 1;
    if let Some(c) = &s.cluster {
        if let Err(e) = c.publish(TOPIC, &b).await {
            tracing::warn!(error = %e, "broadcast relay to other nodes failed");
        } else {
            nodes = 0; // unknown here; other nodes report through their own status
        }
    }
    let st = run_local(&s, b.clone());
    s.events
        .emit(
            "notification.broadcast",
            None,
            json!({ "id": b.id, "subject": b.subject, "code": b.code, "local_targets": st.targets,
                    "batch_size": b.batch_size, "batch_interval_ms": b.batch_interval_ms }),
        )
        .await;
    let mut v = status_json(b.id, &st);
    v["relayed"] = json!(nodes == 0);
    Ok(Json(v))
}

async fn status(
    State(_s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let st = statuses()
        .get(&id)
        .map(|e| e.value().clone())
        .ok_or_else(|| Error::NotFound("broadcast".into()))?;
    Ok(Json(status_json(id, &st)))
}

/// Other nodes' broadcasts: run the batched pass for the sessions connected here.
pub fn start_sync(s: AppState) {
    let Some(c) = s.cluster.clone() else {
        return;
    };
    let mut rx = c.subscribe();
    let me = c.node_id();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) if m.topic == TOPIC && m.origin != me => {
                    if let Ok(b) = m.decode::<Broadcast>() {
                        run_local(&s, b);
                    }
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}
