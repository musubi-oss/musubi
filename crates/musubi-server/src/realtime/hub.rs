use std::collections::{HashMap, HashSet};

use dashmap::DashMap;
use musubi_core::{AccountId, SessionId};
use musubi_proto::{ServerMessage, UserPresence};
use tokio::sync::mpsc;

pub type RemoteHook = Box<dyn Fn(AccountId, Vec<UserPresence>, Vec<UserPresence>) + Send + Sync>;

#[derive(Clone)]
pub struct SessionHandle {
    pub presence: UserPresence,
    pub tx: mpsc::Sender<ServerMessage>,
    /// Fired by `Hub::kick_account`; the session loop closes the socket.
    pub kick: std::sync::Arc<tokio::sync::Notify>,
}

/// A reconnected transport handed to a parked session loop.
pub struct Resumed {
    pub conn: Box<dyn musubi_transport::Connection>,
    pub encoding: musubi_proto::Encoding,
}

/// One session's membership in a channel.
#[derive(Clone, Copy, Debug)]
struct Member {
    hidden: bool,
    /// Room messages are written to history only for senders who joined with `persistence`.
    persistent: bool,
}

/// Process-local registry of live realtime sessions.
#[derive(Default)]
pub struct Hub {
    sessions: DashMap<SessionId, SessionHandle>,
    /// resume token → the parked session loop waiting for a new connection
    parked: DashMap<String, tokio::sync::oneshot::Sender<Resumed>>,
    /// session id → resume token of a parked session (so another node can steal it)
    parked_sid: DashMap<SessionId, String>,
    by_account: DashMap<AccountId, HashSet<SessionId>>,
    /// target account → sessions that follow its status
    followers: DashMap<AccountId, HashSet<SessionId>>,
    /// session → accounts it follows (for cleanup)
    following: DashMap<SessionId, HashSet<AccountId>>,
    remote: std::sync::OnceLock<RemoteHook>,
    /// chat channel id → sessions in it (hidden flag per session)
    channels: DashMap<String, HashMap<SessionId, Member>>,
    /// session → channels joined (for cleanup)
    in_channels: DashMap<SessionId, HashSet<String>>,
    /// Set by `close_all`: sessions ending now keep their resume record (parked) for another node.
    draining: std::sync::atomic::AtomicBool,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Close every live session of the account on this node. Returns how many were kicked.
    pub fn kick_account(&self, account_id: AccountId) -> usize {
        let sids: Vec<SessionId> = self
            .by_account
            .get(&account_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        let mut n = 0;
        for sid in sids {
            if let Some(h) = self.sessions.get(&sid) {
                h.kick.notify_one();
                n += 1;
            }
        }
        n
    }

    /// Close every session (node draining): a best-effort `Unavailable` hint first so clients
    /// reconnect elsewhere, then the same kick path admins use.
    pub fn is_draining(&self) -> bool {
        self.draining.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn close_all(&self) -> usize {
        self.draining
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let mut n = 0;
        for h in self.sessions.iter() {
            let _ =
                h.tx.try_send(ServerMessage::error(&musubi_core::Error::Unavailable(
                    "node draining, reconnect".into(),
                )));
            h.kick.notify_one();
            n += 1;
        }
        n
    }

    /// Park a session that lost its transport; `resume` hands it the next connection.
    pub fn park(&self, token: String, sid: SessionId, slot: tokio::sync::oneshot::Sender<Resumed>) {
        self.parked_sid.insert(sid, token.clone());
        self.parked.insert(token, slot);
    }

    pub fn unpark(&self, token: &str) {
        self.parked.remove(token);
        self.parked_sid.retain(|_, t| t != token);
    }

    /// Another node rebuilt this parked session from its record: drop our copy so its loop ends.
    pub fn steal(&self, sid: SessionId) -> bool {
        match self.parked_sid.remove(&sid) {
            Some((_, token)) => self.parked.remove(&token).is_some(),
            None => false,
        }
    }

    /// Attach a new connection to a parked session; gives the connection back when the token is
    /// unknown or expired so the caller can tell the client.
    pub fn resume(
        &self,
        token: &str,
        resumed: Resumed,
    ) -> std::result::Result<(), Box<dyn musubi_transport::Connection>> {
        match self.parked.remove(token) {
            Some((_, slot)) => {
                self.parked_sid.retain(|_, t| t != token);
                slot.send(resumed).map_err(|r| r.conn)
            }
            None => Err(resumed.conn),
        }
    }

    pub fn parked_count(&self) -> usize {
        self.parked.len()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Distinct accounts with at least one live socket on this node.
    pub fn online_accounts(&self) -> usize {
        self.by_account
            .iter()
            .filter(|e| !e.value().is_empty())
            .count()
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
        persistent: bool,
    ) -> Vec<UserPresence> {
        let Some(h) = self.sessions.get(&session_id) else {
            return vec![];
        };
        let me = h.presence.clone();
        drop(h);
        let mut members = self.channels.entry(channel_id.to_string()).or_default();
        let already = members
            .insert(session_id, Member { hidden, persistent })
            .is_some();
        let others: Vec<UserPresence> = members
            .iter()
            .filter(|(sid, m)| **sid != session_id && !m.hidden)
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
        let Some(Member { hidden, .. }) = members.remove(&me.session_id) else {
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

    /// Whether `session_id` joined `channel_id` asking for its messages to be written to history.
    pub fn channel_persistent(&self, channel_id: &str, session_id: SessionId) -> bool {
        self.channels
            .get(channel_id)
            .and_then(|m| m.get(&session_id).map(|m| m.persistent))
            .unwrap_or(false)
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

    /// Like `send_to_session`, but hands the message back when the session is not here or its
    /// queue is full, so the caller can forward it without having cloned it first.
    #[allow(clippy::result_large_err)] // the point is to hand the message back
    pub fn try_send_session(
        &self,
        session_id: SessionId,
        msg: ServerMessage,
    ) -> Result<(), ServerMessage> {
        match self.sessions.get(&session_id) {
            Some(h) => h.tx.try_send(msg).map_err(|e| {
                if matches!(e, tokio::sync::mpsc::error::TrySendError::Full(_)) {
                    metrics::counter!("musubi_session_send_full_total").increment(1);
                }
                e.into_inner()
            }),
            None => Err(msg),
        }
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

    /// Install the cross-node hook: every local presence change is also handed to it.
    pub fn set_remote(&self, hook: RemoteHook) {
        let _ = self.remote.set(hook);
    }

    fn notify_followers(
        &self,
        target: AccountId,
        joins: Vec<UserPresence>,
        leaves: Vec<UserPresence>,
    ) {
        if let Some(hook) = self.remote.get() {
            hook(target, joins.clone(), leaves.clone());
        }
        self.notify_followers_local(target, joins, leaves);
    }

    /// Deliver a presence change (local or received from another node) to local followers.
    pub fn notify_followers_local(
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
        (
            SessionHandle {
                presence,
                tx,
                kick: Default::default(),
            },
            rx,
        )
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
