//! Parties: process-local realtime groups with a leader, optional join requests and party-wide
//! matchmaking. Ids are `PartyId` strings; membership is per session (one session = one seat).

use dashmap::DashMap;
use musubi_core::{Error, PartyId, Result, SessionId};
use musubi_proto::{ServerMessage, UserPresence};

use super::Hub;

pub const MAX_PARTY_SIZE: u32 = 256;

struct Party {
    id: String,
    open: bool,
    max_size: u32,
    leader: SessionId,
    members: Vec<UserPresence>,
    requests: Vec<UserPresence>,
    tickets: Vec<String>,
}

impl Party {
    fn member(&self, sid: SessionId) -> Option<&UserPresence> {
        self.members.iter().find(|p| p.session_id == sid)
    }

    fn snapshot(&self, me: &UserPresence) -> ServerMessage {
        ServerMessage::Party {
            party_id: self.id.clone(),
            open: self.open,
            max_size: self.max_size,
            me: me.clone(),
            leader: self
                .member(self.leader)
                .cloned()
                .unwrap_or_else(|| me.clone()),
            presences: self.members.clone(),
        }
    }

    fn sessions(&self) -> Vec<SessionId> {
        self.members.iter().map(|p| p.session_id).collect()
    }
}

#[derive(Default)]
pub struct PartyRegistry {
    parties: DashMap<String, Party>,
    by_session: DashMap<SessionId, String>,
}

fn broadcast(hub: &Hub, sessions: &[SessionId], msg: &ServerMessage) {
    for sid in sessions {
        hub.send_to_session(*sid, msg.clone());
    }
}

impl PartyRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn party_count(&self) -> usize {
        self.parties.len()
    }

    pub fn create(&self, me: &UserPresence, open: bool, max_size: u32) -> Result<ServerMessage> {
        if !(1..=MAX_PARTY_SIZE).contains(&max_size) {
            return Err(Error::InvalidArgument(format!(
                "max_size must be 1..={MAX_PARTY_SIZE}"
            )));
        }
        if self.by_session.contains_key(&me.session_id) {
            return Err(Error::Conflict("already in a party".into()));
        }
        let id = PartyId::new().to_string();
        let party = Party {
            id: id.clone(),
            open,
            max_size,
            leader: me.session_id,
            members: vec![me.clone()],
            requests: vec![],
            tickets: vec![],
        };
        let snap = party.snapshot(me);
        self.by_session.insert(me.session_id, id.clone());
        self.parties.insert(id, party);
        Ok(snap)
    }

    /// Open party → member right away (reply `Party`). Closed → request queued, leader notified
    /// (no reply until accepted; returns `None`).
    pub fn join(
        &self,
        hub: &Hub,
        me: &UserPresence,
        party_id: &str,
    ) -> Result<Option<ServerMessage>> {
        if self.by_session.contains_key(&me.session_id) {
            return Err(Error::Conflict("already in a party".into()));
        }
        let mut p = self
            .parties
            .get_mut(party_id)
            .ok_or_else(|| Error::NotFound("party".into()))?;
        if p.open {
            if p.members.len() as u32 >= p.max_size {
                return Err(Error::ResourceExhausted("party is full".into()));
            }
            let others = p.sessions();
            p.members.push(me.clone());
            self.by_session.insert(me.session_id, party_id.to_string());
            broadcast(
                hub,
                &others,
                &ServerMessage::PartyPresenceEvent {
                    party_id: party_id.into(),
                    joins: vec![me.clone()],
                    leaves: vec![],
                },
            );
            Ok(Some(p.snapshot(me)))
        } else {
            if p.requests.iter().any(|r| r.session_id == me.session_id) {
                return Ok(None);
            }
            if p.requests.len() >= 64 {
                return Err(Error::ResourceExhausted("too many pending requests".into()));
            }
            p.requests.push(me.clone());
            hub.send_to_session(
                p.leader,
                ServerMessage::PartyJoinRequest {
                    party_id: party_id.into(),
                    presences: vec![me.clone()],
                },
            );
            Ok(None)
        }
    }

    pub fn accept(
        &self,
        hub: &Hub,
        me: SessionId,
        party_id: &str,
        who: &UserPresence,
    ) -> Result<()> {
        let mut p = self.leader_party(me, party_id)?;
        let Some(idx) = p
            .requests
            .iter()
            .position(|r| r.session_id == who.session_id)
        else {
            return Err(Error::NotFound("no such join request".into()));
        };
        if p.members.len() as u32 >= p.max_size {
            return Err(Error::ResourceExhausted("party is full".into()));
        }
        let joiner = p.requests.remove(idx);
        if self.by_session.contains_key(&joiner.session_id) {
            return Err(Error::Conflict("user joined another party".into()));
        }
        let others = p.sessions();
        p.members.push(joiner.clone());
        self.by_session
            .insert(joiner.session_id, party_id.to_string());
        broadcast(
            hub,
            &others,
            &ServerMessage::PartyPresenceEvent {
                party_id: party_id.into(),
                joins: vec![joiner.clone()],
                leaves: vec![],
            },
        );
        hub.send_to_session(joiner.session_id, p.snapshot(&joiner));
        Ok(())
    }

    /// Leader kicks a member or rejects a request.
    pub fn remove(
        &self,
        hub: &Hub,
        me: SessionId,
        party_id: &str,
        who: &UserPresence,
    ) -> Result<()> {
        if who.session_id == me {
            return Err(Error::InvalidArgument("use party_leave".into()));
        }
        let mut p = self.leader_party(me, party_id)?;
        if let Some(idx) = p
            .requests
            .iter()
            .position(|r| r.session_id == who.session_id)
        {
            p.requests.remove(idx);
            return Ok(());
        }
        let Some(idx) = p
            .members
            .iter()
            .position(|m| m.session_id == who.session_id)
        else {
            return Err(Error::NotFound("not a member".into()));
        };
        let gone = p.members.remove(idx);
        self.by_session.remove(&gone.session_id);
        let msg = ServerMessage::PartyPresenceEvent {
            party_id: party_id.into(),
            joins: vec![],
            leaves: vec![gone.clone()],
        };
        hub.send_to_session(gone.session_id, msg.clone());
        broadcast(hub, &p.sessions(), &msg);
        Ok(())
    }

    pub fn promote(
        &self,
        hub: &Hub,
        me: SessionId,
        party_id: &str,
        who: &UserPresence,
    ) -> Result<()> {
        let mut p = self.leader_party(me, party_id)?;
        let Some(target) = p.member(who.session_id).cloned() else {
            return Err(Error::NotFound("not a member".into()));
        };
        p.leader = target.session_id;
        broadcast(
            hub,
            &p.sessions(),
            &ServerMessage::PartyLeader {
                party_id: party_id.into(),
                presence: target,
            },
        );
        Ok(())
    }

    pub fn requests(&self, me: SessionId, party_id: &str) -> Result<ServerMessage> {
        let p = self.leader_party(me, party_id)?;
        Ok(ServerMessage::PartyJoinRequest {
            party_id: party_id.into(),
            presences: p.requests.clone(),
        })
    }

    pub fn close(&self, hub: &Hub, me: SessionId, party_id: &str) -> Result<Vec<String>> {
        {
            self.leader_party(me, party_id)?;
        }
        let Some((_, p)) = self.parties.remove(party_id) else {
            return Err(Error::NotFound("party".into()));
        };
        for s in p.sessions() {
            self.by_session.remove(&s);
        }
        broadcast(
            hub,
            &p.sessions(),
            &ServerMessage::PartyClose {
                party_id: party_id.into(),
            },
        );
        Ok(p.tickets)
    }

    /// Leave (or withdraw a request). Leadership passes to the oldest member; an empty party is dropped.
    /// Returns matchmaker tickets that must be cancelled (party composition changed).
    pub fn leave(&self, hub: &Hub, me: SessionId, party_id: &str) -> Result<Vec<String>> {
        let mut p = self
            .parties
            .get_mut(party_id)
            .ok_or_else(|| Error::NotFound("party".into()))?;
        if let Some(idx) = p.requests.iter().position(|r| r.session_id == me) {
            p.requests.remove(idx);
            return Ok(vec![]);
        }
        let Some(idx) = p.members.iter().position(|m| m.session_id == me) else {
            return Err(Error::NotFound("not a member".into()));
        };
        let gone = p.members.remove(idx);
        self.by_session.remove(&me);
        let tickets = std::mem::take(&mut p.tickets);
        if p.members.is_empty() {
            drop(p);
            self.parties.remove(party_id);
            return Ok(tickets);
        }
        let sessions = p.sessions();
        broadcast(
            hub,
            &sessions,
            &ServerMessage::PartyPresenceEvent {
                party_id: party_id.into(),
                joins: vec![],
                leaves: vec![gone],
            },
        );
        if p.leader == me {
            p.leader = p.members[0].session_id;
            let leader = p.members[0].clone();
            broadcast(
                hub,
                &sessions,
                &ServerMessage::PartyLeader {
                    party_id: party_id.into(),
                    presence: leader,
                },
            );
        }
        Ok(tickets)
    }

    /// Disconnect cleanup; returns tickets to cancel.
    pub fn leave_all(&self, hub: &Hub, me: SessionId) -> Vec<String> {
        let Some((_, pid)) = self.by_session.remove(&me) else {
            return vec![];
        };
        // by_session entry is gone already; `leave` tolerates that
        self.leave(hub, me, &pid).unwrap_or_default()
    }

    pub fn data(
        &self,
        hub: &Hub,
        me: &UserPresence,
        party_id: &str,
        op_code: i64,
        data: Vec<u8>,
    ) -> Result<()> {
        if data.len() > 16 * 1024 {
            return Err(Error::InvalidArgument("party data ≤ 16 KB".into()));
        }
        let p = self
            .parties
            .get(party_id)
            .ok_or_else(|| Error::NotFound("party".into()))?;
        if p.member(me.session_id).is_none() {
            return Err(Error::PermissionDenied("not a member".into()));
        }
        broadcast(
            hub,
            &p.sessions(),
            &ServerMessage::PartyData {
                party_id: party_id.into(),
                presence: me.clone(),
                op_code,
                data,
            },
        );
        Ok(())
    }

    /// Leader-only snapshot of members for a matchmaker ticket; remembers the ticket for cleanup.
    pub fn members_for_ticket(&self, me: SessionId, party_id: &str) -> Result<Vec<UserPresence>> {
        let p = self.leader_party(me, party_id)?;
        Ok(p.members.clone())
    }

    pub fn track_ticket(&self, party_id: &str, ticket: &str) {
        if let Some(mut p) = self.parties.get_mut(party_id) {
            p.tickets.push(ticket.to_string());
        }
    }

    pub fn untrack_ticket(&self, me: SessionId, party_id: &str, ticket: &str) -> Result<()> {
        let mut p = self.leader_party(me, party_id)?;
        let before = p.tickets.len();
        p.tickets.retain(|t| t != ticket);
        if p.tickets.len() == before {
            return Err(Error::NotFound("ticket".into()));
        }
        Ok(())
    }

    /// A ticket was consumed by a match: forget it.
    pub fn ticket_consumed(&self, party_id: &str, ticket: &str) {
        if let Some(mut p) = self.parties.get_mut(party_id) {
            p.tickets.retain(|t| t != ticket);
        }
    }

    pub fn member_sessions(&self, party_id: &str) -> Vec<SessionId> {
        self.parties
            .get(party_id)
            .map(|p| p.sessions())
            .unwrap_or_default()
    }

    fn leader_party(
        &self,
        me: SessionId,
        party_id: &str,
    ) -> Result<dashmap::mapref::one::RefMut<'_, String, Party>> {
        let p = self
            .parties
            .get_mut(party_id)
            .ok_or_else(|| Error::NotFound("party".into()))?;
        if p.leader != me {
            return Err(Error::PermissionDenied("leader only".into()));
        }
        Ok(p)
    }
}
