use std::sync::Arc;

use musubi_account::AccountService;
use musubi_auth::AuthService;
use musubi_chat::ChatService;
use musubi_core::config::Config;
use musubi_leaderboard::LeaderboardService;
use musubi_social::{GroupService, NotificationService, SocialService};

use crate::notify::Notifier;
use musubi_storage::StorageService;
use sqlx::PgPool;
use tokio::signal;
use tracing::info;

use crate::realtime::Hub;
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
    let hub = Arc::new(Hub::new());
    let notify = Notifier::new(NotificationService::new(db.clone()), hub.clone());
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
