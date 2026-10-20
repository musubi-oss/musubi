//! UDP edge proxy status and its flood guard: `GET /v1/server/edge`, `POST …/edge/block`,
//! `POST …/edge/unblock`.

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/edge", get(status))
        .route("/v1/server/edge/block", post(block))
        .route("/v1/server/edge/unblock", post(unblock))
}

fn proxy() -> std::result::Result<&'static std::sync::Arc<musubi_edge::UdpProxy>, ApiError> {
    crate::edge::EDGE
        .get()
        .ok_or_else(|| ApiError(Error::NotFound("udp edge proxy is not running here".into())))
}

pub fn status_json(p: &musubi_edge::UdpProxy) -> Value {
    let g = p.guard();
    let c = &g.counters;
    let (forwarded, dropped) = p.counters();
    json!({
        "listen": p.local_addr().to_string(),
        "sessions": p.sessions(),
        "forwarded": forwarded,
        "dropped": dropped,
        "guard": {
            "enabled": g.cfg.enabled,
            "max_pps_per_source": g.cfg.max_pps_per_source,
            "max_bytes_per_source": g.cfg.max_bytes_per_source,
            "max_opens_per_source": g.cfg.max_opens_per_source,
            "global_max_pps": g.cfg.global_max_pps,
            "min_packet_bytes": g.cfg.min_packet_bytes,
            "strikes_to_block": g.cfg.strikes_to_block,
            "strike_window_secs": g.cfg.strike_window_secs,
            "block_secs": g.cfg.block_secs,
            "allow": g.cfg.allow.iter().map(|(ip, l)| format!("{ip}/{l}")).collect::<Vec<_>>(),
            "deny": g.cfg.deny.iter().map(|(ip, l)| format!("{ip}/{l}")).collect::<Vec<_>>(),
            "tracked_sources": g.tracked_sources(),
            "counters": {
                "passed": c.passed.load(Ordering::Relaxed),
                "denied": c.denied.load(Ordering::Relaxed),
                "blocked": c.blocked.load(Ordering::Relaxed),
                "too_small": c.too_small.load(Ordering::Relaxed),
                "global_rate": c.global_rate.load(Ordering::Relaxed),
                "source_rate": c.source_rate.load(Ordering::Relaxed),
                "source_bytes": c.source_bytes.load(Ordering::Relaxed),
                "open_rate": c.open_rate.load(Ordering::Relaxed),
                "strikes": c.strikes.load(Ordering::Relaxed),
                "blocks_issued": c.blocks_issued.load(Ordering::Relaxed),
            },
            "blocklist": g.blocklist().iter().map(|b| json!({
                "ip": b.ip.to_string(), "remaining_secs": b.remaining_secs,
                "strikes": b.strikes, "blocks": b.blocks,
            })).collect::<Vec<_>>(),
        },
    })
}

async fn status(
    State(_s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(status_json(proxy()?)))
}

#[derive(Deserialize)]
struct BlockBody {
    ip: String,
    secs: Option<u64>,
}

/// `POST /v1/server/edge/block {ip, secs?}` — drop everything from `ip` for `secs` (default
/// the configured `block_secs`).
async fn block(
    State(_s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<BlockBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = proxy()?;
    let ip: std::net::IpAddr = b.ip.trim().parse().map_err(|_| {
        ApiError(Error::InvalidArgument(
            "ip must be an IPv4/IPv6 address".into(),
        ))
    })?;
    if p.guard().allowed(ip) {
        return Err(ApiError(Error::InvalidArgument(
            "ip is on the allow list".into(),
        )));
    }
    let secs = b.secs.unwrap_or(p.guard().cfg.block_secs);
    if secs == 0 || secs > 30 * 86_400 {
        return Err(ApiError(Error::InvalidArgument(
            "secs must be 1..=2592000".into(),
        )));
    }
    p.guard().block(ip, secs);
    Ok(Json(json!({ "ip": ip.to_string(), "blocked_secs": secs })))
}

#[derive(Deserialize)]
struct UnblockBody {
    ip: String,
}

async fn unblock(
    State(_s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<UnblockBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = proxy()?;
    let ip: std::net::IpAddr = b.ip.trim().parse().map_err(|_| {
        ApiError(Error::InvalidArgument(
            "ip must be an IPv4/IPv6 address".into(),
        ))
    })?;
    if !p.guard().unblock(ip) {
        return Err(ApiError(Error::NotFound("ip is not blocked".into())));
    }
    Ok(Json(json!({ "ip": ip.to_string(), "unblocked": true })))
}
