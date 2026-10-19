//! Glue between the matchmaker pool and live sessions.

use std::sync::Arc;
use std::time::Duration;

use musubi_cluster::Cluster;
use musubi_matchmaker::{Matched, Matchmaker, ReadyStart};

use crate::fanout::Fanout;
use musubi_proto::realtime::MatchmakerServer;
use musubi_proto::ServerMessage;

use super::{Hub, PartyRegistry, RelayedRegistry};

/// Deliver a formed group to every member session.
pub fn deliver(
    hub: &Hub,
    parties: &PartyRegistry,
    relayed: &RelayedRegistry,
    m: &Matched,
    match_id: &str,
    server: Option<&MatchmakerServer>,
) {
    relayed.register_token(&m.token, match_id);
    let users = m.users();
    for t in &m.tickets {
        if let Some(pid) = &t.party_id {
            parties.ticket_consumed(pid, &t.id);
        }
        for member in &t.members {
            hub.send_to_session(
                member.presence.session_id,
                ServerMessage::MatchmakerMatched {
                    ticket: t.id.clone(),
                    token: m.token.clone(),
                    users: users.clone(),
                    me: member.clone(),
                    server: server.cloned(),
                },
            );
        }
    }
}

/// Every node: ask its local members of a formed group to confirm.
pub fn deliver_ready_check(hub: &Hub, w: &crate::fanout::ReadyCheckWire) {
    let users: Vec<musubi_matchmaker::Candidate> =
        w.tickets.iter().flat_map(|t| t.members.clone()).collect();
    for t in &w.tickets {
        for member in &t.members {
            hub.send_to_session(
                member.presence.session_id,
                ServerMessage::MatchmakerReadyCheck {
                    ticket: t.id.clone(),
                    token: w.token.clone(),
                    deadline_ms: w.deadline_ms,
                    users: users.clone(),
                },
            );
        }
    }
}

/// Every node: tell its local members how a ready-check ended; dodged party tickets are cleared
/// in the party, requeued ones stay.
pub fn deliver_ready_failed(
    hub: &Hub,
    parties: &PartyRegistry,
    w: &crate::fanout::ReadyExpiredWire,
) {
    for (tickets, requeued) in [(&w.requeued, true), (&w.dodged, false)] {
        for t in tickets {
            if !requeued {
                if let Some(pid) = &t.party_id {
                    parties.ticket_consumed(pid, &t.id);
                }
            }
            for member in &t.members {
                hub.send_to_session(
                    member.presence.session_id,
                    ServerMessage::MatchmakerReadyFailed {
                        ticket: t.id.clone(),
                        token: w.token.clone(),
                        requeued,
                        penalty_secs: if requeued { 0 } else { w.penalty_secs },
                    },
                );
            }
        }
    }
}

/// Tell the local member sessions of expired tickets; party tickets are also cleared in the party.
pub fn deliver_expired(
    hub: &Hub,
    parties: &PartyRegistry,
    expired: Vec<musubi_matchmaker::Ticket>,
) {
    for t in expired {
        if let Some(pid) = &t.party_id {
            parties.ticket_consumed(pid, &t.id);
        }
        for member in &t.members {
            hub.send_to_session(
                member.presence.session_id,
                ServerMessage::MatchmakerExpired {
                    ticket: t.id.clone(),
                },
            );
        }
    }
}

/// Leader-only: form groups, deliver locally and announce them to the other nodes.
pub fn process_and_deliver(relayed: &RelayedRegistry, mm: &Matchmaker, fanout: &Fanout) {
    if !fanout.is_leader() {
        return;
    }
    for m in mm.process() {
        dispatch(relayed, mm, fanout, m);
    }
    for e in mm.expire_ready() {
        let queue = e
            .requeued
            .iter()
            .chain(&e.dodged)
            .find_map(|t| t.queue.clone())
            .unwrap_or_else(|| "free".into());
        metrics::counter!("musubi_matchmaker_dodges_total", "queue" => queue)
            .increment(e.dodged.len() as u64);
        fanout.mm_ready_expired(&e);
    }
    for (queue, reason, n) in mm.take_rejections() {
        metrics::counter!("musubi_matchmaker_rejections_total", "queue" => queue, "reason" => reason)
            .increment(n);
    }
    for (queue, n) in mm.waiting() {
        metrics::gauge!("musubi_matchmaker_waiting_tickets", "queue" => queue).set(n as f64);
    }
    for (m, match_id) in mm.process_backfill() {
        fanout.deliver_matched(m, match_id);
    }
}

/// Leader-only: hand one formed group to the ready-check / delivery pipeline.
pub fn dispatch(relayed: &RelayedRegistry, mm: &Matchmaker, fanout: &Fanout, m: Matched) {
    observe(mm, &m);
    let match_id = relayed.new_match_id();
    match mm.start_ready_check(m, &match_id) {
        ReadyStart::Deliver(m) => {
            mm.register_open(&match_id, &m);
            fanout.deliver_matched(m, match_id);
        }
        ReadyStart::Check(p) => fanout.mm_ready_check(&p),
    }
}

/// Health of one queue's external match function (`GET /v1/server/matchmaker/mmf`).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct MmfStatus {
    pub queue: String,
    pub url: String,
    pub calls: u64,
    pub failures: u64,
    pub consecutive_failures: u64,
    pub last_call_ms_ago: Option<u64>,
    pub last_ok_ms_ago: Option<u64>,
    pub last_error: Option<String>,
    pub last_latency_ms: u64,
    pub last_proposals: usize,
    pub last_applied: usize,
    pub last_rejected: Vec<musubi_matchmaker::RejectedProposal>,
    pub fallback_active: bool,
}

#[derive(Default)]
struct MmfState {
    status: MmfStatus,
    last_call: Option<std::time::Instant>,
    last_ok: Option<std::time::Instant>,
}

/// Open Match-style external match functions: the leader POSTs each `mmf: http` queue's waiting
/// pool to the configured URL every `every_ms` and applies the proposals it gets back.
pub struct MmfClient {
    http: reqwest::Client,
    state: parking_lot::Mutex<std::collections::HashMap<String, MmfState>>,
    pending: parking_lot::Mutex<Vec<Matched>>,
}

impl Default for MmfClient {
    fn default() -> Self {
        Self::new()
    }
}

/// What one call to an external match function produced (also the dry-run answer).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MmfOutcome {
    pub tickets: usize,
    pub latency_ms: u64,
    pub proposals: Vec<musubi_matchmaker::Proposal>,
    pub applied: usize,
    pub rejected: Vec<musubi_matchmaker::RejectedProposal>,
}

impl MmfClient {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder().build().unwrap_or_default(),
            state: Default::default(),
            pending: Default::default(),
        }
    }

    /// `POST url {queue, extensions, tickets}` → proposals (`{proposals: [...]}` or `{groups: [[id…]]}`).
    pub async fn call(
        &self,
        cfg: &musubi_matchmaker::QueueConfig,
        tickets: &[musubi_matchmaker::TicketWire],
    ) -> std::result::Result<Vec<musubi_matchmaker::Proposal>, String> {
        let m = cfg.mmf.as_ref().ok_or("queue has no mmf")?;
        let mut req = self
            .http
            .post(&m.url)
            .timeout(Duration::from_millis(m.timeout_ms))
            .json(&serde_json::json!({
                "queue": cfg,
                "extensions": cfg.extensions,
                "tickets": tickets,
            }));
        for (k, v) in &m.headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let res = req
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = res.status();
        let body = res.bytes().await.map_err(|e| format!("read failed: {e}"))?;
        if !status.is_success() {
            return Err(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&body)
                    .chars()
                    .take(200)
                    .collect::<String>()
            ));
        }
        let v: serde_json::Value =
            serde_json::from_slice(&body).map_err(|e| format!("non-JSON answer: {e}"))?;
        if let Some(p) = v.get("proposals") {
            return serde_json::from_value(p.clone()).map_err(|e| format!("bad proposals: {e}"));
        }
        let groups = v.get("groups").cloned().unwrap_or(v);
        let groups: Vec<Vec<String>> =
            serde_json::from_value(groups).map_err(|e| format!("bad groups: {e}"))?;
        Ok(groups
            .into_iter()
            .map(|tickets| musubi_matchmaker::Proposal {
                tickets,
                ..Default::default()
            })
            .collect())
    }

    /// One call for one queue; `dry_run` reports what would happen without touching the pool.
    pub async fn run_queue(
        &self,
        mm: &Matchmaker,
        cfg: &musubi_matchmaker::QueueConfig,
        dry_run: bool,
    ) -> std::result::Result<MmfOutcome, String> {
        let tickets = mm.pool_snapshot(&cfg.name);
        let started = std::time::Instant::now();
        let url = cfg.mmf.as_ref().map(|m| m.url.clone()).unwrap_or_default();
        {
            let mut st = self.state.lock();
            let e = st.entry(cfg.name.clone()).or_default();
            e.status.queue = cfg.name.clone();
            e.status.url = url;
            e.status.calls += 1;
            e.last_call = Some(started);
        }
        let result = self.call(cfg, &tickets).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        let mut st = self.state.lock();
        let e = st.entry(cfg.name.clone()).or_default();
        e.status.last_latency_ms = latency_ms;
        match result {
            Err(err) => {
                e.status.failures += 1;
                e.status.consecutive_failures += 1;
                e.status.last_error = Some(err.clone());
                if !dry_run {
                    mm.mmf_failed(&cfg.name, started);
                }
                metrics::counter!("musubi_matchmaker_mmf_calls_total", "queue" => cfg.name.clone(), "outcome" => "error").increment(1);
                Err(err)
            }
            Ok(proposals) => {
                e.status.consecutive_failures = 0;
                e.status.last_error = None;
                e.last_ok = Some(std::time::Instant::now());
                e.status.last_proposals = proposals.len();
                metrics::counter!("musubi_matchmaker_mmf_calls_total", "queue" => cfg.name.clone(), "outcome" => "ok").increment(1);
                metrics::histogram!("musubi_matchmaker_mmf_seconds", "queue" => cfg.name.clone())
                    .record(latency_ms as f64 / 1000.0);
                if dry_run {
                    return Ok(MmfOutcome {
                        tickets: tickets.len(),
                        latency_ms,
                        proposals,
                        applied: 0,
                        rejected: Vec::new(),
                    });
                }
                drop(st);
                let (matched, rejected) = mm
                    .apply_proposals(&cfg.name, &proposals)
                    .map_err(|e| e.to_string())?;
                let mut st = self.state.lock();
                let e = st.entry(cfg.name.clone()).or_default();
                e.status.last_applied = matched.len();
                e.status.last_rejected = rejected.clone();
                self.pending.lock().extend(matched);
                Ok(MmfOutcome {
                    tickets: tickets.len(),
                    latency_ms,
                    proposals,
                    applied: e.status.last_applied,
                    rejected,
                })
            }
        }
    }

    fn due(&self, cfg: &musubi_matchmaker::QueueConfig, now: std::time::Instant) -> bool {
        let every = cfg.mmf.as_ref().map(|m| m.every_ms).unwrap_or(1000);
        let st = self.state.lock();
        st.get(&cfg.name)
            .and_then(|e| e.last_call)
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_millis(every))
    }

    pub fn statuses(&self, mm: &Matchmaker) -> Vec<MmfStatus> {
        let now = std::time::Instant::now();
        let st = self.state.lock();
        mm.queues()
            .into_iter()
            .filter(|q| q.mmf.as_ref().is_some_and(|m| m.is_http()))
            .map(|q| {
                let mut s = st
                    .get(&q.name)
                    .map(|e| {
                        let mut s = e.status.clone();
                        s.last_call_ms_ago = e
                            .last_call
                            .map(|t| now.saturating_duration_since(t).as_millis() as u64);
                        s.last_ok_ms_ago = e
                            .last_ok
                            .map(|t| now.saturating_duration_since(t).as_millis() as u64);
                        s
                    })
                    .unwrap_or_default();
                s.queue = q.name.clone();
                s.url = q.mmf.as_ref().map(|m| m.url.clone()).unwrap_or_default();
                s.fallback_active = mm.mmf_fallback_active(&q, now);
                s
            })
            .collect()
    }
}

/// Leader-only: call every due `mmf: http` queue whose pool is big enough and dispatch what it
/// formed. Runs from the matchmaker ticker next to the built-in pass.
pub async fn mmf_cycle(
    mmf: &MmfClient,
    relayed: &RelayedRegistry,
    mm: &Matchmaker,
    fanout: &Fanout,
) {
    if !fanout.is_leader() {
        return;
    }
    let now = std::time::Instant::now();
    for cfg in mm.queues() {
        let Some(m) = cfg.mmf.as_ref().filter(|m| m.is_http()) else {
            continue;
        };
        if !mmf.due(&cfg, now) {
            continue;
        }
        let pool = mm.pool_snapshot(&cfg.name);
        let members: usize = pool.iter().map(|t| t.members.len()).sum();
        if members < cfg.min_count.max(m.min_tickets) {
            continue;
        }
        match mmf.run_queue(mm, &cfg, false).await {
            Ok(_) => {}
            Err(e) => tracing::warn!(queue = %cfg.name, error = %e, "match function call failed"),
        }
        for formed in std::mem::take(&mut *mmf.pending.lock()) {
            dispatch(relayed, mm, fanout, formed);
        }
    }
}

/// Wait time per ticket, grade per match and a match counter, labelled by queue and region
/// (`free` for mutual-query tickets, `any` without ping data).
fn observe(mm: &Matchmaker, m: &Matched) {
    let queue = m.queue.clone().unwrap_or_else(|| "free".into());
    let region = m.region.clone().unwrap_or_else(|| "any".into());
    for t in &m.tickets {
        let wait = t.created.elapsed().as_secs_f64();
        metrics::histogram!(
            "musubi_matchmaker_wait_seconds",
            "queue" => queue.clone(),
            "region" => region.clone()
        )
        .record(wait);
        mm.record_wait(&queue, &region, wait as f32);
    }
    if let Some(q) = m.quality {
        metrics::histogram!("musubi_matchmaker_match_quality", "queue" => queue.clone())
            .record(q.score as f64);
        metrics::histogram!("musubi_matchmaker_team_mmr_gap", "queue" => queue.clone())
            .record(q.team_mmr_gap);
    }
    metrics::counter!("musubi_matchmaker_matches_total", "queue" => queue, "region" => region)
        .increment(1);
}

/// WASM override of the queue algorithm: a module exporting `matchmaker_score` gets every
/// candidate match (`CandidateMatch` JSON) and answers a number (negative = veto; or
/// `{"score": n}`); one exporting `matchmaker_propose` gets `{"queue", "tickets"}` for a queue's
/// whole waiting pool and answers groups of ticket ids (`[[id, …], …]` or `{"groups": …}`) — an
/// empty answer falls back to the built-in algorithm.
pub struct RuntimeScorer(pub Arc<musubi_runtime::Runtime>);

impl RuntimeScorer {
    fn call(&self, export: &str, payload: &serde_json::Value) -> Option<serde_json::Value> {
        if !self.0.has_hook(export) {
            return None;
        }
        let ctx = musubi_runtime::CallContext {
            account_id: None,
            session_id: None,
            username: None,
            server: true,
            vars: Default::default(),
        };
        let body = serde_json::to_vec(payload).ok()?;
        match self.0.invoke_sync(export, &ctx, &body) {
            Ok(out) => serde_json::from_slice(&out)
                .map_err(
                    |e| tracing::warn!(export, error = %e, "matchmaker hook returned non-JSON"),
                )
                .ok(),
            Err(e) => {
                tracing::warn!(export, error = %e, "matchmaker hook failed");
                None
            }
        }
    }
}

impl musubi_matchmaker::Scorer for RuntimeScorer {
    fn score(&self, c: &musubi_matchmaker::CandidateMatch) -> Option<f32> {
        let export = match &c.mmf {
            Some(m) if m.kind == "wasm" => {
                if m.score.is_empty() {
                    return None;
                }
                m.score.clone()
            }
            Some(_) => return None,
            None => "matchmaker_score".into(),
        };
        let v = self.call(&export, &serde_json::to_value(c).ok()?)?;
        v.as_f64()
            .or_else(|| v.get("score").and_then(|s| s.as_f64()))
            .map(|s| s as f32)
    }

    fn propose(
        &self,
        queue: &musubi_matchmaker::QueueConfig,
        tickets: &[musubi_matchmaker::TicketWire],
    ) -> Option<Vec<Vec<String>>> {
        let export = match &queue.mmf {
            Some(m) if m.kind == "wasm" => {
                if m.propose.is_empty() {
                    return None;
                }
                m.propose.clone()
            }
            Some(_) => return None,
            None => "matchmaker_propose".into(),
        };
        let v = self.call(
            &export,
            &serde_json::json!({ "queue": queue, "extensions": queue.extensions, "tickets": tickets }),
        )?;
        let groups = v.get("groups").cloned().unwrap_or(v);
        let groups: Vec<Vec<String>> = serde_json::from_value(groups).ok()?;
        (!groups.is_empty()).then_some(groups)
    }
}

pub const MATCHMAKER_LEASE_TTL: Duration = Duration::from_secs(6);

/// Background pass so tickets that waited out `min_wait` still get matched without new arrivals.
#[allow(clippy::too_many_arguments)]
pub fn spawn_ticker(
    relayed: Arc<RelayedRegistry>,
    mm: Arc<Matchmaker>,
    fanout: Fanout,
    cluster: Option<Arc<Cluster>>,
    every: Duration,
    hub: Arc<Hub>,
    parties: Arc<PartyRegistry>,
    mmf: Arc<MmfClient>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        // One matchmaker leader cluster-wide, by lease (short TTL so leadership moves fast).
        let lease = cluster
            .as_ref()
            .map(|c| c.lease("matchmaker", MATCHMAKER_LEASE_TTL));
        loop {
            tick.tick().await;
            fanout.set_leader(lease.as_ref().is_none_or(|l| l.held()));
            // every node expires its own view of the pool and tells its local sessions
            deliver_expired(&hub, &parties, mm.expire());
            process_and_deliver(&relayed, &mm, &fanout);
            mmf_cycle(&mmf, &relayed, &mm, &fanout).await;
        }
    })
}
