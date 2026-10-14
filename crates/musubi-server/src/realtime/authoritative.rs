//! Registry of running authoritative matches and the handler modules they were created from.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use musubi_actor::{MatchHandle, MatchHandler, MatchInfo, Snapshot, Spawn};
use musubi_core::MatchId;
use musubi_core::{Error, Result};
use musubi_proto::{ServerMessage, UserPresence};
use parking_lot::RwLock;
use sqlx::PgPool;
use uuid::Uuid;

use super::Hub;

pub struct MatchRegistry {
    handlers: RwLock<HashMap<String, Arc<dyn MatchHandler>>>,
    matches: Arc<DashMap<String, MatchHandle>>,
    by_session: DashMap<musubi_core::SessionId, Vec<String>>,
    /// Snapshot store (`musubi.match_snapshots`); `None` = in-memory only.
    db: Option<PgPool>,
    node_id: Uuid,
    /// Periodic snapshot interval in ticks (membership changes and end always snapshot).
    pub snapshot_every: u64,
}

impl Default for MatchRegistry {
    fn default() -> Self {
        Self::new(None, Uuid::nil())
    }
}

impl MatchRegistry {
    pub fn new(db: Option<PgPool>, node_id: Uuid) -> Self {
        let r = Self {
            handlers: RwLock::new(HashMap::new()),
            matches: Arc::new(DashMap::new()),
            by_session: DashMap::new(),
            db,
            node_id,
            snapshot_every: 50,
        };
        r.register("echo", Arc::new(musubi_actor::EchoMatch));
        r
    }

    fn deliver(hub: Arc<Hub>) -> musubi_actor::Deliver {
        Arc::new(move |to, msg: ServerMessage| {
            for s in to {
                hub.send_to_session(s, msg.clone());
            }
        })
    }

    fn on_snapshot(&self) -> Option<musubi_actor::OnSnapshot> {
        let db = self.db.clone()?;
        let node = self.node_id;
        Some(Arc::new(move |id: MatchId, snap: Option<Snapshot>| {
            let db = db.clone();
            tokio::spawn(async move {
                let res = match snap {
                    Some(s) => sqlx::query(
                        "insert into musubi.match_snapshots (match_id, module, label, params, state, tick, node_id, updated_at)
                         values ($1, $2, $3, $4, $5, $6, $7, now())
                         on conflict (match_id) do update set label = excluded.label, state = excluded.state, tick = excluded.tick, node_id = excluded.node_id, updated_at = now()",
                    )
                    .bind(id.0)
                    .bind(&s.module)
                    .bind(&s.label)
                    .bind(&s.params)
                    .bind(&s.state)
                    .bind(s.tick as i64)
                    .bind(node)
                    .execute(&db)
                    .await,
                    None => sqlx::query("delete from musubi.match_snapshots where match_id = $1").bind(id.0).execute(&db).await,
                };
                if let Err(e) = res {
                    tracing::warn!(match_id = %id, error = %e, "match snapshot write failed");
                }
            });
        }))
    }

    fn launch(
        &self,
        hub: Arc<Hub>,
        module: &str,
        params: serde_json::Value,
        id: Option<MatchId>,
        resume: Option<Snapshot>,
    ) -> Result<MatchHandle> {
        let handler = self
            .handlers
            .read()
            .get(module)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("match module {module}")))?;
        let matches = self.matches.clone();
        let handle = musubi_actor::spawn_with(
            handler,
            Spawn {
                id,
                module: module.to_string(),
                params,
                resume,
                deliver: Self::deliver(hub),
                on_end: Box::new(move |id| {
                    matches.remove(&id.to_string());
                }),
                on_snapshot: self.on_snapshot(),
                snapshot_every: self.snapshot_every,
            },
        )?;
        self.matches.insert(handle.id.to_string(), handle.clone());
        Ok(handle)
    }

    /// Resume matches whose snapshots belong to no live node (or to `dead_nodes`). Returns the ids resumed.
    pub async fn recover(&self, hub: Arc<Hub>, dead_nodes: &[Uuid]) -> Result<Vec<String>> {
        let Some(db) = &self.db else {
            return Ok(vec![]);
        };
        let rows = sqlx::query_as::<_, (Uuid, String, String, serde_json::Value, Vec<u8>, i64)>(
            "select match_id, module, label, params, state, tick from musubi.match_snapshots where node_id is null or node_id = any($1)",
        )
        .bind(dead_nodes)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (id, module, label, params, state, tick) in rows {
            let snap = Snapshot {
                module: module.clone(),
                label,
                params: params.clone(),
                state,
                tick: tick as u64,
            };
            match self.launch(hub.clone(), &module, params, Some(MatchId(id)), Some(snap)) {
                Ok(h) => out.push(h.id.to_string()),
                Err(e) => tracing::warn!(match_id = %id, error = %e, "could not resume match"),
            }
        }
        Ok(out)
    }

    pub fn register(&self, module: &str, handler: Arc<dyn MatchHandler>) {
        self.handlers.write().insert(module.to_string(), handler);
    }

    pub fn modules(&self) -> Vec<String> {
        let mut v: Vec<String> = self.handlers.read().keys().cloned().collect();
        v.sort();
        v
    }

    pub fn create(
        &self,
        hub: Arc<Hub>,
        module: &str,
        params: serde_json::Value,
    ) -> Result<MatchHandle> {
        self.launch(hub, module, params, None, None)
    }

    pub fn get(&self, match_id: &str) -> Option<MatchHandle> {
        self.matches
            .get(match_id)
            .map(|h| h.clone())
            .filter(|h| h.is_alive())
    }

    pub async fn join(
        &self,
        me: &UserPresence,
        match_id: &str,
        metadata: HashMap<String, String>,
    ) -> Result<ServerMessage> {
        let h = self
            .get(match_id)
            .ok_or_else(|| Error::NotFound("match".into()))?;
        let snap = h.join(me.clone(), metadata).await?;
        self.by_session
            .entry(me.session_id)
            .or_default()
            .push(match_id.to_string());
        Ok(snap)
    }

    pub fn leave(&self, session: musubi_core::SessionId, match_id: &str) -> Result<()> {
        let h = self
            .get(match_id)
            .ok_or_else(|| Error::NotFound("match".into()))?;
        h.leave(session);
        if let Some(mut v) = self.by_session.get_mut(&session) {
            v.retain(|m| m != match_id);
        }
        Ok(())
    }

    pub fn leave_all(&self, session: musubi_core::SessionId) {
        if let Some((_, ids)) = self.by_session.remove(&session) {
            for id in ids {
                if let Some(h) = self.get(&id) {
                    h.leave(session);
                }
            }
        }
    }

    pub async fn list(
        &self,
        label: Option<&str>,
        min_size: Option<u32>,
        max_size: Option<u32>,
        limit: usize,
    ) -> Vec<MatchInfo> {
        let mut out = Vec::new();
        for h in self
            .matches
            .iter()
            .map(|e| e.value().clone())
            .collect::<Vec<_>>()
        {
            let i = h.info().await;
            if label.is_some_and(|l| i.label != l)
                || min_size.is_some_and(|m| i.size < m)
                || max_size.is_some_and(|m| i.size > m)
            {
                continue;
            }
            out.push(i);
            if out.len() >= limit {
                break;
            }
        }
        out
    }

    pub fn count(&self) -> usize {
        self.matches.len()
    }
}
