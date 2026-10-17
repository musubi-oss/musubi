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
