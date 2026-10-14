use std::collections::{HashMap, HashSet};

use dashmap::DashMap;
use musubi_core::{AccountId, SessionId};
use musubi_proto::{ServerMessage, UserPresence};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct SessionHandle {
    pub presence: UserPresence,
    pub tx: mpsc::Sender<ServerMessage>,
}

/// Process-local registry of live realtime sessions.
#[derive(Default)]
pub struct Hub {
    sessions: DashMap<SessionId, SessionHandle>,
    by_account: DashMap<AccountId, HashSet<SessionId>>,
    /// target account → sessions that follow its status
    followers: DashMap<AccountId, HashSet<SessionId>>,
    /// session → accounts it follows (for cleanup)
    following: DashMap<SessionId, HashSet<AccountId>>,
    /// chat channel id → sessions in it (hidden flag per session)
    channels: DashMap<String, HashMap<SessionId, bool>>,
    /// session → channels joined (for cleanup)
    in_channels: DashMap<SessionId, HashSet<String>>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn join(&self, handle: SessionHandle) {
        let p = handle.presence.clone();
        self.by_account
            .entry(p.account_id)
            .or_default()
            .insert(p.session_id);
        self.sessions.insert(p.session_id, handle);
        self.notify_followers(p.account_id, vec![p], vec![]);
    }

    pub fn leave(&self, session_id: SessionId) {
        let Some((_, h)) = self.sessions.remove(&session_id) else {
            return;
        };
        let p = h.presence;
        if let Some(mut set) = self.by_account.get_mut(&p.account_id) {
            set.remove(&session_id);
            if set.is_empty() {
                drop(set);
                self.by_account
                    .remove_if(&p.account_id, |_, s| s.is_empty());
            }
        }
        if let Some((_, targets)) = self.following.remove(&session_id) {
            for t in targets {
                if let Some(mut f) = self.followers.get_mut(&t) {
                    f.remove(&session_id);
                }
            }
        }
        if let Some((_, chans)) = self.in_channels.remove(&session_id) {
            for c in chans {
                self.leave_channel_inner(&c, &p);
            }
        }
        self.notify_followers(p.account_id, vec![], vec![p]);
    }

    /// Join a chat channel; returns the visible presences already there (excluding the joiner).
    pub fn join_channel(
        &self,
        channel_id: &str,
        session_id: SessionId,
        hidden: bool,
    ) -> Vec<UserPresence> {
        let Some(h) = self.sessions.get(&session_id) else {
            return vec![];
        };
        let me = h.presence.clone();
        drop(h);
        let mut members = self.channels.entry(channel_id.to_string()).or_default();
        let already = members.insert(session_id, hidden).is_some();
        let others: Vec<UserPresence> = members
            .iter()
            .filter(|(sid, hidden)| **sid != session_id && !**hidden)
            .filter_map(|(sid, _)| self.sessions.get(sid).map(|h| h.presence.clone()))
            .collect();
        let targets: Vec<SessionId> = members
            .keys()
            .copied()
            .filter(|s| *s != session_id)
            .collect();
        drop(members);
        self.in_channels
            .entry(session_id)
            .or_default()
            .insert(channel_id.to_string());
        if !hidden && !already {
            self.fan_out(
                &targets,
                ServerMessage::ChannelPresenceEvent {
                    channel_id: channel_id.to_string(),
                    joins: vec![me],
                    leaves: vec![],
                },
            );
        }
        others
    }

    pub fn leave_channel(&self, channel_id: &str, session_id: SessionId) {
        let Some(h) = self.sessions.get(&session_id) else {
            return;
        };
        let me = h.presence.clone();
        drop(h);
        if let Some(mut set) = self.in_channels.get_mut(&session_id) {
            set.remove(channel_id);
        }
        self.leave_channel_inner(channel_id, &me);
    }

    fn leave_channel_inner(&self, channel_id: &str, me: &UserPresence) {
        let Some(mut members) = self.channels.get_mut(channel_id) else {
            return;
        };
        let Some(hidden) = members.remove(&me.session_id) else {
            return;
        };
        let targets: Vec<SessionId> = members.keys().copied().collect();
        let empty = members.is_empty();
        drop(members);
        if empty {
            self.channels.remove_if(channel_id, |_, m| m.is_empty());
        }
        if !hidden {
            self.fan_out(
                &targets,
                ServerMessage::ChannelPresenceEvent {
                    channel_id: channel_id.to_string(),
                    joins: vec![],
                    leaves: vec![me.clone()],
                },
            );
        }
    }

    pub fn in_channel(&self, channel_id: &str, session_id: SessionId) -> bool {
        self.channels
            .get(channel_id)
            .is_some_and(|m| m.contains_key(&session_id))
    }

    /// Deliver to every session in the channel (including the sender). Returns the count reached.
    pub fn broadcast_channel(&self, channel_id: &str, msg: ServerMessage) -> usize {
        let Some(members) = self.channels.get(channel_id) else {
            return 0;
        };
        let targets: Vec<SessionId> = members.keys().copied().collect();
        drop(members);
        self.fan_out(&targets, msg)
    }

    fn fan_out(&self, targets: &[SessionId], msg: ServerMessage) -> usize {
        let mut n = 0;
        for sid in targets {
            if let Some(h) = self.sessions.get(sid) {
                if h.tx.try_send(msg.clone()).is_ok() {
                    n += 1;
                }
            }
        }
        n
    }

    /// Change the status text; followers get a leave (old) + join (new) pair, like Nakama.
    pub fn set_status(&self, session_id: SessionId, status: Option<String>) {
        let Some(mut h) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let old = h.presence.clone();
        h.presence.status = status;
        let new = h.presence.clone();
        drop(h);
        self.notify_followers(old.account_id, vec![new], vec![old]);
    }

    /// Follow accounts; returns the snapshot of currently online presences among them.
    pub fn follow(&self, session_id: SessionId, targets: &[AccountId]) -> Vec<UserPresence> {
        let mut snapshot = Vec::new();
        let mut mine = self.following.entry(session_id).or_default();
        for t in targets {
            self.followers.entry(*t).or_default().insert(session_id);
            mine.insert(*t);
            if let Some(sids) = self.by_account.get(t) {
                for sid in sids.iter() {
                    if let Some(h) = self.sessions.get(sid) {
                        snapshot.push(h.presence.clone());
                    }
                }
            }
        }
        snapshot
    }

    pub fn unfollow(&self, session_id: SessionId, targets: &[AccountId]) {
        for t in targets {
            if let Some(mut f) = self.followers.get_mut(t) {
                f.remove(&session_id);
            }
            if let Some(mut mine) = self.following.get_mut(&session_id) {
                mine.remove(t);
            }
        }
    }

    pub fn send_to_session(&self, session_id: SessionId, msg: ServerMessage) -> bool {
        self.sessions
            .get(&session_id)
            .is_some_and(|h| h.tx.try_send(msg).is_ok())
    }

    pub fn send_to_account(&self, account_id: AccountId, msg: ServerMessage) -> usize {
        let Some(sids) = self.by_account.get(&account_id) else {
            return 0;
        };
        let mut n = 0;
        for sid in sids.iter() {
            if let Some(h) = self.sessions.get(sid) {
                if h.tx.try_send(msg.clone()).is_ok() {
                    n += 1;
                }
            }
        }
        n
    }

    pub fn presences_of(&self, account_id: AccountId) -> Vec<UserPresence> {
        let Some(sids) = self.by_account.get(&account_id) else {
            return vec![];
        };
        sids.iter()
            .filter_map(|s| self.sessions.get(s).map(|h| h.presence.clone()))
            .collect()
    }

    fn notify_followers(
        &self,
        target: AccountId,
        joins: Vec<UserPresence>,
        leaves: Vec<UserPresence>,
    ) {
        let Some(f) = self.followers.get(&target) else {
            return;
        };
        let msg = ServerMessage::StatusPresenceEvent { joins, leaves };
        for sid in f.iter() {
            if let Some(h) = self.sessions.get(sid) {
                // Slow consumers drop presence events rather than stall the hub.
                let _ = h.tx.try_send(msg.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(acc: AccountId) -> (SessionHandle, mpsc::Receiver<ServerMessage>) {
        let (tx, rx) = mpsc::channel(8);
        let presence = UserPresence {
            account_id: acc,
            session_id: SessionId::new(),
            username: "u".into(),
            status: None,
        };
        (SessionHandle { presence, tx }, rx)
    }

    #[tokio::test]
    async fn follow_snapshot_and_events() {
        let hub = Hub::new();
        let (a, _rxa) = handle(AccountId::new());
        let (b, mut rxb) = handle(AccountId::new());
        let a_sid = a.presence.session_id;
        let a_acc = a.presence.account_id;
        hub.join(b.clone());
        // B follows A before A is online → empty snapshot, then a join event
        assert!(hub.follow(b.presence.session_id, &[a_acc]).is_empty());
        hub.join(a);
        match rxb.recv().await.unwrap() {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert_eq!(joins.len(), 1);
                assert!(leaves.is_empty());
            }
            m => panic!("{m:?}"),
        }
        hub.set_status(a_sid, Some("busy".into()));
        match rxb.recv().await.unwrap() {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert_eq!(joins[0].status.as_deref(), Some("busy"));
                assert_eq!(leaves[0].status, None);
            }
            m => panic!("{m:?}"),
        }
        hub.leave(a_sid);
        match rxb.recv().await.unwrap() {
            ServerMessage::StatusPresenceEvent { joins, leaves } => {
                assert!(joins.is_empty());
                assert_eq!(leaves.len(), 1);
            }
            m => panic!("{m:?}"),
        }
        assert_eq!(hub.session_count(), 1);
        assert_eq!(hub.send_to_account(a_acc, ServerMessage::Pong), 0);
    }
}
