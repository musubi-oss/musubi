use std::sync::Arc;

use musubi_account::AccountService;
use musubi_auth::AuthService;
use musubi_chat::ChatService;
use musubi_cluster::Cluster;
use musubi_core::config::Config;
use musubi_fleet::FleetService;
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
    pub matchmaker: Arc<Matchmaker>,
    pub parties: Arc<PartyRegistry>,
    pub relayed: Arc<RelayedRegistry>,
    pub matches: Arc<MatchRegistry>,
    pub runtime: Arc<Runtime>,
    pub cluster: Option<Arc<Cluster>>,
    pub fleets: Arc<FleetService>,
    pub fanout: Fanout,
    pub hub: Arc<Hub>,
    pub started_at: std::time::Instant,
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
    let cluster = Arc::new(Cluster::new(
        db.clone(),
        &cfg.node.name,
        &cfg.node.region,
        cfg.node
            .roles
            .iter()
            .map(|r| format!("{r:?}").to_lowercase())
            .collect(),
        &cfg.http.listen.to_string(),
    ));
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
        .spawn_reconciler(std::time::Duration::from_secs(5));
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
    // Failover: adopt matches whose node stopped heartbeating.
    {
        let (cluster, matches, hub) = (cluster.clone(), matches.clone(), hub.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(musubi_cluster::DEAD_AFTER);
            loop {
                tick.tick().await;
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
        matchmaker,
        parties,
        relayed,
        matches,
        runtime,
        cluster: Some(cluster),
        fleets,
        fanout,
        hub,
        started_at: std::time::Instant::now(),
    };
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
