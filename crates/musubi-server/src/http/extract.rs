use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use ipnet::IpNet;
use musubi_auth::Authenticated;
use musubi_core::Error;
use std::net::IpAddr;
use std::sync::OnceLock;

use crate::app::AppState;
use crate::http::ApiError;

/// Extracts and verifies `Authorization: Bearer <access token>`.
pub struct Bearer(pub Authenticated);

impl FromRequestParts<AppState> for Bearer {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Unauthenticated("missing bearer token".into()))?;
        let token = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .ok_or_else(|| Error::Unauthenticated("malformed authorization header".into()))?;
        let auth = state.auth.verify_access(token)?;
        if !crate::scopes::allows(&auth.scopes, parts.method.as_str(), parts.uri.path()) {
            return Err(Error::PermissionDenied(format!(
                "token scopes [{}] do not cover {} {}",
                auth.scopes.join(", "),
                parts.method,
                parts.uri.path()
            ))
            .into());
        }
        state.banned.check(auth.account_id)?;
        if !crate::legal::exempt(parts.uri.path()) {
            state.legal.check(&state.db, auth.account_id).await?;
        }
        state.limits.account(auth.account_id)?;
        Ok(Bearer(auth))
    }
}

/// Server-to-server caller authenticated by the shared `http.server_key`.
pub struct ServerKey;

impl FromRequestParts<AppState> for ServerKey {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let caller = check_server_key(state, &parts.headers)?;
        state.limits.key(&caller)?;
        if !caller.role.allows(parts.method.as_str(), parts.uri.path()) {
            return Err(Error::PermissionDenied(format!(
                "key '{}' has role {}: {} {} is outside its scope",
                caller.name,
                caller.role.as_str(),
                parts.method,
                parts.uri.path()
            ))
            .into());
        }
        if !crate::scopes::allows(&caller.scopes, parts.method.as_str(), parts.uri.path()) {
            return Err(Error::PermissionDenied(format!(
                "key '{}' scopes [{}] do not cover {} {}",
                caller.name,
                caller.scopes.join(", "),
                parts.method,
                parts.uri.path()
            ))
            .into());
        }
        parts.extensions.insert(caller);
        Ok(ServerKey)
    }
}

/// A game server process: `X-Musubi-Server-Key` carries the per-server token the fleet put in
/// its `MUSUBI_SERVER_KEY` (`gs_<server id>_…`), which only identifies that one server — a leaked
/// or rogue server cannot speak for another or reach `/v1/server/*`. With
/// `[fleet].legacy_server_key = true` the operator key plus `X-Musubi-Server-Id` is still accepted.
pub struct GameServerAuth(pub uuid::Uuid);

impl FromRequestParts<AppState> for GameServerAuth {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let key = parts
            .headers
            .get("x-musubi-server-key")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Key".into()))?;
        if key.starts_with("gs_") {
            let id = state.fleets.authenticate_server_token(key).await?;
            state.limits.game_server(id)?;
            return Ok(GameServerAuth(id));
        }
        if !state.cfg.fleet.legacy_server_key {
            return Err(Error::Unauthenticated(
                "game server endpoints take the per-server token from MUSUBI_SERVER_KEY (gs_…); set [fleet].legacy_server_key = true to accept the operator key with X-Musubi-Server-Id"
                    .into(),
            )
            .into());
        }
        let caller = check_server_key(state, &parts.headers)?;
        state.limits.key(&caller)?;
        let id = parts
            .headers
            .get("x-musubi-server-id")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Id".into()))?;
        let id = uuid::Uuid::parse_str(id)
            .map_err(|_| Error::InvalidArgument("bad X-Musubi-Server-Id".into()))?;
        Ok(GameServerAuth(id))
    }
}

/// `X-Musubi-Server-Key`: the config key (constant-time compare, bootstrap admin) or a key issued
/// through `/v1/admin/keys` with its role.
/// Constant-time equality for keys/tokens (length leaks, bytes do not).
pub(crate) fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

pub fn check_server_key(
    state: &AppState,
    headers: &axum::http::HeaderMap,
) -> musubi_core::Result<crate::keys::Caller> {
    // OAuth2 client-credentials tokens (P1-23) are server keys and may travel as a bearer token
    let bearer = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| t.starts_with("msk_"));
    let given = headers
        .get("x-musubi-server-key")
        .and_then(|v| v.to_str().ok())
        .or(bearer)
        .ok_or_else(|| Error::Unauthenticated("missing X-Musubi-Server-Key".into()))?;
    let expected = state.cfg.http.server_key.as_bytes();
    if ct_eq(given.as_bytes(), expected) {
        return Ok(crate::keys::Caller::bootstrap());
    }
    if let Some(c) = state.keys.lookup(given) {
        state.keys.touch(Some(state.db.clone()), c.id);
        return Ok(c);
    }
    Err(Error::Unauthenticated("bad server key".into()))
}

/// Client address as seen through the reverse proxies listed in `[http] trusted_proxies`: when
/// the TCP peer is a trusted proxy, the right-most `X-Forwarded-For` hop that is not itself
/// trusted (then `X-Real-IP`); otherwise the peer address, so an internet client cannot pick
/// its own address for rate limits, bans and geo. `"unknown"` without connect info. Infallible.
pub struct ClientIp(pub String);

static TRUSTED_PROXIES: OnceLock<Vec<IpNet>> = OnceLock::new();

/// Install `[http] trusted_proxies` once at startup; returns the entries that did not parse
/// (`ip` or `ip/prefix`). Later calls are ignored, the config default applies until then.
pub fn set_trusted_proxies(entries: &[String]) -> Vec<String> {
    let (nets, bad) = parse_proxies(entries);
    let _ = TRUSTED_PROXIES.set(nets);
    bad
}

fn parse_proxies(entries: &[String]) -> (Vec<IpNet>, Vec<String>) {
    let mut nets = Vec::new();
    let mut bad = Vec::new();
    for e in entries {
        let e = e.trim();
        if let Ok(n) = e.parse::<IpNet>() {
            nets.push(n);
        } else if let Ok(ip) = e.parse::<IpAddr>() {
            nets.push(IpNet::from(ip));
        } else {
            bad.push(e.to_string());
        }
    }
    (nets, bad)
}

fn is_trusted_proxy(ip: IpAddr) -> bool {
    TRUSTED_PROXIES
        .get_or_init(|| {
            parse_proxies(&musubi_core::config::HttpConfig::default().trusted_proxies).0
        })
        .iter()
        .any(|n| n.contains(&ip))
}

/// The client behind trusted hops: walk `X-Forwarded-For` from the right, skipping trusted
/// proxies and anything that is not an IP; fall back to `X-Real-IP`.
fn forwarded_client(headers: &axum::http::HeaderMap) -> Option<IpAddr> {
    let hops: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|h| {
            h.trim()
                .trim_matches(|c| c == '[' || c == ']' || c == '"')
                .parse()
                .ok()
        })
        .collect();
    if let Some(ip) = hops.iter().rev().find(|ip| !is_trusted_proxy(**ip)) {
        return Some(*ip);
    }
    if let Some(first) = hops.first() {
        return Some(*first);
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
}

pub(crate) fn resolve_client_ip(
    peer: Option<IpAddr>,
    headers: &axum::http::HeaderMap,
) -> Option<IpAddr> {
    match peer {
        Some(p) if is_trusted_proxy(p) => forwarded_client(headers).or(Some(p)),
        Some(p) => Some(p),
        None => forwarded_client(headers),
    }
}

impl<S: Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|c| c.0.ip());
        let ip = resolve_client_ip(peer, &parts.headers)
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "unknown".into());
        Ok(ClientIp(ip))
    }
}

#[cfg(test)]
mod client_ip_tests {
    use super::resolve_client_ip;
    use axum::http::{HeaderMap, HeaderValue};

    fn h(xff: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert("x-forwarded-for", HeaderValue::from_str(xff).unwrap());
        m
    }

    #[test]
    fn internet_peer_cannot_spoof_forwarded_for() {
        let ip = resolve_client_ip(Some("203.0.113.9".parse().unwrap()), &h("10.0.0.1"));
        assert_eq!(ip.unwrap().to_string(), "203.0.113.9");
    }

    #[test]
    fn trusted_proxy_yields_rightmost_untrusted_hop() {
        let ip = resolve_client_ip(
            Some("10.0.0.2".parse().unwrap()),
            &h("1.2.3.4, 198.51.100.7, 172.16.0.3"),
        );
        assert_eq!(ip.unwrap().to_string(), "198.51.100.7");
    }

    #[test]
    fn garbage_hops_are_ignored() {
        let ip = resolve_client_ip(Some("127.0.0.1".parse().unwrap()), &h("evil, 198.51.100.7"));
        assert_eq!(ip.unwrap().to_string(), "198.51.100.7");
        let mut m = HeaderMap::new();
        m.insert("x-real-ip", HeaderValue::from_static("not-an-ip"));
        assert_eq!(
            resolve_client_ip(Some("127.0.0.1".parse().unwrap()), &m)
                .unwrap()
                .to_string(),
            "127.0.0.1"
        );
    }
}
