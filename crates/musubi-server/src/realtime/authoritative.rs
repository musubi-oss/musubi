//! Registry of running authoritative matches and the handler modules they were created from.

use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use musubi_actor::{MatchHandle, MatchHandler, MatchInfo};
use musubi_core::{Error, Result};
use musubi_proto::{ServerMessage, UserPresence};
use parking_lot::RwLock;

use super::Hub;

#[derive(Default)]
pub struct MatchRegistry {
    handlers: RwLock<HashMap<String, Arc<dyn MatchHandler>>>,
    matches: Arc<DashMap<String, MatchHandle>>,
    by_session: DashMap<musubi_core::SessionId, Vec<String>>,
}

impl MatchRegistry {
    pub fn new() -> Self {
        let r = Self::default();
        r.register("echo", Arc::new(musubi_actor::EchoMatch));
        r
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
        let handler = self
            .handlers
            .read()
            .get(module)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("match module {module}")))?;
        let deliver: musubi_actor::Deliver = Arc::new(move |to, msg: ServerMessage| {
            for s in to {
                hub.send_to_session(s, msg.clone());
            }
        });
        let matches = self.matches.clone();
        let handle = musubi_actor::spawn(
            module,
            handler,
            params,
            deliver,
            Box::new(move |id| {
                matches.remove(&id.to_string());
            }),
        )?;
        self.matches.insert(handle.id.to_string(), handle.clone());
        Ok(handle)
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
