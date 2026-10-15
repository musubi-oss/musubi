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
}

pub async fn run(cfg: Config) -> anyhow::Result<()> {
    info!(node = %cfg.node.name, roles = ?cfg.node.roles, "starting");
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
    let matchmaker = Arc::new(Matchmaker::default());
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
    let fleets = Arc::new(FleetService::new(
        db.clone(),
        cluster.node_id().0,
        &format!("http://{}", cfg.http.listen),
        &cfg.http.server_key,
    ));
    fleets
        .clone()
        .spawn_reconciler(std::time::Duration::from_secs(5), {
            let lease = cluster.lease("fleets", musubi_cluster::DEAD_AFTER);
            move || lease.held()
        });
    let relayed = Arc::new(RelayedRegistry::new(cluster.node_id().0));
    let matches = Arc::new(MatchRegistry::new(Some(db.clone()), cluster.node_id().0));
    let wasm_mods = matches.register_wasm(&runtime);
    if !wasm_mods.is_empty() {
        tracing::info!(modules = ?wasm_mods, "wasm match modules registered");
    }
    let resumed = matches.recover(hub.clone(), &[]).await?;
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
            loop {
                tick.tick().await;
                if !lease.held() {
                    continue;
                }
                let dead: Vec<uuid::Uuid> = match cluster.nodes().await {
                    Ok(nodes) => nodes.iter().filter(|n| !n.alive).map(|n| n.id.0).collect(),
                    Err(e) => {
                        tracing::warn!(error = %e, "node list failed");
                        continue;
                    }
                };
                if dead.is_empty() {
                    continue;
                }
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
    );
    fanout.set_fleets(fleets.clone());
    let worlds = Arc::new(crate::realtime::WorldRegistry::new(
        cluster.node_id().0,
        Some(db.clone()),
    ));
    worlds.set_cluster(cluster.clone());
    let adopted = worlds.recover(&[]).await?;
    if !adopted.is_empty() {
        tracing::info!(count = adopted.len(), "adopted orphaned worlds");
    }
    // World failover: adopt worlds whose home node died (one adopter at a time).
    {
        let (cluster, worlds) = (cluster.clone(), worlds.clone());
        let lease = cluster.lease("world-failover", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(musubi_cluster::DEAD_AFTER);
            loop {
                tick.tick().await;
                if !lease.held() {
                    continue;
                }
                let dead: Vec<uuid::Uuid> = match cluster.nodes().await {
                    Ok(nodes) => nodes.iter().filter(|n| !n.alive).map(|n| n.id.0).collect(),
                    Err(_) => continue,
                };
                if dead.is_empty() {
                    continue;
                }
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
    let notify = Notifier::new(NotificationService::new(db.clone()), fanout.clone());
    let state = AppState {
        cfg: Arc::new(cfg),
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
    };
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
    axum::serve(listener, http::router(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    info!("stopped");
    Ok(())
}

async fn shutdown() {
    let ctrl_c = async {
        signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = signal::unix::signal(signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
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
