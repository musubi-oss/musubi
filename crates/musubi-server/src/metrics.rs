//! Prometheus exposition (`GET /metrics`). Counters/histograms are recorded where the work
//! happens via the `metrics` facade; gauges that mirror registry sizes are refreshed on scrape.

use std::sync::OnceLock;
use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use metrics::{counter, gauge, histogram};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

use crate::app::AppState;

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// Install the process-wide recorder once; later calls (several servers in one process, as in
/// tests) share it.
pub fn install() -> PrometheusHandle {
    HANDLE
        .get_or_init(|| {
            PrometheusBuilder::new()
                .set_buckets_for_metric(
                    Matcher::Suffix("_duration_seconds".into()),
                    &[
                        0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5,
                        5.0,
                    ],
                )
                .expect("buckets")
                .install_recorder()
                .expect("prometheus recorder")
        })
        .clone()
}

/// Per-request counter + latency histogram, labelled by matched route (not the raw path, so
/// ids never explode the cardinality).
pub async fn track_http(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_owned();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned())
        .unwrap_or_else(|| "unmatched".into());
    let started = Instant::now();
    let res = next.run(req).await;
    let status = res.status().as_u16().to_string();
    counter!("musubi_http_requests_total", "method" => method.clone(), "route" => route.clone(), "status" => status)
        .increment(1);
    histogram!("musubi_http_request_duration_seconds", "method" => method, "route" => route)
        .record(started.elapsed().as_secs_f64());
    res
}

pub async fn render(State(s): State<AppState>) -> Response {
    gauge!("musubi_uptime_seconds").set(s.started_at.elapsed().as_secs_f64());
    gauge!("musubi_realtime_sessions").set(s.hub.session_count() as f64);
    gauge!("musubi_authoritative_matches").set(s.matches.count() as f64);
    gauge!("musubi_relayed_matches").set(s.relayed.match_count() as f64);
    gauge!("musubi_parties").set(s.parties.party_count() as f64);
    gauge!("musubi_matchmaker_tickets").set(s.matchmaker.ticket_count() as f64);
    gauge!("musubi_worlds").set(s.worlds.list().len() as f64);
    gauge!("musubi_runtime_modules").set(s.runtime.module_names().len() as f64);
    if let Some(c) = &s.cluster {
        if let Ok(nodes) = c.nodes().await {
            let alive = nodes.iter().filter(|n| n.alive).count();
            gauge!("musubi_cluster_nodes", "state" => "alive").set(alive as f64);
            gauge!("musubi_cluster_nodes", "state" => "dead").set((nodes.len() - alive) as f64);
        }
        gauge!("musubi_raft_member").set(if c.raft_id() == 0 { 0.0 } else { 1.0 });
    }
    if let Ok(fleets) = s.fleets.list().await {
        for f in fleets {
            if let Ok(servers) = s.fleets.servers(f.id, None).await {
                for state in ["starting", "ready", "allocated", "shutdown", "unhealthy"] {
                    let n = servers.iter().filter(|g| g.state == state).count();
                    gauge!("musubi_fleet_servers", "fleet" => f.name.clone(), "state" => state)
                        .set(n as f64);
                }
            }
        }
    }
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        s.metrics.render(),
    )
        .into_response()
}
