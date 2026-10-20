//! UDP edge proxy routing backed by fleet allocations: the token a client prefixes its datagrams
//! with is its connect token, which names exactly one allocated/in-game game server.

use std::net::SocketAddr;
use std::sync::Arc;

use musubi_fleet::FleetService;

pub struct FleetRouter(pub Arc<FleetService>);

#[async_trait::async_trait]
impl musubi_edge::Router for FleetRouter {
    async fn resolve(&self, token: &[u8]) -> Option<SocketAddr> {
        let token = std::str::from_utf8(token).ok()?;
        self.0.route_token(token).await
    }
}

/// The node's UDP edge proxy, once started (`GET /v1/server/edge`).
pub static EDGE: std::sync::OnceLock<Arc<musubi_edge::UdpProxy>> = std::sync::OnceLock::new();

/// `[edge.guard]` → the proxy's guard settings; bad CIDRs are a startup error.
pub fn guard_config(
    c: &musubi_core::config::EdgeGuardConfig,
) -> musubi_core::Result<musubi_edge::GuardConfig> {
    let cidrs = |list: &[String], what: &str| -> musubi_core::Result<Vec<(std::net::IpAddr, u8)>> {
        list.iter()
            .map(|t| {
                musubi_edge::guard::parse_cidr(t).ok_or_else(|| {
                    musubi_core::Error::InvalidArgument(format!(
                        "edge.guard.{what}: bad CIDR {t:?}"
                    ))
                })
            })
            .collect()
    };
    Ok(musubi_edge::GuardConfig {
        enabled: c.enabled,
        max_pps_per_source: c.max_pps_per_source,
        max_bytes_per_source: c.max_bytes_per_source,
        max_opens_per_source: c.max_opens_per_source,
        global_max_pps: c.global_max_pps,
        min_packet_bytes: c.min_packet_bytes,
        strikes_to_block: c.strikes_to_block,
        strike_window_secs: c.strike_window_secs,
        block_secs: c.block_secs,
        allow: cidrs(&c.allow, "allow")?,
        deny: cidrs(&c.deny, "deny")?,
    })
}
