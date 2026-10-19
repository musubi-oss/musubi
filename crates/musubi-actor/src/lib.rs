//! Authoritative match actors. A handler is pure over an opaque state blob: every callback receives
//! the current state and returns the next one, so the latest state is always a complete snapshot
//! (the basis for failover and migration). The actor owns the tick loop, membership and delivery.

pub mod state_sync;

use std::collections::{HashMap, VecDeque};
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
    /// Simulation tick the input belongs to (rollback matches); `None` = next tick.
    #[serde(default)]
    pub tick: Option<u64>,
    /// Client counter echoed in `match_input_ack`.
    #[serde(default)]
    pub seq: Option<u64>,
}

/// `params.rollback = { window, hash_every }`: keep the last `window` pre-tick states so a late
/// input can be honoured by re-simulating from its tick; publish a state hash every `hash_every`
/// ticks (0 = never).
#[derive(Debug, Clone, Copy)]
pub struct Rollback {
    pub window: u64,
    pub hash_every: u64,
}

impl Rollback {
    pub fn from_params(params: &serde_json::Value) -> Option<Self> {
        let r = params.get("rollback")?;
        if r.is_null() || r == &serde_json::Value::Bool(false) {
            return None;
        }
        let window = r
            .get("window")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 240);
        let hash_every = r.get("hash_every").and_then(|v| v.as_u64()).unwrap_or(1);
        Some(Self { window, hash_every })
    }
}

/// FNV-1a 64 over the state bytes — cheap, deterministic, same on every client runtime.
pub fn state_hash(state: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in state {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
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
    /// Module default; `Spawn::crash_policy` and `params.crash_policy` override it.
    pub crash_policy: CrashPolicy,
}

#[derive(Debug, Clone, Default)]
pub struct TickResult {
    pub state: Vec<u8>,
    pub outbound: Vec<Outbound>,
    pub label: Option<String>,
    pub terminate: bool,
}

/// What happens when the tick handler fails (`Err` / wasm trap) mid-match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashPolicy {
    /// Roll back to the state before the failing tick and keep running; the inputs of that tick
    /// are dropped. Consecutive crashes back off exponentially (1 s, 2 s, … 30 s) and more than
    /// `MAX_RESTARTS` within `CRASH_WINDOW` destroy the match.
    #[default]
    Restart,
    /// Snapshot the pre-crash state and stop; players get a leave event and the next join wakes
    /// the match from the snapshot (`Spawn::on_sleep` stores it).
    Sleep,
    /// Terminate the match.
    Destroy,
}

impl CrashPolicy {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "restart" => Some(Self::Restart),
            "sleep" => Some(Self::Sleep),
            "destroy" => Some(Self::Destroy),
            _ => None,
        }
    }
}

pub const MAX_RESTARTS: u32 = 5;
pub const CRASH_WINDOW: Duration = Duration::from_secs(60);

/// A handler failure and what the actor did about it.
#[derive(Debug, Clone, Serialize)]
pub struct Crash {
    pub match_id: MatchId,
    pub module: String,
    pub tick: u64,
    pub error: String,
    pub policy: CrashPolicy,
    /// Consecutive crashes within `CRASH_WINDOW`, this one included.
    pub crashes: u32,
    /// `restart`: how long the match pauses before the next tick.
    pub restart_in_ms: Option<u64>,
    /// `destroy`, or `restart` that exhausted `MAX_RESTARTS`.
    pub destroyed: bool,
    pub slept: bool,
}

pub type OnCrash = Arc<dyn Fn(Crash) + Send + Sync>;
pub type OnSleep = Arc<dyn Fn(MatchId, Snapshot) + Send + Sync>;

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

fn running() -> String {
    "running".into()
}

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
    /// Sync the JSON state to players (`match_state` on join, `match_state_delta` after every
    /// handler call); the list holds server-private JSON pointers stripped first.
    pub sync: Option<Vec<String>>,
    /// Overrides the module's `Init::crash_policy` (`params.crash_policy` wins over both).
    pub crash_policy: Option<CrashPolicy>,
    pub on_crash: Option<OnCrash>,
    /// Receives the snapshot of a match put to sleep by `CrashPolicy::Sleep`.
    pub on_sleep: Option<OnSleep>,
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
    /// `running` | `crash_loop_backoff` | `sleeping`.
    #[serde(default = "running")]
    pub status: String,
    /// Consecutive tick-handler crashes within `CRASH_WINDOW`.
    #[serde(default)]
    pub crashes: u32,
    /// Player cap from `params.max_players` (listing by open slots needs it).
    #[serde(default)]
    pub max_players: Option<u32>,
    /// Region of the node hosting the match (filled by the registry).
    #[serde(default)]
    pub region: String,
}

enum Cmd {
    Join {
        presence: UserPresence,
        metadata: HashMap<String, String>,
        /// `Some(delay_ticks)`: watch only, every message held back that many ticks.
        spectator: Option<u64>,
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
    /// `spectator: Some(delay_ticks)`: watch only — receives match data and presence events
    /// `delay_ticks` late (floored by the match's `params.spectator_delay_ticks`, so a stream
    /// cannot be used to ghost the players), is not counted as a player, never reaches the
    /// handler, and its own inputs are refused.
    pub async fn join(
        &self,
        presence: UserPresence,
        metadata: HashMap<String, String>,
        spectator: Option<u64>,
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

/// Delta of the public state view since the last broadcast, when the module syncs JSON state.
fn sync_ops(
    sync: &mut Option<state_sync::SyncState>,
    state: &[u8],
) -> Option<Vec<serde_json::Value>> {
    let sy = sync.as_mut()?;
    let v = state_sync::as_json(state)?;
    sy.advance(&v)
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
            sync: None,
            on_journal: None,
            journal_every: 0,
            crash_policy: None,
            on_crash: None,
            on_sleep: None,
        },
    )
}

pub fn spawn_with(handler: Arc<dyn MatchHandler>, sp: Spawn) -> Result<MatchHandle> {
    let sync = sp.sync.clone().map(state_sync::SyncState::new);
    let param_policy = match sp.params.get("crash_policy") {
        None => None,
        Some(v) => Some(v.as_str().and_then(CrashPolicy::parse).ok_or_else(|| {
            Error::InvalidArgument("crash_policy: restart | sleep | destroy".into())
        })?),
    };
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
                crash_policy: fresh.crash_policy,
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
    let policy = param_policy
        .or(sp.crash_policy)
        .unwrap_or(init.crash_policy);
    let info = Arc::new(RwLock::new(MatchInfo {
        spectators: Vec::new(),
        match_id: id.to_string(),
        module: sp.module.clone(),
        label: init.label.clone(),
        size: 0,
        tick_rate: init.tick_rate,
        tick: start_tick,
        state_bytes: init.state.len(),
        max_players: sp
            .params
            .get("max_players")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        region: String::new(),
        status: running(),
        crashes: 0,
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
            policy,
            on_crash: sp.on_crash,
            on_sleep: sp.on_sleep,
        },
        sync,
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
    policy: CrashPolicy,
    on_crash: Option<OnCrash>,
    on_sleep: Option<OnSleep>,
}

impl Persist {
    fn crash(&self, c: Crash) {
        if let Some(cb) = &self.on_crash {
            cb(c);
        }
    }
    fn sleep(&self, id: MatchId, label: &str, state: &[u8], tick: u64) {
        if let Some(cb) = &self.on_sleep {
            cb(
                id,
                Snapshot {
                    module: self.module.clone(),
                    label: label.to_string(),
                    params: self.params.clone(),
                    state: state.to_vec(),
                    tick,
                    replay: vec![],
                },
            );
        }
    }
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
    mut sync: Option<state_sync::SyncState>,
) {
    let match_id = id.to_string();
    let rollback = Rollback::from_params(&persist.params);
    // rollback: (tick, state before the tick, inputs applied in it), oldest first
    let mut history: std::collections::VecDeque<(u64, Vec<u8>, Vec<Input>)> =
        std::collections::VecDeque::new();
    let mut future: std::collections::BTreeMap<u64, Vec<Input>> = std::collections::BTreeMap::new();
    let mut state = init.state;
    let mut label = init.label;
    let mut members: Vec<UserPresence> = Vec::new();
    let mut spectators: Vec<UserPresence> = Vec::new();
    // spectators watching with a delay, and what is held back for them until its tick comes
    let mut delays: HashMap<SessionId, u64> = HashMap::new();
    let mut delayed: VecDeque<(u64, SessionId, ServerMessage)> = VecDeque::new();
    let min_delay = persist
        .params
        .get("spectator_delay_ticks")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let mut inputs: Vec<Input> = Vec::new();
    let mut tick: u64 = persist.start_tick;
    let mut interval = tokio::time::interval(Duration::from_millis(1000 / init.tick_rate as u64));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // first tick after a full period, so commands already queued (the join that woke a sleeping
    // match) are applied before the handler runs
    interval.reset();
    let sessions = |m: &[UserPresence]| m.iter().map(|p| p.session_id).collect::<Vec<_>>();
    let mut grace: Option<u32> = None;
    let mut crashes: u32 = 0;
    let mut last_crash: Option<std::time::Instant> = None;
    let mut sleeping = false;
    let journaling = persist.on_journal.is_some() && persist.journal_every > 0;
    let mut journal: Vec<JournalEntry> = Vec::new();
    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    Cmd::Join { presence, metadata, spectator, reply } => {
                        if let Some(delay) = spectator {
                            if !spectators.iter().any(|p| p.session_id == presence.session_id) {
                                spectators.push(presence.clone());
                            }
                            let delay = delay.max(min_delay);
                            if delay > 0 {
                                delays.insert(presence.session_id, delay);
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
                                        if let Some(sy) = sync.as_mut() {
                                            if let Some(ops) = state_sync::as_json(&state).and_then(|v| sy.advance(&v)) {
                                                fan(&deliver, &delays, &mut delayed, tick, others.clone(), ServerMessage::MatchStateDelta { match_id: match_id.clone(), tick, ops });
                                            }
                                            if let Some(view) = sy.last.clone() {
                                                deliver(vec![presence.session_id], ServerMessage::MatchState { match_id: match_id.clone(), tick, state: view });
                                            }
                                        }
                                        fan(&deliver, &delays, &mut delayed, tick, others, ServerMessage::MatchPresenceEvent { match_id: match_id.clone(), joins: vec![presence.clone()], leaves: vec![] });
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
                            delays.remove(&session);
                            delayed.retain(|(_, s, _)| *s != session);
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
                            if let Some(ops) = sync_ops(&mut sync, &state) {
                                fan(&deliver, &delays, &mut delayed, tick, watchers(&members, &spectators), ServerMessage::MatchStateDelta { match_id: match_id.clone(), tick, ops });
                            }
                            fan(&deliver, &delays, &mut delayed, tick, watchers(&members, &spectators), ServerMessage::MatchPresenceEvent { match_id: match_id.clone(), joins: vec![], leaves: vec![gone] });
                        }
                    }
                    Cmd::Data(input) => {
                        if !members.iter().any(|p| p.session_id == input.presence.session_id) {
                            continue;
                        }
                        let Some(t) = input.tick else { inputs.push(input); continue; };
                        let ack = |applied: bool| ServerMessage::MatchInputAck { match_id: match_id.clone(), tick: t, seq: input.seq.unwrap_or(0), applied };
                        let Some(rb) = rollback else {
                            deliver(vec![input.presence.session_id], ack(false));
                            inputs.push(input);
                            continue;
                        };
                        if t > tick {
                            deliver(vec![input.presence.session_id], ack(true));
                            future.entry(t).or_default().push(input);
                        } else if let Some(k) = history.iter().position(|(ht, _, _)| *ht == t) {
                            // late input inside the window: re-simulate from its tick with it added
                            deliver(vec![input.presence.session_id], ack(true));
                            history[k].2.push(input);
                            let mut st = history[k].1.clone();
                            let mut ok = true;
                            for (ht, before, ins) in history.iter_mut().skip(k) {
                                *before = st.clone();
                                match handler.tick(std::mem::take(&mut st), *ht, ins.clone(), &members) {
                                    Ok(r) => st = r.state,
                                    Err(e) => { tracing::error!(%match_id, error = %e, tick = *ht, "rollback re-simulation failed; keeping current state"); ok = false; break; }
                                }
                            }
                            if ok {
                                state = st;
                                let _ = rb;
                            }
                        } else {
                            deliver(vec![input.presence.session_id], ack(false));
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
                let mut batch = std::mem::take(&mut inputs);
                if rollback.is_some() {
                    let due: Vec<u64> = future.range(..=tick).map(|(t, _)| *t).collect();
                    for t in due {
                        if let Some(v) = future.remove(&t) { batch.extend(v); }
                    }
                }
                let before = rollback.map(|_| (state.clone(), batch.clone()));
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
                let before_crash = if persist.policy == CrashPolicy::Destroy { Vec::new() } else { state.clone() };
                let res = match handler.tick(std::mem::take(&mut state), tick, batch, &members) {
                    Ok(r) => r,
                    Err(e) => {
                        let now = std::time::Instant::now();
                        if last_crash.is_some_and(|t| now.duration_since(t) > CRASH_WINDOW) {
                            crashes = 0;
                        }
                        crashes += 1;
                        last_crash = Some(now);
                        state = before_crash;
                        let exhausted = persist.policy == CrashPolicy::Restart && crashes > MAX_RESTARTS;
                        let destroyed = persist.policy == CrashPolicy::Destroy || exhausted;
                        let slept = persist.policy == CrashPolicy::Sleep;
                        let restart_in = (!destroyed && !slept)
                            .then(|| Duration::from_secs(1u64 << (crashes - 1).min(5)).min(Duration::from_secs(30)));
                        tracing::error!(%match_id, error = %e, tick, crashes, policy = ?persist.policy, destroyed, slept,
                            "tick handler failed");
                        persist.crash(Crash {
                            match_id: id,
                            module: persist.module.clone(),
                            tick,
                            error: e.to_string(),
                            policy: persist.policy,
                            crashes,
                            restart_in_ms: restart_in.map(|d| d.as_millis() as u64),
                            destroyed,
                            slept,
                        });
                        if slept {
                            persist.sleep(id, &label, &state, tick);
                            sleeping = true;
                            break;
                        }
                        if destroyed {
                            break;
                        }
                        let delay = restart_in.unwrap_or_default();
                        {
                            let mut i = info.write().await;
                            i.status = "crash_loop_backoff".into();
                            i.crashes = crashes;
                        }
                        tokio::time::sleep(delay).await;
                        // a full period before the next tick: commands queued during the pause
                        // (joins, inputs that may fix the state) are applied first
                        interval.reset();
                        info.write().await.status = running();
                        continue;
                    }
                };
                state = res.state;
                if let (Some(rb), Some((before, ins))) = (rollback, before) {
                    history.push_back((tick, before, ins));
                    while history.len() as u64 > rb.window { history.pop_front(); }
                    if rb.hash_every > 0 && tick % rb.hash_every == 0 {
                        fan(&deliver, &delays, &mut delayed, tick, watchers(&members, &spectators), ServerMessage::MatchTickHash { match_id: match_id.clone(), tick, hash: state_hash(&state) });
                    }
                }
                if let Some(l) = res.label { label = l; }
                if let Some(ops) = sync_ops(&mut sync, &state) {
                    fan(&deliver, &delays, &mut delayed, tick, watchers(&members, &spectators), ServerMessage::MatchStateDelta { match_id: match_id.clone(), tick, ops });
                }
                let mut due = Vec::new();
                delayed.retain(|(at, s, m)| {
                    if *at <= tick {
                        due.push((*s, m.clone()));
                        false
                    } else {
                        true
                    }
                });
                for (s, m) in due {
                    deliver(vec![s], m);
                }
                for out in res.outbound {
                    let to = if out.to.is_empty() { watchers(&members, &spectators) } else { out.to };
                    fan(&deliver, &delays, &mut delayed, tick, to, ServerMessage::MatchData { match_id: match_id.clone(), presence: None, op_code: out.op_code, data: out.data, reliable: out.reliable, channel: None, seq: None });
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
    if sleeping {
        info.write().await.status = "sleeping".into();
    } else if let Err(e) = handler.terminate(std::mem::take(&mut state), 0) {
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
    if !sleeping {
        persist.clear(id);
    }
    on_end(id);
}

/// Send `msg` to `to` now, except to spectators watching with a delay: theirs is held back until
/// `tick + delay` and released by the tick loop.
fn fan(
    deliver: &Deliver,
    delays: &HashMap<SessionId, u64>,
    delayed: &mut VecDeque<(u64, SessionId, ServerMessage)>,
    tick: u64,
    to: Vec<SessionId>,
    msg: ServerMessage,
) {
    let (now, later): (Vec<SessionId>, Vec<SessionId>) = to
        .into_iter()
        .partition(|s| delays.get(s).is_none_or(|d| *d == 0));
    for s in later {
        delayed.push_back((tick + delays[&s], s, msg.clone()));
    }
    if !now.is_empty() {
        deliver(now, msg);
    }
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
            crash_policy: Default::default(),
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

#[cfg(test)]
mod rollback_tests {
    use super::*;

    /// Fails `tick` while the state's first byte is 1; otherwise counts ticks in the second byte.
    struct Crashy;

    impl MatchHandler for Crashy {
        fn init(&self, params: &serde_json::Value) -> Result<Init> {
            let crash = params
                .get("crash")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(Init {
                state: vec![crash as u8, 0],
                tick_rate: 50,
                label: String::new(),
                crash_policy: Default::default(),
            })
        }
        fn join_attempt(
            &self,
            state: Vec<u8>,
            _: &UserPresence,
            _: &HashMap<String, String>,
            _: &[UserPresence],
        ) -> Result<(Vec<u8>, Result<()>)> {
            Ok((state, Ok(())))
        }
        fn join(&self, state: Vec<u8>, _: &[UserPresence], _: &[UserPresence]) -> Result<Vec<u8>> {
            Ok(state)
        }
        fn leave(&self, state: Vec<u8>, _: &[UserPresence], _: &[UserPresence]) -> Result<Vec<u8>> {
            Ok(state)
        }
        fn tick(
            &self,
            mut state: Vec<u8>,
            _: u64,
            inputs: Vec<Input>,
            _: &[UserPresence],
        ) -> Result<TickResult> {
            if inputs.iter().any(|i| i.op_code == 9) {
                state[0] = 0;
            }
            if state[0] == 1 {
                return Err(Error::Internal("boom".into()));
            }
            state[1] = state[1].wrapping_add(1);
            Ok(TickResult {
                state,
                ..Default::default()
            })
        }
        fn terminate(&self, state: Vec<u8>, _: u32) -> Result<Vec<u8>> {
            Ok(state)
        }
    }

    type Crashes = Arc<std::sync::Mutex<Vec<Crash>>>;
    type Slept = Arc<std::sync::Mutex<Option<Snapshot>>>;

    fn crashy_spawn(policy: &str, crashes: Crashes, slept: Slept) -> Spawn {
        Spawn {
            id: None,
            module: "crashy".into(),
            params: serde_json::json!({ "crash": true, "crash_policy": policy }),
            resume: None,
            deliver: Arc::new(|_, _| {}),
            on_end: Box::new(|_| {}),
            on_snapshot: None,
            snapshot_every: 0,
            on_journal: None,
            journal_every: 0,
            sync: None,
            crash_policy: None,
            on_crash: Some(Arc::new(move |c| crashes.lock().unwrap().push(c))),
            on_sleep: Some(Arc::new(move |_, s| *slept.lock().unwrap() = Some(s))),
        }
    }

    #[tokio::test]
    async fn crash_policy_restart_rolls_back_and_backs_off_destroy_ends_sleep_snapshots() {
        let crashes: Crashes = Default::default();
        let slept: Slept = Default::default();
        assert!(spawn_with(
            Arc::new(Crashy),
            Spawn {
                params: serde_json::json!({ "crash_policy": "nope" }),
                ..crashy_spawn("restart", crashes.clone(), slept.clone())
            }
        )
        .is_err());

        // restart: first tick crashes, 1 s backoff, state rolled back; op 9 clears the flag
        let h = spawn_with(
            Arc::new(Crashy),
            crashy_spawn("restart", crashes.clone(), slept.clone()),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        let info = h.info().await;
        assert_eq!(info.status, "crash_loop_backoff");
        assert_eq!(info.crashes, 1);
        assert!(h.is_alive());
        {
            let c = crashes.lock().unwrap();
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].policy, CrashPolicy::Restart);
            assert_eq!(c[0].restart_in_ms, Some(1000));
            assert!(!c[0].destroyed && !c[0].slept);
            assert!(c[0].error.contains("boom"));
        }
        let me: UserPresence = serde_json::from_value(serde_json::json!({
            "account_id": musubi_core::AccountId::new().0, "session_id": musubi_core::SessionId::new().0, "username": "p"
        }))
        .unwrap();
        // commands queue up behind the backoff; a non-member's inputs are ignored, so join first
        h.join(me.clone(), HashMap::new(), None).await.unwrap();
        h.data(Input {
            presence: me,
            op_code: 9,
            data: vec![],
            reliable: true,
            tick: None,
            seq: None,
        })
        .unwrap();
        // the flag-clearing input may land after a second crashing tick (2 s backoff): poll
        let mut info = h.info().await;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(150)).await;
            info = h.info().await;
            if info.status == "running" && info.tick > 2 {
                break;
            }
        }
        assert_eq!(info.status, "running", "{info:?}");
        assert!(h.is_alive());
        assert!(info.tick > 2, "{info:?}");
        let n = crashes.lock().unwrap().len();
        assert!(n <= 2, "crashes: {n}");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            crashes.lock().unwrap().len(),
            n,
            "no crash after the flag cleared"
        );

        // destroy: ends at the first crash
        crashes.lock().unwrap().clear();
        let h = spawn_with(
            Arc::new(Crashy),
            crashy_spawn("destroy", crashes.clone(), slept.clone()),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!h.is_alive());
        {
            let c = crashes.lock().unwrap();
            assert_eq!(c.len(), 1);
            assert!(c[0].destroyed && !c[0].slept && c[0].restart_in_ms.is_none());
        }

        // sleep: snapshot of the pre-crash state handed out, actor gone
        crashes.lock().unwrap().clear();
        let h = spawn_with(
            Arc::new(Crashy),
            crashy_spawn("sleep", crashes.clone(), slept.clone()),
        )
        .unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(!h.is_alive());
        assert_eq!(h.info().await.status, "sleeping");
        let snap = slept.lock().unwrap().clone().expect("on_sleep called");
        assert_eq!(snap.state, vec![1, 0]);
        assert_eq!(snap.module, "crashy");
        let c = crashes.lock().unwrap();
        assert!(c[0].slept && !c[0].destroyed);
    }

    /// state = little-endian u64 total; every tick adds `tick * sum(input bytes)` — the result
    /// depends on which tick an input lands in, so a late input is only "right" after rollback.
    struct Ledger;

    fn total(state: &[u8]) -> u64 {
        if state.len() == 8 {
            u64::from_le_bytes(state.try_into().unwrap())
        } else {
            0
        }
    }

    impl MatchHandler for Ledger {
        fn init(&self, _params: &serde_json::Value) -> Result<Init> {
            Ok(Init {
                state: 0u64.to_le_bytes().to_vec(),
                tick_rate: 50,
                label: "ledger".into(),
                crash_policy: Default::default(),
            })
        }
        fn join_attempt(
            &self,
            state: Vec<u8>,
            _presence: &UserPresence,
            _metadata: &HashMap<String, String>,
            _members: &[UserPresence],
        ) -> Result<(Vec<u8>, Result<()>)> {
            Ok((state, Ok(())))
        }
        fn join(
            &self,
            state: Vec<u8>,
            _j: &[UserPresence],
            _m: &[UserPresence],
        ) -> Result<Vec<u8>> {
            Ok(state)
        }
        fn leave(
            &self,
            state: Vec<u8>,
            _l: &[UserPresence],
            _m: &[UserPresence],
        ) -> Result<Vec<u8>> {
            Ok(state)
        }
        fn tick(
            &self,
            state: Vec<u8>,
            tick: u64,
            inputs: Vec<Input>,
            _members: &[UserPresence],
        ) -> Result<TickResult> {
            let add: u64 = inputs
                .iter()
                .map(|i| i.data.iter().map(|b| *b as u64).sum::<u64>())
                .sum();
            Ok(TickResult {
                state: (total(&state) + tick * add).to_le_bytes().to_vec(),
                outbound: vec![],
                label: None,
                terminate: false,
            })
        }
        fn terminate(&self, state: Vec<u8>, _g: u32) -> Result<Vec<u8>> {
            Ok(state)
        }
    }

    fn presence() -> UserPresence {
        serde_json::from_value(serde_json::json!({
            "account_id": musubi_core::AccountId::new().0,
            "session_id": musubi_core::SessionId::new().0,
            "username": "a",
        }))
        .unwrap()
    }

    async fn next_hash(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMessage>) -> (u64, u64) {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
                .await
                .expect("message")
                .expect("open")
            {
                ServerMessage::MatchTickHash { tick, hash, .. } => return (tick, hash),
                _ => continue,
            }
        }
    }

    async fn next_ack(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<ServerMessage>,
    ) -> (u64, u64, bool) {
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
                .await
                .expect("message")
                .expect("open")
            {
                ServerMessage::MatchInputAck {
                    tick, seq, applied, ..
                } => return (tick, seq, applied),
                _ => continue,
            }
        }
    }

    #[tokio::test]
    async fn late_inputs_rollback_future_inputs_wait_old_ones_drop() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let deliver: Deliver = Arc::new(move |_to, msg| {
            let _ = tx.send(msg);
        });
        let h = spawn_with(
            Arc::new(Ledger),
            Spawn {
                id: None,
                module: "ledger".into(),
                params: serde_json::json!({ "rollback": { "window": 32, "hash_every": 1 } }),
                resume: None,
                deliver,
                on_end: Box::new(|_| {}),
                on_snapshot: None,
                snapshot_every: 0,
                on_journal: None,
                journal_every: 0,
                sync: None,
                crash_policy: None,
                on_crash: None,
                on_sleep: None,
            },
        )
        .unwrap();
        let me = presence();
        h.join(me.clone(), HashMap::new(), None).await.unwrap();
        let input = |tick: u64, seq: u64, byte: u8| Input {
            presence: me.clone(),
            op_code: 1,
            data: vec![byte],
            reliable: true,
            tick: Some(tick),
            seq: Some(seq),
        };

        // let a few ticks pass; nothing happened yet so the hash is of total 0
        let (t0, h0) = next_hash(&mut rx).await;
        assert_eq!(h0, state_hash(&0u64.to_le_bytes()));
        let (t1, _) = next_hash(&mut rx).await;
        assert!(t1 > t0);

        // a late input for tick t0 (already simulated) is honoured by rollback: total = t0 * 10
        h.data(input(t0, 1, 10)).unwrap();
        let (at, seq, applied) = next_ack(&mut rx).await;
        assert_eq!((at, seq, applied), (t0, 1, true));
        let (_, hash) = next_hash(&mut rx).await;
        assert_eq!(
            hash,
            state_hash(&(t0 * 10).to_le_bytes()),
            "re-simulated from tick {t0}"
        );

        // an input for a future tick waits for it: total += tf * 1
        let (now, _) = next_hash(&mut rx).await;
        let tf = now + 5;
        h.data(input(tf, 2, 1)).unwrap();
        assert_eq!(next_ack(&mut rx).await, (tf, 2, true));
        let expected = state_hash(&(t0 * 10 + tf).to_le_bytes());
        let mut seen = None;
        for _ in 0..12 {
            let (t, hs) = next_hash(&mut rx).await;
            if t >= tf {
                seen = Some(hs);
                break;
            }
        }
        assert_eq!(
            seen,
            Some(expected),
            "future input applied exactly at tick {tf}"
        );

        // older than the window: dropped and acknowledged as not applied; state unchanged
        let (now, _) = next_hash(&mut rx).await;
        h.data(input(now.saturating_sub(100), 3, 99)).unwrap();
        let (_, seq, applied) = next_ack(&mut rx).await;
        assert_eq!((seq, applied), (3, false));
        let (_, hs) = next_hash(&mut rx).await;
        assert_eq!(hs, expected);

        // a tick with no rollback config acknowledges `applied: false` but still applies next tick
        let (tx2, mut rx2) = tokio::sync::mpsc::unbounded_channel();
        let deliver2: Deliver = Arc::new(move |_to, msg| {
            let _ = tx2.send(msg);
        });
        let plain = spawn_with(
            Arc::new(Ledger),
            Spawn {
                id: None,
                module: "ledger".into(),
                params: serde_json::json!({}),
                resume: None,
                deliver: deliver2,
                on_end: Box::new(|_| {}),
                on_snapshot: None,
                snapshot_every: 0,
                on_journal: None,
                journal_every: 0,
                sync: None,
                crash_policy: None,
                on_crash: None,
                on_sleep: None,
            },
        )
        .unwrap();
        plain.join(me.clone(), HashMap::new(), None).await.unwrap();
        plain.data(input(1, 7, 1)).unwrap();
        assert_eq!(next_ack(&mut rx2).await, (1, 7, false));
        h.terminate(0);
        plain.terminate(0);
    }
}
