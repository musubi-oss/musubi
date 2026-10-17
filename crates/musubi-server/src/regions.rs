//! Client georouting helpers: the advertised region list, the country → region fallback and a
//! UDP echo clients time their probes against.

use std::net::SocketAddr;

use musubi_core::config::RegionsConfig;
use musubi_core::{Error, Result};
use serde_json::{json, Value};

/// Region the request most likely comes from, by the country header a CDN/proxy sets.
pub fn suggest(
    cfg: &RegionsConfig,
    headers: &axum::http::HeaderMap,
) -> (Option<String>, Option<String>) {
    let country = [
        "cf-ipcountry",
        "x-country",
        "x-musubi-country",
        "cloudfront-viewer-country",
    ]
    .iter()
    .find_map(|h| headers.get(*h).and_then(|v| v.to_str().ok()))
    .map(|c| c.trim().to_ascii_uppercase())
    .filter(|c| !c.is_empty() && c != "XX");
    let suggested = country.as_deref().and_then(|c| {
        cfg.country_map
            .get(c)
            .cloned()
            .or_else(|| cfg.country_map.get("*").cloned())
    });
    (country, suggested)
}

/// Advertised regions: configured entries plus any fleet region not listed (without probes).
pub fn list(cfg: &RegionsConfig, fleet_regions: impl IntoIterator<Item = String>) -> Vec<Value> {
    let mut out: Vec<Value> = cfg
        .regions
        .iter()
        .map(|r| json!({ "name": r.name, "echo": r.echo, "http": r.http }))
        .collect();
    for r in fleet_regions {
        if r.is_empty() || out.iter().any(|o| o["name"] == r) {
            continue;
        }
        out.push(json!({ "name": r, "echo": "", "http": "" }));
    }
    out
}

/// Echo small datagrams back to the sender so clients can measure RTT to this node.
pub async fn spawn_echo(listen: SocketAddr) -> Result<SocketAddr> {
    let sock = tokio::net::UdpSocket::bind(listen)
        .await
        .map_err(Error::internal)?;
    let local = sock.local_addr().map_err(Error::internal)?;
    tokio::spawn(async move {
        let mut buf = [0u8; 64];
        loop {
            match sock.recv_from(&mut buf).await {
                Ok((n, from)) => {
                    let _ = sock.send_to(&buf[..n], from).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "region echo socket error");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    });
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_by_country_header_with_wildcard() {
        let mut cfg = RegionsConfig::default();
        cfg.country_map.insert("DE".into(), "eu".into());
        cfg.country_map.insert("*".into(), "us".into());
        let mut h = axum::http::HeaderMap::new();
        h.insert("cf-ipcountry", "de".parse().unwrap());
        assert_eq!(suggest(&cfg, &h), (Some("DE".into()), Some("eu".into())));
        h.insert("cf-ipcountry", "BR".parse().unwrap());
        assert_eq!(suggest(&cfg, &h).1, Some("us".into()));
        h.insert("cf-ipcountry", "XX".parse().unwrap());
        assert_eq!(suggest(&cfg, &h), (None, None));
    }
}
