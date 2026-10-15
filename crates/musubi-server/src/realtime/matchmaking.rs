//! Glue between the matchmaker pool and live sessions.

use std::sync::Arc;
use std::time::Duration;

use musubi_cluster::Cluster;
use musubi_matchmaker::{Matched, Matchmaker};

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

/// Leader-only: form groups, deliver locally and announce them to the other nodes.
pub fn process_and_deliver(relayed: &RelayedRegistry, mm: &Matchmaker, fanout: &Fanout) {
    if !fanout.is_leader() {
        return;
    }
    for m in mm.process() {
        let match_id = relayed.new_match_id();
        fanout.deliver_matched(m, match_id);
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
            process_and_deliver(&relayed, &mm, &fanout);
        }
    })
}
