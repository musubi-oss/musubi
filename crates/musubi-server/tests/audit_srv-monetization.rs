//! Audit regression tests for the monetization HTTP surface (unit `srv-monetization`):
//! ads, offers, referrals, subscriptions. Run with `MUSUBI_TEST_DATABASE_URL=postgres://...`;
//! skipped otherwise. The app bootstrap mirrors `tests/api.rs`.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use musubi_auth::AuthService;
use musubi_core::config::Config;
use musubi_server::app::{AppState, AppStateInner};
use musubi_storage::StorageService;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

async fn app() -> Option<Router> {
    app_with(|_| {}).await
}

async fn app_with(tweak: impl FnOnce(&mut Config)) -> Option<Router> {
    app_and_state_with(tweak).await.map(|(app, _)| app)
}

async fn app_and_state_with(tweak: impl FnOnce(&mut Config)) -> Option<(Router, AppState)> {
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    cfg.database.max_connections = 3;
    cfg.market.anti_snipe_secs = 0;
    // the chunk cache would land in ./data of the test's cwd; tests that want it set a temp dir
    cfg.builds.cache = false;
    tweak(&mut cfg);
    let analytics_cfg = cfg.analytics.clone();
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let auth = AuthService::new(db.clone(), &cfg.session).unwrap();
    let storage = StorageService::new(db.clone()).with_limits(
        cfg.storage.max_objects_per_user,
        cfg.storage.max_bytes_per_user,
    );
    let hub = Arc::new(musubi_server::realtime::Hub::new());
    let runtime = test_runtime();
    auth.set_captcha_verifier(Some(musubi_server::app::captcha_verifier(
        runtime.clone(),
        &cfg.session,
    )));
    let cluster = test_cluster(&db, &unique("grp"), &cfg).await;
    let matchmaker = Arc::new(musubi_matchmaker::Matchmaker::default());
    let mmf = Arc::new(musubi_server::realtime::matchmaking::MmfClient::new());
    let parties = Arc::new(
        musubi_server::realtime::PartyRegistry::new(cluster.as_ref().unwrap().node_id().0)
            .with_directory(db.clone()),
    );
    let relayed = Arc::new(musubi_server::realtime::RelayedRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
    ));
    let fanout = musubi_server::fanout::Fanout::new(
        hub.clone(),
        cluster.clone(),
        matchmaker.clone(),
        parties.clone(),
        relayed.clone(),
    );
    let blobs = musubi_server::blobstore::S3Store::new(&cfg.ugc.s3).map(Arc::new);
    let mut fleets = musubi_fleet::FleetService::new(
        db.clone(),
        cluster.as_ref().unwrap().node_id().0,
        "http://127.0.0.1:0",
        "defaultkey",
    );
    if let Some(b) = &blobs {
        fleets = fleets.with_log_archive(Arc::new(musubi_server::blobstore::FleetLogArchive(
            b.clone(),
        )));
    }
    fleets = fleets.with_scale_hook(Arc::new(musubi_server::runtime_host::RuntimeScaleHook(
        runtime.clone(),
    )));
    let fleets = Arc::new(fleets);
    fleets.spawn_log_writer();
    if !cfg.fleet.ssh.hosts.is_empty() || !cfg.fleet.capacity.pools.is_empty() {
        fleets
            .enable_ssh(db.clone(), &cfg.fleet.ssh, &cfg.fleet.capacity)
            .await
            .unwrap();
    }
    fanout.set_fleets(fleets.clone());
    let worlds = Arc::new(
        musubi_server::realtime::WorldRegistry::new(
            cluster.as_ref().unwrap().node_id().0,
            Some(db.clone()),
        )
        .with_modes(
            cfg.worlds.enabled,
            cfg.node.roles.contains(&musubi_core::config::Role::Sim),
        ),
    );
    fanout.set_worlds(worlds.clone());
    worlds.clone().spawn_ticker(fanout.clone());
    let logins = Arc::new(musubi_server::logins::Social::new(cfg.social.clone()));
    let push = Arc::new(musubi_server::push::Push::new(cfg.push.clone(), db.clone()));
    let schedules = Arc::new(musubi_server::schedules::Schedules::from_config(
        &cfg.runtime,
    ));
    let liveops = cfg.zanshin.url.as_deref().map(|u| {
        let c = musubi_integrations::ZanshinClient::new(u, &cfg.zanshin.server_key);
        c.clone()
            .spawn_flusher(std::time::Duration::from_millis(200));
        c
    });
    let compliance_cfg = cfg.compliance.clone();
    let geoip = musubi_server::geoip::GeoIp::new(&cfg.geoip, &cfg.node.data_dir).unwrap();
    let state = AppState(Arc::new(AppStateInner {
        anticheat: Arc::new(musubi_server::anticheat::AntiCheat::new()),
        dev: Arc::new(musubi_server::dev::DevTools::new()),
        audit: musubi_server::audit::Sink::new(&db),
        cfg: Arc::new(cfg),
        blobs,
        auth,
        idl: musubi_server::idl::IdlRegistry::new(db.clone(), storage.clone()),
        audiences: Default::default(),
        storage,
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        game_sessions: musubi_server::game_sessions::GameSessionService::new(db.clone()),
        async_mm: musubi_server::async_mm::AsyncMatchService::new(db.clone()),
        auth_providers: musubi_server::auth_providers::AuthProviderService::new(db.clone()),
        fleet_metrics: musubi_server::fleet_metrics::FleetMetricsService::new(db.clone()),
        lobbies: musubi_server::lobby::LobbyService::new(
            db.clone(),
            musubi_server::notify::Notifier::new(
                musubi_social::NotificationService::new(db.clone()),
                fanout.clone(),
            )
            .with_push(push.clone()),
        ),
        turns: musubi_server::turns::TurnService::new(
            db.clone(),
            musubi_server::notify::Notifier::new(
                musubi_social::NotificationService::new(db.clone()),
                fanout.clone(),
            )
            .with_push(push.clone()),
        ),
        notify: musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            fanout.clone(),
        )
        .with_push(push.clone()),
        chat: musubi_chat::ChatService::new(db.clone()),
        leaderboards: musubi_leaderboard::LeaderboardService::new(db.clone()),
        tournaments: musubi_leaderboard::TournamentService::new(db.clone()),
        ratings: musubi_leaderboard::RatingService::new(db.clone()),
        economy: musubi_economy::EconomyService::new(db.clone()),
        matchmaker,
        mmf: mmf.clone(),
        parties,
        relayed,
        matches: test_matches(&db, &runtime),
        worlds: worlds.clone(),
        liveops: liveops.clone(),
        voice: None,
        rtc: None,
        apple_iap: None,
        google_iap: None,
        huawei_iap: None,
        samsung_iap: None,
        console_iap: Default::default(),
        steam_iap: None,
        runtime: runtime.clone(),
        schedules,
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        events: {
            let e = musubi_server::events::Events::new(db.clone());
            if let Some(l) = &liveops {
                e.set_liveops(l.clone());
            }
            Arc::new(e)
        },
        banned: Arc::new(musubi_server::bans::Bans::default()),
        legal: Arc::new(musubi_server::legal::Legal::default()),
        geoip,
        ban_rules: Arc::new(musubi_server::bans::BanRules::default()),
        age: Arc::new(musubi_server::compliance::AgeGate::new(compliance_cfg)),
        liveops_cache: Default::default(),
        analytics: musubi_server::analytics::Analytics::new(db.clone(), analytics_cfg),
        keys: Arc::new(musubi_server::keys::ServerKeys::default()),
        logins,
        draining: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        db,
        started_at: std::time::Instant::now(),
        metrics: musubi_server::metrics::install(),
        limits: Arc::new(musubi_server::limits::RateLimiter::new(
            &musubi_core::config::LimitsConfig {
                enabled: false,
                ..Default::default()
            },
        )),
    }));
    state.matches.set_idl(state.idl.clone());
    state.anticheat.bind(&state);
    musubi_server::runtime_host::install(&state);
    {
        let st = state.clone();
        state
            .analytics
            .set_store_resolver(Arc::new(move |name: String| {
                let st = st.clone();
                Box::pin(
                    async move { musubi_server::http::storage_endpoints::store(&st, &name).await },
                )
            }));
    }
    Some((musubi_server::http::router(state.clone()), state))
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    let req = match body {
        Some(b) => req
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(b.to_string()))
            .unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

async fn migrate_once(db: &sqlx::PgPool) {
    static ONCE: tokio::sync::OnceCell<()> = tokio::sync::OnceCell::const_new();
    ONCE.get_or_init(|| async { musubi_server::db::migrate(db).await.unwrap() })
        .await;
}

async fn call_server(
    app: &Router,
    method: &str,
    path: &str,
    key: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-musubi-server-key", key);
    let body = match body {
        Some(b) => {
            req = req.header(header::CONTENT_TYPE, "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, v)
}

fn test_runtime() -> Arc<musubi_runtime::Runtime> {
    let rt = Arc::new(musubi_runtime::Runtime::new(musubi_runtime::Limits::default()).unwrap());
    rt.load_bytes("example", musubi_runtime::EXAMPLE_WAT.as_bytes())
        .unwrap();
    rt
}

async fn test_cluster(
    db: &sqlx::PgPool,
    group: &str,
    cfg: &Config,
) -> Option<Arc<musubi_cluster::Cluster>> {
    let mut c = musubi_cluster::Cluster::new(
        db.clone(),
        "test-node",
        "local",
        vec!["all".into()],
        "127.0.0.1:0",
    )
    .with_group(group);
    if cfg.mesh_net.enabled {
        c = c
            .with_data_dir(&cfg.node.data_dir)
            .unwrap()
            .with_mesh_net(&cfg.mesh_net)
            .await
            .unwrap();
    }
    let c = Arc::new(c);
    c.start().await.unwrap();
    Some(c)
}

fn test_matches(
    db: &sqlx::PgPool,
    runtime: &Arc<musubi_runtime::Runtime>,
) -> Arc<musubi_server::realtime::MatchRegistry> {
    let m = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    assert_eq!(m.register_wasm(runtime), vec!["demo"]);
    Arc::new(m)
}

async fn device(app: &Router, tag: &str) -> (String, String) {
    let (_, a) = call(
        app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique(tag)})),
    )
    .await;
    (
        a["access_token"].as_str().unwrap().to_string(),
        a["account_id"].as_str().unwrap().to_string(),
    )
}

async fn currency(app: &Router, k: &str) -> String {
    let cur = unique("ac")
        .replace('-', "")
        .chars()
        .take(12)
        .collect::<String>();
    let (st, v) = call_server(
        app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": cur, "name": "Audit coin"})),
    )
    .await;
    assert!(st.is_success(), "{v}");
    cur
}

async fn fund(app: &Router, k: &str, account: &str, cur: &str, delta: i64) {
    let (st, v) = call_server(
        app,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(
            json!({"account_id": account, "currency": cur, "delta": delta,
            "reason": "test", "reference": unique("audit-fund")}),
        ),
    )
    .await;
    assert!(st.is_success(), "{v}");
}

async fn balance(app: &Router, token: &str, cur: &str) -> i64 {
    let (st, v) = call(app, "GET", "/v1/wallet", Some(token), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["currency"] == cur)
        .and_then(|b| b["balance"].as_i64())
        .unwrap_or(0)
}

/// Fires `n` identical requests at once and returns their (status, body) pairs.
async fn burst(
    app: &Router,
    n: usize,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> Vec<(StatusCode, Value)> {
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..n {
        let (app, method, path, token, body) = (
            app.clone(),
            method.to_string(),
            path.to_string(),
            token.to_string(),
            body.clone(),
        );
        set.spawn(async move { call(&app, &method, &path, Some(&token), body).await });
    }
    let mut out = Vec::new();
    while let Some(r) = set.join_next().await {
        out.push(r.unwrap());
    }
    out
}

/// A reward table that grants one unit of a fresh stackable item.
async fn reward_table(app: &Router, k: &str) -> String {
    let item = unique("aditem");
    let (st, v) = call_server(
        app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": item, "name": item, "stackable": true, "max_stack": 999})),
    )
    .await;
    assert!(st.is_success(), "{v}");
    let table = unique("adtbl");
    let (st, v) = call_server(
        app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({
            "id": table, "name": "Ad reward", "kind": "weighted",
            "entries": [{"weight": 1, "rarity": "common", "item_id": item, "amount": 1}]
        })),
    )
    .await;
    assert!(st.is_success(), "{v}");
    table
}

/// ads.rs `reward()`: the claim is serialized per account, so neither the same view completed
/// concurrently nor two views racing a `daily_cap = 1` can roll the reward table twice.
#[tokio::test]
async fn ad_rewards_are_claimed_once_under_concurrency() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let table = reward_table(&app, k).await;
    let pid = unique("adcap");
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/ad_placements/{pid}"),
        k,
        Some(
            json!({"name": "Capped", "reward_table": table, "daily_cap": 1, "trust_client": true}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    // same view, completed six times at once: one grant, five duplicates
    let (ta, _) = device(&app, "ad-a").await;
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/ads/placements/{pid}/start"),
        Some(&ta),
        Some(json!({"platform": "ios"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let view = v["view_id"].as_str().unwrap().to_string();
    let results = burst(
        &app,
        6,
        "POST",
        &format!("/v1/ads/placements/{pid}/complete"),
        &ta,
        Some(json!({"view_id": view})),
    )
    .await;
    let granted = results
        .iter()
        .filter(|(_, v)| v["status"] == "granted")
        .count();
    let duplicates = results
        .iter()
        .filter(|(_, v)| v["status"] == "duplicate")
        .count();
    assert!(
        results.iter().all(|(st, _)| *st == StatusCode::OK),
        "{results:?}"
    );
    assert_eq!(granted, 1, "{results:?}");
    assert_eq!(duplicates, 5, "{results:?}");
    let (_, v) = call(
        &app,
        "GET",
        "/v1/ads/rewards?status=granted",
        Some(&ta),
        None,
    )
    .await;
    assert_eq!(v["rewards"].as_array().unwrap().len(), 1, "{v}");

    // two different views racing a daily cap of one: exactly one grant, the other hits the cap
    let (tb, _) = device(&app, "ad-b").await;
    let mut views = Vec::new();
    for _ in 0..2 {
        let (_, v) = call(
            &app,
            "POST",
            &format!("/v1/ads/placements/{pid}/start"),
            Some(&tb),
            Some(json!({"platform": "ios"})),
        )
        .await;
        views.push(v["view_id"].as_str().unwrap().to_string());
    }
    let mut set = tokio::task::JoinSet::new();
    for view in views {
        let (app, tb, pid) = (app.clone(), tb.clone(), pid.clone());
        set.spawn(async move {
            call(
                &app,
                "POST",
                &format!("/v1/ads/placements/{pid}/complete"),
                Some(&tb),
                Some(json!({"view_id": view})),
            )
            .await
        });
    }
    let mut results = Vec::new();
    while let Some(r) = set.join_next().await {
        results.push(r.unwrap());
    }
    let granted = results
        .iter()
        .filter(|(_, v)| v["status"] == "granted")
        .count();
    let capped = results
        .iter()
        .filter(|(_, v)| v["status"] == "rejected" && v["reason"] == "daily_cap")
        .count();
    assert_eq!((granted, capped), (1, 1), "{results:?}");
    let (_, v) = call(&app, "GET", "/v1/ads/rewards", Some(&tb), None).await;
    let rows = v["rewards"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{v}");
    assert!(rows.iter().all(|r| r["status"] != "pending"), "{v}");
}

/// offers.rs `buy_offer()`: the activation is claimed before the purchase, so a burst of buys
/// pays the discounted price once; a failed purchase releases the claim for a retry.
#[tokio::test]
async fn offer_is_bought_once_under_concurrency_and_claim_is_released_on_failure() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let cur = currency(&app, k).await;
    let item = unique("ofitem");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": item, "name": "Chest", "stackable": true, "max_stack": 99})),
    )
    .await;
    assert!(st.is_success(), "{v}");
    let sku = unique("ofsku");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/store",
        k,
        Some(json!({"id": sku, "item_id": item, "quantity": 1, "prices": {cur.clone(): 100}})),
    )
    .await;
    assert!(st.is_success(), "{v}");
    let pl = unique("banner");
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/placements/{pl}"),
        k,
        Some(json!({"name": "Banner", "max_offers": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let offer = unique("starter");
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/offers/{offer}"),
        k,
        Some(
            json!({"placement_id": pl, "name": "Starter", "store_item_id": sku,
            "prices": {cur.clone(): 60}, "max_purchases": 1}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // operator-supplied spans are capped so timestamp arithmetic cannot overflow
    let (st, _) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/offers/{offer}-long"),
        k,
        Some(
            json!({"placement_id": pl, "name": "Long", "store_item_id": sku,
            "cooldown_secs": i64::MAX / 2}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (ta, aid) = device(&app, "of-a").await;
    fund(&app, k, &aid, &cur, 300).await;
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/placements/{pl}/offers"),
        Some(&ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["offers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["offer_id"] == offer),
        "{v}"
    );
    let results = burst(
        &app,
        5,
        "POST",
        &format!("/v1/placements/{pl}/offers/{offer}/buy"),
        &ta,
        Some(json!({"currency": cur})),
    )
    .await;
    let ok = results
        .iter()
        .filter(|(st, _)| *st == StatusCode::OK)
        .count();
    assert_eq!(ok, 1, "{results:?}");
    assert!(
        results
            .iter()
            .filter(|(st, _)| *st != StatusCode::OK)
            .all(|(st, _)| *st == StatusCode::NOT_FOUND),
        "{results:?}"
    );
    assert_eq!(balance(&app, &ta, &cur).await, 240);

    // a player who cannot pay keeps the activation and succeeds once funded
    let (tb, bid) = device(&app, "of-b").await;
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/placements/{pl}/offers"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/placements/{pl}/offers/{offer}/buy"),
        Some(&tb),
        Some(json!({"currency": cur})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    assert_ne!(st, StatusCode::NOT_FOUND, "the claim must be released: {v}");
    fund(&app, k, &bid, &cur, 60).await;
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/placements/{pl}/offers/{offer}/buy"),
        Some(&tb),
        Some(json!({"currency": cur})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(balance(&app, &tb, &cur).await, 0);
}

/// referrals.rs `redeem()`: the code row is locked for the redemption, so concurrent referees
/// cannot push a referrer past `max_referrals_per_player`, and reward errors are not echoed.
#[tokio::test]
async fn referral_cap_holds_under_concurrent_redeems() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let cur = currency(&app, k).await;
    let table = unique("rtref");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({"id": table, "name": "ref", "kind": "guaranteed",
            "entries": [{"currency": cur, "amount": 25}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let pid = unique("refp").to_lowercase();
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/referrals/programs/{pid}"),
        k,
        Some(json!({"name": "Invite", "referrer_reward_table": table,
            "max_referrals_per_player": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (inviter, _) = device(&app, "rf-inv").await;
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/referrals/{pid}/invite"),
        Some(&inviter),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let code = v["code"].as_str().unwrap().to_string();

    let mut set = tokio::task::JoinSet::new();
    for i in 0..6 {
        let (app, pid, code) = (app.clone(), pid.clone(), code.clone());
        set.spawn(async move {
            let (t, _) = device(&app, &format!("rf{i}")).await;
            call(
                &app,
                "POST",
                &format!("/v1/referrals/{pid}/redeem"),
                Some(&t),
                Some(json!({"code": code})),
            )
            .await
        });
    }
    let mut results = Vec::new();
    while let Some(r) = set.join_next().await {
        results.push(r.unwrap());
    }
    let ok = results
        .iter()
        .filter(|(st, _)| *st == StatusCode::OK)
        .count();
    let exhausted = results
        .iter()
        .filter(|(st, _)| *st == StatusCode::TOO_MANY_REQUESTS)
        .count();
    assert_eq!((ok, exhausted), (2, 4), "{results:?}");
    let accepted: i64 = sqlx::query_scalar(
        "select count(*) from musubi.referrals where program_id = $1 and status = 'accepted'",
    )
    .bind(&pid)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(accepted, 2);
    assert_eq!(balance(&app, &inviter, &cur).await, 50);
}

/// subscriptions.rs `subscribe()`: the claim comes before the charge under a per-(account, plan)
/// lock, so a burst charges the wallet once; a failed charge leaves no row behind.
#[tokio::test]
async fn subscribe_charges_once_under_concurrency_and_caps_plan_spans() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let cur = currency(&app, k).await;
    let plan = unique("plan");
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/subscription_plans/{plan}"),
        k,
        Some(
            json!({"name": "VIP", "period_secs": 3600, "price": {"currency": cur, "amount": 50},
            "features": ["vip"]}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/subscription_plans/{plan}-long"),
        k,
        Some(json!({"name": "Forever", "period_secs": i64::MAX / 4})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (ta, aid) = device(&app, "sub-a").await;
    fund(&app, k, &aid, &cur, 500).await;
    let results = burst(
        &app,
        5,
        "POST",
        "/v1/subscriptions/subscribe",
        &ta,
        Some(json!({"plan_id": plan})),
    )
    .await;
    let ok = results
        .iter()
        .filter(|(st, _)| *st == StatusCode::OK)
        .count();
    let conflicts = results
        .iter()
        .filter(|(st, _)| *st == StatusCode::CONFLICT)
        .count();
    assert_eq!((ok, conflicts), (1, 4), "{results:?}");
    assert_eq!(balance(&app, &ta, &cur).await, 450);
    let rows: i64 = sqlx::query_scalar(
        "select count(*) from musubi.subscriptions where account_id = $1::uuid and plan_id = $2",
    )
    .bind(&aid)
    .bind(&plan)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(rows, 1);
    let (_, e) = call(
        &app,
        "GET",
        "/v1/subscriptions/entitled?feature=vip",
        Some(&ta),
        None,
    )
    .await;
    assert_eq!(e["entitled"], true);

    // empty wallet: no subscription row survives the failed charge, and a retry works once funded
    let (tb, bid) = device(&app, "sub-b").await;
    let (st, v) = call(
        &app,
        "POST",
        "/v1/subscriptions/subscribe",
        Some(&tb),
        Some(json!({"plan_id": plan})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{v}");
    let rows: i64 =
        sqlx::query_scalar("select count(*) from musubi.subscriptions where account_id = $1::uuid")
            .bind(&bid)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(rows, 0);
    let (st, v) = call(
        &app,
        "POST",
        "/v1/subscriptions/subscribe",
        Some(&tb),
        Some(json!({"plan_id": plan, "metadata": {"blob": "x".repeat(5000)}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    fund(&app, k, &bid, &cur, 50).await;
    let (st, v) = call(
        &app,
        "POST",
        "/v1/subscriptions/subscribe",
        Some(&tb),
        Some(json!({"plan_id": plan})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "active");
}

/// sweepstakes.rs / steam_ownership.rs: player-supplied counts and blobs are bounded before
/// they reach the economy / the ticket decoder.
#[tokio::test]
async fn monetization_input_caps() {
    let Some(app) = app().await else { return };
    let (t, _) = device(&app, "cap").await;
    let (st, v) = call(
        &app,
        "POST",
        "/v1/sweepstakes/nope/enter",
        Some(&t),
        Some(json!({"entries": i32::MAX})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/sweepstakes/nope/enter",
        Some(&t),
        Some(json!({"entries": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/steam/app_ticket",
        "defaultkey",
        Some(json!({"ticket": "a".repeat(9000)})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
}
