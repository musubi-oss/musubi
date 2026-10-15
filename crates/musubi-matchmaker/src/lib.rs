//! Matchmaker: a process-local ticket pool with mutual-query matching. Tickets carry one or more
//! presences (a party submits a single ticket). Matching is greedy FIFO: the oldest ticket seeds a
//! group, compatible tickets join until the seed's `max_count` is filled (or at least `min_count`
//! once the seed has waited `min_wait`), honouring `count_multiple`.

pub mod query;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use musubi_core::{AccountId, Error, Result, SessionId};
use parking_lot::Mutex;

pub use query::{Properties, Query};

pub const MAX_TICKETS_PER_SESSION: usize = 3;
pub const MAX_COUNT: usize = 100;

pub type Candidate = musubi_proto::realtime::MatchmakerUser;

#[derive(Debug, Clone)]
pub struct Ticket {
    pub id: String,
    pub party_id: Option<String>,
    pub members: Vec<Candidate>,
    pub query: Query,
    pub query_src: String,
    pub props: Properties,
    pub min_count: usize,
    pub max_count: usize,
    pub count_multiple: usize,
    pub created: Instant,
}

#[derive(Debug, Clone)]
pub struct Matched {
    pub token: String,
    pub tickets: Vec<Ticket>,
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
    pub tickets: Vec<TicketWire>,
}

impl Matched {
    pub fn to_wire(&self, match_id: &str) -> MatchedWire {
        MatchedWire {
            token: self.token.clone(),
            match_id: match_id.to_string(),
            tickets: self.tickets.iter().map(TicketWire::from).collect(),
        }
    }

    pub fn from_wire(w: MatchedWire) -> Result<Self> {
        Ok(Self {
            token: w.token,
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

#[derive(Default)]
struct Pool {
    tickets: Vec<Ticket>,
    by_session: HashMap<SessionId, Vec<String>>,
}

pub struct Matchmaker {
    pool: Mutex<Pool>,
    /// How long a seed waits for `max_count` before settling for `min_count`.
    min_wait: Duration,
}

impl Default for Matchmaker {
    fn default() -> Self {
        Self::new(Duration::from_secs(15))
    }
}

impl Matchmaker {
    pub fn new(min_wait: Duration) -> Self {
        Self {
            pool: Mutex::new(Pool::default()),
            min_wait,
        }
    }

    pub fn ticket_count(&self) -> usize {
        self.pool.lock().tickets.len()
    }

    pub fn add(&self, t: AddTicket) -> Result<String> {
        if t.members.is_empty() {
            return Err(Error::InvalidArgument(
                "ticket needs at least one member".into(),
            ));
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
            members: t.members,
            query,
            query_src: t.query,
            props,
            min_count: t.min_count,
            max_count: t.max_count,
            count_multiple: t.count_multiple,
            created: Instant::now(),
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
        let now = Instant::now();
        let mut pool = self.pool.lock();
        let mut out = Vec::new();
        let mut i = 0;
        'seed: while i < pool.tickets.len() {
            let seed = pool.tickets[i].clone();
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
    a.query.matches(&b.props) && b.query.matches(&a.props) && a.count_multiple == b.count_multiple
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
