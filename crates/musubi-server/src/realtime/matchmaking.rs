//! Glue between the matchmaker pool and live sessions.

use std::sync::Arc;
use std::time::Duration;

use musubi_matchmaker::{Matched, Matchmaker};
use musubi_proto::ServerMessage;

use super::Hub;

/// Deliver a formed group to every member session.
pub fn deliver(hub: &Hub, m: &Matched) {
    let users = m.users();
    for t in &m.tickets {
        for member in &t.members {
            hub.send_to_session(
                member.presence.session_id,
                ServerMessage::MatchmakerMatched {
                    ticket: t.id.clone(),
                    token: m.token.clone(),
                    users: users.clone(),
                    me: member.clone(),
                },
            );
        }
    }
}

pub fn process_and_deliver(hub: &Hub, mm: &Matchmaker) {
    for m in mm.process() {
        deliver(hub, &m);
    }
}

/// Background pass so tickets that waited out `min_wait` still get matched without new arrivals.
pub fn spawn_ticker(
    hub: Arc<Hub>,
    mm: Arc<Matchmaker>,
    every: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            process_and_deliver(&hub, &mm);
        }
    })
}
