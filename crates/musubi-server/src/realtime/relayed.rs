//! Relayed (client-hosted) matches: the server fans data out between members and tracks presence,
//! nothing more. Authoritative matches live in the actor runtime. Like parties, a relayed match is
//! homed on one node (`<uuid>.<node>`); members elsewhere reach it over the bus (`RelayedCmd`).

use dashmap::DashMap;
use musubi_core::{Error, MatchId, Result, SessionId};
use musubi_proto::{ServerMessage, UserPresence};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::party::home_of;
use crate::fanout::Fanout;

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
pub struct RelayedRegistry {
    node_id: Uuid,
    matches: DashMap<String, Relayed>,
    by_token: DashMap<String, String>,
    by_session: DashMap<SessionId, Vec<String>>,
}

impl RelayedRegistry {
    pub fn new(node_id: Uuid) -> Self {
        Self {
            node_id,
            matches: DashMap::new(),
            by_token: DashMap::new(),
            by_session: DashMap::new(),
        }
    }

    pub fn is_home(&self, match_id: &str) -> bool {
        home_of(match_id) == Some(self.node_id)
    }

    /// A fresh id homed on this node (the matchmaker leader mints one per formed group).
    pub fn new_match_id(&self) -> String {
        format!("{}.{}", MatchId::new(), self.node_id)
    }

    pub fn token_match(&self, token: &str) -> Option<String> {
        self.by_token.get(token).map(|v| v.clone())
    }

    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Called by matchmaking when a group forms so the token can be redeemed by any member.
    pub fn register_token(&self, token: &str, match_id: &str) {
        self.by_token
            .insert(token.to_string(), match_id.to_string());
        if self.is_home(match_id) {
            self.matches
                .entry(match_id.to_string())
                .or_insert_with(|| Relayed {
                    id: match_id.to_string(),
                    label: None,
                    members: vec![],
                });
        }
    }

    pub fn create(&self, me: &UserPresence, name: Option<String>) -> Result<ServerMessage> {
        if name.as_ref().is_some_and(|n| n.chars().count() > 64) {
            return Err(Error::InvalidArgument("match name ≤ 64 chars".into()));
        }
        let id = self.new_match_id();
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
        fanout: &Fanout,
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
            fanout.session(s, ev.clone());
        }
        Ok(m.snapshot(me))
    }

    pub fn leave(&self, fanout: &Fanout, me: SessionId, match_id: &str) -> Result<()> {
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
            fanout.session(s, ev.clone());
        }
        drop(m);
        if empty {
            self.matches.remove(match_id);
            self.by_token.retain(|_, v| v != match_id);
        }
        Ok(())
    }

    pub fn leave_all(&self, fanout: &Fanout, me: SessionId) {
        let Some((_, ids)) = self.by_session.remove(&me) else {
            return;
        };
        for id in ids {
            let _ = self.leave(fanout, me, &id);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn send(
        &self,
        fanout: &Fanout,
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
            fanout.session(s, msg.clone());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RelayedCmd {
    Join,
    Leave,
    Data {
        op_code: i64,
        data: Vec<u8>,
        only: Vec<UserPresence>,
        reliable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayedCmdEnv {
    pub match_id: String,
    pub me: UserPresence,
    pub cmd: RelayedCmd,
}

/// Execute on the home node; the reply (if any) goes back to `me`'s session.
pub fn exec(
    relayed: &RelayedRegistry,
    fanout: &Fanout,
    me: &UserPresence,
    match_id: &str,
    cmd: RelayedCmd,
) -> Option<ServerMessage> {
    let res: Result<Option<ServerMessage>> = match cmd {
        RelayedCmd::Join => relayed.join(fanout, me, Some(match_id), None).map(Some),
        RelayedCmd::Leave => relayed.leave(fanout, me.session_id, match_id).map(|_| None),
        RelayedCmd::Data {
            op_code,
            data,
            only,
            reliable,
        } => relayed
            .send(fanout, me, match_id, op_code, data, &only, reliable)
            .map(|_| None),
    };
    match res {
        Ok(r) => r,
        Err(e) => Some(ServerMessage::error(&e)),
    }
}
