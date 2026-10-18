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
    /// Set by `release_all`: no snapshot may re-own a row this node has just disowned.
    draining: Arc<std::sync::atomic::AtomicBool>,
    epoch: i64,
    /// Matches created with `params.record = true`: every journal chunk is also kept for replay.
    recording: Arc<std::sync::Mutex<std::collections::HashSet<Uuid>>>,
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
            draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            epoch: 0,
            recording: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
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
        let draining = self.draining.clone();
        let epoch = self.epoch;
        let recording = self.recording.clone();
        Some(Arc::new(move |id: MatchId, snap: Option<Snapshot>| {
            let db = db.clone();
            let group = group.clone();
            let events = events.clone();
            let tick_for_journal = snap.as_ref().map(|s| s.tick);
            match &snap {
                Some(s) if recorded(&s.params) => {
                    let db = db.clone();
                    let (module, label, params, state, tick) = (
                        s.module.clone(),
                        s.label.clone(),
                        s.params.clone(),
                        s.state.clone(),
                        s.tick as i64,
                    );
                    tokio::spawn(async move {
                        let r = sqlx::query(
                            "insert into musubi.match_recordings (match_id, module, label, params, last_tick)
                             values ($1, $2, $3, $4, $5)
                             on conflict (match_id) do update set label = excluded.label, last_tick = greatest(musubi.match_recordings.last_tick, excluded.last_tick)",
                        )
                        .bind(id.0)
                        .bind(&module)
                        .bind(&label)
                        .bind(&params)
                        .bind(tick)
                        .execute(&db)
                        .await;
                        let r = match r {
                            Ok(_) => sqlx::query(
                                "insert into musubi.match_recording_frames (match_id, kind, tick_from, tick_to, state)
                                 values ($1, 'snapshot', $2, $2, $3) on conflict do nothing",
                            )
                            .bind(id.0)
                            .bind(tick)
                            .bind(&state)
                            .execute(&db)
                            .await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = r {
                            tracing::warn!(match_id = %id, error = %e, "replay snapshot write failed");
                        }
                    });
                }
                None if recording.lock().unwrap().remove(&id.0) => {
                    let db = db.clone();
                    tokio::spawn(async move {
                        let _ = sqlx::query(
                            "update musubi.match_recordings set ended_at = now() where match_id = $1 and ended_at is null",
                        )
                        .bind(id.0)
                        .execute(&db)
                        .await;
                    });
                }
                _ => {}
            }
            if snap.is_none() {
                if let Some(ev) = events {
                    tokio::spawn(async move {
                        ev.emit("match.ended", None, serde_json::json!({ "match_id": id }))
                            .await;
                    });
                }
            }
            if draining.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::spawn(async move {
                let res = match snap {
                    Some(s) => sqlx::query(
                        "insert into musubi.match_snapshots (match_id, module, label, params, state, tick, node_id, updated_at, cluster_group, node_epoch)
                         values ($1, $2, $3, $4, $5, $6, $7, now(), $8, $9)
                         on conflict (match_id) do update set label = excluded.label, state = excluded.state, tick = excluded.tick, node_id = case when musubi.match_snapshots.node_id is null or musubi.match_snapshots.node_id = excluded.node_id then excluded.node_id else musubi.match_snapshots.node_id end, node_epoch = case when musubi.match_snapshots.node_id is null or musubi.match_snapshots.node_id = excluded.node_id then excluded.node_epoch else musubi.match_snapshots.node_epoch end, updated_at = now()
                         where musubi.match_snapshots.node_id is null or musubi.match_snapshots.node_id = excluded.node_id",
                    )
                    .bind(id.0)
                    .bind(&s.module)
                    .bind(&s.label)
                    .bind(&s.params)
                    .bind(&s.state)
                    .bind(s.tick as i64)
                    .bind(node)
                    .bind(&group)
                    .bind(epoch)
                    .execute(&db)
                    .await,
                    None => sqlx::query("delete from musubi.match_snapshots where match_id = $1 and (node_id is null or node_id = $2)").bind(id.0).bind(node).execute(&db).await,
                };
                // a node that lost the match to another one (its row was claimed) must not touch it
                let fenced = match &res {
                    Ok(r) => r.rows_affected() == 0,
                    Err(_) => false,
                };
                if fenced {
                    tracing::warn!(match_id = %id, node = %node, "match snapshot fenced: another node owns the match");
                    return;
                }
                let _ = match tick_for_journal {
                    Some(t) => sqlx::query(
                        "delete from musubi.match_journal where match_id = $1 and tick_to <= $2
                         and not exists (select 1 from musubi.match_snapshots s where s.match_id = $1 and s.node_id <> $3)",
                    )
                    .bind(id.0)
                    .bind(t as i64)
                    .bind(node)
                    .execute(&db)
                    .await,
                    None => {
                        sqlx::query(
                            "delete from musubi.match_journal where match_id = $1
                             and not exists (select 1 from musubi.match_snapshots s where s.match_id = $1 and s.node_id <> $2)",
                        )
                        .bind(id.0)
                        .bind(node)
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
        let record = recorded(&params);
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
        if record {
            self.recording.lock().unwrap().insert(handle.id.0);
        }
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
            "update musubi.match_snapshots set node_id = $2, node_epoch = $4, updated_at = now()
             where cluster_group = $3 and (node_id is null or node_id = any($1)
                    or (node_id = $2 and node_epoch <> $4)
                    or exists (select 1 from musubi.nodes n where n.id = match_snapshots.node_id and n.epoch > match_snapshots.node_epoch
                                 and n.started_at < now() - interval '10 seconds'))
               and (node_id is null or node_id = $2 or node_id = any($1) or not exists
                    (select 1 from musubi.nodes n where n.id = match_snapshots.node_id
                       and n.heartbeat_at > now() - interval '10 seconds'
                       and (n.epoch <= match_snapshots.node_epoch or n.started_at > now() - interval '10 seconds')))
             returning match_id, module, label, params, state, tick",
        )
        .bind(dead_nodes)
        .bind(self.node_id)
        .bind(&self.group)
        .bind(self.epoch)
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
        let recording = self.recording.clone();
        let node = self.node_id;
        Some(Arc::new(move |id: MatchId, chunk: JournalChunk| {
            let db = db.clone();
            let record = recording.lock().unwrap().contains(&id.0);
            tokio::spawn(async move {
                let entries = match serde_json::to_value(&chunk.entries) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(match_id = %id, error = %e, "journal encode failed");
                        return;
                    }
                };
                if record {
                    let r = sqlx::query(
                        "insert into musubi.match_recording_frames (match_id, kind, tick_from, tick_to, entries, node_id)
                         select $1, 'journal', $2, $3, $4, $5
                         where not exists (select 1 from musubi.match_snapshots s where s.match_id = $1 and s.node_id <> $5)
                         on conflict do nothing",
                    )
                    .bind(id.0)
                    .bind(chunk.tick_from as i64)
                    .bind(chunk.tick_to as i64)
                    .bind(&entries)
                    .bind(node)
                    .execute(&db)
                    .await;
                    if let Err(e) = r {
                        tracing::warn!(match_id = %id, error = %e, "replay journal write failed");
                    }
                }
                let r = sqlx::query(
                    "insert into musubi.match_journal (match_id, tick_from, tick_to, entries, node_id)
                     select $1, $2, $3, $4, $5
                     where not exists (select 1 from musubi.match_snapshots s where s.match_id = $1 and s.node_id <> $5)
                     on conflict (match_id, tick_from) do update set tick_to = excluded.tick_to, entries = excluded.entries, node_id = excluded.node_id",
                )
                .bind(id.0)
                .bind(chunk.tick_from as i64)
                .bind(chunk.tick_to as i64)
                .bind(entries)
                .bind(node)
                .execute(&db)
                .await;
                match r {
                    Err(e) => {
                        tracing::warn!(match_id = %id, error = %e, "match journal write failed")
                    }
                    Ok(r) if r.rows_affected() == 0 => {
                        tracing::warn!(match_id = %id, node = %node, "match journal fenced: another node owns the match")
                    }
                    Ok(_) => {}
                }
            });
        }))
    }

    /// Drain: disown our matches in the snapshot store so the failover leader resumes them
    /// elsewhere (from their last snapshot). Returns how many were handed off.
    /// Boot epoch written with every snapshot this node owns (see migration 0047).
    pub fn with_epoch(mut self, epoch: i64) -> Self {
        self.epoch = epoch;
        self
    }

    pub async fn release_all(&self) -> Result<u64> {
        let Some(db) = &self.db else {
            return Ok(0);
        };
        self.draining
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

    /// The handler registered for `module`, if this node loaded it.
    pub fn handler(&self, module: &str) -> Option<Arc<dyn MatchHandler>> {
        self.handlers.read().get(module).cloned()
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
        spectator: bool,
    ) -> Result<ServerMessage> {
        let h = self
            .get(match_id)
            .ok_or_else(|| Error::NotFound("match".into()))?;
        let snap = h.join(me.clone(), metadata, spectator).await?;
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

/// `params.record == true` keeps every snapshot and journal chunk of the match for replay.
fn recorded(params: &serde_json::Value) -> bool {
    params
        .get("record")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
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
