use std::sync::Arc;

use musubi_account::AccountService;
use musubi_auth::AuthService;
use musubi_chat::ChatService;
use musubi_cluster::Cluster;
use musubi_core::config::Config;
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
    let parties = Arc::new(PartyRegistry::new());
    let relayed = Arc::new(RelayedRegistry::new());
    let matches = Arc::new(MatchRegistry::new());
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
    crate::realtime::matchmaking::spawn_ticker(
        hub.clone(),
        parties.clone(),
        relayed.clone(),
        matchmaker.clone(),
        std::time::Duration::from_secs(1),
    );
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
    let fanout = Fanout::new(hub.clone(), Some(cluster.clone()));
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
        fanout,
        hub,
        started_at: std::time::Instant::now(),
    };
    let listener = tokio::net::TcpListener::bind(state.cfg.http.listen).await?;
    info!(addr = %state.cfg.http.listen, "http listening");
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
