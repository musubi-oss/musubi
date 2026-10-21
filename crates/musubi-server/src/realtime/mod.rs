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

/// Hub channel carrying `storage_update` for one object (`key`) or an owner's whole collection.
pub fn storage_channel(collection: &str, owner: uuid::Uuid, key: Option<&str>) -> String {
    match key {
        Some(k) => format!("storage:{collection}:{owner}:{k}"),
        None => format!("storage:{collection}:{owner}"),
    }
}
