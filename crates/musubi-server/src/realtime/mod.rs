//! Realtime sessions: one task per socket, a process-local [`Hub`] for presence and fan-out.
//! Cluster-wide presence arrives with `musubi-cluster`; the Hub API is already shaped for it.

mod hub;
mod session;

pub use hub::Hub;
pub use session::{serve, ConnectParams};
