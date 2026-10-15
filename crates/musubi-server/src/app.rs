use std::sync::Arc;

use musubi_account::AccountService;
use musubi_auth::AuthService;
use musubi_chat::ChatService;
use musubi_cluster::Cluster;
use musubi_core::config::Config;
use musubi_fleet::FleetService;
use musubi_integrations::{AurixClient, ZanshinClient};
use musubi_leaderboard::{LeaderboardService, TournamentService};
use musubi_matchmaker::Matchmaker;
use musubi_runtime::Runtime;

use crate::fanout::Fanout;
use musubi_social::{GroupService, NotificationService, SocialService};

use crate::notify::Notifier;
use musubi_storage::StorageService;
use sqlx::PgPool;
use tokio::signal;
use tracing::info;

use crate::realtime::{Hub, MatchRegistry, PartyRegistry, RelayedRegistry};
use crate::{db, http};

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub auth: AuthService,
    pub storage: StorageService,
    pub accounts: AccountService,
    pub social: SocialService,
    pub groups: GroupService,
    pub notify: Notifier,
    pub chat: ChatService,
    pub leaderboards: LeaderboardService,
    pub tournaments: TournamentService,
    pub economy: musubi_economy::EconomyService,
    pub matchmaker: Arc<Matchmaker>,
    pub parties: Arc<PartyRegistry>,
    pub relayed: Arc<RelayedRegistry>,
    pub matches: Arc<MatchRegistry>,
    pub worlds: Arc<crate::realtime::WorldRegistry>,
    pub liveops: Option<Arc<ZanshinClient>>,
    pub voice: Option<Arc<AurixClient>>,
    pub apple_iap: Option<Arc<musubi_integrations::AppleStoreClient>>,
    pub google_iap: Option<Arc<musubi_integrations::GooglePlayClient>>,
    pub steam_iap: Option<Arc<musubi_integrations::SteamClient>>,
    pub runtime: Arc<Runtime>,
    pub cluster: Option<Arc<Cluster>>,
    pub fleets: Arc<FleetService>,
    pub fanout: Fanout,
    pub hub: Arc<Hub>,
    pub started_at: std::time::Instant,
    pub metrics: metrics_exporter_prometheus::PrometheusHandle,
    pub limits: Arc<crate::limits::RateLimiter>,
    pub events: Arc<crate::events::Events>,
    pub banned: Arc<crate::bans::Bans>,
    pub keys: Arc<crate::keys::ServerKeys>,
    pub logins: Arc<crate::logins::Social>,
    /// Set on SIGTERM: `/readyz` answers 503 so load balancers stop routing here.
    pub draining: Arc<std::sync::atomic::AtomicBool>,
    /// Fired by `POST /v1/admin/cluster/nodes/{id}/drain`: same path as SIGTERM.
    pub shutdown: Arc<tokio::sync::Notify>,
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    info!(node = %cfg.node.name, roles = ?cfg.node.roles, "starting");
    let problems = cfg.validate();
    for p in &problems {
        match p.severity {
            musubi_core::config::Severity::Error => tracing::error!(key = %p.key, "{}", p.message),
            musubi_core::config::Severity::Warning => tracing::warn!(key = %p.key, "{}", p.message),
        }
    }
    let errors: Vec<_> = problems
        .iter()
        .filter(|p| p.severity == musubi_core::config::Severity::Error)
        .map(|p| format!("{}: {}", p.key, p.message))
        .collect();
    if !errors.is_empty() {
        anyhow::bail!(
            "refusing to start: {} (set node.allow_insecure_defaults = true only for local development; `musubi doctor` lists everything)",
            errors.join("; ")
        );
    }
    let db = db::connect(&cfg.database).await?;
    if cfg.database.run_migrations {
        db::migrate(&db).await?;
    }
    let auth = AuthService::new(db.clone(), &cfg.session)?;
    let storage = StorageService::new(db.clone());
    let accounts = AccountService::new(db.clone());
    let social = SocialService::new(db.clone());
    let groups = GroupService::new(db.clone());
    let chat = ChatService::new(db.clone());
    let leaderboards = LeaderboardService::new(db.clone());
    let economy = musubi_economy::EconomyService::new(db.clone());
    let tournaments = TournamentService::new(db.clone());
    let hub = Arc::new(Hub::new());
    let matchmaker = Arc::new(
        Matchmaker::new(std::time::Duration::from_secs(cfg.matchmaker.min_wait_secs)).with_ttl(
            std::time::Duration::from_secs(cfg.matchmaker.ticket_ttl_secs),
        ),
    );
    let runtime = Arc::new(Runtime::new(musubi_runtime::Limits {
        fuel: cfg.runtime.fuel,
        max_memory_bytes: cfg.runtime.max_memory_mb << 20,
        timeout: std::time::Duration::from_millis(cfg.runtime.timeout_ms),
        max_payload_bytes: cfg.runtime.max_payload_kb << 10,
    })?);
    if let Some(dir) = &cfg.runtime.modules_dir {
        let (m, r) = runtime.load_dir(dir)?;
        tracing::info!(modules = m, rpcs = r, dir = %dir.display(), "runtime modules loaded");
    }
    let mut cluster = Cluster::new(
        db.clone(),
        &cfg.node.name,
        &cfg.node.region,
        cfg.node
            .roles
            .iter()
            .map(|r| format!("{r:?}").to_lowercase())
            .collect(),
        &cfg.http.listen.to_string(),
    );
    cluster = cluster.with_group(&cfg.node.cluster_group);
    cluster = cluster.with_data_dir(&cfg.node.data_dir)?;
    if !cfg.node.bus_listen.is_empty() {
        let listen: std::net::SocketAddr = cfg
            .node
            .bus_listen
            .parse()
            .map_err(|_| musubi_core::Error::InvalidArgument("node.bus_listen".into()))?;
        let adv = (!cfg.node.bus_advertise.is_empty()).then_some(cfg.node.bus_advertise.as_str());
        cluster = cluster.with_bus(listen, adv)?;
        tracing::info!(addr = ?cluster.bus_addr(), "quic node bus enabled");
    }
    let cluster = Arc::new(cluster);
    cluster.start().await?;
    let parties = Arc::new(PartyRegistry::new(cluster.node_id().0));
    let mut fleets = FleetService::new(
        db.clone(),
        cluster.node_id().0,
        &format!("http://{}", cfg.http.listen),
        &cfg.http.server_key,
    );
    if let Some(kube) = kube_config(&cfg.fleet.kubernetes) {
        use musubi_fleet::kubernetes::{KubernetesProvider, Mode};
        let modes: &[(&str, Mode)] = if cfg.fleet.kubernetes.agones {
            &[("kubernetes", Mode::Pods), ("agones", Mode::Agones)]
        } else {
            &[("kubernetes", Mode::Pods)]
        };
        for (name, mode) in modes {
            match KubernetesProvider::new(kube.clone(), *mode) {
                Ok(p) => {
                    tracing::info!(provider = name, url = %kube.url, namespace = %kube.namespace, "fleet provider registered");
                    fleets = fleets.with_provider(name, Arc::new(p));
                }
                Err(e) => tracing::warn!(provider = name, error = %e, "fleet provider disabled"),
            }
        }
    }
    let fleets = Arc::new(fleets);
    fleets
        .clone()
        .spawn_reconciler(std::time::Duration::from_secs(5), {
            let lease = cluster.lease("fleets", musubi_cluster::DEAD_AFTER);
            move || lease.held()
        });
    let relayed = Arc::new(RelayedRegistry::new(cluster.node_id().0));
    let matches = Arc::new(
        MatchRegistry::new(Some(db.clone()), cluster.node_id().0)
            .with_group(&cfg.node.cluster_group),
    );
    let wasm_mods = matches.register_wasm(&runtime);
    if !wasm_mods.is_empty() {
        tracing::info!(modules = ?wasm_mods, "wasm match modules registered");
    }
    // Our own id too: with a stable node id the matches we ran before a restart are ours again.
    let resumed = matches.recover(hub.clone(), &[cluster.node_id().0]).await?;
    if !resumed.is_empty() {
        tracing::info!(
            count = resumed.len(),
            "resumed authoritative matches from snapshots"
        );
    }
    // Market: close due auctions / expire trade offers; one node at a time.
    let market_lease = cluster.lease("market", musubi_cluster::DEAD_AFTER);
    // Failover: adopt matches whose node stopped heartbeating.
    {
        let (cluster, matches, hub) = (cluster.clone(), matches.clone(), hub.clone());
        let lease = cluster.lease("failover", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(musubi_cluster::DEAD_AFTER);
            let mut leaves = cluster.subscribe();
            loop {
                wait_failover_round(&mut tick, &mut leaves).await;
                if !await_lease(&lease).await {
                    continue;
                }
                let Some(dead) = dead_nodes(&cluster).await else {
                    continue;
                };
                match matches.recover(hub.clone(), &dead).await {
                    Ok(ids) if !ids.is_empty() => {
                        tracing::info!(count = ids.len(), ?dead, "adopted matches from dead nodes")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "failover recover failed"),
                }
            }
        });
    }
    let fanout = Fanout::new(
        hub.clone(),
        Some(cluster.clone()),
        matchmaker.clone(),
        parties.clone(),
        relayed.clone(),
    );
    crate::realtime::matchmaking::spawn_ticker(
        relayed.clone(),
        matchmaker.clone(),
        fanout.clone(),
        Some(cluster.clone()),
        std::time::Duration::from_secs(1),
        hub.clone(),
        parties.clone(),
    );
    fanout.set_fleets(fleets.clone());
    let worlds = Arc::new(
        crate::realtime::WorldRegistry::new(cluster.node_id().0, Some(db.clone()))
            .with_group(&cfg.node.cluster_group),
    );
    worlds.set_cluster(cluster.clone());
    // Admin kicks fan out to every node: each hub closes the account's local sockets.
    {
        let (cluster, hub) = (cluster.clone(), hub.clone());
        let mut rx = cluster.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m)
                        if m.topic == crate::http::admin::KICK_TOPIC
                            && m.origin != cluster.node_id() =>
                    {
                        if let Some(id) = m
                            .payload
                            .get("account_id")
                            .and_then(|v| v.as_str())
                            .and_then(|v| uuid::Uuid::parse_str(v).ok())
                        {
                            hub.kick_account(musubi_core::AccountId(id));
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }
    let adopted = worlds.recover(&[cluster.node_id().0]).await?;
    if !adopted.is_empty() {
        tracing::info!(count = adopted.len(), "adopted orphaned worlds");
    }
    // World failover: adopt worlds whose home node died (one adopter at a time).
    {
        let (cluster, worlds) = (cluster.clone(), worlds.clone());
        let lease = cluster.lease("world-failover", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(musubi_cluster::DEAD_AFTER);
            let mut leaves = cluster.subscribe();
            loop {
                wait_failover_round(&mut tick, &mut leaves).await;
                if !await_lease(&lease).await {
                    continue;
                }
                let Some(dead) = dead_nodes(&cluster).await else {
                    continue;
                };
                match worlds.recover(&dead).await {
                    Ok(names) if !names.is_empty() => {
                        tracing::info!(?names, ?dead, "adopted worlds from dead nodes")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "world failover failed"),
                }
            }
        });
    }
    fanout.set_worlds(worlds.clone());
    let liveops = cfg.zanshin.url.as_deref().map(|u| {
        let c = ZanshinClient::new(u, &cfg.zanshin.server_key);
        c.clone().spawn_flusher(std::time::Duration::from_secs(
            cfg.zanshin.flush_secs.max(1),
        ));
        c
    });
    let voice = cfg
        .aurix
        .url
        .as_deref()
        .map(|u| AurixClient::new(u, &cfg.aurix.api_key));
    let apple_iap = iap_client("apple", &cfg.iap.apple.private_key_pem_path, |pem| {
        musubi_integrations::AppleStoreClient::new(
            &cfg.iap.apple.url,
            &cfg.iap.apple.issuer_id,
            &cfg.iap.apple.key_id,
            &cfg.iap.apple.bundle_id,
            pem,
        )
    });
    let steam_iap = if cfg.iap.steam.key.is_empty() {
        None
    } else {
        match musubi_integrations::SteamClient::new(
            &cfg.iap.steam.url,
            &cfg.iap.steam.key,
            cfg.iap.steam.app_id,
            cfg.iap.steam.sandbox,
        ) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!(provider = "steam", error = %e, "iap provider disabled");
                None
            }
        }
    };
    let google_iap = iap_client("google", &cfg.iap.google.private_key_pem_path, |pem| {
        musubi_integrations::GooglePlayClient::new_with_notifications(
            &cfg.iap.google.url,
            &cfg.iap.google.token_url,
            &cfg.iap.google.package_name,
            &cfg.iap.google.client_email,
            pem,
            &cfg.iap.google.notifications_token,
        )
    });
    worlds.clone().spawn_ticker(fanout.clone());
    let notify = Notifier::new(NotificationService::new(db.clone()), fanout.clone()).with_push(
        Arc::new(crate::push::Push::new(cfg.push.clone(), db.clone())),
    );
    let limits = Arc::new(crate::limits::RateLimiter::new(&cfg.limits));
    let logins = Arc::new(crate::logins::Social::new(cfg.social.clone()));
    let state = AppState {
        cfg: Arc::new(cfg),
        events: Arc::new(crate::events::Events::new(db.clone())),
        banned: Arc::new(crate::bans::Bans::default()),
        keys: Arc::new(crate::keys::ServerKeys::default()),
        logins,
        draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        db,
        auth,
        storage,
        accounts,
        social,
        groups,
        notify,
        chat,
        leaderboards,
        tournaments,
        economy,
        matchmaker,
        parties,
        relayed,
        matches,
        worlds,
        liveops,
        voice,
        apple_iap,
        google_iap,
        steam_iap,
        runtime,
        cluster: Some(cluster),
        fleets,
        fanout,
        hub,
        started_at: std::time::Instant::now(),
        metrics: crate::metrics::install(),
        limits,
    };
    crate::runtime_host::install(&state);
    if let Some(c) = state.cluster.clone() {
        state.limits.start_gossip(c);
    }
    match crate::modules::load_all(&state.db, &state.runtime, &state.matches).await {
        Ok(n) if n > 0 => tracing::info!(modules = n, "stored runtime modules loaded"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "stored runtime modules failed to load"),
    }
    crate::modules::start_sync(state.clone());
    state.matches.set_events(state.events.clone());
    crate::http::admin::start_drain_listener(state.clone());
    state.worlds.set_events(state.events.clone());
    crate::events::start_cluster_events(state.clone());
    match crate::jwt::load(&state.db, state.auth.signer()).await {
        Ok(n) if n > 0 => {
            info!(keys = n, active = ?state.auth.signer().active_kid(), "jwt keys loaded")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "jwt keys load failed"),
    }
    if let Some(c) = &state.cluster {
        crate::jwt::start(c, state.db.clone(), state.auth.clone());
    }
    match state.keys.load(&state.db).await {
        Ok(n) => info!(keys = n, "server keys loaded"),
        Err(e) => tracing::warn!(error = %e, "server keys load failed"),
    }
    if let Some(c) = &state.cluster {
        state.keys.start(c, state.db.clone());
    }
    match state.banned.load(&state.db).await {
        Ok(n) => tracing::info!(banned = n, "ban cache loaded"),
        Err(e) => tracing::warn!(error = %e, "ban cache load failed"),
    }
    if let Some(c) = &state.cluster {
        state.banned.start(c);
    }
    if let Some(c) = state.cluster.clone() {
        let ev = state.events.clone();
        let lease = c.lease("webhooks", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                tick.tick().await;
                if !lease.held() {
                    continue;
                }
                match ev.dispatch_once().await {
                    Ok((ok, failed)) if ok + failed > 0 => {
                        tracing::info!(delivered = ok, failed, "webhooks dispatched")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "webhook dispatch failed"),
                }
            }
        });
    }
    if let Some(c) = state.cluster.clone() {
        let st = state.clone();
        let lease = c.lease("retention", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let every = std::time::Duration::from_secs(st.cfg.retention.interval_secs.max(5));
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                if !lease.held() {
                    continue;
                }
                match crate::retention::run(&st.db, &st.cfg.retention).await {
                    Ok(v) => tracing::info!(removed = %v, "retention pass"),
                    Err(e) => tracing::warn!(error = %e, "retention pass failed"),
                }
            }
        });
    }
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                if !market_lease.held() {
                    continue;
                }
                match crate::http::economy::settle_market(&st).await {
                    Ok((l, t)) if l + t > 0 => {
                        tracing::info!(listings = l, trades = t, "market settled")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "market settle failed"),
                }
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(state.cfg.http.listen).await?;
    info!(addr = %state.cfg.http.listen, "http listening");
    if let Some(listen) = state.cfg.quic.listen {
        crate::realtime::quic::start(state.clone(), listen).await?;
    }
    let draining = state.clone();
    axum::serve(
        listener,
        http::router(state.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown(draining.shutdown.clone()).await;
        drain(&draining).await;
    })
    .await?;
    if let Some(c) = &state.cluster {
        c.stop().await;
    }
    info!("stopped");
    Ok(())
}

/// Nodes currently considered dead. A failover round is owed when this runs, so a transient
/// database error (pool contention) is retried a few times instead of losing the round.
async fn dead_nodes(cluster: &musubi_cluster::Cluster) -> Option<Vec<uuid::Uuid>> {
    for attempt in 0..3 {
        match cluster.nodes().await {
            Ok(nodes) => {
                return Some(nodes.iter().filter(|n| !n.alive).map(|n| n.id.0).collect());
            }
            Err(e) => {
                tracing::warn!(error = %e, attempt, "node list failed");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
    None
}

/// A failover round is owed; give leadership up to `DEAD_AFTER` to settle on us (a draining
/// leader hands over while its leave is in flight) before giving the round up.
async fn await_lease(lease: &musubi_cluster::Lease) -> bool {
    let deadline = std::time::Instant::now() + musubi_cluster::DEAD_AFTER;
    while !lease.held() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    true
}

/// Next failover round: the periodic tick, or immediately when a node announces it is leaving.
async fn wait_failover_round(
    tick: &mut tokio::time::Interval,
    leaves: &mut tokio::sync::broadcast::Receiver<musubi_cluster::BusMessage>,
) {
    loop {
        tokio::select! {
            _ = tick.tick() => return,
            m = leaves.recv() => match m {
                Ok(m) if m.topic == musubi_cluster::LEAVE_TOPIC => {
                    // let the leaver finish disowning its matches/worlds
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    return;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => {
                    tick.tick().await;
                    return;
                }
            },
        }
    }
}

/// SIGTERM drain, bounded by `node.drain_timeout_secs`: stop advertising readiness, snapshot and
/// disown our matches/worlds so the failover leader re-homes them, leave the cluster (peers mark
/// us dead at once, leases released), then close every realtime session so clients reconnect to
/// another node. axum finishes in-flight HTTP after this returns.
async fn drain(state: &AppState) {
    state
        .draining
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let budget = std::time::Duration::from_secs(state.cfg.node.drain_timeout_secs.max(1));
    let work = async {
        match state.worlds.release_all().await {
            Ok(w) if !w.is_empty() => info!(worlds = ?w, "drain: worlds handed off"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "drain: world hand-off failed"),
        }
        match state.matches.release_all().await {
            Ok(n) if n > 0 => info!(matches = n, "drain: matches handed off"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "drain: match hand-off failed"),
        }
        if let Some(c) = &state.cluster {
            c.leave().await;
        }
        let closed = state.hub.close_all();
        info!(sessions = closed, "drain: sessions closed");
    };
    if tokio::time::timeout(budget, work).await.is_err() {
        tracing::warn!(?budget, "drain: timed out, exiting anyway");
    }
}

async fn shutdown(requested: Arc<tokio::sync::Notify>) {
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    let api = requested.notified();
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {}, _ = api => {} }
    info!("shutdown signal received");
}

fn iap_client<T>(
    name: &str,
    pem_path: &str,
    build: impl FnOnce(&str) -> musubi_core::Result<Arc<T>>,
) -> Option<Arc<T>> {
    if pem_path.is_empty() {
        return None;
    }
    match std::fs::read_to_string(pem_path)
        .map_err(musubi_core::Error::internal)
        .and_then(|pem| build(&pem))
    {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!(provider = name, error = %e, "iap provider disabled");
            None
        }
    }
}

/// `[fleet.kubernetes]` → client config; falls back to the in-cluster service account.
fn kube_config(
    k: &musubi_core::config::KubernetesConfig,
) -> Option<musubi_fleet::kubernetes::KubeConfig> {
    let mut cfg = match &k.url {
        Some(url) => musubi_fleet::kubernetes::KubeConfig {
            url: url.clone(),
            token: None,
            namespace: "default".into(),
            ca_cert_pem: None,
            insecure: k.insecure,
        },
        None => musubi_fleet::kubernetes::KubeConfig::in_cluster()?,
    };
    if let Some(t) = &k.token {
        cfg.token = Some(t.clone());
    } else if let Some(p) = &k.token_path {
        cfg.token = std::fs::read_to_string(p)
            .ok()
            .map(|t| t.trim().to_string());
    }
    if let Some(p) = &k.ca_cert_path {
        cfg.ca_cert_pem = std::fs::read(p).ok();
    }
    if let Some(ns) = &k.namespace {
        cfg.namespace = ns.clone();
    }
    cfg.insecure |= k.insecure;
    Some(cfg)
}
