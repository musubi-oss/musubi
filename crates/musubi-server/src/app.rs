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

/// Everything a request handler can reach. One `Arc` for the whole state: every middleware layer
/// and `State` extractor clones the handle, so a flat struct of ~50 fields cost ~50 refcount
/// round-trips per layer per request.
#[derive(Clone)]
pub struct AppState(pub Arc<AppStateInner>);

impl std::ops::Deref for AppState {
    type Target = AppStateInner;
    fn deref(&self) -> &AppStateInner {
        &self.0
    }
}

pub struct AppStateInner {
    pub audit: crate::audit::Sink,
    pub cfg: Arc<Config>,
    pub db: PgPool,
    pub auth: AuthService,
    pub storage: StorageService,
    pub accounts: AccountService,
    pub social: SocialService,
    pub groups: GroupService,
    pub notify: Notifier,
    pub lobbies: crate::lobby::LobbyService,
    pub chat: ChatService,
    pub leaderboards: LeaderboardService,
    pub tournaments: TournamentService,
    pub ratings: musubi_leaderboard::RatingService,
    pub economy: musubi_economy::EconomyService,
    pub matchmaker: Arc<Matchmaker>,
    pub parties: Arc<PartyRegistry>,
    pub relayed: Arc<RelayedRegistry>,
    pub matches: Arc<MatchRegistry>,
    pub worlds: Arc<crate::realtime::WorldRegistry>,
    pub liveops: Option<Arc<ZanshinClient>>,
    pub voice: Option<Arc<AurixClient>>,
    /// WebRTC signalling + data channels (`[webrtc] listen`).
    pub rtc: Option<Arc<musubi_transport::webrtc::RtcServer>>,
    pub apple_iap: Option<Arc<musubi_integrations::AppleStoreClient>>,
    pub google_iap: Option<Arc<musubi_integrations::GooglePlayClient>>,
    pub steam_iap: Option<Arc<musubi_integrations::SteamClient>>,
    pub runtime: Arc<Runtime>,
    pub schedules: Arc<crate::schedules::Schedules>,
    pub cluster: Option<Arc<Cluster>>,
    pub fleets: Arc<FleetService>,
    pub fanout: Fanout,
    pub hub: Arc<Hub>,
    pub started_at: std::time::Instant,
    pub metrics: metrics_exporter_prometheus::PrometheusHandle,
    pub limits: Arc<crate::limits::RateLimiter>,
    pub events: Arc<crate::events::Events>,
    pub banned: Arc<crate::bans::Bans>,
    pub anticheat: Arc<crate::anticheat::AntiCheat>,
    pub dev: Arc<crate::dev::DevTools>,
    pub liveops_cache: crate::runtime_host::LiveopsCache,
    pub keys: Arc<crate::keys::ServerKeys>,
    pub logins: Arc<crate::logins::Social>,
    /// UGC blobs in object storage (`[ugc.s3]`); `None` keeps them in Postgres.
    pub blobs: Option<Arc<crate::blobstore::S3Store>>,
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
    serve(cfg).await
}

/// Everything one project needs: services, registries, cluster membership and background loops.
/// Called once for the base configuration and once per `[[projects]]` entry.
pub async fn build(cfg: Config) -> anyhow::Result<AppState> {
    let db = db::connect(&cfg.database).await?;
    if cfg.database.run_migrations {
        db::migrate(&db).await?;
    }
    let auth = AuthService::new(db.clone(), &cfg.session)?;
    let storage = StorageService::new(db.clone()).with_limits(
        cfg.storage.max_objects_per_user,
        cfg.storage.max_bytes_per_user,
    );
    let accounts = AccountService::new(db.clone());
    let social = SocialService::new(db.clone());
    let groups = GroupService::new(db.clone()).with_level_xp(cfg.social.group_level_xp.clone());
    let chat = ChatService::new(db.clone());
    let leaderboards = LeaderboardService::new(db.clone());
    let economy = musubi_economy::EconomyService::new(db.clone());
    let tournaments = TournamentService::new(db.clone());
    let ratings = musubi_leaderboard::RatingService::new(db.clone());
    let hub = Arc::new(Hub::new());
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
    auth.set_captcha_verifier(Some(captcha_verifier(runtime.clone(), &cfg.session)));
    let queues: Vec<musubi_matchmaker::QueueConfig> = cfg
        .matchmaker
        .queues
        .iter()
        .cloned()
        .map(serde_json::from_value)
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("[[matchmaker.queues]]: {e}"))?;
    let matchmaker = Arc::new(
        Matchmaker::new(std::time::Duration::from_secs(cfg.matchmaker.min_wait_secs))
            .with_backfill(Some(std::time::Duration::from_secs(
                cfg.matchmaker.backfill_secs,
            )))
            .with_ttl(std::time::Duration::from_secs(
                cfg.matchmaker.ticket_ttl_secs,
            ))
            .with_ready_check(
                std::time::Duration::from_secs(cfg.matchmaker.ready_check_secs),
                std::time::Duration::from_secs(cfg.matchmaker.dodge_penalty_secs),
            )
            .with_scorer(Arc::new(crate::realtime::matchmaking::RuntimeScorer(
                runtime.clone(),
            )))
            .with_queues(queues)?,
    );
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
    )
    .with_edge(cfg.edge.public_addr.or(cfg.edge.listen));
    if let Some(bin) = &cfg.fleet.firecracker.binary {
        match firecracker_settings(bin, &cfg.fleet.firecracker)
            .and_then(musubi_fleet::firecracker::FirecrackerProvider::new)
        {
            Ok(p) => {
                tracing::info!(provider = "firecracker", binary = %bin, "fleet provider registered");
                fleets = fleets.with_provider("firecracker", Arc::new(p));
            }
            Err(e) => {
                tracing::warn!(provider = "firecracker", error = %e, "fleet provider disabled")
            }
        }
    }
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
    if !cfg.fleet.edgegap.token.is_empty() {
        tracing::info!(provider = "edgegap", url = %cfg.fleet.edgegap.url, "fleet provider registered");
        fleets = fleets.with_provider(
            "edgegap",
            Arc::new(musubi_fleet::edgegap::EdgegapProvider::new(
                cfg.fleet.edgegap.clone(),
            )),
        );
    }
    if !cfg.fleet.gamelift.access_key.is_empty() {
        tracing::info!(provider = "gamelift", region = %cfg.fleet.gamelift.region, "fleet provider registered");
        fleets = fleets.with_provider(
            "gamelift",
            Arc::new(musubi_fleet::gamelift::GameLiftProvider::new(
                cfg.fleet.gamelift.clone(),
            )),
        );
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
            .with_epoch(cluster.epoch())
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
            let mut members = cluster.member_events();
            loop {
                wait_failover_round(&mut tick, &mut leaves, &mut members, false).await;
                await_dead_agreement(&cluster).await;
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
    fanout.set_social(musubi_social::SocialService::new(db.clone()));
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
    if cfg.session.login_queue_capacity > 0 {
        let f = fanout.clone();
        auth.set_login_queue(Some(musubi_auth::LoginQueue {
            capacity: cfg.session.login_queue_capacity,
            admit_per_sec: cfg.session.login_queue_admit_per_sec,
            ttl_secs: cfg.session.login_queue_ttl_secs.max(5),
            online: std::sync::Arc::new(move || f.online_accounts()),
        }));
    }
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
    match worlds.recover_hosted(&fanout).await {
        Ok(cells) if !cells.is_empty() => {
            tracing::info!(?cells, "hosted cells handed back on boot")
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "hosted cells not restored on boot"),
    }
    // World failover: adopt worlds whose home node died (one adopter at a time).
    {
        let (cluster, worlds, fanout) = (cluster.clone(), worlds.clone(), fanout.clone());
        let lease = cluster.lease("world-failover", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(musubi_cluster::DEAD_AFTER);
            let mut leaves = cluster.subscribe();
            let mut members = cluster.member_events();
            let mut owed = false;
            let mut soon_for: Vec<uuid::Uuid> = Vec::new();
            let mut soon_left: u32 = 0;
            loop {
                wait_failover_round(&mut tick, &mut leaves, &mut members, owed).await;
                owed = false;
                // SWIM's verdict comes within seconds, the heartbeat check a few seconds later:
                // poll until they agree instead of waiting for the next tick
                await_dead_agreement(&cluster).await;
                // cells of our worlds hosted on a dead node come back to us (no lease: each home
                // node handles its own worlds)
                // every stage is bounded and named: a hang here froze world failover cluster-wide
                // once without a single log line to point at it
                const STAGE: std::time::Duration = std::time::Duration::from_secs(30);
                if tokio::time::timeout(STAGE, worlds.refresh_homes())
                    .await
                    .is_err()
                {
                    tracing::warn!("world failover: refresh_homes timed out");
                }
                match tokio::time::timeout(STAGE, worlds.reconcile_handed_back()).await {
                    Err(_) => tracing::warn!("world failover: reconcile_handed_back timed out"),
                    Ok(r) => match r {
                        Ok(n) if n > 0 => {
                            tracing::info!(entities = n, "handed-back cells reconciled")
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "hand-back reconciliation failed"),
                    },
                }
                if let Some(dead) = dead_nodes(&cluster).await {
                    tracing::info!(?dead, "world failover round");
                    match tokio::time::timeout(STAGE, worlds.recover_cells(&dead, &fanout)).await {
                        Ok(Ok(cells)) if !cells.is_empty() => {
                            tracing::info!(?cells, ?dead, "re-homed cells of dead hosts")
                        }
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => tracing::warn!(error = %e, "cell failover failed"),
                        Err(_) => tracing::warn!(?dead, "world failover: recover_cells timed out"),
                    }
                }
                if !await_lease(&lease).await {
                    continue;
                }
                // every round, dead nodes or not: worlds created for this region elsewhere wait
                // as unowned rows for a node here to adopt them
                let dead = dead_nodes(&cluster).await.unwrap_or_default();
                match tokio::time::timeout(STAGE, worlds.recover(&dead)).await {
                    Ok(Ok(names)) if !names.is_empty() => {
                        tracing::info!(
                            ?names,
                            ?dead,
                            "adopted worlds (dead homes or pending placement)"
                        );
                        owed = false;
                        // a world adopted from a dead home may have had cells on that same
                        // node: re-home them now, not a whole round later (the stand saw the
                        // mesh settle 15 s after the world for exactly this)
                        if !dead.is_empty() {
                            match tokio::time::timeout(STAGE, worlds.recover_cells(&dead, &fanout))
                                .await
                            {
                                Ok(Ok(cells)) if !cells.is_empty() => {
                                    tracing::info!(
                                        ?cells,
                                        ?dead,
                                        "re-homed cells of adopted worlds"
                                    )
                                }
                                Ok(Ok(_)) => {}
                                Ok(Err(e)) => {
                                    tracing::warn!(error = %e, "cell failover after adoption failed")
                                }
                                Err(_) => {
                                    tracing::warn!(?dead, "world failover: recover_cells timed out")
                                }
                            }
                        }
                    }
                    Ok(Ok(_)) if dead.is_empty() => {}
                    Ok(Ok(_)) => {
                        // the dead node's leases may not have run out yet: retry soon a few
                        // times, then fall back to the period — a node that owned nothing stays
                        // dead for a long while and must not keep the round spinning every 2 s
                        if dead != soon_for {
                            soon_for = dead.clone();
                            soon_left = 6;
                        }
                        owed = soon_left > 0;
                        soon_left = soon_left.saturating_sub(1);
                        tracing::info!(?dead, "world failover round: nothing to adopt")
                    }
                    Ok(Err(e)) => tracing::warn!(error = %e, "world failover failed"),
                    Err(_) => tracing::warn!(?dead, "world failover: recover timed out"),
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
    match storage.load_schemas().await {
        Ok(n) if n > 0 => tracing::info!(collections = n, "storage schemas loaded"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "storage schemas not loaded"),
    }
    wire_storage_live(&storage, &fanout);
    let notify = Notifier::new(NotificationService::new(db.clone()), fanout.clone()).with_push(
        Arc::new(crate::push::Push::new(cfg.push.clone(), db.clone())),
    );
    let limits = Arc::new(crate::limits::RateLimiter::new(&cfg.limits));
    let logins = Arc::new(crate::logins::Social::new(cfg.social.clone()));
    let schedules = Arc::new(crate::schedules::Schedules::from_config(&cfg.runtime));
    for p in &schedules.problems {
        tracing::warn!(problem = %p, "runtime schedule ignored");
    }
    let rtc = match cfg.webrtc.listen {
        Some(listen) => {
            let s = musubi_transport::webrtc::RtcServer::bind(listen, cfg.webrtc.public_addr)
                .await
                .map_err(|e| anyhow::anyhow!("webrtc: {e}"))?;
            tracing::info!(addr = %s.local_addr(), public = %s.public_addr(), "webrtc listening");
            Some(s)
        }
        None => None,
    };
    let mailer = (!cfg.email.host.is_empty())
        .then(|| Arc::new(musubi_integrations::Mailer::new(cfg.email.clone())));
    let blobs = crate::blobstore::S3Store::new(&cfg.ugc.s3).map(Arc::new);
    if let Some(b) = &blobs {
        tracing::info!(bucket = b.bucket(), "ugc blobs in object storage");
    }
    let state = AppState(Arc::new(AppStateInner {
        blobs,
        audit: crate::audit::Sink::new(&db),
        cfg: Arc::new(cfg),
        events: {
            let e = crate::events::Events::new(db.clone());
            if let Some(l) = &liveops {
                e.set_liveops(l.clone());
            }
            if let Some(m) = &mailer {
                e.set_mailer(m.clone());
            }
            Arc::new(e)
        },
        banned: Arc::new(crate::bans::Bans::default()),
        anticheat: Arc::new(crate::anticheat::AntiCheat::new()),
        dev: Arc::new(crate::dev::DevTools::new()),
        liveops_cache: Default::default(),
        keys: Arc::new(crate::keys::ServerKeys::default()),
        logins,
        draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        lobbies: crate::lobby::LobbyService::new(db.clone(), notify.clone()),
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
        ratings,
        economy,
        matchmaker,
        parties,
        relayed,
        matches,
        worlds,
        liveops,
        voice,
        rtc,
        apple_iap,
        google_iap,
        steam_iap,
        runtime,
        schedules,
        cluster: Some(cluster),
        fleets,
        fanout,
        hub,
        started_at: std::time::Instant::now(),
        metrics: crate::metrics::install(),
        limits,
    }));
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
    if let Err(e) = state.limits.load_overrides(&state.db).await {
        tracing::warn!(error = %e, "account limits not loaded");
    }
    match state.keys.load(&state.db).await {
        Ok(n) => info!(keys = n, "server keys loaded"),
        Err(e) => tracing::warn!(error = %e, "server keys load failed"),
    }
    if let Some(c) = &state.cluster {
        state.keys.start(c, state.db.clone());
        state.limits.start(c, state.db.clone());
    }
    state.anticheat.bind(&state);
    match state.banned.load(&state.db).await {
        Ok(n) => tracing::info!(banned = n, "ban cache loaded"),
        Err(e) => tracing::warn!(error = %e, "ban cache load failed"),
    }
    if let Some(c) = &state.cluster {
        state.banned.start(c);
        crate::runtime_host::start_liveops_sync(&state, c);
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
                match crate::http::admin::purge_deleted_accounts(&st).await {
                    Ok(k) if k > 0 => tracing::info!(accounts = k, "deleted accounts purged"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "account purge failed"),
                }
            }
        });
    }
    if let Some(c) = state.cluster.clone() {
        let st = state.clone();
        let lease = c.lease("push_schedule", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                if !lease.held() {
                    continue;
                }
                match crate::http::push::dispatch_due(&st).await {
                    Ok(n) if n > 0 => tracing::info!(pushed = n, "scheduled push pass"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "scheduled push pass failed"),
                }
            }
        });
    }
    if let Some(c) = state.cluster.clone() {
        let st = state.clone();
        let lease = c.lease("lobbies", musubi_cluster::DEAD_AFTER);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                tick.tick().await;
                if !lease.held() {
                    continue;
                }
                match st.lobbies.sweep(crate::lobby::TTL).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(removed = n, "stale lobbies swept"),
                    Err(e) => tracing::warn!(error = %e, "lobby sweep failed"),
                }
            }
        });
    }
    if !state.schedules.is_empty() {
        let st = state.clone();
        let lease = st
            .cluster
            .clone()
            .map(|c| c.lease("schedules", musubi_cluster::DEAD_AFTER));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tick.tick().await;
                if let Some(l) = &lease {
                    if !l.held() {
                        // another node runs them; keep our clock in step so we do not replay
                        // every fire we missed the moment the lease moves here
                        let _ = st.schedules.due();
                        continue;
                    }
                }
                for (name, at) in st.schedules.due() {
                    match st.schedules.run_one(&st.runtime, &name, Some(at)).await {
                        Ok(_) => tracing::info!(schedule = %name, "schedule ran"),
                        Err(e) => {
                            tracing::warn!(schedule = %name, error = %e, "schedule failed");
                            st.events
                                .emit(
                                    "runtime.schedule_failed",
                                    None,
                                    serde_json::json!({ "name": name, "scheduled_for": at, "error": e.to_string() }),
                                )
                                .await;
                        }
                    }
                }
            }
        });
    }
    {
        let st = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            let mut n: u64 = 0;
            loop {
                tick.tick().await;
                if !market_lease.held() {
                    continue;
                }
                n += 1;
                // finished windows: fire `hook_tournament_end` / `hook_leaderboard_reset` once each
                if n % 12 == 0 {
                    match st.leaderboards.unended_windows().await {
                        Ok(wins) => {
                            for (id, expiry, is_t) in wins {
                                if !st
                                    .leaderboards
                                    .claim_end(&id, expiry)
                                    .await
                                    .unwrap_or(false)
                                {
                                    continue;
                                }
                                let records = match st
                                    .leaderboards
                                    .records(&id, &[], 100, None, Some(expiry), None)
                                    .await
                                {
                                    Ok(l) => l.records,
                                    Err(e) => {
                                        tracing::warn!(error = %e, id, "end-of-window records");
                                        vec![]
                                    }
                                };
                                let payload = serde_json::json!({
                                    "id": id, "expiry": expiry, "size": records.len(),
                                    "records": records,
                                });
                                let ctx = musubi_runtime::CallContext {
                                    server: true,
                                    ..Default::default()
                                };
                                let event = if is_t {
                                    "tournament_end"
                                } else {
                                    "leaderboard_reset"
                                };
                                st.runtime.event(event, ctx, payload).await;
                                if is_t {
                                    if let Err(e) =
                                        crate::http::tournaments::pay_prizes(&st, &id, expiry).await
                                    {
                                        tracing::warn!(error = %e, id, "tournament prizes");
                                    }
                                }
                            }
                        }
                        Err(e) => tracing::warn!(error = %e, "end-of-window scan failed"),
                    }
                }
                // subscriptions past their period: every minute, same leader as the market sweep
                if n % 12 == 0 {
                    match crate::http::economy::sweep_subscriptions(&st).await {
                        Ok(k) if k > 0 => tracing::info!(subscriptions = k, "subscription sweep"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "subscription sweep failed"),
                    }
                }
                // expired bans: every minute, same leader as the market sweep
                if n % 12 == 0 {
                    match crate::http::admin::lift_expired_bans(&st).await {
                        Ok(k) if k > 0 => tracing::info!(accounts = k, "expired bans lifted"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "ban expiry sweep failed"),
                    }
                }
                // league rollovers: every minute, same leader as the market sweep
                if n % 12 == 0 {
                    match st.leaderboards.rollover_due().await {
                        Ok(r) if !r.is_empty() => {
                            tracing::info!(boards = r.len(), "league rollovers applied")
                        }
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "league rollover failed"),
                    }
                }
                // rating decay: every ten minutes, same leader as the market sweep
                if n % 120 == 0 {
                    match st.ratings.decay_all().await {
                        Ok(k) if k > 0 => tracing::info!(ratings = k, "rating decay applied"),
                        Ok(_) => {}
                        Err(e) => tracing::warn!(error = %e, "rating decay failed"),
                    }
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
    Ok(state)
}

/// The base project plus every `[[projects]]` entry behind one HTTP listener; each project also
/// gets its own QUIC / WebTransport / UDP / edge listeners when it configures distinct addresses.
async fn serve(cfg: Config) -> anyhow::Result<()> {
    let projects = cfg.projects.clone();
    let main = build(cfg).await?;
    let mut extra: Vec<(String, Vec<String>, AppState)> = Vec::new();
    for p in &projects {
        let pc = main
            .cfg
            .for_project(p)
            .map_err(|e| anyhow::anyhow!("[[projects]] {}: {e}", p.name))?;
        if extra.iter().any(|(n, _, _)| *n == p.name) {
            anyhow::bail!("[[projects]] {}: duplicate name", p.name);
        }
        let st = build(pc).await?;
        info!(project = %p.name, hosts = ?p.hosts, "project started");
        // draining any project drains the node
        let (from, to) = (st.shutdown.clone(), main.shutdown.clone());
        tokio::spawn(async move {
            from.notified().await;
            to.notify_one();
        });
        extra.push((p.name.clone(), p.hosts.clone(), st));
    }
    let state = &main;
    let socket = match state.cfg.http.listen {
        std::net::SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4()?,
        std::net::SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6()?,
    };
    socket.set_reuseaddr(true)?;
    socket.bind(state.cfg.http.listen)?;
    // Thousands of clients reconnecting at once (region failover, match start) must not be
    // reset by the default accept backlog; the kernel caps this at somaxconn.
    let listener = socket.listen(4096)?;
    info!(addr = %state.cfg.http.listen, "http listening");
    start_listeners(state, None).await?;
    for (_, _, st) in &extra {
        start_listeners(st, Some(&main.cfg)).await?;
    }
    let router = if extra.is_empty() {
        http::router(main.clone())
    } else {
        http::multi_router(
            (main.cfg.node.project.clone(), http::router(main.clone())),
            extra
                .iter()
                .map(|(n, h, s)| (n.clone(), h.clone(), http::router(s.clone())))
                .collect(),
        )
    };
    let draining = main.clone();
    let others: Vec<AppState> = extra.iter().map(|(_, _, s)| s.clone()).collect();
    // many small realtime frames per connection: Nagle + the peer's delayed ACK would hold
    // every second frame for up to 40 ms
    let listener = {
        use axum::serve::ListenerExt;
        listener.tap_io(|tcp| {
            let _ = tcp.set_nodelay(true);
        })
    };
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown(draining.shutdown.clone()).await;
        drain(&draining).await;
        for s in &others {
            drain(s).await;
        }
    })
    .await?;
    for s in std::iter::once(&main).chain(extra.iter().map(|(_, _, s)| s)) {
        if let Some(c) = &s.cluster {
            c.stop().await;
        }
    }
    info!("stopped");
    Ok(())
}

/// Datagram/stream listeners of one project. An extra project only gets the ones whose address
/// differs from the base configuration's — a shared address would just be the base project's.
async fn start_listeners(state: &AppState, base: Option<&Config>) -> anyhow::Result<()> {
    let own =
        |what: &str, mine: Option<std::net::SocketAddr>, theirs: Option<std::net::SocketAddr>| {
            match (mine, base) {
                (Some(a), Some(_)) if theirs == Some(a) => {
                    tracing::warn!(project = %state.cfg.node.project, listener = what, addr = %a,
                    "same address as the base project: set a distinct one to serve this project");
                    None
                }
                (a, _) => a,
            }
        };
    if let Some(listen) = own(
        "quic",
        state.cfg.quic.listen,
        base.and_then(|b| b.quic.listen),
    ) {
        crate::realtime::quic::start(state.clone(), listen).await?;
    }
    if let Some(listen) = own(
        "webtransport",
        state.cfg.webtransport.listen,
        base.and_then(|b| b.webtransport.listen),
    ) {
        crate::realtime::webtransport::start(state.clone(), listen).await?;
    }
    if let Some(listen) = own("udp", state.cfg.udp.listen, base.and_then(|b| b.udp.listen)) {
        let secret = match &state.cfg.udp.secret {
            Some(hex) => {
                let bytes = (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(hex.get(i..i + 2).unwrap_or("zz"), 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .ok()
                    .filter(|b| b.len() == 32)
                    .ok_or_else(|| anyhow::anyhow!("udp.secret must be 32 bytes of hex"))?;
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                Some(key)
            }
            None => None,
        };
        crate::realtime::udp::start(state.clone(), listen, secret).await?;
    }
    if let Some(listen) = own(
        "edge",
        state.cfg.edge.listen,
        base.and_then(|b| b.edge.listen),
    ) {
        let proxy = musubi_edge::UdpProxy::start(
            listen,
            Arc::new(crate::edge::FleetRouter(state.fleets.clone())),
            std::time::Duration::from_secs(state.cfg.edge.idle_secs.max(1)),
        )
        .await?;
        tracing::info!(listen = %proxy.local_addr(), public = ?state.fleets.edge(), "udp edge proxy listening");
        std::mem::forget(proxy);
    }
    if let Some(listen) = own(
        "regions.echo",
        state.cfg.regions.echo_listen,
        base.and_then(|b| b.regions.echo_listen),
    ) {
        let local = crate::regions::spawn_echo(listen).await?;
        tracing::info!(listen = %local, "region latency echo listening");
    }
    Ok(())
}

/// Nodes currently considered dead. A failover round is owed when this runs, so a transient
/// database error (pool contention) is retried a few times instead of losing the round.
/// SWIM's verdict comes within seconds, the database heartbeat fence a few seconds later:
/// poll until they agree instead of waiting for the next round. Only nodes whose heartbeat has
/// stopped are waited for: a node SWIM misjudges (restarted a moment ago, flapping) keeps
/// heartbeating, and waiting on it would stall every round by the full poll budget — the stand
/// re-homed a dead host's cells 10 s late exactly so.
async fn await_dead_agreement(cluster: &musubi_cluster::Cluster) {
    let mut seen: std::collections::HashMap<uuid::Uuid, time::OffsetDateTime> =
        std::collections::HashMap::new();
    for _ in 0..24 {
        let Ok(nodes) = cluster.nodes().await else {
            return;
        };
        let now = time::OffsetDateTime::now_utc();
        let mut pending = false;
        for n in nodes.iter().filter(|n| !n.alive) {
            if now - n.heartbeat_at > musubi_cluster::HEARTBEAT + time::Duration::seconds(2) {
                continue;
            }
            match seen.get(&n.id.0) {
                // its heartbeat moved on since we first looked: alive, SWIM lags behind
                Some(prev) if *prev != n.heartbeat_at => {}
                _ => {
                    seen.insert(n.id.0, n.heartbeat_at);
                    pending = true;
                }
            }
        }
        if !pending {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn dead_nodes(cluster: &musubi_cluster::Cluster) -> Option<Vec<uuid::Uuid>> {
    for attempt in 0..3 {
        match cluster.nodes().await {
            Ok(nodes) => {
                // SWIM's verdict alone would also fire for a node that restarted with the same
                // identity a second ago: it must have stopped heartbeating to the database too
                let now = time::OffsetDateTime::now_utc();
                return Some(
                    nodes
                        .iter()
                        .filter(|n| {
                            !n.alive
                                && now - n.heartbeat_at
                                    > musubi_cluster::HEARTBEAT + time::Duration::seconds(2)
                        })
                        .map(|n| n.id.0)
                        .collect(),
                );
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
/// A failover round runs on the periodic tick, right after a peer announces it is leaving, or as
/// soon as SWIM declares a peer dead (so a crash is handled at detection time, not at the next tick).
async fn wait_failover_round(
    tick: &mut tokio::time::Interval,
    leaves: &mut tokio::sync::broadcast::Receiver<musubi_cluster::BusMessage>,
    members: &mut tokio::sync::broadcast::Receiver<musubi_cluster::MemberEvent>,
    soon: bool,
) {
    // a round that saw dead nodes but could adopt nothing yet (their leases still run) is
    // retried shortly instead of a whole period later
    const SOON: std::time::Duration = std::time::Duration::from_secs(2);
    let retry = tokio::time::sleep(SOON);
    tokio::pin!(retry);
    loop {
        tokio::select! {
            _ = tick.tick() => return,
            _ = &mut retry, if soon => return,
            m = members.recv() => match m {
                Ok(m) if m.state == "dead" => return,
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => {
                    tick.tick().await;
                    return;
                }
            },
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
        let cells = state.worlds.release_hosted(&state.fanout).await;
        if !cells.is_empty() {
            info!(?cells, "drain: hosted cells handed back");
        }
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

/// `[fleet.firecracker]` → provider settings; the network is optional but all-or-nothing.
fn firecracker_settings(
    bin: &str,
    c: &musubi_core::config::FirecrackerConfig,
) -> musubi_core::Result<musubi_fleet::firecracker::FirecrackerSettings> {
    use musubi_fleet::firecracker::{FirecrackerNetwork, FirecrackerSettings};
    let network = match (&c.bridge, &c.subnet, &c.gateway) {
        (None, None, None) => None,
        (Some(bridge), Some(subnet), Some(gateway)) => {
            let (net, prefix) = subnet.split_once('/').ok_or_else(|| {
                musubi_core::Error::InvalidArgument("fleet.firecracker.subnet must be CIDR".into())
            })?;
            let bad = |what: &str| {
                musubi_core::Error::InvalidArgument(format!("fleet.firecracker.{what} invalid"))
            };
            Some(FirecrackerNetwork {
                bridge: bridge.clone(),
                subnet: net.parse().map_err(|_| bad("subnet"))?,
                prefix: prefix.parse().map_err(|_| bad("subnet"))?,
                gateway: gateway.parse().map_err(|_| bad("gateway"))?,
            })
        }
        _ => {
            return Err(musubi_core::Error::InvalidArgument(
                "fleet.firecracker needs bridge, subnet and gateway together".into(),
            ))
        }
    };
    Ok(FirecrackerSettings {
        binary: bin.into(),
        kernel: c.kernel.clone().into(),
        rootfs: c.rootfs.clone().into(),
        vcpus: c.vcpus,
        mem_mib: c.mem_mib,
        run_dir: c.run_dir.clone().into(),
        boot_args: c.boot_args.clone(),
        network,
    })
}

/// Captcha check behind the email-login gate: a `hook_auth_captcha` module (`{token, ip}` →
/// `{ok}` or a bare bool) wins; otherwise the configured siteverify endpoint (Turnstile, hCaptcha,
/// reCAPTCHA all take `secret` + `response` [+ `remoteip`] and answer `{success}`). With neither
/// the gate logs and lets the attempt through — lockout still applies.
pub fn captcha_verifier(
    runtime: Arc<Runtime>,
    cfg: &musubi_core::config::SessionConfig,
) -> musubi_auth::CaptchaVerifier {
    let provider = match (&cfg.captcha_verify_url, &cfg.captcha_secret) {
        (Some(u), Some(s)) if !u.is_empty() && !s.is_empty() => Some((u.clone(), s.clone())),
        _ => None,
    };
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("reqwest client");
    let configured = {
        let (runtime, has_provider) = (runtime.clone(), provider.is_some());
        Arc::new(move || has_provider || runtime.has_hook("hook_auth_captcha"))
    };
    let verify = Arc::new(move |token: String, ip: Option<String>| {
        let (runtime, provider, http) = (runtime.clone(), provider.clone(), http.clone());
        Box::pin(async move {
            if runtime.has_hook("hook_auth_captcha") {
                let payload = serde_json::to_vec(&serde_json::json!({ "token": token, "ip": ip }))
                    .unwrap_or_default();
                let out = runtime
                    .hook(
                        "hook_auth_captcha",
                        musubi_runtime::CallContext::default(),
                        payload,
                    )
                    .await
                    .unwrap_or(Ok(Vec::new()))?;
                let v: serde_json::Value = serde_json::from_slice(&out).unwrap_or_default();
                return Ok(v
                    .as_bool()
                    .unwrap_or_else(|| v["ok"].as_bool().unwrap_or(false)));
            }
            let Some((url, secret)) = provider else {
                tracing::warn!("captcha gate reached but no verifier is configured (hook_auth_captcha or captcha_verify_url)");
                return Ok(true);
            };
            let mut form = vec![("secret", secret), ("response", token)];
            if let Some(ip) = ip {
                form.push(("remoteip", ip));
            }
            let v: serde_json::Value = http
                .post(&url)
                .form(&form)
                .send()
                .await
                .map_err(musubi_core::Error::internal)?
                .json()
                .await
                .map_err(musubi_core::Error::internal)?;
            Ok(v["success"].as_bool().unwrap_or(false))
        }) as musubi_auth::CaptchaFuture
    });
    musubi_auth::CaptchaVerifier { configured, verify }
}

/// Turn committed storage writes into `storage_update` messages on the subscription channels
/// (one per object, one per owner collection) on every node.
pub fn wire_storage_live(storage: &musubi_storage::StorageService, fanout: &crate::fanout::Fanout) {
    let f = fanout.clone();
    storage.set_on_change(move |c: musubi_storage::Change| {
        let msg = musubi_proto::realtime::ServerMessage::StorageUpdate {
            collection: c.collection.clone(),
            key: c.key.clone(),
            owner_id: musubi_core::AccountId(c.owner_id),
            version: c.version,
            value: c.value,
            deleted: c.deleted,
        };
        f.channel(
            &crate::realtime::storage_channel(&c.collection, c.owner_id, Some(&c.key)),
            msg.clone(),
        );
        f.channel(
            &crate::realtime::storage_channel(&c.collection, c.owner_id, None),
            msg,
        );
    });
}
