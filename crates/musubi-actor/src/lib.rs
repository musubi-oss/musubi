//! Authoritative match actors. A handler is pure over an opaque state blob: every callback receives
//! the current state and returns the next one, so the latest state is always a complete snapshot
//! (the basis for failover and migration). The actor owns the tick loop, membership and delivery.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use musubi_core::{Error, MatchId, Result, SessionId};
use musubi_proto::{ServerMessage, UserPresence};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, RwLock};

/// A client message routed to the match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Input {
    pub presence: UserPresence,
    pub op_code: i64,
    pub data: Vec<u8>,
    pub reliable: bool,
}

/// Data the handler wants delivered. Empty `to` = everyone in the match.
#[derive(Debug, Clone, Default)]
pub struct Outbound {
    pub to: Vec<SessionId>,
    pub op_code: i64,
    pub data: Vec<u8>,
    pub reliable: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Init {
    pub state: Vec<u8>,
    pub tick_rate: u32,
    pub label: String,
}

#[derive(Debug, Clone, Default)]
pub struct TickResult {
    pub state: Vec<u8>,
    pub outbound: Vec<Outbound>,
    pub label: Option<String>,
    pub terminate: bool,
}

/// Match logic. Implemented natively in Rust today; the WebAssembly binding exposes the same shape.
pub trait MatchHandler: Send + Sync {
    fn init(&self, params: &serde_json::Value) -> Result<Init>;
    /// `Err` rejects the join with that message.
    fn join_attempt(
        &self,
        state: Vec<u8>,
        presence: &UserPresence,
        metadata: &HashMap<String, String>,
        members: &[UserPresence],
    ) -> Result<(Vec<u8>, Result<()>)>;
    fn join(
        &self,
        state: Vec<u8>,
        joined: &[UserPresence],
        members: &[UserPresence],
    ) -> Result<Vec<u8>>;
    fn leave(
        &self,
        state: Vec<u8>,
        left: &[UserPresence],
        members: &[UserPresence],
    ) -> Result<Vec<u8>>;
    fn tick(
        &self,
        state: Vec<u8>,
        tick: u64,
        inputs: Vec<Input>,
        members: &[UserPresence],
    ) -> Result<TickResult>;
    fn terminate(&self, state: Vec<u8>, grace_seconds: u32) -> Result<Vec<u8>>;
}

pub type Deliver = Arc<dyn Fn(Vec<SessionId>, ServerMessage) + Send + Sync>;

/// Persisted view of a match; `None` state on `on_snapshot` means the match ended.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub module: String,
    pub label: String,
    pub params: serde_json::Value,
    pub state: Vec<u8>,
    pub tick: u64,
    /// Journal entries after `tick` (see `replay`); empty when none were persisted.
    pub replay: Vec<JournalEntry>,
}

pub type OnSnapshot = Arc<dyn Fn(MatchId, Option<Snapshot>) + Send + Sync>;

/// One journaled step. Membership commands carry the tick they were processed at; `Tick` carries
/// the tick it produced. Replaying every entry with `tick > snapshot.tick` on top of the snapshot
/// reproduces the state, provided the handler is deterministic (no clocks/randomness outside state).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub tick: u64,
    pub op: Op,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    Join {
        presence: UserPresence,
        metadata: HashMap<String, String>,
    },
    Leave {
        session: SessionId,
    },
    Tick {
        inputs: Vec<Input>,
    },
}

/// Entries flushed together (every `journal_every` ticks).
#[derive(Debug, Clone)]
pub struct JournalChunk {
    pub tick_from: u64,
    pub tick_to: u64,
    pub entries: Vec<JournalEntry>,
}

pub type OnJournal = Arc<dyn Fn(MatchId, JournalChunk) + Send + Sync>;

/// Replay journal entries newer than the snapshot on top of its state. Deliveries are dropped;
/// the members list is rebuilt only to drive the handler and discarded (clients rejoin).
pub fn replay(
    handler: &dyn MatchHandler,
    mut state: Vec<u8>,
    mut label: String,
    from_tick: u64,
    entries: &[JournalEntry],
) -> (Vec<u8>, String, u64) {
    let mut members: Vec<UserPresence> = Vec::new();
    let mut tick = from_tick;
    for e in entries.iter().filter(|e| e.tick > from_tick) {
        match &e.op {
            Op::Join { presence, metadata } => {
                if members.iter().any(|p| p.session_id == presence.session_id) {
                    continue;
                }
                match handler.join_attempt(std::mem::take(&mut state), presence, metadata, &members)
                {
                    Ok((s, Ok(()))) => {
                        state = s;
                        if let Ok(s) = handler.join(
                            std::mem::take(&mut state),
                            std::slice::from_ref(presence),
                            &members,
                        ) {
                            state = s;
                        }
                        members.push(presence.clone());
                    }
                    Ok((s, Err(_))) => state = s,
                    Err(_) => {}
                }
            }
            Op::Leave { session } => {
                if let Some(i) = members.iter().position(|p| p.session_id == *session) {
                    let gone = members.remove(i);
                    if let Ok(s) = handler.leave(
                        std::mem::take(&mut state),
                        std::slice::from_ref(&gone),
                        &members,
                    ) {
                        state = s;
                    }
                }
            }
            Op::Tick { inputs } => {
                tick = e.tick;
                match handler.tick(std::mem::take(&mut state), tick, inputs.clone(), &members) {
                    Ok(r) => {
                        state = r.state;
                        if let Some(l) = r.label {
                            label = l;
                        }
                        if r.terminate {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
    (state, label, tick)
}

/// How a match starts: fresh from `init`, or resumed from a snapshot taken elsewhere.
pub struct Spawn {
    pub id: Option<MatchId>,
    pub module: String,
    pub params: serde_json::Value,
    pub resume: Option<Snapshot>,
    pub deliver: Deliver,
    pub on_end: Box<dyn FnOnce(MatchId) + Send>,
    pub on_snapshot: Option<OnSnapshot>,
    /// Snapshot every N ticks (0 = only on membership changes and end).
    pub snapshot_every: u64,
    pub on_journal: Option<OnJournal>,
    /// Flush journal entries every N ticks (0 = no journal).
    pub journal_every: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchInfo {
    pub match_id: String,
    pub module: String,
    pub label: String,
    pub size: u32,
    pub tick_rate: u32,
    pub tick: u64,
    pub state_bytes: usize,
    /// Sessions watching without playing.
    pub spectators: Vec<SessionId>,
}

enum Cmd {
    Join {
        presence: UserPresence,
        metadata: HashMap<String, String>,
        spectator: bool,
        reply: oneshot::Sender<Result<ServerMessage>>,
    },
    Leave {
        session: SessionId,
    },
    Data(Input),
    Terminate {
        grace_seconds: u32,
    },
}

#[derive(Clone)]
pub struct MatchHandle {
    pub id: MatchId,
    tx: mpsc::Sender<Cmd>,
    info: Arc<RwLock<MatchInfo>>,
}

impl MatchHandle {
    /// `spectator`: watch only — receives match data and presence events, is not counted as a
    /// player, never reaches the handler, and its own inputs are refused.
    pub async fn join(
        &self,
        presence: UserPresence,
        metadata: HashMap<String, String>,
        spectator: bool,
    ) -> Result<ServerMessage> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Cmd::Join {
                presence,
                metadata,
                spectator,
                reply,
            })
            .await
            .map_err(|_| Error::NotFound("match ended".into()))?;
        rx.await
            .map_err(|_| Error::NotFound("match ended".into()))?
    }

    pub fn leave(&self, session: SessionId) {
        let _ = self.tx.try_send(Cmd::Leave { session });
    }

    pub fn data(&self, input: Input) -> Result<()> {
        self.tx
            .try_send(Cmd::Data(input))
            .map_err(|_| Error::ResourceExhausted("match inbox full".into()))
    }

    pub fn terminate(&self, grace_seconds: u32) {
        let _ = self.tx.try_send(Cmd::Terminate { grace_seconds });
    }

    pub async fn info(&self) -> MatchInfo {
        self.info.read().await.clone()
    }

    pub fn is_alive(&self) -> bool {
        !self.tx.is_closed()
    }
}

/// Spawn a match actor.
pub fn spawn(
    module: &str,
    handler: Arc<dyn MatchHandler>,
    params: serde_json::Value,
    deliver: Deliver,
    on_end: Box<dyn FnOnce(MatchId) + Send>,
) -> Result<MatchHandle> {
    spawn_with(
        handler,
        Spawn {
            id: None,
            module: module.to_string(),
            params,
            resume: None,
            deliver,
            on_end,
            on_snapshot: None,
            snapshot_every: 0,
            on_journal: None,
            journal_every: 0,
        },
    )
}

pub fn spawn_with(handler: Arc<dyn MatchHandler>, sp: Spawn) -> Result<MatchHandle> {
    let mut start_tick = sp.resume.as_ref().map(|r| r.tick).unwrap_or(0);
    let init = match &sp.resume {
        Some(snap) => {
            let fresh = handler.init(&sp.params)?;
            let (state, label, tick) = replay(
                handler.as_ref(),
                snap.state.clone(),
                snap.label.clone(),
                snap.tick,
                &snap.replay,
            );
            if tick > snap.tick {
                tracing::info!(from = snap.tick, to = tick, "match journal replayed");
            }
            start_tick = tick;
            Init {
                state,
                tick_rate: fresh.tick_rate,
                label,
            }
        }
        None => handler.init(&sp.params)?,
    };
    if !(1..=60).contains(&init.tick_rate) {
        return Err(Error::InvalidArgument("tick_rate must be 1..=60".into()));
    }
    if init.label.len() > 2048 {
        return Err(Error::InvalidArgument("label ≤ 2 KB".into()));
    }
    let id = sp.id.unwrap_or_default();
    let info = Arc::new(RwLock::new(MatchInfo {
        spectators: Vec::new(),
        match_id: id.to_string(),
        module: sp.module.clone(),
        label: init.label.clone(),
        size: 0,
        tick_rate: init.tick_rate,
        tick: start_tick,
        state_bytes: init.state.len(),
    }));
    let (tx, rx) = mpsc::channel(1024);
    let handle = MatchHandle {
        id,
        tx,
        info: info.clone(),
    };
    tokio::spawn(run(
        id,
        handler,
        init,
        rx,
        sp.deliver,
        info,
        sp.on_end,
        Persist {
            module: sp.module,
            params: sp.params,
            on_snapshot: sp.on_snapshot,
            every: sp.snapshot_every,
            start_tick,
            on_journal: sp.on_journal,
            journal_every: sp.journal_every,
        },
    ));
    Ok(handle)
}

struct Persist {
    module: String,
    params: serde_json::Value,
    on_snapshot: Option<OnSnapshot>,
    every: u64,
    start_tick: u64,
    on_journal: Option<OnJournal>,
    journal_every: u64,
}

impl Persist {
    fn save(&self, id: MatchId, label: &str, state: &[u8], tick: u64) {
        if let Some(cb) = &self.on_snapshot {
            cb(
                id,
                Some(Snapshot {
                    module: self.module.clone(),
                    label: label.to_string(),
                    params: self.params.clone(),
                    state: state.to_vec(),
                    tick,
                    replay: vec![],
                }),
            );
        }
    }
    fn clear(&self, id: MatchId) {
        if let Some(cb) = &self.on_snapshot {
            cb(id, None);
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run(
    id: MatchId,
    handler: Arc<dyn MatchHandler>,
    init: Init,
    mut rx: mpsc::Receiver<Cmd>,
    deliver: Deliver,
    info: Arc<RwLock<MatchInfo>>,
    on_end: Box<dyn FnOnce(MatchId) + Send>,
    persist: Persist,
) {
    let match_id = id.to_string();
    let mut state = init.state;
    let mut label = init.label;
    let mut members: Vec<UserPresence> = Vec::new();
    let mut spectators: Vec<UserPresence> = Vec::new();
    let mut inputs: Vec<Input> = Vec::new();
    let mut tick: u64 = persist.start_tick;
    let mut interval = tokio::time::interval(Duration::from_millis(1000 / init.tick_rate as u64));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let sessions = |m: &[UserPresence]| m.iter().map(|p| p.session_id).collect::<Vec<_>>();
    let mut grace: Option<u32> = None;
    let journaling = persist.on_journal.is_some() && persist.journal_every > 0;
    let mut journal: Vec<JournalEntry> = Vec::new();
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Join { presence, metadata, spectator, reply } => {
                        if spectator {
                            if !spectators.iter().any(|p| p.session_id == presence.session_id) {
                                spectators.push(presence.clone());
                            }
                            let _ = reply.send(Ok(snapshot(&match_id, &label, &members, &presence)));
                            let mut i = info.write().await;
                            i.spectators = spectators.iter().map(|p| p.session_id).collect();
                            continue;
                        }
                        if members.iter().any(|p| p.session_id == presence.session_id) {
                            let _ = reply.send(Ok(snapshot(&match_id, &label, &members, &presence)));
                            continue;
                        }
                        match handler.join_attempt(std::mem::take(&mut state), &presence, &metadata, &members) {
                            Ok((s, verdict)) => {
                                state = s;
                                match verdict {
                                    Ok(()) => {
                                        match handler.join(std::mem::take(&mut state), std::slice::from_ref(&presence), &members) {
                                            Ok(s) => state = s,
                                            Err(e) => { tracing::error!(%match_id, error = %e, "join handler failed"); }
                                        }
                                        let others = sessions(&members);
                                        if journaling {
                                            journal.push(JournalEntry { tick, op: Op::Join { presence: presence.clone(), metadata: metadata.clone() } });
                                        }
                                        members.push(presence.clone());
                                        deliver(others, ServerMessage::MatchPresenceEvent { match_id: match_id.clone(), joins: vec![presence.clone()], leaves: vec![] });
                                        let _ = reply.send(Ok(snapshot(&match_id, &label, &members, &presence)));
                                    }
                                    Err(e) => { let _ = reply.send(Err(e)); }
                                }
                            }
                            Err(e) => { let _ = reply.send(Err(e)); }
                        }
                    }
                    Cmd::Leave { session } => {
                        if let Some(idx) = spectators.iter().position(|p| p.session_id == session) {
                            spectators.remove(idx);
                            let mut i = info.write().await;
                            i.spectators = spectators.iter().map(|p| p.session_id).collect();
                            continue;
                        }
                        if let Some(idx) = members.iter().position(|p| p.session_id == session) {
                            let gone = members.remove(idx);
                            if journaling {
                                journal.push(JournalEntry { tick, op: Op::Leave { session } });
                            }
                            match handler.leave(std::mem::take(&mut state), std::slice::from_ref(&gone), &members) {
                                Ok(s) => state = s,
                                Err(e) => tracing::error!(%match_id, error = %e, "leave handler failed"),
                            }
                            deliver(watchers(&members, &spectators), ServerMessage::MatchPresenceEvent { match_id: match_id.clone(), joins: vec![], leaves: vec![gone] });
                        }
                    }
                    Cmd::Data(input) => {
                        if members.iter().any(|p| p.session_id == input.presence.session_id) {
                            inputs.push(input);
                        }
                    }
                    Cmd::Terminate { grace_seconds } => { grace = Some(grace_seconds); }
                }
                let mut i = info.write().await;
                i.size = members.len() as u32;
                i.state_bytes = state.len();
                drop(i);
                persist.save(id, &label, &state, tick);
            }
            _ = interval.tick() => {
                tick += 1;
                let batch = std::mem::take(&mut inputs);
                if journaling {
                    journal.push(JournalEntry { tick, op: Op::Tick { inputs: batch.clone() } });
                    if tick % persist.journal_every == 0 {
                        if let Some(cb) = &persist.on_journal {
                            let entries = std::mem::take(&mut journal);
                            cb(id, JournalChunk {
                                tick_from: entries.first().map(|e| e.tick).unwrap_or(tick),
                                tick_to: tick,
                                entries,
                            });
                        }
                    }
                }
                let res = match handler.tick(std::mem::take(&mut state), tick, batch, &members) {
                    Ok(r) => r,
                    Err(e) => { tracing::error!(%match_id, error = %e, "tick handler failed; terminating"); break; }
                };
                state = res.state;
                if let Some(l) = res.label { label = l; }
                for out in res.outbound {
                    let to = if out.to.is_empty() { watchers(&members, &spectators) } else { out.to };
                    deliver(to, ServerMessage::MatchData { match_id: match_id.clone(), presence: None, op_code: out.op_code, data: out.data, reliable: out.reliable });
                }
                {
                    let mut i = info.write().await;
                    i.tick = tick;
                    i.size = members.len() as u32;
                    i.label = label.clone();
                    i.state_bytes = state.len();
                }
                if persist.every > 0 && tick % persist.every == 0 {
                    persist.save(id, &label, &state, tick);
                }
                if res.terminate || grace.is_some_and(|g| g == 0) {
                    break;
                }
                if let Some(g) = grace.as_mut() { *g = g.saturating_sub(1000 / init.tick_rate.max(1)); }
            }
        }
    }
    if let Err(e) = handler.terminate(std::mem::take(&mut state), 0) {
        tracing::warn!(%match_id, error = %e, "terminate handler failed");
    }
    let all = watchers(&members, &spectators);
    if !all.is_empty() {
        deliver(
            all,
            ServerMessage::MatchPresenceEvent {
                match_id: match_id.clone(),
                joins: vec![],
                leaves: members.clone(),
            },
        );
    }
    persist.clear(id);
    on_end(id);
}

/// Players plus spectators: everyone who receives what the match sends.
fn watchers(members: &[UserPresence], spectators: &[UserPresence]) -> Vec<SessionId> {
    members
        .iter()
        .chain(spectators.iter())
        .map(|p| p.session_id)
        .collect()
}

fn snapshot(
    match_id: &str,
    label: &str,
    members: &[UserPresence],
    me: &UserPresence,
) -> ServerMessage {
    ServerMessage::Match {
        match_id: match_id.to_string(),
        authoritative: true,
        label: Some(label.to_string()),
        size: members.len() as u32,
        presences: members.to_vec(),
        me: me.clone(),
        host: None,
    }
}

/// Reference handler: relays every input to everyone as a server message, caps players via
/// `params.max_players` (default 8), ends when empty for `params.empty_ticks` ticks (default 10 s worth).
pub struct EchoMatch;

#[derive(Serialize, Deserialize, Default)]
struct EchoState {
    max_players: usize,
    empty_ticks: u64,
    idle: u64,
    relayed: u64,
    had_members: bool,
}

impl MatchHandler for EchoMatch {
    fn init(&self, params: &serde_json::Value) -> Result<Init> {
        let tick_rate = params
            .get("tick_rate")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as u32;
        let st = EchoState {
            max_players: params
                .get("max_players")
                .and_then(|v| v.as_u64())
                .unwrap_or(8) as usize,
            empty_ticks: params
                .get("empty_ticks")
                .and_then(|v| v.as_u64())
                .unwrap_or(tick_rate as u64 * 10),
            ..Default::default()
        };
        Ok(Init {
            state: serde_json::to_vec(&st)?,
            tick_rate,
            label: params
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or("echo")
                .to_string(),
        })
    }
    fn join_attempt(
        &self,
        state: Vec<u8>,
        _p: &UserPresence,
        _m: &HashMap<String, String>,
        members: &[UserPresence],
    ) -> Result<(Vec<u8>, Result<()>)> {
        let st: EchoState = serde_json::from_slice(&state)?;
        let verdict = if members.len() >= st.max_players {
            Err(Error::ResourceExhausted("match is full".into()))
        } else {
            Ok(())
        };
        Ok((state, verdict))
    }
    fn join(&self, state: Vec<u8>, _j: &[UserPresence], _m: &[UserPresence]) -> Result<Vec<u8>> {
        Ok(state)
    }
    fn leave(&self, state: Vec<u8>, _l: &[UserPresence], _m: &[UserPresence]) -> Result<Vec<u8>> {
        Ok(state)
    }
    fn tick(
        &self,
        state: Vec<u8>,
        _tick: u64,
        inputs: Vec<Input>,
        members: &[UserPresence],
    ) -> Result<TickResult> {
        let mut st: EchoState = serde_json::from_slice(&state)?;
        let outbound = inputs
            .into_iter()
            .map(|i| {
                st.relayed += 1;
                Outbound {
                    to: vec![],
                    op_code: i.op_code,
                    data: i.data,
                    reliable: i.reliable,
                }
            })
            .collect();
        if !members.is_empty() {
            st.had_members = true;
        }
        st.idle = if members.is_empty() && st.had_members {
            st.idle + 1
        } else {
            0
        };
        let terminate = st.idle >= st.empty_ticks;
        Ok(TickResult {
            state: serde_json::to_vec(&st)?,
            outbound,
            label: None,
            terminate,
        })
    }
    fn terminate(&self, state: Vec<u8>, _g: u32) -> Result<Vec<u8>> {
        Ok(state)
    }
}
