//! Realtime sessions: one task per socket, a process-local [`Hub`] for presence and fan-out.
//! Cluster-wide presence arrives with `musubi-cluster`; the Hub API is already shaped for it.

pub mod authoritative;
pub mod connect;
mod hub;
pub mod matchmaking;
pub mod netsim;
pub mod party;
pub mod quic;
pub mod relayed;
pub mod resume;
mod session;

pub mod hojournal;
pub mod udp;
pub mod wasm_match;
pub mod webrtc;
pub mod webtransport;
pub mod world;

pub use authoritative::MatchRegistry;
pub use hub::{Hub, Resumed, SessionSendError};
pub use party::PartyRegistry;
pub use relayed::RelayedRegistry;
pub use session::{resume_or_reject, serve, ConnectParams};
pub use world::WorldRegistry;

/// Matches (authoritative or relayed) one session may be in at once on this node: the
/// per-session membership lists are client-driven and were unbounded.
pub const MAX_MATCHES_PER_SESSION: usize = 64;

/// Room for `match_id` in a session's membership list: fine when already listed or under the cap.
pub(crate) fn check_match_room(list: &[String], match_id: &str) -> musubi_core::Result<()> {
    if list.iter().any(|m| m == match_id) || list.len() < MAX_MATCHES_PER_SESSION {
        Ok(())
    } else {
        Err(musubi_core::Error::ResourceExhausted(format!(
            "session is already in {MAX_MATCHES_PER_SESSION} matches"
        )))
    }
}

/// Record `match_id` in a session's membership list once (a re-join must not add a duplicate).
pub(crate) fn track_match(list: &mut Vec<String>, match_id: &str) {
    if !list.iter().any(|m| m == match_id) {
        list.push(match_id.to_string());
    }
}

/// Hub channel carrying `storage_update` for one object (`key`) or an owner's whole collection.
pub fn storage_channel(collection: &str, owner: uuid::Uuid, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("storage:{collection}:{owner}:{k}"),
        None => format!("storage:{collection}:{owner}"),
    }
}

#[cfg(test)]
mod match_cap_tests {
    use super::*;

    #[test]
    fn per_session_match_list_is_capped_and_deduplicated() {
        let mut list = Vec::new();
        for i in 0..MAX_MATCHES_PER_SESSION {
            let id = format!("m{i}");
            check_match_room(&list, &id).unwrap();
            track_match(&mut list, &id);
            track_match(&mut list, &id);
        }
        assert_eq!(list.len(), MAX_MATCHES_PER_SESSION);
        // re-joining a listed match is always allowed
        check_match_room(&list, "m0").unwrap();
        let err = check_match_room(&list, "one-too-many").unwrap_err();
        assert!(matches!(err, musubi_core::Error::ResourceExhausted(_)));
    }
}
