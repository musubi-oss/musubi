//! Relayed (client-hosted) matches: the server fans data out between members and tracks presence,
//! nothing more. Authoritative matches live in the actor runtime.

use dashmap::DashMap;
use musubi_core::{Error, MatchId, Result, SessionId};
use musubi_proto::{ServerMessage, UserPresence};

use super::Hub;

pub const MAX_RELAYED_SIZE: usize = 64;
pub const MAX_DATA_BYTES: usize = 16 * 1024;

struct Relayed {
    id: String,
    label: Option<String>,
    members: Vec<UserPresence>,
}

impl Relayed {
    fn snapshot(&self, me: &UserPresence) -> ServerMessage {
        ServerMessage::Match {
            match_id: self.id.clone(),
            authoritative: false,
            label: self.label.clone(),
            size: self.members.len() as u32,
            presences: self.members.clone(),
            me: me.clone(),
        }
    }

    fn sessions(&self) -> Vec<SessionId> {
        self.members.iter().map(|p| p.session_id).collect()
    }
}

/// Matches formed by the matchmaker: `token` → match id, created lazily on first join.
#[derive(Default)]
pub struct RelayedRegistry {
    matches: DashMap<String, Relayed>,
    by_token: DashMap<String, String>,
    by_session: DashMap<SessionId, Vec<String>>,
}

impl RelayedRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Called by matchmaking when a group forms so the token can be redeemed by any member.
    pub fn register_token(&self, token: &str) {
        let id = MatchId::new().to_string();
        self.by_token.insert(token.to_string(), id.clone());
        self.matches.insert(
            id.clone(),
            Relayed {
                id,
                label: None,
                members: vec![],
            },
        );
    }

    pub fn create(&self, me: &UserPresence, name: Option<String>) -> Result<ServerMessage> {
        if name.as_ref().is_some_and(|n| n.chars().count() > 64) {
            return Err(Error::InvalidArgument("match name ≤ 64 chars".into()));
        }
        let id = MatchId::new().to_string();
        let m = Relayed {
            id: id.clone(),
            label: name,
            members: vec![me.clone()],
        };
        let snap = m.snapshot(me);
        self.by_session
            .entry(me.session_id)
            .or_default()
            .push(id.clone());
        self.matches.insert(id, m);
        Ok(snap)
    }

    pub fn join(
        &self,
        hub: &Hub,
        me: &UserPresence,
        match_id: Option<&str>,
        token: Option<&str>,
    ) -> Result<ServerMessage> {
        let id = match (match_id, token) {
            (Some(id), _) => id.to_string(),
            (None, Some(t)) => self
                .by_token
                .get(t)
                .map(|v| v.clone())
                .ok_or_else(|| Error::NotFound("unknown match token".into()))?,
            (None, None) => {
                return Err(Error::InvalidArgument("match_id or token required".into()))
            }
        };
        let mut m = self
            .matches
            .get_mut(&id)
            .ok_or_else(|| Error::NotFound("match".into()))?;
        if m.members.iter().any(|p| p.session_id == me.session_id) {
            return Ok(m.snapshot(me));
        }
        if m.members.len() >= MAX_RELAYED_SIZE {
            return Err(Error::ResourceExhausted("match is full".into()));
        }
        let others = m.sessions();
        m.members.push(me.clone());
        self.by_session
            .entry(me.session_id)
            .or_default()
            .push(id.clone());
        let ev = ServerMessage::MatchPresenceEvent {
            match_id: id,
            joins: vec![me.clone()],
            leaves: vec![],
        };
        for s in others {
            hub.send_to_session(s, ev.clone());
        }
        Ok(m.snapshot(me))
    }

    pub fn leave(&self, hub: &Hub, me: SessionId, match_id: &str) -> Result<()> {
        let mut m = self
            .matches
            .get_mut(match_id)
            .ok_or_else(|| Error::NotFound("match".into()))?;
        let Some(idx) = m.members.iter().position(|p| p.session_id == me) else {
            return Err(Error::NotFound("not in match".into()));
        };
        let gone = m.members.remove(idx);
        if let Some(mut v) = self.by_session.get_mut(&me) {
            v.retain(|id| id != match_id);
        }
        let empty = m.members.is_empty();
        let ev = ServerMessage::MatchPresenceEvent {
            match_id: match_id.into(),
            joins: vec![],
            leaves: vec![gone],
        };
        for s in m.sessions() {
            hub.send_to_session(s, ev.clone());
        }
        drop(m);
        if empty {
            self.matches.remove(match_id);
            self.by_token.retain(|_, v| v != match_id);
        }
        Ok(())
    }

    pub fn leave_all(&self, hub: &Hub, me: SessionId) {
        let Some((_, ids)) = self.by_session.remove(&me) else {
            return;
        };
        for id in ids {
            let _ = self.leave(hub, me, &id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &self,
        hub: &Hub,
        me: &UserPresence,
        match_id: &str,
        op_code: i64,
        data: Vec<u8>,
        only: &[UserPresence],
        reliable: bool,
    ) -> Result<()> {
        if data.len() > MAX_DATA_BYTES {
            return Err(Error::InvalidArgument(format!(
                "match data ≤ {MAX_DATA_BYTES} bytes"
            )));
        }
        let m = self
            .matches
            .get(match_id)
            .ok_or_else(|| Error::NotFound("match".into()))?;
        if !m.members.iter().any(|p| p.session_id == me.session_id) {
            return Err(Error::PermissionDenied("not in match".into()));
        }
        let msg = ServerMessage::MatchData {
            match_id: match_id.into(),
            presence: Some(me.clone()),
            op_code,
            data,
            reliable,
        };
        let targets: Vec<SessionId> = if only.is_empty() {
            m.sessions()
                .into_iter()
                .filter(|s| *s != me.session_id)
                .collect()
        } else {
            m.sessions()
                .into_iter()
                .filter(|s| only.iter().any(|p| p.session_id == *s) && *s != me.session_id)
                .collect()
        };
        for s in targets {
            hub.send_to_session(s, msg.clone());
        }
        Ok(())
    }
}
