//! Realtime sessions: one task per socket, a process-local [`Hub`] for presence and fan-out.
//! Cluster-wide presence arrives with `musubi-cluster`; the Hub API is already shaped for it.

pub mod authoritative;
pub mod connect;
mod hub;
pub mod matchmaking;
pub mod party;
pub mod quic;
pub mod relayed;
mod session;
pub mod wasm_match;
pub mod world;

pub use authoritative::MatchRegistry;
pub use hub::{Hub, Resumed};
pub use party::PartyRegistry;
pub use relayed::RelayedRegistry;
pub use session::{resume_or_reject, serve, ConnectParams};
pub use world::WorldRegistry;
