//! Parties: realtime groups with a leader, optional join requests and party-wide matchmaking.
//! A party lives on its *home node* (the creator's); its id is `<uuid>.<home node id>` so any node
//! can route commands to it over the cluster bus (`PartyCmd`). Events reach members on other nodes
//! through `Fanout::session`. Membership is per session (one session = one seat).

use dashmap::DashMap;
use musubi_core::{Error, PartyId, Result, SessionId};
use musubi_matchmaker::{AddTicket, Matchmaker};
use musubi_proto::{ServerMessage, UserPresence};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::fanout::Fanout;

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

pub struct PartyRegistry {
    node_id: Uuid,
    parties: DashMap<String, Party>,
    by_session: DashMap<SessionId, String>,
}

fn broadcast(fanout: &Fanout, sessions: &[SessionId], msg: &ServerMessage) {
    for sid in sessions {
        fanout.session(*sid, msg.clone());
    }
}

/// Home node of a party id (`<uuid>.<node>`).
pub fn home_of(party_id: &str) -> Option<Uuid> {
    party_id
        .rsplit_once('.')
        .and_then(|(_, n)| Uuid::parse_str(n).ok())
}

impl PartyRegistry {
    pub fn new(node_id: Uuid) -> Self {
        Self {
            node_id,
            parties: DashMap::new(),
            by_session: DashMap::new(),
        }
    }

    pub fn node_id(&self) -> Uuid {
        self.node_id
    }

    pub fn is_home(&self, party_id: &str) -> bool {
        home_of(party_id) == Some(self.node_id)
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
        let id = format!("{}.{}", PartyId::new(), self.node_id);
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
        fanout: &Fanout,
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
                fanout,
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
            fanout.session(
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
        fanout: &Fanout,
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
            fanout,
            &others,
            &ServerMessage::PartyPresenceEvent {
                party_id: party_id.into(),
                joins: vec![joiner.clone()],
                leaves: vec![],
            },
        );
        fanout.session(joiner.session_id, p.snapshot(&joiner));
        Ok(())
    }

    /// Leader kicks a member or rejects a request.
    pub fn remove(
        &self,
        fanout: &Fanout,
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
        fanout.session(gone.session_id, msg.clone());
        broadcast(fanout, &p.sessions(), &msg);
        Ok(())
    }

    pub fn promote(
        &self,
        fanout: &Fanout,
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
            fanout,
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

    pub fn close(&self, fanout: &Fanout, me: SessionId, party_id: &str) -> Result<Vec<String>> {
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
            fanout,
            &p.sessions(),
            &ServerMessage::PartyClose {
                party_id: party_id.into(),
            },
        );
        Ok(p.tickets)
    }

    /// Leave (or withdraw a request). Leadership passes to the oldest member; an empty party is dropped.
    /// Returns matchmaker tickets that must be cancelled (party composition changed).
    pub fn leave(&self, fanout: &Fanout, me: SessionId, party_id: &str) -> Result<Vec<String>> {
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
            fanout,
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
                fanout,
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
    pub fn leave_all(&self, fanout: &Fanout, me: SessionId) -> Vec<String> {
        let Some((_, pid)) = self.by_session.remove(&me) else {
            return vec![];
        };
        // by_session entry is gone already; `leave` tolerates that
        self.leave(fanout, me, &pid).unwrap_or_default()
    }

    pub fn data(
        &self,
        fanout: &Fanout,
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
            fanout,
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

/// A party command from a member whose session lives on another node, executed on the home node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PartyCmd {
    Join,
    Leave,
    Accept {
        who: UserPresence,
    },
    Remove {
        who: UserPresence,
    },
    Promote {
        who: UserPresence,
    },
    Close,
    RequestList,
    Data {
        op_code: i64,
        data: Vec<u8>,
    },
    MatchmakerAdd {
        min_count: u32,
        max_count: u32,
        query: String,
        string_properties: std::collections::HashMap<String, String>,
        numeric_properties: std::collections::HashMap<String, f64>,
        count_multiple: u32,
    },
    MatchmakerRemove {
        ticket: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartyCmdEnv {
    pub party_id: String,
    pub me: UserPresence,
    pub cmd: PartyCmd,
}

/// Execute a command on the party's home node. The reply (if any) goes back to `me`'s session.
pub fn exec(
    parties: &PartyRegistry,
    fanout: &Fanout,
    mm: &Matchmaker,
    me: &UserPresence,
    party_id: &str,
    cmd: PartyCmd,
) -> Option<ServerMessage> {
    let sid = me.session_id;
    let cancel = |tickets: Vec<String>| {
        for t in tickets {
            mm.cancel(&t);
            fanout.mm_remove(&t);
        }
    };
    let res: Result<Option<ServerMessage>> = (|| match cmd {
        PartyCmd::Join => parties.join(fanout, me, party_id),
        PartyCmd::Leave => parties.leave(fanout, sid, party_id).map(|t| {
            cancel(t);
            None
        }),
        PartyCmd::Accept { who } => parties.accept(fanout, sid, party_id, &who).map(|_| None),
        PartyCmd::Remove { who } => parties.remove(fanout, sid, party_id, &who).map(|_| None),
        PartyCmd::Promote { who } => parties.promote(fanout, sid, party_id, &who).map(|_| None),
        PartyCmd::Close => parties.close(fanout, sid, party_id).map(|t| {
            cancel(t);
            None
        }),
        PartyCmd::RequestList => parties.requests(sid, party_id).map(Some),
        PartyCmd::Data { op_code, data } => parties
            .data(fanout, me, party_id, op_code, data)
            .map(|_| None),
        PartyCmd::MatchmakerAdd {
            min_count,
            max_count,
            query,
            string_properties,
            numeric_properties,
            count_multiple,
        } => {
            let members = parties.members_for_ticket(sid, party_id)?;
            let candidates = members
                .into_iter()
                .map(|presence| musubi_matchmaker::Candidate {
                    presence,
                    string_properties: string_properties.clone(),
                    numeric_properties: numeric_properties.clone(),
                })
                .collect();
            let ticket = mm.add(AddTicket {
                party_id: Some(party_id.to_string()),
                members: candidates,
                query,
                min_count: min_count as usize,
                max_count: max_count as usize,
                count_multiple: count_multiple as usize,
            })?;
            parties.track_ticket(party_id, &ticket);
            let msg = ServerMessage::PartyMatchmakerTicket {
                party_id: party_id.to_string(),
                ticket: ticket.clone(),
            };
            for s in parties.member_sessions(party_id) {
                if s != sid {
                    fanout.session(s, msg.clone());
                }
            }
            fanout.mm_add(&ticket);
            fanout.matchmake_now();
            Ok(Some(msg))
        }
        PartyCmd::MatchmakerRemove { ticket } => {
            parties.untrack_ticket(sid, party_id, &ticket)?;
            mm.cancel(&ticket);
            fanout.mm_remove(&ticket);
            Ok(None)
        }
    })();
    match res {
        Ok(r) => r,
        Err(e) => Some(ServerMessage::error(&e)),
    }
}
