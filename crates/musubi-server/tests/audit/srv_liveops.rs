//! Regression tests for the srv-liveops audit (tutorials, live events, news, web push).
//! Run with `MUSUBI_TEST_DATABASE_URL=postgres://...`; skipped otherwise.

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

/// Tests that switch the global access policy (maintenance / limited) take the write side; logins
/// made through `call` / `call_server` take the read side, so a parallel test never sees another
/// test's maintenance window (the api suite shares one database).
static LOGIN_GATE: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());
tokio::task_local! {
    static GATE_HELD: ();
}
fn gated_login(path: &str) -> bool {
    (path.starts_with("/v1/auth/") || path.starts_with("/v1/server/auth/"))
        && GATE_HELD.try_with(|_| ()).is_err()
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let _gate = if gated_login(path) {
        Some(LOGIN_GATE.read().await)
    } else {
        None
    };
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
    let _gate = if gated_login(path) {
        Some(LOGIN_GATE.read().await)
    } else {
        None
    };
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

async fn login(app: &Router) -> (String, String) {
    let (st, s) = call(
        app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("pf")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    (
        s["access_token"].as_str().unwrap().to_string(),
        s["account_id"].as_str().unwrap().to_string(),
    )
}

/// Finishing a tutorial pays its reward table: the state transition must be a single
/// conditional UPDATE so parallel `complete` calls cannot pay twice, and a player must not be able
/// to reopen a completed tutorial (reset → accept → complete loop) — only the server may.
#[tokio::test]
async fn tutorial_reward_is_paid_once_under_races_and_resets() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (gold, table, tid) = (unique("gold"), unique("tbl"), unique("tut"));
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold", "max_balance": 100000})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({"id": table, "kind": "guaranteed", "entries": [{"currency": gold, "amount": 10}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/tutorials/{tid}"),
        k,
        Some(json!({"title": "Audit", "start_step": 0, "max_step": 3, "enabled": true, "reward_table": table})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (tok, acc) = login(&app).await;
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/tutorials/{tid}/accept"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let app = app.clone();
        let tok = tok.clone();
        let tid = tid.clone();
        set.spawn(async move {
            call(
                &app,
                "PUT",
                &format!("/v1/tutorials/{tid}/step"),
                Some(&tok),
                Some(json!({"complete": true})),
            )
            .await
            .0
        });
    }
    let mut ok = 0;
    let mut conflict = 0;
    while let Some(r) = set.join_next().await {
        match r.unwrap() {
            StatusCode::OK => ok += 1,
            StatusCode::CONFLICT => conflict += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert_eq!((ok, conflict), (1, 7), "exactly one completion wins");
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/tutorials/{tid}"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "completed", "{v}");
    // ... and the reward table was paid exactly once
    let (st, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{w}");
    let paid: i64 = w["balances"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["currency"] == gold)
        .map(|b| b["balance"].as_i64().unwrap())
        .sum();
    assert_eq!(paid, 10, "{w}");

    // the player may reset and run the tutorial again, but the reward table is paid only once
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/tutorials/{tid}/reset"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/tutorials/{tid}/accept"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "PUT",
        &format!("/v1/tutorials/{tid}/step"),
        Some(&tok),
        Some(json!({"complete": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "completed", "{v}");
    assert_eq!(v["rewards"], json!([]), "{v}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    let paid: i64 = w["balances"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|b| b["currency"] == gold)
        .map(|b| b["balance"].as_i64().unwrap())
        .sum();
    assert_eq!(paid, 10, "second completion must not pay again: {w}");

    // the operator reset path and a reset from `accepted` keep working
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/accounts/{acc}/tutorials/{tid}/reset"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tutorials/{tid}/accept"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = call(
        &app,
        "PUT",
        &format!("/v1/tutorials/{tid}/step"),
        Some(&tok),
        Some(json!({"step": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // a stale/duplicate step (not advancing) is refused instead of silently rewriting
    let (st, _) = call(
        &app,
        "PUT",
        &format!("/v1/tutorials/{tid}/step"),
        Some(&tok),
        Some(json!({"step": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/tutorials/{tid}/reset"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

/// `max_participants` is enforced under a per-cycle advisory lock: N concurrent joins against a
/// 2-seat event admit exactly two. Event durations are bounded so timeline arithmetic cannot
/// overflow.
#[tokio::test]
async fn live_event_capacity_holds_under_concurrent_joins() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let id = unique("ev");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let body = |max: i64| {
        json!({
            "id": id, "name": "Audit event",
            "phases": [{"name": "live", "offset_secs": 0, "duration_secs": 86_400}],
            "starts_at": now - 60, "participation": "explicit", "max_participants": max,
        })
    };
    let (st, v) = call_server(&app, "POST", "/v1/server/live_events", k, Some(body(2))).await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let mut toks = Vec::new();
    for _ in 0..6 {
        toks.push(login(&app).await.0);
    }
    let mut set = tokio::task::JoinSet::new();
    for tok in toks {
        let app = app.clone();
        let id = id.clone();
        set.spawn(async move {
            call(
                &app,
                "POST",
                &format!("/v1/liveops/live-events/{id}/join"),
                Some(&tok),
                None,
            )
            .await
        });
    }
    let mut joined = 0;
    let mut full = 0;
    while let Some(r) = set.join_next().await {
        let (st, v) = r.unwrap();
        match st {
            StatusCode::OK => joined += 1,
            StatusCode::CONFLICT => full += 1,
            other => panic!("unexpected status {other}: {v}"),
        }
    }
    assert_eq!(
        (joined, full),
        (2, 4),
        "capacity must hold under concurrency"
    );
    let db = musubi_server::db::connect(&musubi_core::config::DatabaseConfig {
        url: std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap(),
        max_connections: 1,
        ..Default::default()
    })
    .await
    .unwrap();
    let n: i64 = sqlx::query_scalar(
        "select count(*) from musubi.live_event_participants where event_id = $1",
    )
    .bind(&id)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(n, 2);

    // absurd spans are rejected instead of overflowing in phase/timeline arithmetic
    for bad in [
        json!({"id": unique("ev"), "name": "x", "starts_at": now,
               "phases": [{"name": "p", "offset_secs": 0, "duration_secs": i64::MAX}]}),
        json!({"id": unique("ev"), "name": "x", "starts_at": i64::MAX - 10,
               "phases": [{"name": "p", "offset_secs": 0, "duration_secs": 60}]}),
        json!({"id": unique("ev"), "name": "x", "starts_at": now, "recurrence_secs": i64::MAX,
               "phases": [{"name": "p", "offset_secs": 0, "duration_secs": 60}]}),
    ] {
        let (st, v) = call_server(&app, "POST", "/v1/server/live_events", k, Some(bad)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    }
}

/// A player may only mark news they can actually see: a disabled item is 404 (no existence
/// oracle, no phantom read rows); once enabled the same id is accepted.
#[tokio::test]
async fn news_mark_read_only_for_visible_items() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/news",
        k,
        Some(json!({"title": "Hidden", "body": "draft", "enabled": false})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = v["id"].as_str().unwrap().to_string();
    let (tok, _) = login(&app).await;
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/news/{id}/read"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    let (st, v) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/news/{id}"),
        k,
        Some(json!({"title": "Hidden", "body": "draft", "enabled": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/news/{id}/read"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

/// Web Push subscriptions are player-supplied URLs the node POSTs to: private, link-local and
/// plain-http external endpoints are refused at registration time.
#[tokio::test]
async fn webpush_subscription_rejects_ssrf_endpoints() {
    let Some(app) = app().await else { return };
    let (tok, _) = login(&app).await;
    let sub = |endpoint: &str| {
        json!({
            "platform": "webpush",
            "token": json!({"endpoint": endpoint, "keys": {
                "p256dh": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
                "auth": "BTBZMqHH6r4Tts7J_aSIgg"}}).to_string(),
        })
    };
    for bad in [
        "http://169.254.169.254/latest/meta-data/",
        "https://10.0.0.5/hook",
        "http://push.example.com/plain",
        "https://[fd00::1]/x",
    ] {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/account/push_tokens",
            Some(&tok),
            Some(sub(bad)),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}: {v}");
    }
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/push_tokens",
        Some(&tok),
        Some(sub(
            "https://updates.push.services.mozilla.com/wpush/v2/audit",
        )),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}
