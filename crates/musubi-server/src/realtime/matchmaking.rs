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
        let v = self.call("matchmaker_score", &serde_json::to_value(c).ok()?)?;
        v.as_f64()
            .or_else(|| v.get("score").and_then(|s| s.as_f64()))
            .map(|s| s as f32)
    }

    fn propose(
        &self,
        queue: &musubi_matchmaker::QueueConfig,
        tickets: &[musubi_matchmaker::TicketWire],
    ) -> Option<Vec<Vec<String>>> {
        let v = self.call(
            "matchmaker_propose",
            &serde_json::json!({ "queue": queue, "tickets": tickets }),
        )?;
        let groups = v.get("groups").cloned().unwrap_or(v);
        let groups: Vec<Vec<String>> = serde_json::from_value(groups).ok()?;
        (!groups.is_empty()).then_some(groups)
    }
}

pub const MATCHMAKER_LEASE_TTL: Duration = Duration::from_secs(6);

/// Background pass so tickets that waited out `min_wait` still get matched without new arrivals.
pub fn spawn_ticker(
    relayed: Arc<RelayedRegistry>,
    mm: Arc<Matchmaker>,
    fanout: Fanout,
    cluster: Option<Arc<Cluster>>,
    every: Duration,
    hub: Arc<Hub>,
    parties: Arc<PartyRegistry>,
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
        }
    })
}
