//! Registry of running authoritative matches and the handler modules they were created from.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use musubi_actor::{
    JournalChunk, JournalEntry, MatchHandle, MatchHandler, MatchInfo, Snapshot, Spawn,
};
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
    /// `node.cluster_group`: failover only adopts snapshots of our own cluster.
    group: String,
    /// Outbox for `match.ended`.
    events: std::sync::OnceLock<Arc<crate::events::Events>>,
    /// Periodic snapshot interval in ticks (membership changes and end always snapshot).
    pub snapshot_every: u64,
    /// Journal flush interval in ticks (0 = off); bounds what a failover loses.
    pub journal_every: u64,
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
            group: "default".into(),
            events: std::sync::OnceLock::new(),
            snapshot_every: 50,
            journal_every: 5,
        };
        r.register("echo", Arc::new(musubi_actor::EchoMatch));
        r
    }

    pub fn with_group(mut self, group: &str) -> Self {
        self.group = group.to_string();
        self
    }

    pub fn set_events(&self, events: Arc<crate::events::Events>) {
        let _ = self.events.set(events);
    }

    fn emit(&self, kind: &'static str, payload: serde_json::Value) {
        if let Some(ev) = self.events.get().cloned() {
            tokio::spawn(async move {
                ev.emit(kind, None, payload).await;
            });
        }
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
        let group = self.group.clone();
        let events = self.events.get().cloned();
        Some(Arc::new(move |id: MatchId, snap: Option<Snapshot>| {
            let db = db.clone();
            let group = group.clone();
            let events = events.clone();
            let tick_for_journal = snap.as_ref().map(|s| s.tick);
            if snap.is_none() {
                if let Some(ev) = events {
                    tokio::spawn(async move {
                        ev.emit("match.ended", None, serde_json::json!({ "match_id": id }))
                            .await;
                    });
                }
            }
            tokio::spawn(async move {
                let res = match snap {
                    Some(s) => sqlx::query(
                        "insert into musubi.match_snapshots (match_id, module, label, params, state, tick, node_id, updated_at, cluster_group)
                         values ($1, $2, $3, $4, $5, $6, $7, now(), $8)
                         on conflict (match_id) do update set label = excluded.label, state = excluded.state, tick = excluded.tick, node_id = excluded.node_id, updated_at = now()",
                    )
                    .bind(id.0)
                    .bind(&s.module)
                    .bind(&s.label)
                    .bind(&s.params)
                    .bind(&s.state)
                    .bind(s.tick as i64)
                    .bind(node)
                    .bind(&group)
                    .execute(&db)
                    .await,
                    None => sqlx::query("delete from musubi.match_snapshots where match_id = $1").bind(id.0).execute(&db).await,
                };
                let _ = match tick_for_journal {
                    Some(t) => sqlx::query(
                        "delete from musubi.match_journal where match_id = $1 and tick_to <= $2",
                    )
                    .bind(id.0)
                    .bind(t as i64)
                    .execute(&db)
                    .await,
                    None => {
                        sqlx::query("delete from musubi.match_journal where match_id = $1")
                            .bind(id.0)
                            .execute(&db)
                            .await
                    }
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
                on_journal: self.on_journal(),
                journal_every: self.journal_every,
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
        // Claim atomically so two surviving nodes never resume the same match.
        let rows = sqlx::query_as::<_, (Uuid, String, String, serde_json::Value, Vec<u8>, i64)>(
            "update musubi.match_snapshots set node_id = $2, updated_at = now()
             where cluster_group = $3 and (node_id is null or node_id = any($1))
               and (node_id is null or node_id = $2 or not exists
                    (select 1 from musubi.nodes n where n.id = match_snapshots.node_id
                       and n.heartbeat_at > now() - interval '10 seconds'))
             returning match_id, module, label, params, state, tick",
        )
        .bind(dead_nodes)
        .bind(self.node_id)
        .bind(&self.group)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        // adopt concurrently: journal reads and module launches are independent per match
        let hub_ref = &hub;
        let adopted = futures::future::join_all(rows.into_iter().map(
            |(id, module, label, params, state, tick)| async move {
                let hub = hub_ref.clone();
            let journal: Vec<(serde_json::Value,)> = sqlx::query_as(
                "select entries from musubi.match_journal where match_id = $1 and tick_to > $2 order by tick_from",
            )
            .bind(id)
            .bind(tick)
            .fetch_all(db)
            .await
            .map_err(Error::internal)?;
            let mut replay: Vec<JournalEntry> = Vec::new();
            for (v,) in journal {
                match serde_json::from_value::<Vec<JournalEntry>>(v) {
                    Ok(mut e) => replay.append(&mut e),
                    Err(e) => {
                        tracing::warn!(match_id = %id, error = %e, "bad journal chunk skipped")
                    }
                }
            }
            let snap = Snapshot {
                module: module.clone(),
                label,
                params: params.clone(),
                state,
                tick: tick as u64,
                replay,
            };
            match self.launch(hub.clone(), &module, params, Some(MatchId(id)), Some(snap)) {
                Ok(h) => {
                    self.emit("match.resumed", serde_json::json!({ "match_id": h.id, "module": module, "node_id": self.node_id }));
                    Ok(Some(h.id.to_string()))
                }
                Err(e) => {
                    tracing::warn!(match_id = %id, error = %e, "could not resume match");
                    Ok::<Option<String>, Error>(None)
                }
            }
        },
        ))
        .await;
        for r in adopted {
            if let Some(id) = r? {
                out.push(id);
            }
        }
        Ok(out)
    }

    fn on_journal(&self) -> Option<musubi_actor::OnJournal> {
        let db = self.db.clone()?;
        if self.journal_every == 0 {
            return None;
        }
        Some(Arc::new(move |id: MatchId, chunk: JournalChunk| {
            let db = db.clone();
            tokio::spawn(async move {
                let entries = match serde_json::to_value(&chunk.entries) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(match_id = %id, error = %e, "journal encode failed");
                        return;
                    }
                };
                let r = sqlx::query(
                    "insert into musubi.match_journal (match_id, tick_from, tick_to, entries) values ($1, $2, $3, $4)
                     on conflict (match_id, tick_from) do update set tick_to = excluded.tick_to, entries = excluded.entries",
                )
                .bind(id.0)
                .bind(chunk.tick_from as i64)
                .bind(chunk.tick_to as i64)
                .bind(entries)
                .execute(&db)
                .await;
                if let Err(e) = r {
                    tracing::warn!(match_id = %id, error = %e, "match journal write failed");
                }
            });
        }))
    }

    /// Drain: disown our matches in the snapshot store so the failover leader resumes them
    /// elsewhere (from their last snapshot). Returns how many were handed off.
    pub async fn release_all(&self) -> Result<u64> {
        let Some(db) = &self.db else {
            return Ok(0);
        };
        let r = sqlx::query("update musubi.match_snapshots set node_id = null where node_id = $1")
            .bind(self.node_id)
            .execute(db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected())
    }

    /// Register every `match_<m>_*` module the runtime currently exposes.
    pub fn register_wasm(&self, runtime: &Arc<musubi_runtime::Runtime>) -> Vec<String> {
        let mods = runtime.match_modules();
        for m in &mods {
            self.register(
                m,
                Arc::new(super::wasm_match::WasmMatchHandler::new(runtime.clone(), m)),
            );
        }
        mods
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
        let h = self.launch(hub, module, params.clone(), None, None)?;
        self.emit(
            "match.started",
            serde_json::json!({ "match_id": h.id, "module": module, "params": params }),
        );
        Ok(h)
    }

    pub fn is_member(&self, session: musubi_core::SessionId, match_id: &str) -> bool {
        self.by_session
            .get(&session)
            .is_some_and(|v| v.iter().any(|m| m == match_id))
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
        self.list_query(label, None, min_size, max_size, limit)
            .await
    }

    /// `query` uses the matchmaker syntax over the label: a JSON-object label exposes its fields
    /// as `label.<field>` (and bare `<field>`), any other label as the string `label`.
    pub async fn list_query(
        &self,
        label: Option<&str>,
        query: Option<&musubi_matchmaker::Query>,
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
                || query.is_some_and(|q| !q.matches(&label_props(&i.label)))
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

/// Matchmaker properties derived from a match label (see `MatchRegistry::list_query`).
pub fn label_props(label: &str) -> musubi_matchmaker::Properties {
    let mut p = musubi_matchmaker::Properties::default();
    match serde_json::from_str::<serde_json::Value>(label) {
        Ok(serde_json::Value::Object(map)) => {
            for (k, v) in map {
                let keys = [format!("label.{k}"), k.clone()];
                match v {
                    serde_json::Value::Number(n) => {
                        if let Some(f) = n.as_f64() {
                            for key in keys {
                                p.numbers.insert(key, f);
                            }
                        }
                    }
                    serde_json::Value::Bool(b) => {
                        for key in keys {
                            p.numbers.insert(key.clone(), if b { 1.0 } else { 0.0 });
                            p.strings.insert(key, b.to_string());
                        }
                    }
                    serde_json::Value::String(st) => {
                        for key in keys {
                            p.strings.insert(key, st.clone());
                        }
                    }
                    other => {
                        for key in keys {
                            p.strings.insert(key, other.to_string());
                        }
                    }
                }
            }
        }
        _ => {
            p.strings.insert("label".into(), label.to_string());
        }
    }
    p
}

#[cfg(test)]
mod label_query_tests {
    use super::label_props;
    use musubi_matchmaker::Query;

    #[test]
    fn label_query_filters_matches() {
        let open = label_props(r#"{"mode":"ranked","open":true,"players":3,"region":"eu"}"#);
        let full = label_props(r#"{"mode":"ranked","open":false,"players":8,"region":"us"}"#);
        let plain = label_props("lobby-1");
        let q = Query::parse("+label.mode:ranked +label.open:1 +label.players:<5").unwrap();
        assert!(q.matches(&open));
        assert!(!q.matches(&full));
        assert!(!q.matches(&plain));
        let q2 = Query::parse("+region:us").unwrap();
        assert!(q2.matches(&full) && !q2.matches(&open));
        let q3 = Query::parse("+label:lobby-1").unwrap();
        assert!(q3.matches(&plain));
    }
}
