//! Matchmaker: a process-local ticket pool with mutual-query matching. Tickets carry one or more
//! presences (a party submits a single ticket). Matching is greedy FIFO: the oldest ticket seeds a
//! group, compatible tickets join until the seed's `max_count` is filled (or at least `min_count`
//! once the seed has waited `min_wait`), honouring `count_multiple`.

pub mod query;
pub mod queue;
pub mod ready;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use musubi_core::{AccountId, Error, Result, SessionId};
use parking_lot::Mutex;

pub use query::{Properties, Query};
use queue::{assign_teams, quality};
pub use queue::{CandidateMatch, MmrRule, PingRule, Quality, QueueConfig, Recent, Scorer, Teams};
pub use ready::{Pending, ReadyExpired, ReadyStart};
use std::collections::HashSet;
use std::sync::Arc;

pub const MAX_TICKETS_PER_SESSION: usize = 3;
pub const MAX_COUNT: usize = 100;

pub type Candidate = musubi_proto::realtime::MatchmakerUser;

#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: String,
    pub party_id: Option<String>,
    /// Named queue (string property `queue`); such tickets follow the queue's rules, not mutual queries.
    pub queue: Option<String>,
    pub members: Vec<Candidate>,
    pub query: Query,
    pub query_src: String,
    pub props: Properties,
    pub min_count: usize,
    pub max_count: usize,
    pub count_multiple: usize,
    pub created: Instant,
}

#[derive(Debug, Clone, Default)]
pub struct Matched {
    pub token: String,
    pub tickets: Vec<Ticket>,
    /// Set for queue matches: the queue, the region with the lowest worst ping and the grade.
    pub queue: Option<String>,
    pub region: Option<String>,
    pub quality: Option<Quality>,
}

impl Matched {
    pub fn users(&self) -> Vec<Candidate> {
        self.tickets
            .iter()
            .flat_map(|t| t.members.iter().cloned())
            .collect()
    }
}

/// Serializable ticket for replication between nodes.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TicketWire {
    pub id: String,
    pub party_id: Option<String>,
    #[serde(default)]
    pub queue: Option<String>,
    pub members: Vec<Candidate>,
    pub query: String,
    pub min_count: usize,
    pub max_count: usize,
    pub count_multiple: usize,
    pub age_ms: u64,
}

impl From<&Ticket> for TicketWire {
    fn from(t: &Ticket) -> Self {
        Self {
            id: t.id.clone(),
            party_id: t.party_id.clone(),
            queue: t.queue.clone(),
            members: t.members.clone(),
            query: t.query_src.clone(),
            min_count: t.min_count,
            max_count: t.max_count,
            count_multiple: t.count_multiple,
            age_ms: t.created.elapsed().as_millis() as u64,
        }
    }
}

impl TicketWire {
    pub fn into_ticket(self) -> Result<Ticket> {
        let query = Query::parse(&self.query)?;
        let props = merged_props(&self.members);
        Ok(Ticket {
            id: self.id,
            party_id: self.party_id,
            queue: self.queue,
            members: self.members,
            query,
            query_src: self.query,
            props,
            min_count: self.min_count,
            max_count: self.max_count,
            count_multiple: self.count_multiple,
            created: Instant::now()
                .checked_sub(Duration::from_millis(self.age_ms))
                .unwrap_or_else(Instant::now),
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatchedWire {
    pub token: String,
    /// Relayed match id minted by the leader (homed on it) so every node maps token → match.
    #[serde(default)]
    pub match_id: String,
    #[serde(default)]
    pub server: Option<musubi_proto::realtime::MatchmakerServer>,
    pub tickets: Vec<TicketWire>,
    #[serde(default)]
    pub queue: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub quality: Option<Quality>,
}

impl Matched {
    /// Fleet every member asked for via the `fleet` string property (None if absent or mixed).
    pub fn fleet(&self) -> Option<String> {
        let mut out: Option<String> = None;
        for t in &self.tickets {
            for m in &t.members {
                let f = m.string_properties.get("fleet")?;
                match &out {
                    None => out = Some(f.clone()),
                    Some(x) if x == f => {}
                    Some(_) => return None,
                }
            }
        }
        out
    }

    pub fn to_wire(
        &self,
        match_id: &str,
        server: Option<musubi_proto::realtime::MatchmakerServer>,
    ) -> MatchedWire {
        MatchedWire {
            token: self.token.clone(),
            match_id: match_id.to_string(),
            server,
            tickets: self.tickets.iter().map(TicketWire::from).collect(),
            queue: self.queue.clone(),
            region: self.region.clone(),
            quality: self.quality,
        }
    }

    pub fn from_wire(w: MatchedWire) -> Result<Self> {
        Ok(Self {
            token: w.token,
            queue: w.queue,
            region: w.region,
            quality: w.quality,
            tickets: w
                .tickets
                .into_iter()
                .map(TicketWire::into_ticket)
                .collect::<Result<_>>()?,
        })
    }
}

pub struct AddTicket {
    pub party_id: Option<String>,
    pub members: Vec<Candidate>,
    pub query: String,
    pub min_count: usize,
    pub max_count: usize,
    pub count_multiple: usize,
}

/// A matchmaker-formed match that still has room: later tickets compatible with everyone already in
/// it are sent there instead of waiting for a brand-new group.
#[derive(Debug, Clone)]
struct OpenMatch {
    match_id: String,
    token: String,
    tickets: Vec<Ticket>,
    size: usize,
    max: usize,
    created: Instant,
}

#[derive(Default)]
struct Pool {
    tickets: Vec<Ticket>,
    open: Vec<OpenMatch>,
    by_session: HashMap<SessionId, Vec<String>>,
}

pub struct Matchmaker {
    pool: Mutex<Pool>,
    queues: parking_lot::RwLock<HashMap<String, QueueConfig>>,
    recent: Mutex<Recent>,
    scorer: Option<Arc<dyn Scorer>>,
    /// Why queue candidates were kept apart, per (queue, reason) — the operator's expansion view.
    rejections: Mutex<HashMap<(String, &'static str), u64>>,
    /// Formed groups waiting for every member to confirm (leader only).
    pending: Mutex<Vec<Pending>>,
    penalties: Mutex<ready::Penalties>,
    /// Default ready-check length (zero = off) and dodge keep-out for queues that do not set one.
    ready_check: Duration,
    dodge_penalty: Duration,
    /// How long a seed waits for `max_count` before settling for `min_count`.
    min_wait: Duration,
    /// Tickets older than this are dropped by `expire` (the client gets `MatchmakerExpired`).
    ttl: Duration,
    /// Formed matches stay open for backfill this long (`None` = off).
    backfill_ttl: Option<Duration>,
    /// Recent wait times (seconds) per (queue, region) — the operator's p50/p95 without Prometheus.
    waits: Mutex<HashMap<(String, String), std::collections::VecDeque<f32>>>,
}

const WAIT_SAMPLES: usize = 2048;

/// Wait-time percentiles of one (queue, region) over the last `WAIT_SAMPLES` matched tickets.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WaitStat {
    pub queue: String,
    pub region: String,
    pub count: usize,
    pub p50_secs: f32,
    pub p95_secs: f32,
    pub p99_secs: f32,
    pub max_secs: f32,
}

/// How long the tickets currently waiting in a queue have been there.
#[derive(Debug, Clone, serde::Serialize, Default)]
pub struct WaitingAges {
    pub count: usize,
    pub oldest_secs: f32,
    pub mean_secs: f32,
}

fn percentile(sorted: &[f32], p: f64) -> f32 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

pub const DEFAULT_TICKET_TTL: Duration = Duration::from_secs(180);

impl Default for Matchmaker {
    fn default() -> Self {
        Self::new(Duration::from_secs(15))
    }
}

impl Matchmaker {
    pub fn new(min_wait: Duration) -> Self {
        Self {
            pool: Mutex::new(Pool::default()),
            queues: parking_lot::RwLock::new(HashMap::new()),
            recent: Mutex::new(Recent::default()),
            scorer: None,
            rejections: Mutex::new(HashMap::new()),
            pending: Mutex::new(Vec::new()),
            penalties: Mutex::new(ready::Penalties::default()),
            ready_check: Duration::ZERO,
            dodge_penalty: Duration::from_secs(60),
            min_wait,
            backfill_ttl: None,
            ttl: DEFAULT_TICKET_TTL,
            waits: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_queues(self, queues: Vec<QueueConfig>) -> Result<Self> {
        for q in queues {
            self.set_queue(q)?;
        }
        Ok(self)
    }
    /// Ready-check every formed group for `check` (zero = off); members who do not confirm are
    /// kept out for `penalty` (doubling per repeat within 24 h, up to 8×). Queues may override.
    pub fn with_ready_check(mut self, check: Duration, penalty: Duration) -> Self {
        self.ready_check = check;
        self.dodge_penalty = penalty;
        self
    }

    /// Leader: turn a formed group into a pending ready-check if its queue (or the default) asks
    /// for one. `None` means deliver straight away.
    pub fn start_ready_check(&self, m: Matched, match_id: &str) -> ReadyStart {
        let (check, penalty) = {
            let queues = self.queues.read();
            let q = m.queue.as_ref().and_then(|n| queues.get(n));
            (
                q.and_then(|q| q.ready_check_secs)
                    .map_or(self.ready_check, Duration::from_secs),
                q.and_then(|q| q.dodge_penalty_secs)
                    .map_or(self.dodge_penalty, Duration::from_secs),
            )
        };
        if check.is_zero() {
            return ReadyStart::Deliver(m);
        }
        let p = Pending {
            token: m.token.clone(),
            match_id: match_id.to_string(),
            matched: m,
            deadline: Instant::now() + check,
            ready: HashSet::new(),
            penalty,
        };
        self.pending.lock().push(p.clone());
        ReadyStart::Check(p)
    }

    /// Leader: a member of `ticket` confirmed. Returns the match (with its id) once every ticket
    /// of the group has; `Err` names the problem (unknown ticket, foreign session).
    pub fn mark_ready(
        &self,
        ticket: &str,
        session: SessionId,
    ) -> Result<Option<(Matched, String)>> {
        let mut pending = self.pending.lock();
        let Some(i) = pending
            .iter()
            .position(|p| p.matched.tickets.iter().any(|t| t.id == ticket))
        else {
            return Err(Error::NotFound(format!(
                "ticket {ticket} is not in a ready-check"
            )));
        };
        let p = &mut pending[i];
        let t = p.matched.tickets.iter().find(|t| t.id == ticket).unwrap();
        if !t.members.iter().any(|m| m.presence.session_id == session) {
            return Err(Error::PermissionDenied("not your ticket".into()));
        }
        p.ready.insert(ticket.to_string());
        if !p.all_ready() {
            return Ok(None);
        }
        let p = pending.remove(i);
        Ok(Some((p.matched, p.match_id)))
    }

    /// Leader: expire ready-checks past their deadline. Confirmed tickets return to the pool with
    /// their original wait; the others' accounts are penalised. Also forgets stale penalties.
    pub fn expire_ready(&self) -> Vec<ReadyExpired> {
        let now = Instant::now();
        let due: Vec<Pending> = {
            let mut pending = self.pending.lock();
            let (due, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut *pending)
                .into_iter()
                .partition(|p| p.deadline <= now);
            *pending = keep;
            due
        };
        let mut out = Vec::new();
        for p in due {
            let (requeued, dodged): (Vec<Ticket>, Vec<Ticket>) = p
                .matched
                .tickets
                .into_iter()
                .partition(|t| p.ready.contains(&t.id));
            self.requeue(requeued.clone());
            let mut penalty = Duration::ZERO;
            {
                let mut pen = self.penalties.lock();
                for t in &dodged {
                    for m in &t.members {
                        penalty = penalty.max(pen.strike(m.presence.account_id, p.penalty, now));
                    }
                }
                pen.prune(now);
            }
            out.push(ReadyExpired {
                token: p.token,
                requeued,
                dodged,
                penalty,
            });
        }
        out
    }

    /// Put tickets back into the pool unchanged (same id, same `created`), skipping ids present.
    pub fn requeue(&self, tickets: Vec<Ticket>) {
        let mut pool = self.pool.lock();
        for t in tickets {
            if pool.tickets.iter().any(|x| x.id == t.id) {
                continue;
            }
            for m in &t.members {
                pool.by_session
                    .entry(m.presence.session_id)
                    .or_default()
                    .push(t.id.clone());
            }
            pool.tickets.push(t);
        }
    }

    /// Replica: adopt a keep-out decided by the leader.
    pub fn penalize_until(&self, account: AccountId, until: Instant) {
        self.penalties.lock().set(account, until);
    }

    /// How much longer the account is kept out of the matchmaker, if at all.
    pub fn penalty_remaining(&self, account: AccountId) -> Option<Duration> {
        self.penalties.lock().remaining(account, Instant::now())
    }

    pub fn pending_count(&self) -> usize {
        self.pending.lock().len()
    }

    pub fn with_scorer(mut self, scorer: Arc<dyn Scorer>) -> Self {
        self.scorer = Some(scorer);
        self
    }
    /// Add or replace a queue at runtime; waiting tickets of that queue follow the new rules.
    pub fn set_queue(&self, q: QueueConfig) -> Result<()> {
        q.validate()?;
        self.queues.write().insert(q.name.clone(), q);
        Ok(())
    }
    pub fn remove_queue(&self, name: &str) -> bool {
        self.queues.write().remove(name).is_some()
    }
    pub fn queues(&self) -> Vec<QueueConfig> {
        let mut v: Vec<QueueConfig> = self.queues.read().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }
    /// Waiting tickets per queue (`""` = free-form tickets).
    pub fn waiting(&self) -> BTreeMap<String, usize> {
        let pool = self.pool.lock();
        let mut out = BTreeMap::new();
        for t in &pool.tickets {
            *out.entry(t.queue.clone().unwrap_or_default()).or_insert(0) += 1;
        }
        out
    }
    /// Remember how long a matched ticket waited (seconds) in `queue` / `region`.
    pub fn record_wait(&self, queue: &str, region: &str, secs: f32) {
        let mut w = self.waits.lock();
        let v = w
            .entry((queue.to_string(), region.to_string()))
            .or_default();
        if v.len() >= WAIT_SAMPLES {
            v.pop_front();
        }
        v.push_back(secs);
    }

    /// p50/p95/p99/max wait per (queue, region) over the recent samples.
    pub fn wait_stats(&self) -> Vec<WaitStat> {
        let w = self.waits.lock();
        let mut keys: Vec<_> = w.keys().cloned().collect();
        keys.sort();
        keys.into_iter()
            .map(|k| {
                let mut v: Vec<f32> = w[&k].iter().copied().collect();
                v.sort_by(|a, b| a.total_cmp(b));
                WaitStat {
                    queue: k.0,
                    region: k.1,
                    count: v.len(),
                    p50_secs: percentile(&v, 0.5),
                    p95_secs: percentile(&v, 0.95),
                    p99_secs: percentile(&v, 0.99),
                    max_secs: v.last().copied().unwrap_or(0.0),
                }
            })
            .collect()
    }

    /// Age of the tickets waiting right now, per queue (`""` = mutual-query tickets).
    pub fn waiting_ages(&self) -> BTreeMap<String, WaitingAges> {
        self.waiting_ages_at(Instant::now())
    }

    pub fn waiting_ages_at(&self, now: Instant) -> BTreeMap<String, WaitingAges> {
        let pool = self.pool.lock();
        let mut out: BTreeMap<String, WaitingAges> = BTreeMap::new();
        for t in &pool.tickets {
            let age = now.saturating_duration_since(t.created).as_secs_f32();
            let e = out.entry(t.queue.clone().unwrap_or_default()).or_default();
            e.count += 1;
            e.oldest_secs = e.oldest_secs.max(age);
            e.mean_secs += age;
        }
        for e in out.values_mut() {
            if e.count > 0 {
                e.mean_secs /= e.count as f32;
            }
        }
        out
    }

    /// Counts of (queue, reason) a pair of tickets was kept apart since the last call.
    pub fn take_rejections(&self) -> Vec<(String, &'static str, u64)> {
        std::mem::take(&mut *self.rejections.lock())
            .into_iter()
            .map(|((q, r), n)| (q, r, n))
            .collect()
    }
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Drop tickets that waited longer than the TTL and return them so their members can be told.
    /// Replicated tickets carry their age, so every node expires the same tickets.
    pub fn expire(&self) -> Vec<Ticket> {
        self.expire_older_than(self.ttl)
    }

    pub fn expire_older_than(&self, ttl: Duration) -> Vec<Ticket> {
        let now = Instant::now();
        let mut pool = self.pool.lock();
        let mut out = Vec::new();
        let mut i = 0;
        while i < pool.tickets.len() {
            if now.duration_since(pool.tickets[i].created) >= ttl {
                let t = pool.tickets.remove(i);
                unindex(&mut pool, &t);
                out.push(t);
            } else {
                i += 1;
            }
        }
        out
    }

    /// Keep formed matches open for `ttl` so later compatible tickets join them (backfill).
    pub fn with_backfill(mut self, ttl: Option<Duration>) -> Self {
        self.backfill_ttl = ttl.filter(|d| !d.is_zero());
        self
    }

    /// Remember a just-formed match as open for backfill when its tickets allow more players.
    pub fn register_open(&self, match_id: &str, m: &Matched) {
        if self.backfill_ttl.is_none() || m.tickets.is_empty() {
            return;
        }
        let size: usize = m.tickets.iter().map(|t| t.members.len()).sum();
        let max = m.tickets.iter().map(|t| t.max_count).min().unwrap_or(0);
        if size >= max {
            return;
        }
        self.pool.lock().open.push(OpenMatch {
            match_id: match_id.to_string(),
            token: m.token.clone(),
            tickets: m.tickets.clone(),
            size,
            max,
            created: Instant::now(),
        });
    }

    pub fn open_count(&self) -> usize {
        self.pool
            .lock()
            .open
            .iter()
            .filter(|o| o.size < o.max)
            .count()
    }

    /// A running match wants players again: `Some(n)` frees `n` slots (a member left, or the game
    /// server asked for more) and restarts the backfill window; `None` closes the match for good
    /// (it ended). Unknown matches — formed before this leader, or not by the matchmaker — are
    /// ignored. Returns whether the match was known.
    pub fn release_slots(&self, match_id: &str, slots: Option<usize>) -> bool {
        let mut pool = self.pool.lock();
        let Some(k) = pool.open.iter().position(|o| o.match_id == match_id) else {
            return false;
        };
        match slots {
            None => {
                pool.open.remove(k);
            }
            Some(n) => {
                let o = &mut pool.open[k];
                o.size = o.size.saturating_sub(n);
                o.created = Instant::now();
            }
        }
        true
    }

    /// Send waiting tickets into open matches they are compatible with (every member already there
    /// must accept them and vice versa, the size must stay within everyone's limits and multiple).
    /// Returns each backfilled group with the id of the match it joins (same token as the original).
    pub fn process_backfill(&self) -> Vec<(Matched, String)> {
        let Some(ttl) = self.backfill_ttl else {
            return vec![];
        };
        let now = Instant::now();
        let mut pool = self.pool.lock();
        // full matches stay remembered until the ttl runs out: a leaver (or the game server)
        // may release slots in them, see `release_slots`
        pool.open.retain(|o| now.duration_since(o.created) < ttl);
        if pool.open.iter().all(|o| o.size >= o.max) {
            return vec![];
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < pool.tickets.len() {
            let t = pool.tickets[i].clone();
            let n = t.members.len();
            let slot = pool.open.iter().position(|o| {
                o.size < o.max
                    && o.size + n <= o.max
                    && (o.size + n) % t.count_multiple == 0
                    && o.size + n >= t.min_count
                    && o.size + n <= t.max_count
                    && o.tickets.iter().all(|e| {
                        compatible(e, &t)
                            && o.size + n <= e.max_count
                            && (o.size + n) % e.count_multiple == 0
                    })
            });
            let Some(k) = slot else {
                i += 1;
                continue;
            };
            let t = pool.tickets.remove(i);
            unindex(&mut pool, &t);
            let o = &mut pool.open[k];
            o.size += n;
            o.tickets.push(t.clone());
            out.push((
                Matched {
                    token: o.token.clone(),
                    tickets: vec![t],
                    ..Default::default()
                },
                o.match_id.clone(),
            ));
        }
        out
    }

    pub fn ticket_count(&self) -> usize {
        self.pool.lock().tickets.len()
    }

    pub fn add(&self, t: AddTicket) -> Result<String> {
        self.add_at(t, Instant::now())
    }

    /// `add` with an explicit clock for the ticket's `created`.
    pub fn add_at(&self, t: AddTicket, now: Instant) -> Result<String> {
        if t.members.is_empty() {
            return Err(Error::InvalidArgument(
                "ticket needs at least one member".into(),
            ));
        }
        {
            let pen = self.penalties.lock();
            for m in &t.members {
                if let Some(left) = pen.remaining(m.presence.account_id, now) {
                    return Err(Error::PermissionDenied(format!(
                        "{} dodged a ready-check: matchmaking blocked for {} s",
                        m.presence.username,
                        left.as_secs().max(1)
                    )));
                }
            }
        }
        if t.min_count < 2 || t.max_count < t.min_count || t.max_count > MAX_COUNT {
            return Err(Error::InvalidArgument(format!(
                "need 2 <= min_count <= max_count <= {MAX_COUNT}"
            )));
        }
        if t.count_multiple == 0
            || t.min_count % t.count_multiple != 0
            || t.max_count % t.count_multiple != 0
        {
            return Err(Error::InvalidArgument(
                "min_count and max_count must be multiples of count_multiple".into(),
            ));
        }
        if t.members.len() > t.max_count {
            return Err(Error::InvalidArgument("party larger than max_count".into()));
        }
        let query = Query::parse(&t.query)?;
        let props = merged_props(&t.members);
        let queue = props.strings.get("queue").cloned();
        let (min_count, max_count, count_multiple) = match &queue {
            Some(name) => {
                let queues = self.queues.read();
                let q = queues.get(name).ok_or_else(|| {
                    Error::NotFound(format!("matchmaker queue {name} does not exist"))
                })?;
                if t.members.len() > q.max_count
                    || q.teams.is_some_and(|tm| t.members.len() > tm.size)
                {
                    return Err(Error::InvalidArgument(
                        "party larger than the queue's team".into(),
                    ));
                }
                (q.min_count, q.max_count, 1)
            }
            None => (t.min_count, t.max_count, t.count_multiple),
        };
        let mut pool = self.pool.lock();
        for m in &t.members {
            if pool
                .by_session
                .get(&m.presence.session_id)
                .is_some_and(|v| v.len() >= MAX_TICKETS_PER_SESSION)
            {
                return Err(Error::ResourceExhausted(format!(
                    "at most {MAX_TICKETS_PER_SESSION} tickets per session"
                )));
            }
        }
        let id = uuid::Uuid::now_v7().to_string();
        for m in &t.members {
            pool.by_session
                .entry(m.presence.session_id)
                .or_default()
                .push(id.clone());
        }
        pool.tickets.push(Ticket {
            id: id.clone(),
            party_id: t.party_id,
            queue,
            members: t.members,
            query,
            query_src: t.query,
            props,
            min_count,
            max_count,
            count_multiple,
            created: now,
        });
        Ok(id)
    }

    pub fn get(&self, ticket: &str) -> Option<TicketWire> {
        self.pool
            .lock()
            .tickets
            .iter()
            .find(|t| t.id == ticket)
            .map(TicketWire::from)
    }

    /// Insert a ticket replicated from another node (no per-session limits; idempotent).
    pub fn add_replica(&self, w: TicketWire) -> Result<bool> {
        let t = w.into_ticket()?;
        let mut pool = self.pool.lock();
        if pool.tickets.iter().any(|x| x.id == t.id) {
            return Ok(false);
        }
        for m in &t.members {
            pool.by_session
                .entry(m.presence.session_id)
                .or_default()
                .push(t.id.clone());
        }
        pool.tickets.push(t);
        Ok(true)
    }

    /// Remove a ticket; only a session that is part of it may do so.
    pub fn remove(&self, session: SessionId, ticket: &str) -> Result<()> {
        let mut pool = self.pool.lock();
        let Some(idx) = pool.tickets.iter().position(|t| t.id == ticket) else {
            return Err(Error::NotFound("ticket".into()));
        };
        if !pool.tickets[idx]
            .members
            .iter()
            .any(|m| m.presence.session_id == session)
        {
            return Err(Error::PermissionDenied("not your ticket".into()));
        }
        let t = pool.tickets.remove(idx);
        unindex(&mut pool, &t);
        Ok(())
    }

    /// Cancel a ticket by id regardless of who asks (party composition changed, disconnect cleanup).
    pub fn cancel(&self, ticket: &str) -> bool {
        let mut pool = self.pool.lock();
        let Some(idx) = pool.tickets.iter().position(|t| t.id == ticket) else {
            return false;
        };
        let t = pool.tickets.remove(idx);
        unindex(&mut pool, &t);
        true
    }

    /// Drop every ticket the session takes part in (disconnect). Returns the affected ticket ids.
    pub fn remove_session(&self, session: SessionId) -> Vec<String> {
        let mut pool = self.pool.lock();
        let Some(ids) = pool.by_session.remove(&session) else {
            return vec![];
        };
        for id in &ids {
            if let Some(idx) = pool.tickets.iter().position(|t| &t.id == id) {
                let t = pool.tickets.remove(idx);
                unindex(&mut pool, &t);
            }
        }
        ids
    }

    /// Run one matching pass; returns all groups formed. Call periodically and after `add`.
    pub fn process(&self) -> Vec<Matched> {
        self.process_at(Instant::now())
    }

    /// `process` with an explicit clock (the queue simulator runs on virtual time).
    pub fn process_at(&self, now: Instant) -> Vec<Matched> {
        let mut pool = self.pool.lock();
        let mut out = self.process_queues(&mut pool, now);
        let mut i = 0;
        'seed: while i < pool.tickets.len() {
            let seed = pool.tickets[i].clone();
            if seed.queue.is_some() {
                i += 1;
                continue;
            }
            let patient = now.duration_since(seed.created) < self.min_wait;
            let mut group: Vec<usize> = vec![i];
            let mut size = seed.members.len();
            // candidates ordered by how many optional terms they satisfy, then by age
            let mut cands: Vec<(usize, usize)> = pool
                .tickets
                .iter()
                .enumerate()
                .filter(|(j, t)| {
                    *j != i && compatible(&seed, t) && group_compatible(&pool.tickets, &group, t)
                })
                .map(|(j, t)| (j, seed.query.preference(&t.props)))
                .collect();
            cands.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            for (j, _) in cands {
                let t = &pool.tickets[j];
                if size + t.members.len() > seed.max_count
                    || !group_compatible(&pool.tickets, &group, t)
                {
                    continue;
                }
                group.push(j);
                size += t.members.len();
                if size == seed.max_count {
                    break;
                }
            }
            let mut final_size = size;
            if !(size == seed.max_count && accepts_all(&pool.tickets, &group, size)) {
                if patient {
                    i += 1;
                    continue 'seed;
                }
                let mut found = None;
                for n in (seed.min_count..=size).rev() {
                    let mut g = group.clone();
                    if trim(&pool.tickets, &mut g, n) && accepts_all(&pool.tickets, &g, n) {
                        found = Some((g, n));
                        break;
                    }
                }
                let Some((g, n)) = found else {
                    i += 1;
                    continue 'seed;
                };
                group = g;
                final_size = n;
            }
            debug_assert!(final_size >= seed.min_count);
            group.sort_unstable_by(|a, b| b.cmp(a));
            let mut tickets = Vec::with_capacity(group.len());
            for k in &group {
                let t = pool.tickets.remove(*k);
                unindex(&mut pool, &t);
                tickets.push(t);
            }
            tickets.reverse();
            out.push(Matched {
                token: uuid::Uuid::now_v7().simple().to_string(),
                tickets,
                ..Default::default()
            });
            // the pool shifted; restart scanning from the beginning of what remains
            i = 0;
        }
        out
    }
}

/// Drop tickets from the tail of `group` (never the seed) until the member total is exactly `target`.
fn trim(tickets: &[Ticket], group: &mut Vec<usize>, target: usize) -> bool {
    let total = |g: &[usize]| g.iter().map(|&k| tickets[k].members.len()).sum::<usize>();
    while total(group) > target && group.len() > 1 {
        // remove the ticket whose removal keeps us >= target if possible, else the last one
        let excess = total(group) - target;
        if let Some(pos) = group
            .iter()
            .skip(1)
            .position(|&k| tickets[k].members.len() == excess)
        {
            group.remove(pos + 1);
        } else {
            group.pop();
        }
    }
    total(group) == target
}

fn accepts_all(tickets: &[Ticket], group: &[usize], n: usize) -> bool {
    group.iter().all(|&k| {
        let t = &tickets[k];
        n >= t.min_count && n <= t.max_count && n % t.count_multiple == 0
    })
}

fn unindex(pool: &mut Pool, t: &Ticket) {
    for m in &t.members {
        if let Some(v) = pool.by_session.get_mut(&m.presence.session_id) {
            v.retain(|id| id != &t.id);
            if v.is_empty() {
                pool.by_session.remove(&m.presence.session_id);
            }
        }
    }
}

fn compatible(a: &Ticket, b: &Ticket) -> bool {
    a.queue == b.queue
        && a.query.matches(&b.props)
        && b.query.matches(&a.props)
        && a.count_multiple == b.count_multiple
}

impl Matchmaker {
    /// Queue pass: per queue, a scorer proposal or the greedy seed-and-fill under the queue's
    /// rules, then team balancing, grading and (optionally) the scorer's verdict.
    fn process_queues(&self, pool: &mut Pool, now: Instant) -> Vec<Matched> {
        let queues = self.queues.read();
        if queues.is_empty() {
            return vec![];
        }
        let mut recent = self.recent.lock();
        let mut rejections = self.rejections.lock();
        let mut out = Vec::new();
        let mut remove: Vec<usize> = Vec::new();
        for (name, cfg) in queues.iter() {
            let idx: Vec<usize> = pool
                .tickets
                .iter()
                .enumerate()
                .filter(|(_, t)| t.queue.as_deref() == Some(name.as_str()))
                .map(|(i, _)| i)
                .collect();
            let members: usize = idx.iter().map(|&i| pool.tickets[i].members.len()).sum();
            if members < cfg.min_count {
                continue;
            }
            let mut bump = |reason: &'static str| {
                *rejections.entry((name.clone(), reason)).or_insert(0) += 1;
            };
            let mut groups: Vec<Vec<usize>> = Vec::new();
            if let Some(sc) = &self.scorer {
                let wires: Vec<TicketWire> = idx
                    .iter()
                    .map(|&i| TicketWire::from(&pool.tickets[i]))
                    .collect();
                if let Some(proposal) = sc.propose(cfg, &wires) {
                    let by_id: HashMap<&str, usize> = idx
                        .iter()
                        .map(|&i| (pool.tickets[i].id.as_str(), i))
                        .collect();
                    let mut used: HashSet<usize> = HashSet::new();
                    for g in proposal {
                        let mut gi = Vec::new();
                        for id in &g {
                            match by_id.get(id.as_str()) {
                                Some(&i) if !used.contains(&i) => gi.push(i),
                                _ => {
                                    gi.clear();
                                    break;
                                }
                            }
                        }
                        let n: usize = gi.iter().map(|&i| pool.tickets[i].members.len()).sum();
                        if !gi.is_empty() && cfg.accepts(n) {
                            used.extend(gi.iter().copied());
                            groups.push(gi);
                        } else {
                            bump("bad_proposal");
                        }
                    }
                }
            }
            if groups.is_empty() {
                groups = greedy_queue(
                    cfg,
                    &pool.tickets,
                    &idx,
                    now,
                    &recent,
                    self.min_wait,
                    &mut bump,
                );
            }
            for g in groups {
                let mut tickets: Vec<Ticket> = g.iter().map(|&i| pool.tickets[i].clone()).collect();
                let Some(teams) = assign_teams(cfg, &tickets) else {
                    bump("roles");
                    continue;
                };
                let refs: Vec<&Ticket> = tickets.iter().collect();
                let region = cfg.best_region(&refs);
                let mut q = quality(cfg, &tickets, &teams, region.as_ref(), now);
                if let Some(sc) = &self.scorer {
                    let cand = CandidateMatch {
                        queue: name.clone(),
                        tickets: tickets.iter().map(TicketWire::from).collect(),
                        teams: teams
                            .iter()
                            .map(|g| g.iter().map(|&k| tickets[k].id.clone()).collect())
                            .collect(),
                        region: region.as_ref().map(|r| r.0.clone()),
                        quality: q,
                    };
                    if let Some(s) = sc.score(&cand) {
                        if s < 0.0 {
                            bump("vetoed");
                            continue;
                        }
                        q.score = s;
                    }
                }
                for (team, g) in teams.iter().enumerate() {
                    for &k in g {
                        for m in &mut tickets[k].members {
                            m.team = Some(team as u32);
                        }
                    }
                }
                if cfg.no_repeat_secs > 0 {
                    let accounts: Vec<AccountId> = tickets
                        .iter()
                        .flat_map(|t| t.members.iter().map(|m| m.presence.account_id))
                        .collect();
                    recent.record(&accounts, now, Duration::from_secs(cfg.no_repeat_secs));
                }
                remove.extend(g.iter().copied());
                out.push(Matched {
                    token: uuid::Uuid::now_v7().simple().to_string(),
                    tickets,
                    queue: Some(name.clone()),
                    region: region.map(|r| r.0),
                    quality: Some(q),
                });
            }
        }
        remove.sort_unstable_by(|a, b| b.cmp(a));
        for k in remove {
            let t = pool.tickets.remove(k);
            unindex(pool, &t);
        }
        let keep = queues.values().map(|q| q.no_repeat_secs).max().unwrap_or(0);
        recent.prune(now, Duration::from_secs(keep));
        out
    }
}

/// Seed-and-fill under a queue's rules: the longest-waiting (priority first) unused ticket seeds a
/// group, the closest compatible tickets (by MMR distance, then age) join until the match is full;
/// a seed past `min_wait` settles for the largest accepted smaller size.
fn greedy_queue(
    cfg: &QueueConfig,
    tickets: &[Ticket],
    idx: &[usize],
    now: Instant,
    recent: &Recent,
    min_wait: Duration,
    bump: &mut dyn FnMut(&'static str),
) -> Vec<Vec<usize>> {
    let waited = |i: usize| now.saturating_duration_since(tickets[i].created);
    let priority = Duration::from_secs(cfg.priority_after_secs);
    let mut order: Vec<usize> = idx.to_vec();
    order.sort_by(|&a, &b| {
        let pa = cfg.priority_after_secs > 0 && waited(a) >= priority;
        let pb = cfg.priority_after_secs > 0 && waited(b) >= priority;
        pb.cmp(&pa)
            .then(tickets[a].created.cmp(&tickets[b].created))
    });
    let mut used: HashSet<usize> = HashSet::new();
    let mut groups = Vec::new();
    for &s in &order {
        if used.contains(&s) {
            continue;
        }
        let seed = &tickets[s];
        let mut group = vec![s];
        let mut size = seed.members.len();
        let mut cands: Vec<(usize, f64)> = Vec::new();
        for &c in &order {
            if c == s || used.contains(&c) {
                continue;
            }
            let t = &tickets[c];
            if !(seed.query.matches(&t.props) && t.query.matches(&seed.props)) {
                bump("query");
                continue;
            }
            if let Some(reason) = cfg.incompatible(seed, t, now, recent) {
                bump(reason);
                continue;
            }
            let d = match (cfg.mmr_of(seed), cfg.mmr_of(t)) {
                (Some(a), Some(b)) => (a - b).abs(),
                _ => 0.0,
            };
            cands.push((c, d));
        }
        cands.sort_by(|a, b| {
            a.1.total_cmp(&b.1)
                .then(tickets[a.0].created.cmp(&tickets[b.0].created))
        });
        for (c, _) in cands {
            let t = &tickets[c];
            if size + t.members.len() > cfg.max_count {
                continue;
            }
            if group
                .iter()
                .any(|&g| cfg.incompatible(&tickets[g], t, now, recent).is_some())
            {
                continue;
            }
            if cfg.teams.is_none() {
                let roles = group.iter().chain(std::iter::once(&c)).flat_map(|&k| {
                    tickets[k]
                        .members
                        .iter()
                        .map(|m| m.string_properties.get("role").cloned())
                });
                if !cfg.roles_fit(roles) {
                    bump("roles");
                    continue;
                }
            }
            group.push(c);
            size += t.members.len();
            if size == cfg.max_count {
                break;
            }
        }
        if size < cfg.max_count {
            if waited(s) < min_wait {
                bump("waiting_for_full");
                continue;
            }
            while size > 0 && !cfg.accepts(size) && group.len() > 1 {
                let last = group.pop().unwrap();
                size -= tickets[last].members.len();
            }
            if !cfg.accepts(size) {
                bump("too_few");
                continue;
            }
        }
        used.extend(group.iter().copied());
        groups.push(group);
    }
    groups
}

fn group_compatible(tickets: &[Ticket], group: &[usize], t: &Ticket) -> bool {
    group.iter().all(|&k| compatible(&tickets[k], t))
}

fn merged_props(members: &[Candidate]) -> Properties {
    // A party's properties are the first member's, with numeric fields averaged across members.
    let mut p = Properties::default();
    if let Some(first) = members.first() {
        p.strings = first.string_properties.clone();
    }
    let mut sums: HashMap<String, (f64, usize)> = HashMap::new();
    for m in members {
        for (k, v) in &m.numeric_properties {
            let e = sums.entry(k.clone()).or_insert((0.0, 0));
            e.0 += v;
            e.1 += 1;
        }
    }
    p.numbers = sums
        .into_iter()
        .map(|(k, (s, n))| (k, s / n as f64))
        .collect();
    p
}

pub fn account_ids(m: &Matched) -> Vec<AccountId> {
    m.users()
        .into_iter()
        .map(|c| c.presence.account_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use musubi_proto::UserPresence;

    fn cand(region: &str, rank: f64) -> Candidate {
        Candidate {
            presence: UserPresence {
                account_id: AccountId::new(),
                session_id: SessionId::new(),
                username: "u".into(),
                status: None,
            },
            string_properties: [("region".to_string(), region.to_string())].into(),
            numeric_properties: [("rank".to_string(), rank)].into(),
            team: None,
        }
    }

    fn ticket(c: Vec<Candidate>, query: &str, min: usize, max: usize) -> AddTicket {
        AddTicket {
            party_id: None,
            members: c,
            query: query.into(),
            min_count: min,
            max_count: max,
            count_multiple: 1,
        }
    }

    #[test]
    fn backfills_open_match_until_full() {
        let mm =
            Matchmaker::new(Duration::from_millis(0)).with_backfill(Some(Duration::from_secs(60)));
        mm.add(ticket(vec![cand("eu", 10.0)], "+region:eu", 2, 3))
            .unwrap();
        mm.add(ticket(vec![cand("eu", 11.0)], "+region:eu", 2, 3))
            .unwrap();
        let m = mm.process();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].users().len(), 2);
        mm.register_open("match-1", &m[0]);
        assert_eq!(mm.open_count(), 1);
        // an incompatible ticket is left waiting, a compatible one joins the open match
        mm.add(ticket(vec![cand("us", 10.0)], "+region:us", 2, 3))
            .unwrap();
        mm.add(ticket(vec![cand("eu", 12.0)], "+region:eu", 2, 3))
            .unwrap();
        let b = mm.process_backfill();
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].1, "match-1");
        assert_eq!(b[0].0.token, m[0].token);
        assert_eq!(b[0].0.users().len(), 1);
        assert_eq!(mm.ticket_count(), 1, "the us ticket stays");
        // the match is full now: another eu ticket waits for a new group
        mm.add(ticket(vec![cand("eu", 13.0)], "+region:eu", 2, 3))
            .unwrap();
        assert!(mm.process_backfill().is_empty());
        assert_eq!(mm.open_count(), 0, "a full match takes nobody");
        // a member leaves the running match: the slot is offered again and the waiting eu ticket
        // takes it; the game server closing the match ends backfill for good
        assert!(mm.release_slots("match-1", Some(1)));
        assert_eq!(mm.open_count(), 1);
        let b = mm.process_backfill();
        assert_eq!(b.len(), 1, "{b:?}");
        assert_eq!(b[0].1, "match-1");
        assert!(mm.release_slots("match-1", None));
        assert!(
            !mm.release_slots("match-1", Some(1)),
            "closed matches are forgotten"
        );
        mm.add(ticket(vec![cand("eu", 14.0)], "+region:eu", 2, 3))
            .unwrap();
        assert!(mm.process_backfill().is_empty());

        let off = Matchmaker::new(Duration::from_millis(0));
        off.register_open("x", &m[0]);
        assert_eq!(off.open_count(), 0);
    }

    #[test]
    fn fills_max_count_immediately() {
        let mm = Matchmaker::new(Duration::from_secs(60));
        for _ in 0..3 {
            mm.add(ticket(vec![cand("eu", 10.0)], "+region:eu", 2, 3))
                .unwrap();
        }
        let m = mm.process();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].users().len(), 3);
        assert_eq!(mm.ticket_count(), 0);
    }

    #[test]
    fn waits_for_max_then_settles_for_min() {
        let mm = Matchmaker::new(Duration::from_millis(0));
        mm.add(ticket(vec![cand("eu", 10.0)], "+region:eu", 2, 4))
            .unwrap();
        mm.add(ticket(vec![cand("eu", 12.0)], "+region:eu", 2, 4))
            .unwrap();
        let m = mm.process();
        assert_eq!(m.len(), 1, "min_wait elapsed → 2 is enough");
        assert_eq!(m[0].users().len(), 2);

        let patient = Matchmaker::new(Duration::from_secs(60));
        patient
            .add(ticket(vec![cand("eu", 10.0)], "+region:eu", 2, 4))
            .unwrap();
        patient
            .add(ticket(vec![cand("eu", 12.0)], "+region:eu", 2, 4))
            .unwrap();
        assert!(patient.process().is_empty(), "still hoping for 4");
        assert_eq!(patient.ticket_count(), 2);
    }

    #[test]
    fn mutual_queries_and_regions() {
        let mm = Matchmaker::new(Duration::from_millis(0));
        mm.add(ticket(vec![cand("eu", 10.0)], "+region:eu +rank:>=8", 2, 2))
            .unwrap();
        mm.add(ticket(vec![cand("eu", 5.0)], "+region:eu", 2, 2))
            .unwrap(); // rank too low for the first
        mm.add(ticket(vec![cand("us", 10.0)], "+region:us", 2, 2))
            .unwrap();
        assert!(mm.process().is_empty());
        mm.add(ticket(vec![cand("eu", 9.0)], "+region:eu +rank:>=8", 2, 2))
            .unwrap();
        let m = mm.process();
        assert_eq!(m.len(), 1);
        assert_eq!(mm.ticket_count(), 2);
    }

    #[test]
    fn parties_count_as_members_and_remove_works() {
        let mm = Matchmaker::new(Duration::from_secs(60));
        let party = vec![cand("eu", 10.0), cand("eu", 20.0)];
        let sid = party[0].presence.session_id;
        let id = mm
            .add(AddTicket {
                party_id: Some("p".into()),
                members: party,
                query: "*".into(),
                min_count: 2,
                max_count: 4,
                count_multiple: 2,
            })
            .unwrap();
        assert!(mm.remove(SessionId::new(), &id).is_err());
        let solo = ticket(vec![cand("eu", 1.0)], "*", 2, 4);
        let solo = AddTicket {
            count_multiple: 2,
            ..solo
        };
        mm.add(solo).unwrap();
        let a2 = AddTicket {
            count_multiple: 2,
            ..ticket(vec![cand("eu", 1.0)], "*", 2, 4)
        };
        mm.add(a2).unwrap();
        let m = mm.process();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].users().len(), 4);
        assert_eq!(m[0].tickets.len(), 3);
        assert_eq!(mm.ticket_count(), 0);
        assert!(mm.remove(sid, &id).is_err(), "already consumed");
        let id2 = mm.add(ticket(vec![cand("eu", 1.0)], "*", 2, 2)).unwrap();
        let s2 = {
            let p = mm.pool.lock();
            p.tickets[0].members[0].presence.session_id
        };
        assert_eq!(mm.remove_session(s2), vec![id2]);
        assert_eq!(mm.ticket_count(), 0);
    }
}
