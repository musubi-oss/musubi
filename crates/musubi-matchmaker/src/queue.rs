//! Named queues: a queue is a server-side configuration (match size, teams, MMR and ping rules
//! that widen with waiting time, role quotas, cross-play groups, no-repeat) that a ticket joins by
//! carrying the string property `queue`. Tickets without it keep the free-form mutual-query model.
//!
//! Property conventions for queue tickets: numeric `mmr` (or the queue's `mmr.property`), string
//! `region` plus numeric `ping_<region>` for every region the client measured, string `role`,
//! string `platform`, string `avoid` (comma-separated account ids the player refuses to play with).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};

use crate::{Ticket, TicketWire};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct QueueConfig {
    pub name: String,
    pub min_count: usize,
    pub max_count: usize,
    /// Fixed teams; the match then needs `count * size` players (or an equal split of a smaller
    /// accepted size) and every party stays on one team.
    pub teams: Option<Teams>,
    pub mmr: Option<MmrRule>,
    pub ping: Option<PingRule>,
    /// Per-team (or per-match without teams) maximum per `role`.
    pub roles: BTreeMap<String, usize>,
    /// Do not put two players who met in a match this recently into one match again (0 = off).
    pub no_repeat_secs: u64,
    /// Platforms that may share a match; a platform in no group only plays with itself. Empty =
    /// everyone plays with everyone.
    pub crossplay: Vec<Vec<String>>,
    /// Tickets that waited this long are seeded first regardless of arrival order (0 = off).
    pub priority_after_secs: u64,
    /// Members must confirm within this many seconds after the group forms (None = the
    /// matchmaker's default, 0 = no ready-check).
    pub ready_check_secs: Option<u64>,
    /// Base keep-out for a member who did not confirm (None = the matchmaker's default).
    pub dodge_penalty_secs: Option<u64>,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            min_count: 2,
            max_count: 2,
            teams: None,
            mmr: None,
            ping: None,
            roles: BTreeMap::new(),
            no_repeat_secs: 0,
            crossplay: Vec::new(),
            priority_after_secs: 0,
            ready_check_secs: None,
            dodge_penalty_secs: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Teams {
    pub count: usize,
    pub size: usize,
}

/// Allowed MMR distance between any two tickets: `initial_delta`, plus `step` every `every_secs`
/// of the more patient ticket's wait, capped at `max_delta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MmrRule {
    pub property: String,
    pub initial_delta: f64,
    pub step: f64,
    pub every_secs: u64,
    pub max_delta: f64,
}

impl Default for MmrRule {
    fn default() -> Self {
        Self {
            property: "mmr".into(),
            initial_delta: 50.0,
            step: 25.0,
            every_secs: 10,
            max_delta: 400.0,
        }
    }
}

/// Maximum acceptable ping (ms) to the match region, widening with wait like the MMR rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PingRule {
    pub prefix: String,
    pub initial_ms: f64,
    pub step_ms: f64,
    pub every_secs: u64,
    pub max_ms: f64,
}

impl Default for PingRule {
    fn default() -> Self {
        Self {
            prefix: "ping_".into(),
            initial_ms: 40.0,
            step_ms: 40.0,
            every_secs: 10,
            max_ms: 120.0,
        }
    }
}

fn widen(initial: f64, step: f64, every_secs: u64, max: f64, waited: Duration) -> f64 {
    if every_secs == 0 || step <= 0.0 {
        return initial.min(max);
    }
    let steps = waited.as_secs() / every_secs;
    (initial + step * steps as f64).min(max)
}

impl QueueConfig {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::InvalidArgument("queue needs a name".into()));
        }
        if self.min_count < 2 || self.max_count < self.min_count {
            return Err(Error::InvalidArgument(format!(
                "queue {}: need 2 <= min_count <= max_count",
                self.name
            )));
        }
        if let Some(t) = self.teams {
            if t.count < 2 || t.size == 0 {
                return Err(Error::InvalidArgument(format!(
                    "queue {}: teams need count >= 2 and size >= 1",
                    self.name
                )));
            }
            if t.count * t.size != self.max_count {
                return Err(Error::InvalidArgument(format!(
                    "queue {}: teams.count * teams.size must equal max_count",
                    self.name
                )));
            }
            if self.min_count % t.count != 0 {
                return Err(Error::InvalidArgument(format!(
                    "queue {}: min_count must split evenly into {} teams",
                    self.name, t.count
                )));
            }
        }
        Ok(())
    }

    /// Match sizes this queue accepts: every size in `min..=max` that splits into equal teams.
    pub fn accepts(&self, n: usize) -> bool {
        n >= self.min_count && n <= self.max_count && self.teams.is_none_or(|t| n % t.count == 0)
    }

    pub fn mmr_delta(&self, waited: Duration) -> Option<f64> {
        self.mmr
            .as_ref()
            .map(|r| widen(r.initial_delta, r.step, r.every_secs, r.max_delta, waited))
    }

    pub fn ping_limit(&self, waited: Duration) -> Option<f64> {
        self.ping
            .as_ref()
            .map(|r| widen(r.initial_ms, r.step_ms, r.every_secs, r.max_ms, waited))
    }

    pub fn mmr_of(&self, t: &Ticket) -> Option<f64> {
        let key = self
            .mmr
            .as_ref()
            .map(|r| r.property.as_str())
            .unwrap_or("mmr");
        t.props.numbers.get(key).copied()
    }

    /// Regions the ticket measured, with its ping to each.
    pub fn pings_of(&self, t: &Ticket) -> HashMap<String, f64> {
        let prefix = self
            .ping
            .as_ref()
            .map(|r| r.prefix.as_str())
            .unwrap_or("ping_");
        let mut out: HashMap<String, f64> = t
            .props
            .numbers
            .iter()
            .filter_map(|(k, v)| k.strip_prefix(prefix).map(|r| (r.to_string(), *v)))
            .collect();
        if out.is_empty() {
            if let Some(r) = t.props.strings.get("region") {
                out.insert(r.clone(), 0.0);
            }
        }
        out
    }

    fn crossplay_ok(&self, a: &Ticket, b: &Ticket) -> bool {
        if self.crossplay.is_empty() {
            return true;
        }
        let (Some(pa), Some(pb)) = (
            a.props.strings.get("platform"),
            b.props.strings.get("platform"),
        ) else {
            return true;
        };
        if pa == pb {
            return true;
        }
        self.crossplay
            .iter()
            .any(|g| g.contains(pa) && g.contains(pb))
    }

    /// Why `a` and `b` cannot share a match right now (`None` = they can). Wait-based rules widen
    /// with the more patient of the two.
    pub fn incompatible(
        &self,
        a: &Ticket,
        b: &Ticket,
        now: Instant,
        recent: &Recent,
    ) -> Option<&'static str> {
        let waited = now
            .saturating_duration_since(a.created)
            .max(now.saturating_duration_since(b.created));
        if let Some(delta) = self.mmr_delta(waited) {
            if let (Some(x), Some(y)) = (self.mmr_of(a), self.mmr_of(b)) {
                if (x - y).abs() > delta {
                    return Some("mmr");
                }
            }
        }
        if let Some(limit) = self.ping_limit(waited) {
            let pa = self.pings_of(a);
            let pb = self.pings_of(b);
            if !pa.is_empty()
                && !pb.is_empty()
                && !pa
                    .iter()
                    .any(|(r, x)| *x <= limit && pb.get(r).is_some_and(|y| *y <= limit))
            {
                return Some("ping");
            }
        }
        if !self.crossplay_ok(a, b) {
            return Some("crossplay");
        }
        if avoids(a, b) || avoids(b, a) {
            return Some("avoid");
        }
        if self.no_repeat_secs > 0 {
            let horizon = Duration::from_secs(self.no_repeat_secs);
            if recent.met(a, b, now, horizon) {
                return Some("recent");
            }
        }
        None
    }

    /// Role quota check for one team (or the whole match without teams).
    pub fn roles_fit(&self, members: impl Iterator<Item = Option<String>>) -> bool {
        if self.roles.is_empty() {
            return true;
        }
        let mut seen: HashMap<String, usize> = HashMap::new();
        for r in members.flatten() {
            *seen.entry(r).or_default() += 1;
        }
        seen.iter()
            .all(|(r, n)| self.roles.get(r).is_none_or(|q| n <= q))
    }

    /// Region that minimises the worst ping across the group (`None` without ping data).
    pub fn best_region(&self, tickets: &[&Ticket]) -> Option<(String, f64)> {
        let mut worst: HashMap<String, f64> = HashMap::new();
        let mut counts: HashMap<String, usize> = HashMap::new();
        for t in tickets {
            for (r, p) in self.pings_of(t) {
                let w = worst.entry(r.clone()).or_insert(0.0);
                if p > *w {
                    *w = p;
                }
                *counts.entry(r).or_default() += 1;
            }
        }
        worst
            .into_iter()
            .filter(|(r, _)| counts[r] == tickets.len())
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }
}

fn avoids(a: &Ticket, b: &Ticket) -> bool {
    let Some(list) = a.props.strings.get("avoid") else {
        return false;
    };
    let ids: HashSet<&str> = list.split(',').map(str::trim).collect();
    b.members
        .iter()
        .any(|m| ids.contains(m.presence.account_id.0.to_string().as_str()))
}

fn role_of(t: &Ticket, member: usize) -> Option<String> {
    t.members[member]
        .string_properties
        .get("role")
        .cloned()
        .or_else(|| t.props.strings.get("role").cloned())
}

/// Who met whom recently (for `no_repeat_secs`); the leader records every formed match.
#[derive(Default)]
pub struct Recent {
    met: HashMap<AccountId, Vec<(AccountId, Instant)>>,
}

impl Recent {
    pub fn record(&mut self, accounts: &[AccountId], now: Instant, keep: Duration) {
        for a in accounts {
            let v = self.met.entry(*a).or_default();
            v.retain(|(_, t)| now.saturating_duration_since(*t) < keep);
            for b in accounts {
                if a != b {
                    v.push((*b, now));
                }
            }
        }
    }

    fn met(&self, a: &Ticket, b: &Ticket, now: Instant, horizon: Duration) -> bool {
        a.members.iter().any(|x| {
            self.met.get(&x.presence.account_id).is_some_and(|v| {
                v.iter().any(|(id, t)| {
                    now.saturating_duration_since(*t) < horizon
                        && b.members.iter().any(|y| y.presence.account_id == *id)
                })
            })
        })
    }

    pub fn prune(&mut self, now: Instant, keep: Duration) {
        self.met.retain(|_, v| {
            v.retain(|(_, t)| now.saturating_duration_since(*t) < keep);
            !v.is_empty()
        });
    }
}

/// Split tickets into balanced teams: parties stay together, team MMR totals are levelled by a
/// greedy fill (largest parties first, each to the lightest team with room) followed by swaps of
/// equal-sized tickets while they lower the MMR gap and keep role quotas. Returns ticket indices
/// per team.
pub fn assign_teams(cfg: &QueueConfig, tickets: &[Ticket]) -> Option<Vec<Vec<usize>>> {
    let total: usize = tickets.iter().map(|t| t.members.len()).sum();
    let Some(teams) = cfg.teams else {
        let roles = tickets
            .iter()
            .flat_map(|t| (0..t.members.len()).map(move |i| role_of(t, i)));
        return cfg.roles_fit(roles).then(Vec::new);
    };
    if total % teams.count != 0 {
        return None;
    }
    let size = total / teams.count;
    let mmr = |i: usize| cfg.mmr_of(&tickets[i]).unwrap_or(0.0) * tickets[i].members.len() as f64;
    // quota-limited roles are placed first so the role-blind MMR fill cannot paint itself into a
    // corner (two healers left for one team)
    let constrained = |i: usize| {
        (0..tickets[i].members.len())
            .filter(|&m| role_of(&tickets[i], m).is_some_and(|r| cfg.roles.contains_key(&r)))
            .count()
    };
    let mut order: Vec<usize> = (0..tickets.len()).collect();
    order.sort_by(|&a, &b| {
        constrained(b)
            .cmp(&constrained(a))
            .then(tickets[b].members.len().cmp(&tickets[a].members.len()))
            .then(mmr(b).total_cmp(&mmr(a)))
    });
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); teams.count];
    let mut fill = vec![0usize; teams.count];
    let mut sum = vec![0f64; teams.count];
    let fits = |g: &[usize], extra: usize| {
        let roles = g
            .iter()
            .chain(std::iter::once(&extra))
            .flat_map(|&k| (0..tickets[k].members.len()).map(move |i| role_of(&tickets[k], i)));
        cfg.roles_fit(roles)
    };
    for i in order {
        let n = tickets[i].members.len();
        let mut best: Option<usize> = None;
        for t in 0..teams.count {
            if fill[t] + n > size || !fits(&groups[t], i) {
                continue;
            }
            if best.is_none_or(|b| sum[t] < sum[b]) {
                best = Some(t);
            }
        }
        let t = best?;
        groups[t].push(i);
        fill[t] += n;
        sum[t] += mmr(i);
    }
    // local search: swap equal-sized tickets between the heaviest and lightest teams
    let gap = |sum: &[f64]| {
        sum.iter().cloned().fold(f64::MIN, f64::max) - sum.iter().cloned().fold(f64::MAX, f64::min)
    };
    for _ in 0..32 {
        let cur = gap(&sum);
        let mut improved = false;
        'outer: for a in 0..teams.count {
            for b in 0..teams.count {
                if a == b {
                    continue;
                }
                for x in 0..groups[a].len() {
                    for y in 0..groups[b].len() {
                        let (i, j) = (groups[a][x], groups[b][y]);
                        if tickets[i].members.len() != tickets[j].members.len() {
                            continue;
                        }
                        let d = mmr(i) - mmr(j);
                        let mut s = sum.clone();
                        s[a] -= d;
                        s[b] += d;
                        if gap(&s) + 1e-9 < cur {
                            let mut ga = groups[a].clone();
                            let mut gb = groups[b].clone();
                            ga[x] = j;
                            gb[y] = i;
                            let ok = cfg.roles_fit(ga.iter().flat_map(|&k| {
                                (0..tickets[k].members.len()).map(move |m| role_of(&tickets[k], m))
                            })) && cfg.roles_fit(gb.iter().flat_map(|&k| {
                                (0..tickets[k].members.len()).map(move |m| role_of(&tickets[k], m))
                            }));
                            if ok {
                                groups[a] = ga;
                                groups[b] = gb;
                                sum = s;
                                improved = true;
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }
        if !improved {
            break;
        }
    }
    Some(groups)
}

/// How good a formed match is; `score` is 0..1 (1 = perfect), the parts are for operators.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Quality {
    pub score: f32,
    pub mmr_spread: f64,
    pub team_mmr_gap: f64,
    pub ping_ms: f64,
    pub longest_wait_secs: f64,
}

pub fn quality(
    cfg: &QueueConfig,
    tickets: &[Ticket],
    teams: &[Vec<usize>],
    region: Option<&(String, f64)>,
    now: Instant,
) -> Quality {
    let mmrs: Vec<f64> = tickets.iter().filter_map(|t| cfg.mmr_of(t)).collect();
    let spread = match (
        mmrs.iter().cloned().reduce(f64::max),
        mmrs.iter().cloned().reduce(f64::min),
    ) {
        (Some(a), Some(b)) => a - b,
        _ => 0.0,
    };
    let avg = |g: &[usize]| {
        let (s, n) = g.iter().fold((0.0, 0usize), |(s, n), &k| {
            let t = &tickets[k];
            (
                s + cfg.mmr_of(t).unwrap_or(0.0) * t.members.len() as f64,
                n + t.members.len(),
            )
        });
        if n == 0 {
            0.0
        } else {
            s / n as f64
        }
    };
    let team_gap = if teams.is_empty() {
        0.0
    } else {
        let a: Vec<f64> = teams.iter().map(|g| avg(g)).collect();
        a.iter().cloned().fold(f64::MIN, f64::max) - a.iter().cloned().fold(f64::MAX, f64::min)
    };
    let ping = region.map(|r| r.1).unwrap_or(0.0);
    let wait = tickets
        .iter()
        .map(|t| now.saturating_duration_since(t.created).as_secs_f64())
        .fold(0.0, f64::max);
    let max_mmr = cfg
        .mmr
        .as_ref()
        .map(|r| r.max_delta)
        .unwrap_or(1.0)
        .max(1.0);
    let max_ping = cfg.ping.as_ref().map(|r| r.max_ms).unwrap_or(1.0).max(1.0);
    let score = 1.0
        - 0.4 * (spread / max_mmr).clamp(0.0, 1.0)
        - 0.3 * (team_gap / max_mmr).clamp(0.0, 1.0)
        - 0.3 * (ping / max_ping).clamp(0.0, 1.0);
    Quality {
        score: score as f32,
        mmr_spread: spread,
        team_mmr_gap: team_gap,
        ping_ms: ping,
        longest_wait_secs: wait,
    }
}

/// A match the queue algorithm is about to form, handed to a [`Scorer`] for a verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateMatch {
    pub queue: String,
    pub tickets: Vec<TicketWire>,
    /// Ticket ids per team (empty without teams).
    pub teams: Vec<Vec<String>>,
    pub region: Option<String>,
    pub quality: Quality,
}

/// Game-specific override of the queue algorithm (WASM `matchmaker_score` / `matchmaker_propose`
/// in the server). `score` may re-grade or veto (negative) a candidate; `propose` may form the
/// groups itself from the whole waiting pool of a queue (ticket ids per group, teams nested as
/// groups of groups are not needed: return flat groups and the built-in balancer splits them).
pub trait Scorer: Send + Sync {
    fn score(&self, _candidate: &CandidateMatch) -> Option<f32> {
        None
    }
    fn propose(&self, _queue: &QueueConfig, _tickets: &[TicketWire]) -> Option<Vec<Vec<String>>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_widen_with_wait_and_cap() {
        let q = QueueConfig {
            mmr: Some(MmrRule::default()),
            ping: Some(PingRule::default()),
            ..Default::default()
        };
        assert_eq!(q.mmr_delta(Duration::ZERO), Some(50.0));
        assert_eq!(q.mmr_delta(Duration::from_secs(25)), Some(100.0));
        assert_eq!(q.mmr_delta(Duration::from_secs(1000)), Some(400.0));
        assert_eq!(q.ping_limit(Duration::from_secs(10)), Some(80.0));
        assert_eq!(q.ping_limit(Duration::from_secs(60)), Some(120.0));
    }

    #[test]
    fn validate_teams() {
        let mut q = QueueConfig {
            name: "5v5".into(),
            min_count: 6,
            max_count: 10,
            teams: Some(Teams { count: 2, size: 5 }),
            ..Default::default()
        };
        q.validate().unwrap();
        assert!(q.accepts(6) && q.accepts(8) && !q.accepts(7) && !q.accepts(12));
        q.min_count = 5;
        assert!(q.validate().is_err());
    }
}
