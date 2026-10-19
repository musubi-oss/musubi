pub mod access;
mod account;
pub mod account_policy;
pub mod admin;
pub mod ads;
mod analytics;
mod anticheat;
pub mod async_mm;
pub mod audiences;
mod auth;
pub mod auth_providers;
mod bans;
pub mod challenges;
mod chat;
mod cluster;
mod codes;
pub mod configs;
pub mod content;
pub mod crashes;
mod dev;
mod device_codes;
pub mod economy;
pub mod economy_changesets;
pub mod entitlements;
mod environments;
pub(crate) mod error;
pub(crate) mod extract;
mod files;
pub mod fleet_hosts;
pub mod fleet_metrics;
pub mod fleets;
mod friend_codes;
mod friends;
pub mod game_sessions;
pub mod group_challenges;
mod group_pools;
mod groups;
mod integrations;
pub mod items_lifecycle;
mod leaderboards;
mod legal;
mod liveops;
mod lobbies;
pub mod match_results;
mod matches;
pub(crate) mod matchmaker;
mod module_routes;
pub mod news;
mod notifications;
pub mod oauth;
mod oauth_authz;
mod offers;
mod parties;
mod passkeys;
pub mod placements;
pub mod push;
pub mod rating;
mod regions;
pub mod remote_config;
pub mod reports;
mod rpc;
mod rtc;
pub mod sanctions;
pub mod secrets;
mod server_browser;
mod sso;
pub mod stats;
pub mod steam_ownership;
mod storage;
pub mod streams;
pub(crate) mod subscriptions;
pub mod timers;
pub mod tournaments;
mod turns;
mod tx;
mod ugc;
mod workflows;
mod worlds;
mod ws;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::trace::TraceLayer;

use crate::app::AppState;

pub use error::ApiError;
pub use extract::{Bearer, ClientIp, ServerKey};

/// Several projects behind one listener: `x-musubi-project: <name>` picks one explicitly, else
/// the `Host` header is matched against each project's `hosts` (exact or `*.suffix`), else the
/// base project answers. `GET /v1/projects` lists them.
pub fn multi_router(
    base: (String, Router),
    projects: Vec<(String, Vec<String>, Router)>,
) -> Router {
    let mut info = vec![serde_json::json!({"name": base.0, "hosts": [], "default": true})];
    let mut by_name = std::collections::HashMap::new();
    let mut by_host = Vec::new();
    for (name, hosts, router) in projects {
        info.push(serde_json::json!({"name": name, "hosts": hosts, "default": false}));
        for h in hosts {
            by_host.push((h.to_ascii_lowercase(), name.clone()));
        }
        by_name.insert(name, router);
    }
    let dispatch = ProjectDispatch {
        base_name: base.0,
        base: base.1,
        by_name: std::sync::Arc::new(by_name),
        by_host: std::sync::Arc::new(by_host),
        info: std::sync::Arc::new(serde_json::json!({"projects": info})),
    };
    Router::new().fallback_service(dispatch)
}

#[derive(Clone)]
struct ProjectDispatch {
    base_name: String,
    base: Router,
    by_name: std::sync::Arc<std::collections::HashMap<String, Router>>,
    by_host: std::sync::Arc<Vec<(String, String)>>,
    info: std::sync::Arc<serde_json::Value>,
}

impl ProjectDispatch {
    fn pick(&self, req: &axum::http::Request<axum::body::Body>) -> Result<Router, Box<Response>> {
        if let Some(name) = req
            .headers()
            .get("x-musubi-project")
            .and_then(|v| v.to_str().ok())
        {
            if name == self.base_name {
                return Ok(self.base.clone());
            }
            return self.by_name.get(name).cloned().ok_or_else(|| {
                Box::new(
                    ApiError::from(musubi_core::Error::NotFound(format!(
                        "unknown project {name}"
                    )))
                    .into_response(),
                )
            });
        }
        let host = req
            .headers()
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .map(|h| h.split(':').next().unwrap_or(h).to_ascii_lowercase())
            .unwrap_or_default();
        for (pattern, name) in self.by_host.iter() {
            let hit = match pattern.strip_prefix("*.") {
                Some(suffix) => host.ends_with(suffix) && host.len() > suffix.len(),
                None => *pattern == host,
            };
            if hit {
                if let Some(r) = self.by_name.get(name) {
                    return Ok(r.clone());
                }
            }
        }
        Ok(self.base.clone())
    }
}

impl tower::Service<axum::http::Request<axum::body::Body>> for ProjectDispatch {
    type Response = Response;
    type Error = std::convert::Infallible;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Response, std::convert::Infallible>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: axum::http::Request<axum::body::Body>) -> Self::Future {
        if req.uri().path() == "/v1/projects" && req.method() == axum::http::Method::GET {
            let info = self.info.clone();
            return Box::pin(async move { Ok(Json((*info).clone()).into_response()) });
        }
        match self.pick(&req) {
            Ok(router) => Box::pin(async move {
                use tower::ServiceExt;
                router.oneshot(req).await
            }),
            Err(resp) => Box::pin(async move { Ok(*resp) }),
        }
    }
}

pub fn router(state: AppState) -> Router {
    crate::realtime::netsim::configure(&state.cfg.dev.netsim);
    Router::new()
        .route("/healthz", get(healthz))
        .route("/.well-known/jwks.json", get(jwks))
        .route("/metrics", get(crate::metrics::render))
        .route("/v1/admin/debug/profile", get(crate::profile::handler))
        .route("/openapi.json", get(openapi))
        .route("/readyz", get(readyz))
        .route("/v1/node", get(node_info))
        .merge(admin::routes())
        .merge(crate::idl::router())
        .merge(configs::router())
        .merge(timers::router())
        .merge(audiences::router())
        .merge(news::router())
        .merge(placements::router())
        .merge(sso::routes())
        .merge(stats::routes())
        .merge(push::routes())
        .merge(crate::events::routes())
        .merge(
            auth::routes().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::bans::ip_gate,
            )),
        )
        .merge(tx::routes())
        .merge(account::routes())
        .merge(passkeys::routes())
        .merge(friends::routes())
        .merge(friend_codes::routes())
        .merge(groups::routes())
        .merge(group_challenges::routes())
        .merge(notifications::routes())
        .merge(chat::routes())
        .merge(leaderboards::routes())
        .merge(lobbies::routes())
        .merge(device_codes::routes())
        .merge(turns::routes())
        .merge(tournaments::routes())
        .merge(rpc::routes())
        .merge(cluster::routes())
        .merge(matches::routes())
        .merge(matchmaker::routes())
        .merge(rating::routes())
        .merge(entitlements::routes())
        .merge(reports::routes())
        .merge(sanctions::routes())
        .merge(ugc::routes())
        .merge(anticheat::routes())
        .merge(regions::routes())
        .merge(dev::routes())
        .merge(match_results::routes())
        .merge(content::routes())
        .merge(economy_changesets::routes())
        .merge(fleets::routes())
        .merge(workflows::routes())
        .merge(worlds::routes())
        .merge(integrations::routes())
        .merge(rtc::routes())
        .merge(economy::routes())
        .merge(storage::routes())
        .merge(
            ws::routes().route_layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::bans::ip_gate,
            )),
        )
        .merge(bans::routes())
        .merge(group_pools::routes())
        .merge(challenges::routes())
        .merge(account_policy::routes())
        .merge(items_lifecycle::routes())
        .merge(game_sessions::routes())
        .merge(async_mm::routes())
        .merge(auth_providers::routes())
        .merge(fleet_metrics::routes())
        .merge(fleet_hosts::routes())
        .merge(server_browser::routes())
        .merge(streams::routes())
        .merge(module_routes::routes())
        .merge(remote_config::routes())
        .merge(files::routes())
        .merge(codes::routes())
        .merge(legal::routes())
        .merge(parties::routes())
        .merge(subscriptions::routes())
        .merge(liveops::routes())
        .merge(offers::routes())
        .merge(analytics::routes())
        .merge(crashes::routes())
        .merge(steam_ownership::routes())
        .merge(ads::routes())
        .merge(access::routes())
        .merge(oauth::routes())
        .merge(oauth_authz::routes())
        .merge(secrets::routes())
        .merge(environments::routes())
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::hooks::before_after,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::idempotency::replay,
        ))
        .layer(axum::middleware::from_fn(crate::telemetry::trace_id_header))
        .layer(TraceLayer::new_for_http().make_span_with(crate::telemetry::make_span))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::metrics::track_http,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::audit::record,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::dev::record,
        ))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

/// Readiness for load balancers / Kubernetes: 200 only when the database answers, every embedded
/// migration is applied, the Raft group (when enabled) has a leader, and the node is not draining.
async fn readyz(State(s): State<AppState>) -> Response {
    let draining = s.draining.load(std::sync::atomic::Ordering::Relaxed);
    let db_timeout = std::time::Duration::from_secs(2);
    let db = tokio::time::timeout(
        db_timeout,
        sqlx::query_scalar::<_, i32>("select 1").fetch_one(&s.db),
    )
    .await
    .map(|r| r.is_ok())
    .unwrap_or(false);
    let migrations = if db {
        tokio::time::timeout(
            db_timeout,
            sqlx::query_scalar::<_, i64>("select count(*) from _sqlx_migrations where success")
                .fetch_one(&s.db),
        )
        .await
        .ok()
        .and_then(|r| r.ok())
        .map(|n| n as usize >= crate::db::MIGRATOR.migrations.len())
        .unwrap_or(false)
    } else {
        false
    };
    // `None` = Raft not enabled on this node (no bus); only a leaderless group is a failure.
    let raft = match &s.cluster {
        Some(c) if c.raft_id() != 0 => Some(c.raft_leader().is_some()),
        _ => None,
    };
    let ready = !draining && db && migrations && raft.unwrap_or(true);
    let status = if ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        status,
        Json(json!({
            "status": if ready { "ok" } else { "unavailable" },
            "checks": { "db": db, "migrations": migrations, "raft": raft, "draining": draining },
        })),
    )
        .into_response()
}

async fn node_info(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "name": s.cfg.node.name,
        "region": s.cfg.node.region,
        "project": s.cfg.node.project,
        "environment": s.cfg.node.environment,
        "roles": s.cfg.node.roles,
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": s.started_at.elapsed().as_secs(),
        "realtime_sessions": s.hub.session_count(),
        "relayed_matches": s.relayed.match_count(),
        "authoritative_matches": s.matches.count(),
        "parties": s.parties.party_count(),
        "matchmaker_tickets": s.matchmaker.ticket_count(),
    }))
}

async fn openapi(State(s): State<AppState>) -> Json<serde_json::Value> {
    let idl = s.idl.get();
    Json(crate::openapi::document_with(
        env!("CARGO_PKG_VERSION"),
        idl.as_deref(),
    ))
}

/// Public keys of the managed JWT signing keys (RFC 7517), for verifying access tokens elsewhere.
async fn jwks(State(s): State<AppState>) -> Json<serde_json::Value> {
    Json(s.auth.signer().jwks())
}
