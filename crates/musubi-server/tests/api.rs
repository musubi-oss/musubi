//! HTTP integration tests. Run with `MUSUBI_TEST_DATABASE_URL=postgres://...`; skipped otherwise.

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
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    cfg.database.max_connections = 3;
    cfg.market.anti_snipe_secs = 0;
    tweak(&mut cfg);
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
    let cluster = test_cluster(&db, &unique("grp")).await;
    let matchmaker = Arc::new(musubi_matchmaker::Matchmaker::default());
    let parties = Arc::new(musubi_server::realtime::PartyRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
    ));
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
    let fleets = Arc::new(musubi_fleet::FleetService::new(
        db.clone(),
        cluster.as_ref().unwrap().node_id().0,
        "http://127.0.0.1:0",
        "defaultkey",
    ));
    fanout.set_fleets(fleets.clone());
    let worlds = Arc::new(musubi_server::realtime::WorldRegistry::new(
        cluster.as_ref().unwrap().node_id().0,
        Some(db.clone()),
    ));
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
    let state = AppState(Arc::new(AppStateInner {
        anticheat: Arc::new(musubi_server::anticheat::AntiCheat::new()),
        dev: Arc::new(musubi_server::dev::DevTools::new()),
        audit: musubi_server::audit::Sink::new(&db),
        cfg: Arc::new(cfg),
        blobs: None,
        auth,
        idl: musubi_server::idl::IdlRegistry::new(db.clone(), storage.clone()),
        storage,
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        lobbies: musubi_server::lobby::LobbyService::new(
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
        age: Arc::new(musubi_server::compliance::AgeGate::new(compliance_cfg)),
        liveops_cache: Default::default(),
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
    Some(musubi_server::http::router(state))
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

#[tokio::test]
async fn device_auth_refresh_rotation_and_reuse_detection() {
    let Some(app) = app().await else { return };
    let dev = unique("dev");
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev, "vars": {"k": "v"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    assert_eq!(s1["created"], true);
    let at = s1["access_token"].as_str().unwrap();
    let rt = s1["refresh_token"].as_str().unwrap();

    let (st, me) = call(&app, "GET", "/v1/account/me", Some(at), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(me["id"], s1["account_id"]);
    assert_eq!(me["vars"]["k"], "v");

    let (st, _) = call(&app, "GET", "/v1/account/me", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(&app, "GET", "/v1/account/me", Some("garbage"), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": rt})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(s2["account_id"], s1["account_id"]);
    assert_ne!(s2["refresh_token"], s1["refresh_token"]);

    // old token reused → whole family revoked, including the fresh one
    let (st, e) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": rt})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(e["code"], "unauthenticated");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": s2["refresh_token"]})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let (_, again) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev})),
    )
    .await;
    assert_eq!(again["created"], false);
    assert_eq!(again["account_id"], s1["account_id"]);

    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("nope"), "create": false})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn email_auth_and_username_rules() {
    let Some(app) = app().await else { return };
    let email = format!("{}@example.test", unique("u"));
    let user = unique("name").replace('-', "_");
    let user = &user[..user.len().min(32)];
    let (st, s) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email.to_uppercase(), "password": "secret123", "username": user})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["created"], true);

    let (st, e) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "wrongpass1"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{e}");

    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "secret123"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s2["created"], false);
    assert_eq!(s2["account_id"], s["account_id"]);

    let (st, e) = call(&app, "POST", "/v1/auth/email", None, Some(json!({"email": format!("{}@example.test", unique("v")), "password": "secret123", "username": user}))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert_eq!(e["code"], "already_exists");

    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": "bad", "password": "secret123"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": format!("{}@example.test", unique("w")), "password": "short"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn storage_permissions_cas_and_paging() {
    let Some(app) = app().await else { return };
    let (_, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev")})),
    )
    .await;
    let (_, b) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev")})),
    )
    .await;
    let (ta, tb) = (
        a["access_token"].as_str().unwrap(),
        b["access_token"].as_str().unwrap(),
    );
    let col = unique("col");

    let (st, w) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(ta),
        Some(json!({"objects": [
            {"collection": col, "key": "k1", "value": {"n": 1}, "version": "*"},
            {"collection": col, "key": "k2", "value": {"n": 2}, "read_permission": 2},
            {"collection": col, "key": "k3", "value": {"n": 3}, "write_permission": 0},
        ]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");
    let v1 = w["acks"][0]["version"].as_str().unwrap().to_string();

    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(ta),
        Some(json!({"objects": [{"collection": col, "key": "k1", "value": {}, "version": "*"}]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(ta),
        Some(
            json!({"objects": [{"collection": col, "key": "k1", "value": {}, "version": "stale"}]}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, w) = call(&app, "POST", "/v1/storage/write", Some(ta), Some(json!({"objects": [{"collection": col, "key": "k1", "value": {"n": 10}, "version": v1}]}))).await;
    assert_eq!(st, StatusCode::OK, "{w}");
    assert_ne!(w["acks"][0]["version"], v1);

    // write_permission 0 = server-only from now on
    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(ta),
        Some(json!({"objects": [{"collection": col, "key": "k3", "value": {}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/delete",
        Some(ta),
        Some(json!({"objects": [{"collection": col, "key": "k3"}]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(ta),
        Some(json!({"objects": [{"collection": col, "key": "arr", "value": [1, 2]}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // B sees only A's public object
    let owner = a["account_id"].as_str().unwrap();
    let (_, r) = call(
        &app,
        "POST",
        "/v1/storage/read",
        Some(tb),
        Some(json!({"objects": [
            {"collection": col, "key": "k1", "owner_id": owner},
            {"collection": col, "key": "k2", "owner_id": owner},
        ]})),
    )
    .await;
    let keys: Vec<&str> = r["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["k2"]);
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?owner_id={owner}"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(l["objects"].as_array().unwrap().len(), 1);

    // A pages through its own objects
    let (_, p1) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?limit=2"),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(p1["objects"].as_array().unwrap().len(), 2);
    let cursor = p1["cursor"].as_str().expect("cursor");
    let (_, p2) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?limit=2&cursor={cursor}"),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(p2["objects"][0]["key"], "k3");
    assert!(p2["cursor"].is_null());

    let (st, _) = call(&app, "POST", "/v1/storage/delete", Some(ta), Some(json!({"objects": [{"collection": col, "key": "k1"}, {"collection": col, "key": "k2"}]}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, l) = call(&app, "GET", &format!("/v1/storage/{col}"), Some(ta), None).await;
    assert_eq!(l["objects"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn account_update_link_unlink_and_users_lookup() {
    let Some(app) = app().await else { return };
    let dev = unique("dev");
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap();

    let (st, acc) = call(&app, "GET", "/v1/account", Some(t), None).await;
    assert_eq!(st, StatusCode::OK, "{acc}");
    assert_eq!(acc["identities"].as_array().unwrap().len(), 1);

    let name = unique("nm").replace('-', "_");
    let name = &name[..name.len().min(32)];
    let (st, acc) = call(
        &app,
        "PATCH",
        "/v1/account",
        Some(t),
        Some(json!({"username": name, "display_name": "Alice", "metadata": {"lvl": 3}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{acc}");
    assert_eq!(acc["username"], name);
    assert_eq!(acc["metadata"]["lvl"], 3);
    let (st, _) = call(
        &app,
        "PATCH",
        "/v1/account",
        Some(t),
        Some(json!({"metadata": [1]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let email = format!("{}@example.test", unique("l"));
    let (st, e0) = call(
        &app,
        "POST",
        "/v1/account/link/email",
        Some(t),
        Some(json!({"email": email, "password": "secret123"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e0}");
    let (st, e) = call(
        &app,
        "POST",
        "/v1/account/link/email",
        Some(t),
        Some(json!({"email": email, "password": "secret123"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    let (_, acc) = call(&app, "GET", "/v1/account", Some(t), None).await;
    assert_eq!(acc["identities"].as_array().unwrap().len(), 2);
    assert!(acc["identities"][1]["subject"]
        .as_str()
        .unwrap()
        .contains("***@"));

    // email login now reaches the same account
    let (_, s2) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "secret123"})),
    )
    .await;
    assert_eq!(s2["account_id"], s["account_id"]);

    let (st, _) = call(
        &app,
        "DELETE",
        "/v1/account/unlink/device",
        Some(t),
        Some(json!({"id": dev})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, e) = call(
        &app,
        "DELETE",
        "/v1/account/unlink/email",
        Some(t),
        Some(json!({"email": email})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "last identity: {e}");

    let (st, u) = call(
        &app,
        "GET",
        &format!("/v1/users?usernames={name}"),
        Some(t),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    assert_eq!(u["users"][0]["id"], s["account_id"]);
    assert!(u["users"][0].get("identities").is_none());

    let (st, _) = call(&app, "DELETE", "/v1/account", Some(t), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", "/v1/account", Some(t), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn friends_invite_accept_remove_block() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("fr")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, a) = &users[0];
    let (tb, b) = &users[1];
    let (tc, c) = &users[2];

    // A invites B → A sees state 1, B sees state 2
    let (st, e) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    let (_, l) = call(&app, "GET", "/v1/friends", Some(ta), None).await;
    assert_eq!(l["friends"][0]["state"], 1);
    assert_eq!(l["friends"][0]["user"]["id"], *b);
    let (_, l) = call(&app, "GET", "/v1/friends?state=2", Some(tb), None).await;
    assert_eq!(l["friends"][0]["state"], 2);

    // B accepts → both 0
    call(
        &app,
        "POST",
        "/v1/friends",
        Some(tb),
        Some(json!({"ids": [a]})),
    )
    .await;
    let (_, l) = call(&app, "GET", "/v1/friends?state=0", Some(ta), None).await;
    assert_eq!(l["friends"].as_array().unwrap().len(), 1);
    let (_, l) = call(&app, "GET", "/v1/friends?state=0", Some(tb), None).await;
    assert_eq!(l["friends"].as_array().unwrap().len(), 1);

    // self / empty / unknown
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(ta),
        Some(json!({"ids": [a]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(ta),
        Some(json!({"ids": []})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(ta),
        Some(json!({"ids": [uuid::Uuid::now_v7()]})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // A removes B → both empty
    let (st, _) = call(
        &app,
        "DELETE",
        "/v1/friends",
        Some(ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, l) = call(&app, "GET", "/v1/friends", Some(tb), None).await;
    assert!(l["friends"].as_array().unwrap().is_empty());

    // C blocks A: A's invite to C is dropped silently, C sees state 3, A sees nothing
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends/block",
        Some(tc),
        Some(json!({"ids": [a]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(ta),
        Some(json!({"ids": [c]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, l) = call(&app, "GET", "/v1/friends", Some(ta), None).await;
    assert!(l["friends"].as_array().unwrap().is_empty(), "{l}");
    let (_, l) = call(&app, "GET", "/v1/friends", Some(tc), None).await;
    assert_eq!(l["friends"][0]["state"], 3);
    // unblock = delete
    call(
        &app,
        "DELETE",
        "/v1/friends",
        Some(tc),
        Some(json!({"ids": [a]})),
    )
    .await;
    let (_, l) = call(&app, "GET", "/v1/friends", Some(tc), None).await;
    assert!(l["friends"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn groups_lifecycle_and_moderation() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("gr")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, a) = &users[0];
    let (tb, b) = &users[1];
    let (tc, c) = &users[2];

    let name = unique("clan");
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(ta),
        Some(json!({"name": name, "open": false, "max_count": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    assert_eq!(g["edge_count"], 1);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(tb),
        Some(json!({"name": name})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // closed group → join request, visible to admin only
    let (_, j) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(j["state"], 3);
    let (_, m) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/members"),
        Some(tc),
        None,
    )
    .await;
    assert_eq!(m["items"].as_array().unwrap().len(), 1);
    let (_, m) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/members?state=3"),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(m["items"][0]["user"]["id"], *b);

    // non-admin cannot accept; admin accepts → edge_count 2
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/add"),
        Some(tc),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/add"),
        Some(ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, g) = call(&app, "GET", &format!("/v1/groups/{gid}"), Some(ta), None).await;
    assert_eq!(g["edge_count"], 2);

    // full: C joining stays a request even after open=true? open → immediate join fails with 429
    call(
        &app,
        "PATCH",
        &format!("/v1/groups/{gid}"),
        Some(ta),
        Some(json!({"open": true})),
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(tc),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    call(
        &app,
        "PATCH",
        &format!("/v1/groups/{gid}"),
        Some(ta),
        Some(json!({"max_count": 10})),
    )
    .await;
    let (_, j) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(tc),
        None,
    )
    .await;
    assert_eq!(j["state"], 2);

    // promote B to admin; B cannot kick A (higher), can kick C
    call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/promote"),
        Some(ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/kick"),
        Some(tb),
        Some(json!({"ids": [a]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/kick"),
        Some(tb),
        Some(json!({"ids": [c]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, ug) = call(
        &app,
        "GET",
        &format!("/v1/users/{c}/groups"),
        Some(tc),
        None,
    )
    .await;
    assert!(ug["items"].as_array().unwrap().is_empty());

    // last superadmin can't leave while others remain; after promoting B it can
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/leave"),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/promote"),
        Some(ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/leave"),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, ug) = call(
        &app,
        "GET",
        &format!("/v1/users/{b}/groups?state=0"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(ug["items"][0]["group"]["id"], gid);
    assert_eq!(ug["items"][0]["group"]["edge_count"], 1);

    // listing by prefix; delete by superadmin
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/groups?name={}", &name[..8]),
        Some(tc),
        None,
    )
    .await;
    assert!(l["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|g| g["id"] == gid));
    let (st, _) = call(&app, "DELETE", &format!("/v1/groups/{gid}"), Some(tc), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&app, "DELETE", &format!("/v1/groups/{gid}"), Some(tb), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", &format!("/v1/groups/{gid}"), Some(tb), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// Tests in one binary run concurrently against a shared database: apply migrations once per process.
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

#[tokio::test]
async fn leaderboards_operators_ranks_and_authority() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("lb")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let lb = unique("weekly");

    // server key required for creation
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        "wrong",
        Some(json!({"id": lb})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, b) = call_server(&app, "POST", "/v1/server/leaderboards", "defaultkey", Some(json!({"id": lb, "sort_order": "desc", "operator": "best", "reset_schedule": "0 0 * * 1"}))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert!(b["next_reset"].as_i64().unwrap() > 0);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        "defaultkey",
        Some(json!({"id": lb})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // scores: best keeps the max; ranks are 1-based
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[0].0),
        Some(json!({"score": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["rank"], 1);
    let (_, r) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[0].0),
        Some(json!({"score": 50})),
    )
    .await;
    assert_eq!(r["score"], 100);
    assert_eq!(r["num_score"], 2);
    call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[1].0),
        Some(json!({"score": 300, "subscore": 1})),
    )
    .await;
    let (_, r) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[2].0),
        Some(json!({"score": 200})),
    )
    .await;
    assert_eq!(r["rank"], 2);
    assert!(r["expiry_time"].as_i64().unwrap() > 0);

    // listing: ordered desc with ranks, owner_records with rank, paging
    let (_, l) = call(
        &app,
        "GET",
        &format!(
            "/v1/leaderboards/{lb}/records?limit=2&owner_ids={}",
            users[0].1
        ),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(l["records"].as_array().unwrap().len(), 2);
    assert_eq!(l["records"][0]["owner_id"], users[1].1);
    assert_eq!(l["records"][1]["rank"], 2);
    assert_eq!(l["owner_records"][0]["rank"], 3);
    assert_eq!(l["next_cursor"], "2");
    let (_, l2) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/records?limit=2&cursor=2"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(l2["records"][0]["rank"], 3);
    assert!(l2["next_cursor"].is_null());

    // around
    let (st, a) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/around/{}?limit=1", users[2].1),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["records"][0]["owner_id"], users[2].1);

    // client cannot override operator; server can (set)
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[0].0),
        Some(json!({"score": 1, "operator": "set"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, r) = call_server(
        &app,
        "POST",
        &format!("/v1/server/leaderboards/{lb}/records/{}", users[0].1),
        "defaultkey",
        Some(json!({"score": 1, "operator": "set"})),
    )
    .await;
    assert_eq!(r["score"], 1);

    // own delete, then list shows 2
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(l["records"].as_array().unwrap().len(), 2);

    // authoritative board rejects client writes, incr operator accumulates
    let auth_lb = unique("auth");
    call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        "defaultkey",
        Some(json!({"id": auth_lb, "authoritative": true, "operator": "incr"})),
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{auth_lb}/records"),
        Some(&users[0].0),
        Some(json!({"score": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    call_server(
        &app,
        "POST",
        &format!("/v1/server/leaderboards/{auth_lb}/records/{}", users[0].1),
        "defaultkey",
        Some(json!({"score": 5})),
    )
    .await;
    let (_, r) = call_server(
        &app,
        "POST",
        &format!("/v1/server/leaderboards/{auth_lb}/records/{}", users[0].1),
        "defaultkey",
        Some(json!({"score": 7})),
    )
    .await;
    assert_eq!(r["score"], 12);
    let (_, b) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{auth_lb}"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(b["next_reset"], 0);

    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/leaderboards/{auth_lb}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{auth_lb}"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tournaments_join_caps_and_lifetime() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("tn")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let id = unique("cup");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/tournaments",
        "defaultkey",
        Some(json!({"id": id, "title": "Cup"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "needs duration/end/reset");
    let (st, t) = call_server(&app, "POST", "/v1/server/tournaments", "defaultkey", Some(json!({
        "id": id, "title": "Cup", "category": 7, "duration": 3600, "max_size": 2, "max_num_score": 2, "join_required": true
    }))).await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["can_enter"], true);
    assert!(t["end_active"].as_i64().unwrap() > 0);
    assert_eq!(t["size"], 0);

    // join_required: writing before joining is denied
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(&users[0].0),
        Some(json!({"score": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/join"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/join"),
        Some(&users[1].0),
        None,
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/join"),
        Some(&users[2].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "max_size");
    let (_, t) = call(
        &app,
        "GET",
        &format!("/v1/tournaments/{id}"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(t["size"], 2);
    assert_eq!(t["can_enter"], false);

    // max_num_score = 2 submissions
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(&users[0].0),
        Some(json!({"score": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["num_score"], 1);
    call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(&users[0].0),
        Some(json!({"score": 20})),
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(&users[0].0),
        Some(json!({"score": 30})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "max_num_score");
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/tournaments/{id}/records"),
        Some(&users[1].0),
        None,
    )
    .await;
    assert_eq!(l["records"][0]["score"], 20);
    assert_eq!(l["records"][0]["rank"], 1);

    // category filter in list
    let (_, l) = call(
        &app,
        "GET",
        "/v1/tournaments?category_start=7&category_end=7&limit=100",
        Some(&users[0].0),
        None,
    )
    .await;
    assert!(l["tournaments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == id));
    let (_, l) = call(
        &app,
        "GET",
        "/v1/tournaments?category_start=8",
        Some(&users[0].0),
        None,
    )
    .await;
    assert!(!l["tournaments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["id"] == id));

    // ended tournament: not active, join/write fail with 503
    let past = unique("past");
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let (st, t) = call_server(
        &app,
        "POST",
        "/v1/server/tournaments",
        "defaultkey",
        Some(json!({"id": past, "start_time": now - 7200, "end_time": now - 3600})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["can_enter"], false);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{past}/join"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE);

    // a tournament is not a plain leaderboard and vice versa
    let (st, _) = call(
        &app,
        "GET",
        &format!("/v1/tournaments/{}", "nope"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/tournaments/{id}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

fn test_runtime() -> Arc<musubi_runtime::Runtime> {
    let rt = Arc::new(musubi_runtime::Runtime::new(musubi_runtime::Limits::default()).unwrap());
    rt.load_bytes("example", musubi_runtime::EXAMPLE_WAT.as_bytes())
        .unwrap();
    rt
}

#[tokio::test]
async fn rpc_over_http_with_bearer_and_server_key() {
    let Some(app) = app().await else { return };
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("rpc")})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap();
    let (st, out) = call(&app, "POST", "/v1/rpc/echo", Some(t), Some(json!({"x": 1}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["x"], 1);
    let (_, who) = call(&app, "POST", "/v1/rpc/whoami", Some(t), Some(json!({}))).await;
    assert_eq!(who["account_id"], s["account_id"]);
    assert_eq!(who["server"], false);
    let (_, who) = call_server(
        &app,
        "POST",
        "/v1/rpc/whoami",
        "defaultkey",
        Some(json!({})),
    )
    .await;
    assert_eq!(who["server"], true);
    let (st, _) = call(&app, "POST", "/v1/rpc/echo", None, Some(json!({}))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, e) = call(&app, "POST", "/v1/rpc/fail", Some(t), Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(e["message"].as_str().unwrap().ends_with("boom"), "{e}");
    let (st, _) = call(&app, "POST", "/v1/rpc/missing", Some(t), Some(json!({}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, l) = call(&app, "GET", "/v1/rpc", Some(t), None).await;
    assert!(l["rpcs"].as_array().unwrap().iter().any(|r| r == "echo"));
}

async fn test_cluster(db: &sqlx::PgPool, group: &str) -> Option<Arc<musubi_cluster::Cluster>> {
    let c = Arc::new(
        musubi_cluster::Cluster::new(
            db.clone(),
            "test-node",
            "local",
            vec!["all".into()],
            "127.0.0.1:0",
        )
        .with_group(group),
    );
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

#[tokio::test]
async fn fleet_lifecycle_with_local_process_provider() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let name = unique("fleet");
    let (st, f) = call_server(
        &app,
        "POST",
        "/v1/server/fleets",
        k,
        Some(json!({
            "name": name, "provider": "local", "buffer": 1, "max_servers": 2,
            "port_start": 47000, "port_end": 47100,
            "spec": {"command": ["sleep", "60"]}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let fid = f["id"].as_str().unwrap().to_string();
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/fleets",
        k,
        Some(json!({"name": name, "provider": "local", "max_servers": 1, "spec": {"command": ["sleep", "1"]}})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // no ready server yet
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/allocate"),
        k,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    // reconcile is normally the background task; run a pass through the service used by the router
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let node: Option<uuid::Uuid> = None;
    let _ = node;
    let svc =
        musubi_fleet::FleetService::new(db.clone(), uuid::Uuid::now_v7(), "http://127.0.0.1:0", k);
    let (started, _) = svc.reconcile().await.unwrap();
    let _ = started; // another test binary's reconciler may have filled the buffer
    let (_, list) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/servers"),
        k,
        None,
    )
    .await;
    let servers = list["servers"].as_array().unwrap();
    let mine: Vec<&Value> = servers
        .iter()
        .filter(|s| s["state"] == "starting")
        .collect();
    assert!(!mine.is_empty(), "{list}"); // two reconcilers (ours + another test binary) may both have started one
    let sid = mine[0]["id"].as_str().unwrap().to_string();
    assert!(mine[0]["pid"].as_i64().is_some());
    // the game server reports ready through the SDK surface
    let req = Request::post("/v1/gameserver/ready")
        .header("x-musubi-server-key", k)
        .header("x-musubi-server-id", &sid)
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let (st, a) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/allocate"),
        k,
        Some(json!({"payload": {"match": "m1"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["server_id"], sid);
    assert_eq!(a["port"], 47000);
    let tok = a["connect_token"].as_str().unwrap().to_string();
    // the game server validates the one-time connect token
    let req = Request::post("/v1/gameserver/connect")
        .header("x-musubi-server-key", k)
        .header("x-musubi-server-id", &sid)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token": tok}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["payload"]["match"], "m1");
    let req = Request::post("/v1/gameserver/connect")
        .header("x-musubi-server-key", k)
        .header("x-musubi-server-id", &sid)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token": tok}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK,
        "token is shared by the group"
    );
    let req = Request::post("/v1/gameserver/connect")
        .header("x-musubi-server-key", k)
        .header("x-musubi-server-id", &sid)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"token": "wrong"}).to_string()))
        .unwrap();
    assert_eq!(
        app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let (_, gs) = call_server(
        &app,
        "GET",
        &format!("/v1/server/gameservers/{sid}"),
        k,
        None,
    )
    .await;
    assert_eq!(gs["state"], "in_game");
    // shutdown kills the process (via the service that owns it) and marks it terminated
    svc.sdk_shutdown(uuid::Uuid::parse_str(&sid).unwrap())
        .await
        .unwrap();
    let (_, gs) = call_server(
        &app,
        "GET",
        &format!("/v1/server/gameservers/{sid}"),
        k,
        None,
    )
    .await;
    assert_eq!(gs["state"], "terminated");
    let (st, _) = call_server(&app, "DELETE", &format!("/v1/server/fleets/{fid}"), k, None).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn fleet_autoscale_scales_down_idle_and_forecasts_demand() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let name = unique("fleet-as");
    let (st, f) = call_server(
        &app,
        "POST",
        "/v1/server/fleets",
        k,
        Some(json!({
            "name": name, "provider": "local", "buffer": 2, "max_servers": 4,
            "port_start": 47200, "port_end": 47300,
            "spec": {"command": ["sleep", "60"]}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["min_servers"], 0);
    assert_eq!(f["policy"]["forecast"], false);
    let fid = f["id"].as_str().unwrap().to_string();
    let fuid = uuid::Uuid::parse_str(&fid).unwrap();
    // bad shapes are rejected
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"min_servers": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"policy": {"schedule": [{"from_hour": 25, "to_hour": 2, "buffer": 1}]}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let svc =
        musubi_fleet::FleetService::new(db.clone(), uuid::Uuid::now_v7(), "http://127.0.0.1:0", k);
    svc.reconcile().await.unwrap();
    let live = |s: &Value| s["state"] != "terminated";
    let all = svc.servers(fuid, None).await.unwrap();
    assert!(
        all.iter().filter(|s| s.state != "terminated").count() >= 2,
        "{all:?}"
    );
    for s in all.iter().filter(|s| s.state == "starting") {
        svc.sdk_ready(s.id).await.unwrap();
    }
    assert!(svc.servers(fuid, Some("ready")).await.unwrap().len() >= 2);

    // buffer → 0: idle ready servers above the target are stopped, oldest first
    let (st, f) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"buffer": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let (_, reaped) = svc.reconcile().await.unwrap();
    assert!(reaped >= 2, "{reaped}");
    assert!(svc.servers(fuid, Some("ready")).await.unwrap().is_empty());
    let (_, l) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/servers"),
        k,
        None,
    )
    .await;
    assert_eq!(
        l["servers"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| live(s))
            .count(),
        0,
        "{l}"
    );

    // min_servers is a floor even with buffer 0
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"min_servers": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (started, _) = svc.reconcile().await.unwrap();
    assert_eq!(started, 1);
    for s in svc.servers(fuid, Some("starting")).await.unwrap() {
        svc.sdk_ready(s.id).await.unwrap();
    }
    let (_, reaped) = svc.reconcile().await.unwrap();
    assert_eq!(reaped, 0); // the floor is never scaled down

    // 12 allocations in this hour-of-week slot → 12/h; a 10-minute horizon wants 2 ready
    let now = time::OffsetDateTime::now_utc();
    for _ in 0..12 {
        svc.record_demand(fuid, now).await.unwrap();
    }
    let (st, f) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"policy": {"forecast": true, "horizon_min": 10}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let (st, d) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/demand"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["per_hour"], 12.0, "{d}");
    assert_eq!(d["target_ready"], 2, "{d}");
    assert_eq!(d["slots"].as_array().unwrap().len(), 1);
    let (started, _) = svc.reconcile().await.unwrap();
    assert_eq!(started, 1); // 1 ready already
                            // a week later the same slot folds the last count into its EWMA (0.3 × 12 = 3.6/h → 1 ready)
    let next_week = now + time::Duration::days(7);
    svc.record_demand(fuid, next_week).await.unwrap();
    let per_hour = svc.forecast_per_hour(fuid, next_week).await.unwrap();
    assert!((per_hour - 3.6).abs() < 1e-9, "{per_hour}");

    // a schedule window overrides the buffer for its hours
    let (st, f) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"policy": {"forecast": false, "schedule": [{"from_hour": 0, "to_hour": 0, "buffer": 3}]}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let (_, d) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/demand"),
        k,
        None,
    )
    .await;
    assert_eq!(d["buffer"], 3);
    assert_eq!(d["target_ready"], 3);

    let (st, _) = call_server(&app, "DELETE", &format!("/v1/server/fleets/{fid}"), k, None).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn fleet_docker_provider_runs_a_container() {
    let Some(app) = app().await else { return };
    if !musubi_fleet::docker::available().await {
        eprintln!("docker not available; skipping");
        return;
    }
    let k = "defaultkey";
    let name = unique("dockerfleet");
    // random high range so leftovers from other runs never collide
    let port = 50000 + (uuid::Uuid::now_v7().as_u128() % 10000) as i32;
    let (st, f) = call_server(
        &app,
        "POST",
        "/v1/server/fleets",
        k,
        Some(json!({
            "name": name, "provider": "docker", "buffer": 1, "max_servers": 1,
            "port_start": port, "port_end": port + 20,
            "spec": {"image": "alpine:3.20", "command": ["sleep", "60"]}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let fid = uuid::Uuid::parse_str(f["id"].as_str().unwrap()).unwrap();
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let svc = musubi_fleet::FleetService::new(db, uuid::Uuid::now_v7(), "http://127.0.0.1:0", k);
    let (started, _) = svc.reconcile().await.unwrap();
    let all = svc.servers(fid, None).await.unwrap();
    let _ = (started, all); // another test binary's reconciler may have filled the buffer
    let servers = svc.servers(fid, Some("starting")).await.unwrap();
    assert_eq!(servers.len(), 1);
    // the row is written before `docker run` returns (possibly by another test binary's reconciler)
    let mut running = false;
    for _ in 0..40 {
        let out = tokio::process::Command::new("docker")
            .args([
                "ps",
                "--filter",
                &format!("name=musubi-gs-{}", servers[0].id.simple()),
                "--format",
                "{{.Names}}",
            ])
            .output()
            .await
            .unwrap();
        if String::from_utf8_lossy(&out.stdout).contains("musubi-gs-") {
            running = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(running, "container should be running");
    svc.delete(fid).await.unwrap();
    let out = tokio::process::Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name=musubi-gs-{}", servers[0].id.simple()),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .await
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "container removed on delete"
    );
}

#[tokio::test]
async fn mail_audience_broadcast() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    // accounts.lang is capped at 16 chars
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let lang = format!("l{:x}", nanos % 0xFFFF_FFFF_FFFF);
    let other = format!("o{:x}", nanos % 0xFFFF_FFFF_FFFF);
    let (loc1, loc2, tag, track) = (
        unique("loc"),
        unique("loc"),
        unique("cohort"),
        unique("trk"),
    );
    let mut toks = Vec::new();
    let mut accs = Vec::new();
    for (l, loc, md) in [
        (lang.clone(), loc1.clone(), json!({"cohort": tag})),
        (lang.clone(), loc2.clone(), json!({})),
        (other, loc1.clone(), json!({"cohort": tag})),
    ] {
        let (_, sess) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("aud")})),
        )
        .await;
        let tok = sess["access_token"].as_str().unwrap().to_string();
        let (st, r) = call(
            &app,
            "PATCH",
            "/v1/account",
            Some(&tok),
            Some(json!({"lang": l, "location": loc, "metadata": md})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        accs.push(sess["account_id"].as_str().unwrap().to_string());
        toks.push(tok);
    }
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(json!({"id": track, "name": "XP", "levels": [{"xp": 100}, {"xp": 250}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": accs[0], "track_id": track, "delta": 120})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");

    // second account gets a progress row at level 0 so the max_level range has something to match
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": accs[1], "track_id": track, "delta": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let count = |aud: Value| {
        let app = app.clone();
        async move {
            let (st, r) =
                call_server(&app, "POST", "/v1/server/mail/audience/count", k, Some(aud)).await;
            assert_eq!(st, StatusCode::OK, "{r}");
            r["count"].as_i64().unwrap()
        }
    };
    assert_eq!(count(json!({"lang": [lang]})).await, 2);
    assert_eq!(count(json!({"lang": [lang], "location": [loc1]})).await, 1);
    assert_eq!(
        count(json!({"lang": [lang], "track": track, "min_level": 1})).await,
        1
    );
    assert_eq!(
        count(json!({"lang": [lang], "track": track, "max_level": 0})).await,
        1
    );
    assert_eq!(
        count(json!({"lang": [lang], "metadata": {"cohort": tag}})).await,
        1
    );
    assert_eq!(
        count(json!({"lang": [lang], "platform": ["device"]})).await,
        2
    );
    assert_eq!(
        count(json!({"lang": [lang], "platform": ["steam"]})).await,
        0
    );
    assert_eq!(
        count(json!({"lang": [lang], "active_within_days": 1})).await,
        2
    );
    assert_eq!(count(json!({"lang": [lang], "inactive_days": 1})).await, 0);
    assert_eq!(count(json!({"lang": [lang], "created_before": 0})).await, 0);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/mail/audience/count",
        k,
        Some(json!({"min_level": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let (st, r) = call_server(&app, "POST", "/v1/server/mail", k,
        Some(json!({"audience": {"lang": [lang], "location": [loc1]}, "title": "Region drop", "body": "hi"}))).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["sent"], 1);
    let (_, m) = call(&app, "GET", "/v1/mail", Some(&toks[0]), None).await;
    assert_eq!(m["mail"].as_array().unwrap().len(), 1, "{m}");
    let (_, m) = call(&app, "GET", "/v1/mail", Some(&toks[1]), None).await;
    assert!(m["mail"].as_array().unwrap().is_empty(), "{m}");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/mail",
        k,
        Some(json!({"audience": {"lang": [lang]}, "title": "Lang drop", "body": "hi"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["sent"], 2);
    let (_, m) = call(&app, "GET", "/v1/mail", Some(&toks[2]), None).await;
    assert!(m["mail"].as_array().unwrap().is_empty(), "{m}");
}

#[tokio::test]
async fn fraud_velocity_flags_and_blocks() {
    let gold = unique("fgold");
    let g = gold.clone();
    let Some(app) = app_with(move |c| {
        c.fraud.enabled = true;
        c.fraud.max_spend.insert(g, 250);
        c.fraud.action = "block".into();
    })
    .await
    else {
        return;
    };
    let k = "defaultkey";
    let (_, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("fraud")})),
    )
    .await;
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let (potion, offer) = (unique("fpotion"), unique("foffer"));
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold", "max_balance": 100000})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(json!({"account_id": acc, "currency": gold, "delta": 5000, "reason": "seed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": potion, "name": "Potion", "stackable": true, "max_stack": 999})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/store",
        k,
        Some(json!({"id": offer, "item_id": potion, "quantity": 1, "prices": {gold.clone(): 100}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // 100 + 100 = 200 < 250: allowed; the third attempt sees 200... then 300 >= 250 blocks the fourth
    for i in 0..3 {
        let (st, r) = call(&app, "POST", "/v1/store/buy", Some(&tok),
            Some(json!({"store_item_id": offer, "currency": gold, "idempotency_key": format!("b{i}")}))).await;
        assert_eq!(st, StatusCode::OK, "buy {i}: {r}");
    }
    let (st, r) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": offer, "currency": gold, "idempotency_key": "b3"})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{r}");
    let (st, f) = call_server(
        &app,
        "GET",
        &format!("/v1/server/economy/fraud/{acc}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert_eq!(f["spend"][&gold]["spent"], 300);
    assert_eq!(f["spend"][&gold]["limit"], 250);
    assert_eq!(f["flags"].as_array().unwrap().len(), 1, "{f}");
    assert_eq!(f["flags"][0]["rule"], format!("spend:{gold}"));
    assert_eq!(f["flags"][0]["blocked"], true);
    let id = f["flags"][0]["id"].as_str().unwrap().to_string();
    // still blocked, no duplicate open flag
    let (st, _) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": offer, "currency": gold, "idempotency_key": "b4"})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    let (_, open) = call_server(&app, "GET", "/v1/server/economy/fraud/flags", k, None).await;
    assert!(
        open["flags"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["id"] == id),
        "{open}"
    );
    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/server/economy/fraud/flags/{id}/resolve"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r["resolved_at"].is_number());
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/economy/fraud/flags/{id}/resolve"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // flag-only mode: purchases keep going, the flag is recorded
    let Some(app2) = app_with(|c| {
        c.fraud.enabled = true;
        c.fraud.max_purchases = 1;
    })
    .await
    else {
        return;
    };
    let (_, sess) = call(
        &app2,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("fraud2")})),
    )
    .await;
    let tok2 = sess["access_token"].as_str().unwrap().to_string();
    let acc2 = sess["account_id"].as_str().unwrap().to_string();
    let (st, _) = call_server(
        &app2,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(json!({"account_id": acc2, "currency": gold, "delta": 1000, "reason": "seed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for i in 0..3 {
        let (st, r) = call(&app2, "POST", "/v1/store/buy", Some(&tok2),
            Some(json!({"store_item_id": offer, "currency": gold, "idempotency_key": format!("c{i}")}))).await;
        assert_eq!(st, StatusCode::OK, "buy {i}: {r}");
    }
    let (_, f) = call_server(
        &app2,
        "GET",
        &format!("/v1/server/economy/fraud/{acc2}"),
        k,
        None,
    )
    .await;
    assert_eq!(f["purchases"]["count"], 3);
    assert_eq!(f["flags"].as_array().unwrap().len(), 1, "{f}");
    assert_eq!(f["flags"][0]["rule"], "purchases");
    assert_eq!(f["flags"][0]["blocked"], false);
}

#[tokio::test]
async fn economy_wallet_ledger_inventory_and_store() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("eco")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let sword = unique("sword");
    let potion = unique("potion");
    let offer = unique("offer");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold", "max_balance": 1000})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // grant with idempotency: the replay does not apply twice
    let (st, g1) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(json!({"account_id": acc, "currency": gold, "delta": 500, "reason": "daily", "idempotency_key": "d1"}))).await;
    assert_eq!(st, StatusCode::OK, "{g1}");
    assert_eq!(g1["entry"]["balance_after"], 500);
    let (_, g2) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(json!({"account_id": acc, "currency": gold, "delta": 500, "reason": "daily", "idempotency_key": "d1"}))).await;
    assert_eq!(g2["replayed"], true);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 500);
    // cap and no negative balance
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(json!({"account_id": acc, "currency": gold, "delta": 600})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(json!({"account_id": acc, "currency": gold, "delta": -501})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    // catalog: a unique sword and stackable potions
    let (st, _) = call_server(&app, "POST", "/v1/server/items", k, Some(json!({"id": sword, "name": "Sword", "category": "weapon", "stackable": false, "properties": {"dmg": 10}}))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": potion, "name": "Potion", "category": "consumable", "max_stack": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, s1) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": acc, "item_id": sword, "instance": {"durability": 100}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    let (_, _) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": acc, "item_id": potion, "quantity": 2})),
    )
    .await;
    let (_, p2) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": acc, "item_id": potion, "quantity": 2})),
    )
    .await;
    assert_eq!(p2["quantity"], 4, "stacks merge");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": acc, "item_id": potion, "quantity": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "max_stack 5");
    // store: buy a potion pack for 100 gold, idempotent
    let (st, _) = call_server(&app, "POST", "/v1/server/store", k, Some(json!({"id": offer, "item_id": potion, "quantity": 1, "prices": {gold.clone(): 100}, "max_per_account": 2}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, st_list) = call(&app, "GET", "/v1/store", Some(&tok), None).await;
    assert!(st_list["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["id"] == offer));
    let (st, b1) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": offer, "currency": gold, "idempotency_key": "buy1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b1}");
    assert_eq!(b1["balance"], 400);
    assert_eq!(b1["granted"]["quantity"], 5);
    let (_, b1r) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": offer, "currency": gold, "idempotency_key": "buy1"})),
    )
    .await;
    assert_eq!(b1r["replayed"], true);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(
        w["balances"][0]["balance"], 400,
        "replay did not charge twice"
    );
    // consume potions down, then the per-account purchase limit and ledger
    let pid = b1["granted"]["id"].as_str().unwrap();
    let (st, c) = call(
        &app,
        "POST",
        "/v1/inventory/consume",
        Some(&tok),
        Some(json!({"id": pid, "quantity": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(c["remaining"].is_null());
    let (st, _) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": offer, "currency": gold})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": offer, "currency": gold})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "max_per_account");
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/wallet/ledger?currency={gold}"),
        Some(&tok),
        None,
    )
    .await;
    let entries = l["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["delta"], -100);
    assert_eq!(entries[0]["reason"], "store.buy");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    assert!(inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["item_id"] == sword && i["instance"]["durability"] == 100));
}

#[tokio::test]
async fn rewards_tables_rolls_pity_and_audit() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("rw")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gems = unique("gems");
    let common = unique("common");
    let legend = unique("legend");
    let gacha = unique("gacha");
    let bonus = unique("bonus");
    for (code, _) in [(&gems, 0)] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/currencies",
            k,
            Some(json!({"code": code, "name": "Gems"})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    for id in [&common, &legend] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/items",
            k,
            Some(json!({"id": id, "name": id, "stackable": true, "max_stack": 999})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    // weighted gacha: legendary is 1 in 1_000_001 but pity forces it on the 3rd roll
    let (st, body) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({
            "id": gacha, "name": "Gacha", "kind": "weighted",
            "entries": [
                {"weight": 1_000_000, "rarity": "common", "item_id": common, "amount": 1},
                {"weight": 1, "rarity": "legendary", "item_id": legend, "amount": 1}
            ],
            "pity": {"after": 2, "rarity": "legendary"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // validation
    let (st, _) = call_server(&app, "POST", "/v1/server/rewards/tables", k, Some(json!({"id": unique("bad"), "kind": "weighted", "entries": [{"weight": 0, "item_id": common, "amount": 1}]}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // published odds
    let (st, odds) = call(&app, "GET", "/v1/rewards/tables", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK);
    let t = odds["tables"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == gacha)
        .unwrap();
    assert!(t["odds"][0]["percent"].as_f64().unwrap() > 99.99);
    assert_eq!(t["pity"]["after"], 2);
    // rolls 1-2 common, roll 3 forced legendary
    let mut hashes = vec![];
    for i in 0..3 {
        let (st, r) = call(
            &app,
            "POST",
            &format!("/v1/rewards/{gacha}/roll"),
            Some(&tok),
            Some(json!({"client_seed": "abc", "idempotency_key": format!("r{i}")})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        assert_eq!(r["nonce"], i + 1);
        assert_eq!(r["replayed"], false);
        let expected =
            musubi_economy::rewards::roll_hash(r["server_seed"].as_str().unwrap(), "abc", i + 1);
        assert_eq!(
            r["roll"], expected,
            "roll is verifiable from revealed seeds"
        );
        hashes.push(r["roll"].as_str().unwrap().to_string());
        if i < 2 {
            assert_eq!(r["rewards"][0]["rarity"], "common");
            assert_eq!(r["pity_forced"], false);
        } else {
            assert_eq!(r["rewards"][0]["rarity"], "legendary");
            assert_eq!(r["pity_forced"], true);
        }
    }
    // idempotent replay returns the same roll, no extra item
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/rewards/{gacha}/roll"),
        Some(&tok),
        Some(json!({"client_seed": "abc", "idempotency_key": "r2"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r["replayed"], true);
    assert_eq!(r["roll"], hashes[2]);
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    let items = inv["items"].as_array().unwrap();
    assert_eq!(
        items.iter().find(|i| i["item_id"] == common).unwrap()["quantity"],
        2
    );
    assert_eq!(
        items.iter().find(|i| i["item_id"] == legend).unwrap()["quantity"],
        1
    );
    // pity counter reset: the 4th roll is common again
    let (_, r) = call(
        &app,
        "POST",
        &format!("/v1/rewards/{gacha}/roll"),
        Some(&tok),
        Some(json!({"client_seed": "x"})),
    )
    .await;
    assert_eq!(r["pity_forced"], false);
    assert_eq!(r["nonce"], 4);
    // history
    let (_, h) = call(&app, "GET", "/v1/rewards/rolls?limit=10", Some(&tok), None).await;
    assert_eq!(h["rolls"].as_array().unwrap().len(), 4);
    // guaranteed table granted from the server: currency + item in one go, ledger reason
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({
            "id": bonus, "kind": "guaranteed",
            "entries": [{"currency": gems, "amount": 50}, {"item_id": common, "amount": 3}]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, g) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/grant",
        k,
        Some(json!({"account_id": acc, "table_id": bonus, "reference": "login-day-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    assert_eq!(g["rewards"].as_array().unwrap().len(), 2);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 50);
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/wallet/ledger?currency={gems}"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(l["entries"][0]["reason"], "reward.grant");
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/rewards/{}/roll", unique("nope")),
        Some(&tok),
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn progression_xp_levels_energy_and_streaks() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("pg")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gems = unique("gems");
    let bonus = unique("bonus");
    let track = unique("xp");
    let stamina = unique("stamina");
    let daily = unique("daily");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gems})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/rewards/tables", k, Some(json!({"id": bonus, "kind": "guaranteed", "entries": [{"currency": gems, "amount": 50}]}))).await;
    assert_eq!(st, StatusCode::OK);
    // XP track: level 1 at 100 xp (with reward), level 2 at 250
    let (st, b) = call_server(&app, "POST", "/v1/server/progression/tracks", k, Some(json!({"id": track, "name": "Player XP", "levels": [{"xp": 100, "rewards": bonus}, {"xp": 250}]}))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(json!({"id": unique("bad"), "levels": [{"xp": 100}, {"xp": 50}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc, "track_id": track, "delta": 120, "reason": "match"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["previous_level"], 0);
    assert_eq!(r["level_ups"], json!([1]));
    assert_eq!(r["progress"]["level"], 1);
    assert_eq!(r["progress"]["next_level_xp"], 250);
    assert_eq!(r["rewards"][0]["amount"], 50);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 50, "level reward granted");
    let (_, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc, "track_id": track, "delta": 200})),
    )
    .await;
    assert_eq!(r["level_ups"], json!([2]));
    assert!(r["progress"]["next_level_xp"].is_null());
    let (_, p) = call(&app, "GET", "/v1/progression", Some(&tok), None).await;
    let mine = p["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["track_id"] == track)
        .unwrap();
    assert_eq!(mine["xp"], 320);
    assert_eq!(mine["level"], 2);
    // energy: max 5, 1 unit per second
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/energy",
        k,
        Some(json!({"id": stamina, "max": 5, "regen_seconds": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, e) = call(&app, "GET", "/v1/energy", Some(&tok), None).await;
    let mine = e["energy"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["energy_id"] == stamina)
        .unwrap();
    assert_eq!(mine["amount"], 5, "starts full by default");
    let (st, e) = call(
        &app,
        "POST",
        &format!("/v1/energy/{stamina}/spend"),
        Some(&tok),
        Some(json!({"amount": 3})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    assert_eq!(e["amount"], 2);
    let (st, e) = call(
        &app,
        "POST",
        &format!("/v1/energy/{stamina}/spend"),
        Some(&tok),
        Some(json!({"amount": 3})),
    )
    .await;
    assert!(st.is_client_error(), "{e}");
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (_, e) = call(&app, "GET", "/v1/energy", Some(&tok), None).await;
    let mine = e["energy"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["energy_id"] == stamina)
        .unwrap();
    assert!(mine["amount"].as_i64().unwrap() >= 3, "{mine}");
    let (st, e) = call_server(
        &app,
        "POST",
        "/v1/server/energy/grant",
        k,
        Some(json!({"account_id": acc, "energy_id": stamina, "amount": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(e["amount"], 5, "grant is capped at max");
    assert!(e["next_in_seconds"].is_null());
    // streak: first claim hits the count-1 milestone, second claim in the same period is a conflict
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/streaks",
        k,
        Some(json!({"id": daily, "milestones": [{"count": 1, "rewards": bonus}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, s) = call(&app, "GET", "/v1/streaks", Some(&tok), None).await;
    let mine = s["streaks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["streak_id"] == daily)
        .unwrap();
    assert_eq!(mine["claimable"], true);
    assert_eq!(mine["count"], 0);
    let (st, c) = call(
        &app,
        "POST",
        &format!("/v1/streaks/{daily}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["count"], 1);
    assert_eq!(c["rewards"][0]["amount"], 50);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/streaks/{daily}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 100);
    let (_, s) = call(&app, "GET", "/v1/streaks", Some(&tok), None).await;
    let mine = s["streaks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["streak_id"] == daily)
        .unwrap();
    assert_eq!(mine["claimable"], false);
    assert_eq!(mine["count"], 1);
}

#[tokio::test]
async fn mailbox_claim_expire_and_achievements() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("mb")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gems = unique("gems");
    let bonus = unique("bonus");
    let ach = unique("ach");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gems})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/rewards/tables", k, Some(json!({"id": bonus, "kind": "guaranteed", "entries": [{"currency": gems, "amount": 50}]}))).await;
    assert_eq!(st, StatusCode::OK);
    // mail with rewards
    let (st, r) = call_server(&app, "POST", "/v1/server/mail", k, Some(json!({"account_ids": [acc], "title": "Welcome", "body": "Enjoy", "rewards": bonus, "metadata": {"tag": "onboarding"}}))).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["sent"], 1);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/mail",
        k,
        Some(json!({"account_ids": [acc], "title": "x", "rewards": unique("nope")})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, m) = call(&app, "GET", "/v1/mail?unread=true", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK);
    let list = m["mail"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["read"], false);
    assert_eq!(list[0]["metadata"]["tag"], "onboarding");
    let id = list[0]["id"].as_str().unwrap().to_string();
    // cannot delete while rewards are unclaimed
    let (st, _) = call(&app, "DELETE", &format!("/v1/mail/{id}"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/mail/{id}/read"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(r["read"], true);
    let (_, m) = call(&app, "GET", "/v1/mail?unread=true", Some(&tok), None).await;
    assert!(m["mail"].as_array().unwrap().is_empty());
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/mail/{id}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["rewards"][0]["amount"], 50);
    assert_eq!(r["mail"]["claimed"], true);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/mail/{id}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 50);
    let (st, _) = call(&app, "DELETE", &format!("/v1/mail/{id}"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK);
    // expiring mail disappears from the list and cannot be claimed
    let (st, _) = call_server(&app, "POST", "/v1/server/mail", k, Some(json!({"account_ids": [acc], "title": "Flash", "rewards": bonus, "expires_in_seconds": 1}))).await;
    assert_eq!(st, StatusCode::OK);
    let (_, m) = call(&app, "GET", "/v1/mail", Some(&tok), None).await;
    let id = m["mail"][0]["id"].as_str().unwrap().to_string();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (_, m) = call(&app, "GET", "/v1/mail", Some(&tok), None).await;
    assert!(m["mail"].as_array().unwrap().is_empty());
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/mail/{id}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&app, "DELETE", &format!("/v1/mail/{id}"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "expired mail can be deleted unclaimed");
    // achievements: unlock at target grants rewards exactly once
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/achievements",
        k,
        Some(json!({"id": ach, "name": "Win 3", "target": 3, "rewards": bonus})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let hidden = unique("secret");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/achievements",
        k,
        Some(json!({"id": hidden, "target": 1, "hidden": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, a) = call(&app, "GET", "/v1/achievements", Some(&tok), None).await;
    let mine = a["achievements"].as_array().unwrap();
    assert!(mine.iter().any(|x| x["achievement_id"] == ach));
    assert!(
        !mine.iter().any(|x| x["achievement_id"] == hidden),
        "hidden without progress"
    );
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/achievements/progress",
        k,
        Some(json!({"account_id": acc, "achievement_id": ach, "delta": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["progress"], 2);
    assert_eq!(r["unlocked"], false);
    let (_, r) = call_server(
        &app,
        "POST",
        "/v1/server/achievements/progress",
        k,
        Some(json!({"account_id": acc, "achievement_id": ach, "delta": 5})),
    )
    .await;
    assert_eq!(r["progress"], 3, "clamped to target");
    assert_eq!(r["unlocked"], true);
    assert_eq!(r["rewards"][0]["amount"], 50);
    let (_, r) = call_server(
        &app,
        "POST",
        "/v1/server/achievements/progress",
        k,
        Some(json!({"account_id": acc, "achievement_id": ach})),
    )
    .await;
    assert_eq!(r["unlocked"], false, "already unlocked");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 100);
    let (_, r) = call_server(
        &app,
        "POST",
        "/v1/server/achievements/progress",
        k,
        Some(json!({"account_id": acc, "achievement_id": hidden})),
    )
    .await;
    assert_eq!(r["unlocked"], true);
    let (_, a) = call(&app, "GET", "/v1/achievements", Some(&tok), None).await;
    let mine = a["achievements"].as_array().unwrap();
    let h = mine.iter().find(|x| x["achievement_id"] == hidden).unwrap();
    assert_eq!(h["unlocked"], true);
    assert!(h["unlocked_at"].is_string());
}

#[tokio::test]
async fn quests_by_events_and_battle_pass_tiers() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("qs")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gems = unique("gems");
    let bonus = unique("bonus");
    let daily = unique("daily");
    let once = unique("once");
    let track = unique("sxp");
    let season = unique("s1");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gems})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/rewards/tables", k, Some(json!({"id": bonus, "kind": "guaranteed", "entries": [{"currency": gems, "amount": 50}]}))).await;
    assert_eq!(st, StatusCode::OK);
    // quests: a daily needing two wins, a one-off needing one
    let (st, b) = call_server(&app, "POST", "/v1/server/quests", k, Some(json!({"id": daily, "name": "Win twice", "kind": "daily", "objectives": [{"event": "match_won", "target": 2}], "rewards": bonus}))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, _) = call_server(&app, "POST", "/v1/server/quests", k, Some(json!({"id": once, "kind": "one_off", "objectives": [{"event": "match_won", "target": 1}], "rewards": bonus}))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/quests", k, Some(json!({"id": unique("bad"), "kind": "monthly", "objectives": [{"event": "x", "target": 1}]}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/quests/event",
        k,
        Some(json!({"account_id": acc, "event": "match_won"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r["updated"]
        .as_array()
        .unwrap()
        .iter()
        .any(|q| q == &json!(daily)));
    assert!(
        r["completed"].as_array().unwrap().contains(&json!(once)),
        "{r}"
    );
    let (_, q) = call(&app, "GET", "/v1/quests", Some(&tok), None).await;
    let d = q["quests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["quest_id"] == daily)
        .unwrap();
    assert_eq!(d["progress"]["match_won"], 1);
    assert_eq!(d["completed"], false);
    assert!(d["resets_at"].is_string());
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/quests/{daily}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "not completed yet");
    let (_, r) = call_server(
        &app,
        "POST",
        "/v1/server/quests/event",
        k,
        Some(json!({"account_id": acc, "event": "match_won", "delta": 5})),
    )
    .await;
    assert!(
        r["completed"].as_array().unwrap().contains(&json!(daily)),
        "{r}"
    );
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/quests/{daily}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["rewards"][0]["amount"], 50);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/quests/{daily}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/quests/{once}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, q) = call(&app, "GET", "/v1/quests", Some(&tok), None).await;
    let d = q["quests"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["quest_id"] == daily)
        .unwrap();
    assert_eq!(d["progress"]["match_won"], 2, "capped at target");
    assert_eq!(d["claimed"], true);
    // battle pass on its own XP track
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(json!({"id": track, "levels": [{"xp": 100}, {"xp": 200}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let now = time::OffsetDateTime::now_utc();
    let fmt = |t: time::OffsetDateTime| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    };
    let (st, b) = call_server(&app, "POST", "/v1/server/seasons", k, Some(json!({
        "id": season, "name": "Season 1", "track_id": track,
        "starts_at": fmt(now - time::Duration::hours(1)), "ends_at": fmt(now + time::Duration::hours(1)),
        "levels": [{"level": 1, "free": bonus, "premium": bonus}, {"level": 2, "premium": bonus}]
    }))).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, c) = call(&app, "GET", "/v1/seasons/current", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(c["current"]["season"]["id"], season);
    assert_eq!(c["current"]["level"], 0);
    assert_eq!(c["current"]["premium"], false);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/seasons/{season}/claim"),
        Some(&tok),
        Some(json!({"level": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "level not reached");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc, "track_id": track, "delta": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/seasons/{season}/claim"),
        Some(&tok),
        Some(json!({"level": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["rewards"][0]["amount"], 50);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/seasons/{season}/claim"),
        Some(&tok),
        Some(json!({"level": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/seasons/{season}/claim"),
        Some(&tok),
        Some(json!({"level": 1, "tier": "premium"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "premium pass required");
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/seasons/{season}/premium"),
        k,
        Some(json!({"account_id": acc})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/seasons/{season}/claim"),
        Some(&tok),
        Some(json!({"level": 1, "tier": "premium"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/seasons/{season}/claim"),
        Some(&tok),
        Some(json!({"level": 2, "tier": "free"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "no free reward at level 2");
    let (_, c) = call(&app, "GET", "/v1/seasons/current", Some(&tok), None).await;
    assert_eq!(c["current"]["level"], 1);
    assert_eq!(c["current"]["premium"], true);
    assert_eq!(c["current"]["claimed"], json!(["free:1", "premium:1"]));
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(
        w["balances"][0]["balance"], 200,
        "daily + one-off + free + premium"
    );
}

#[tokio::test]
async fn cluster_lease_has_single_holder_and_moves_on_release() {
    let Some(_app) = app().await else { return };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let a = musubi_cluster::Cluster::new(db.clone(), "a", "local", vec![], "");
    let b = musubi_cluster::Cluster::new(db.clone(), "b", "local", vec![], "");
    let name = unique("lease");
    let ttl = std::time::Duration::from_secs(2);
    assert!(a.try_lease(&name, ttl).await.unwrap());
    assert!(!b.try_lease(&name, ttl).await.unwrap(), "held by a");
    assert!(a.try_lease(&name, ttl).await.unwrap(), "owner renews");
    a.release_lease(&name).await.unwrap();
    assert!(b.try_lease(&name, ttl).await.unwrap(), "free after release");
    assert!(!a.try_lease(&name, ttl).await.unwrap());
    // expiry: a short lease lapses and can be taken over
    let short = unique("lease");
    assert!(a
        .try_lease(&short, std::time::Duration::from_millis(300))
        .await
        .unwrap());
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert!(
        b.try_lease(&short, ttl).await.unwrap(),
        "expired lease is taken over"
    );
    b.release_lease(&name).await.unwrap();
    b.release_lease(&short).await.unwrap();
}

#[tokio::test]
async fn quic_bus_delivers_large_messages_and_keeps_postgres_fallback() {
    use std::time::Duration;
    let Some(_app) = app().await else { return };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let group = unique("g");
    let bus = |name: &str| {
        musubi_cluster::Cluster::new(db.clone(), name, "local", vec![], "")
            .with_group(&group)
            .with_bus("127.0.0.1:0".parse().unwrap(), None)
            .unwrap()
    };
    let a = Arc::new(bus("qa"));
    let b = Arc::new(bus("qb"));
    let c = Arc::new(
        musubi_cluster::Cluster::new(db.clone(), "qc", "local", vec![], "").with_group(&group),
    );
    assert!(a.bus_addr().is_some() && c.bus_addr().is_none());
    a.start().await.unwrap();
    b.start().await.unwrap();
    c.start().await.unwrap();
    a.refresh_peers().await.unwrap();
    b.refresh_peers().await.unwrap();
    c.refresh_peers().await.unwrap();
    let mut ra = a.subscribe();
    let mut rb = b.subscribe();
    let mut rc = c.subscribe();
    async fn next(
        rx: &mut tokio::sync::broadcast::Receiver<musubi_cluster::BusMessage>,
        topic: &str,
    ) -> musubi_cluster::BusMessage {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let m = rx.recv().await.unwrap();
                if m.topic == topic {
                    return m;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("no {topic} message"))
    }
    // 50 KiB is far above the 8000-byte NOTIFY cap: only the QUIC path can carry it.
    let big = unique("big");
    a.publish(&big, &json!({"data": "x".repeat(50_000)}))
        .await
        .unwrap();
    let m = next(&mut rb, &big).await;
    assert_eq!(m.origin, a.node_id());
    assert_eq!(m.payload["data"].as_str().unwrap().len(), 50_000);
    // small messages reach QUIC peers once and legacy (Postgres-only) peers via NOTIFY
    let small = unique("small");
    a.publish(&small, &json!({"n": 1})).await.unwrap();
    assert_eq!(next(&mut rb, &small).await.payload["n"], 1);
    assert_eq!(next(&mut rc, &small).await.payload["n"], 1);
    // b → a over QUIC, c → a over Postgres
    let t1 = unique("ba");
    b.publish(&t1, &json!({"from": "b"})).await.unwrap();
    assert_eq!(next(&mut ra, &t1).await.origin, b.node_id());
    let t2 = unique("ca");
    c.publish(&t2, &json!({"from": "c"})).await.unwrap();
    assert_eq!(next(&mut ra, &t2).await.origin, c.node_id());
    // no duplicates: b must not also see the Postgres copy of `small`
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut dup = 0;
    while let Ok(m) = rb.try_recv() {
        if m.topic == small {
            dup += 1;
        }
    }
    assert_eq!(dup, 0, "quic peer received the postgres copy too");
    a.stop().await;
    b.stop().await;
    c.stop().await;
}

#[tokio::test]
async fn swim_detects_dead_bus_peer_within_seconds() {
    use std::time::Duration;
    let Some(_app) = app().await else { return };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let group = unique("g");
    let bus = |name: &str| {
        Arc::new(
            musubi_cluster::Cluster::new(db.clone(), name, "local", vec![], "")
                .with_group(&group)
                .with_bus("127.0.0.1:0".parse().unwrap(), None)
                .unwrap(),
        )
    };
    let (a, b, c) = (bus("sa"), bus("sb"), bus("sc"));
    for n in [&a, &b, &c] {
        n.start().await.unwrap();
    }
    for n in [&a, &b, &c] {
        n.refresh_peers().await.unwrap();
    }
    let state_of = |nodes: &[musubi_cluster::NodeInfo], id: musubi_core::NodeId| {
        nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| (n.state.clone(), n.alive))
            .unwrap()
    };
    // after a few protocol periods everyone has probed/gossiped everyone
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let nodes = a.nodes().await.unwrap();
    assert_eq!(state_of(&nodes, b.node_id()), ("alive".into(), true));
    assert_eq!(state_of(&nodes, c.node_id()), ("alive".into(), true));
    assert_eq!(
        a.member_state(b.node_id().0).await,
        Some(musubi_cluster::State::Alive)
    );
    // c's network dies but its Postgres heartbeat keeps going: SWIM must still call it dead fast
    c.close_bus();
    let started = std::time::Instant::now();
    loop {
        let na = a.nodes().await.unwrap();
        let nb = b.nodes().await.unwrap();
        if state_of(&na, c.node_id()) == ("dead".into(), false)
            && state_of(&nb, c.node_id()) == ("dead".into(), false)
        {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "c not declared dead: a={:?} b={:?}",
            state_of(&na, c.node_id()),
            state_of(&nb, c.node_id())
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    tracing::info!(elapsed = ?started.elapsed(), "dead detected");
    // a and b still see each other alive; dead peers are skipped by the bus fan-out
    let na = a.nodes().await.unwrap();
    assert_eq!(state_of(&na, b.node_id()), ("alive".into(), true));
    let mut rb = b.subscribe();
    let topic = unique("after");
    a.publish(&topic, &json!({"ok": true})).await.unwrap();
    let m = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let m = rb.recv().await.unwrap();
            if m.topic == topic {
                return m;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(m.payload["ok"], true);
    a.stop().await;
    b.stop().await;
    c.stop().await;
}

#[tokio::test]
async fn leaving_leader_hands_over_raft_leadership() {
    use std::time::{Duration, Instant};
    let Some(_app) = app().await else { return };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let group = unique("g");
    let bus = |name: &str| {
        Arc::new(
            musubi_cluster::Cluster::new(db.clone(), name, "local", vec![], "")
                .with_group(&group)
                .with_bus("127.0.0.1:0".parse().unwrap(), None)
                .unwrap(),
        )
    };
    let nodes = [bus("la"), bus("lb"), bus("lc")];
    for n in &nodes {
        n.start().await.unwrap();
    }
    for n in &nodes {
        n.refresh_peers().await.unwrap();
    }
    let t0 = Instant::now();
    let leader = loop {
        let leaders: Vec<_> = nodes.iter().map(|n| n.raft_leader()).collect();
        let voters: Vec<_> = nodes.iter().map(|n| n.raft_voters().len()).collect();
        if leaders.iter().all(|l| l.is_some() && *l == leaders[0]) && voters.iter().all(|v| *v == 3)
        {
            break leaders[0].unwrap();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(90),
            "no stable 3-node raft: leaders={leaders:?} voters={voters:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // a leader that leaves voluntarily hands leadership over instead of letting the survivors
    // sit through an election timeout (3-5 s) while its worlds and matches wait for the catalog
    let leaving = nodes.iter().find(|n| n.raft_id() == leader).unwrap();
    let t1 = Instant::now();
    leaving.leave().await;
    let left_in = t1.elapsed();
    let survivors: Vec<_> = nodes.iter().filter(|n| n.raft_id() != leader).collect();
    let t2 = Instant::now();
    loop {
        let leaders: Vec<_> = survivors.iter().map(|n| n.raft_leader()).collect();
        if leaders
            .iter()
            .all(|l| l.is_some_and(|l| l != leader) && *l == leaders[0])
        {
            break;
        }
        assert!(
            t2.elapsed() < Duration::from_secs(3),
            "no new leader after the leader left: old={leader} leaders={leaders:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(left_in < Duration::from_secs(3), "leave took {left_in:?}");
    for n in survivors {
        n.leave().await;
    }
}

#[tokio::test]
async fn raft_directory_replicates_and_survives_leader_loss() {
    use std::time::{Duration, Instant};
    let Some(_app) = app().await else { return };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let group = unique("g");
    let bus = |name: &str| {
        Arc::new(
            musubi_cluster::Cluster::new(db.clone(), name, "local", vec![], "")
                .with_group(&group)
                .with_bus("127.0.0.1:0".parse().unwrap(), None)
                .unwrap(),
        )
    };
    let nodes = [bus("ra"), bus("rb"), bus("rc")];
    for n in &nodes {
        n.start().await.unwrap();
    }
    for n in &nodes {
        n.refresh_peers().await.unwrap();
    }
    // bootstrap + membership growth to 3 voters with one leader everyone agrees on
    let t0 = Instant::now();
    let leader = loop {
        let leaders: Vec<_> = nodes.iter().map(|n| n.raft_leader()).collect();
        let voters: Vec<_> = nodes.iter().map(|n| n.raft_voters().len()).collect();
        if leaders.iter().all(|l| l.is_some() && *l == leaders[0]) && voters.iter().all(|v| *v == 3)
        {
            break leaders[0].unwrap();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(90),
            "no stable 3-node raft: leaders={leaders:?} voters={voters:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    tracing::info!(elapsed = ?t0.elapsed(), leader, "raft formed");
    let follower = nodes.iter().find(|n| n.raft_id() != leader).unwrap();
    let key = unique("world:");
    // write through a follower (forwarded to the leader) and read everywhere
    follower
        .directory_write(musubi_cluster::DirectoryOp::Set {
            key: key.clone(),
            value: json!({"home": "n1"}),
        })
        .await
        .unwrap();
    let t1 = Instant::now();
    loop {
        let all = futures::future::join_all(nodes.iter().map(|n| n.directory_get(&key))).await;
        if all
            .iter()
            .all(|v| v.as_ref().is_some_and(|v| v["home"] == "n1"))
        {
            break;
        }
        assert!(
            t1.elapsed() < Duration::from_secs(10),
            "directory not replicated: {all:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        follower
            .directory_prefix("world:")
            .await
            .iter()
            .filter(|(k, _)| *k == key)
            .count(),
        1
    );
    // leader's network dies: survivors elect a new leader, drop the dead voter and keep writing
    let dead = nodes.iter().find(|n| n.raft_id() == leader).unwrap();
    dead.close_bus();
    let survivors: Vec<_> = nodes.iter().filter(|n| n.raft_id() != leader).collect();
    let t2 = Instant::now();
    let new_leader = loop {
        let leaders: Vec<_> = survivors.iter().map(|n| n.raft_leader()).collect();
        let voters: Vec<_> = survivors.iter().map(|n| n.raft_voters()).collect();
        if leaders
            .iter()
            .all(|l| l.is_some_and(|l| l != leader) && *l == leaders[0])
            && voters.iter().all(|v| v.len() == 2)
        {
            break leaders[0].unwrap();
        }
        assert!(
            t2.elapsed() < Duration::from_secs(40),
            "no re-election: leaders={leaders:?} voters={voters:?}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    tracing::info!(elapsed = ?t2.elapsed(), new_leader, "re-elected");
    let key2 = unique("world:");
    survivors[0]
        .directory_write(musubi_cluster::DirectoryOp::Set {
            key: key2.clone(),
            value: json!({"home": "n2"}),
        })
        .await
        .unwrap();
    let t3 = Instant::now();
    loop {
        let all = futures::future::join_all(survivors.iter().map(|n| n.directory_get(&key2))).await;
        if all
            .iter()
            .all(|v| v.as_ref().is_some_and(|v| v["home"] == "n2"))
        {
            break;
        }
        assert!(
            t3.elapsed() < Duration::from_secs(10),
            "post-failover write not replicated: {all:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let r = survivors[1]
        .directory_write(musubi_cluster::DirectoryOp::Delete { key: key.clone() })
        .await
        .unwrap();
    assert_eq!(r.value.unwrap()["home"], "n1");
    for n in &nodes {
        n.stop().await;
    }
}

#[tokio::test]
async fn raft_leader_owns_leases_and_world_directory() {
    use std::time::{Duration, Instant};
    let Some(_app) = app().await else { return };
    let db = sqlx::postgres::PgPoolOptions::new()
        .max_connections(6)
        .connect(&std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    let group = unique("g");
    let bus = |name: &str| {
        Arc::new(
            musubi_cluster::Cluster::new(db.clone(), name, "local", vec![], "")
                .with_group(&group)
                .with_bus("127.0.0.1:0".parse().unwrap(), None)
                .unwrap(),
        )
    };
    let (a, b) = (bus("la"), bus("lb"));
    a.start().await.unwrap();
    b.start().await.unwrap();
    a.refresh_peers().await.unwrap();
    b.refresh_peers().await.unwrap();
    let t0 = Instant::now();
    while !(a.raft_leader().is_some()
        && a.raft_leader() == b.raft_leader()
        && a.raft_voters().len() == 2)
    {
        assert!(t0.elapsed() < Duration::from_secs(90), "raft not formed");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    // duties follow the raft leader, not the Postgres CAS lease
    let name = unique("duty");
    let (la, lb) = (
        a.lease(&name, Duration::from_secs(2)),
        b.lease(&name, Duration::from_secs(2)),
    );
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_ne!(la.held(), lb.held(), "exactly one holder");
    assert_eq!(la.held(), a.is_raft_leader());
    assert_eq!(lb.held(), b.is_raft_leader());
    // a world created on a is resolvable on b through the directory alone (no DB on b's registry)
    let wa = musubi_server::realtime::WorldRegistry::new(a.node_id().0, Some(db.clone()));
    wa.set_cluster(a.clone());
    let wb = musubi_server::realtime::WorldRegistry::new(b.node_id().0, None);
    wb.set_cluster(b.clone());
    let world = unique("w");
    wa.create(&world, Default::default(), Default::default(), "")
        .await
        .unwrap();
    let t1 = Instant::now();
    loop {
        if wb.home_of(&world).await.unwrap() == Some(a.node_id().0) {
            break;
        }
        assert!(
            t1.elapsed() < Duration::from_secs(10),
            "world home not replicated"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    wa.delete(&world).await.unwrap();
    let t2 = Instant::now();
    loop {
        if b.directory_get(&format!("world:{world}")).await.is_none() {
            break;
        }
        assert!(
            t2.elapsed() < Duration::from_secs(10),
            "world home not removed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn market_listings_bids_buyout_settle_and_direct_trades() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut users = Vec::new();
    for _ in 0..3 {
        let (st, sess) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("mkt")})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        users.push((
            sess["access_token"].as_str().unwrap().to_string(),
            sess["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (seller, seller_id) = users[0].clone();
    let (buyer, buyer_id) = users[1].clone();
    let (other, other_id) = users[2].clone();
    let gold = unique("gold");
    let sword = unique("sword");
    let ore = unique("ore");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for (id, stackable) in [(&sword, false), (&ore, true)] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/items",
            k,
            Some(json!({"id": id, "name": id, "stackable": stackable, "max_stack": 99})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    for (acc, amount) in [(&buyer_id, 1000), (&other_id, 1000)] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/wallet/adjust",
            k,
            Some(json!({"account_id": acc, "currency": gold, "delta": amount})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, g) = call_server(&app, "POST", "/v1/server/inventory/grant", k, Some(json!({"account_id": seller_id, "item_id": sword, "quantity": 1, "instance": {"enchant": 3}}))).await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let sword_entry = g["id"].as_str().unwrap().to_string();
    let (st, g) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": seller_id, "item_id": ore, "quantity": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let ore_entry = g["id"].as_str().unwrap().to_string();

    // list the sword with a buyout; it leaves the seller's inventory immediately
    let (st, l1) = call(
        &app,
        "POST",
        "/v1/market/listings",
        Some(&seller),
        Some(json!({"inventory_id": sword_entry, "currency": gold, "buyout": 300})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l1}");
    let l1id = l1["id"].as_str().unwrap().to_string();
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&seller), None).await;
    assert!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["item_id"] != sword),
        "{inv}"
    );
    // seller cannot buy own listing; a bid below buyout on a buyout-only listing is rejected
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l1id}/buy"),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l1id}/bid"),
        Some(&buyer),
        Some(json!({"amount": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // search shows it; buyout pays the seller minus the 5% fee and delivers the exact instance
    let (_, found) = call(
        &app,
        "GET",
        &format!("/v1/market/listings?item_id={sword}"),
        Some(&buyer),
        None,
    )
    .await;
    assert_eq!(found["listings"].as_array().unwrap().len(), 1);
    let (st, sale) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l1id}/buy"),
        Some(&buyer),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sale}");
    assert_eq!(sale["price"], 300);
    assert_eq!(sale["fee"], 15);
    let (st, again) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l1id}/buy"),
        Some(&buyer),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "replay returns the same sale");
    assert_eq!(again["id"], sale["id"]);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l1id}/buy"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&buyer), None).await;
    assert_eq!(w["balances"][0]["balance"], 700);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&seller), None).await;
    assert_eq!(w["balances"][0]["balance"], 285);
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&buyer), None).await;
    let got = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["item_id"] == sword)
        .unwrap();
    assert_eq!(got["instance"]["enchant"], 3);
    let (_, notes) = call(&app, "GET", "/v1/notifications", Some(&seller), None).await;
    assert!(
        notes["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["code"] == 110),
        "{notes}"
    );

    // auction: 5 ore, min bid 10, 1 second; bids escrow money and refund the outbid player
    let (st, l2) = call(&app, "POST", "/v1/market/listings", Some(&seller), Some(json!({"inventory_id": ore_entry, "quantity": 5, "currency": gold, "min_bid": 10, "duration_secs": 1}))).await;
    assert_eq!(st, StatusCode::OK, "{l2}");
    let l2id = l2["id"].as_str().unwrap().to_string();
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&seller), None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["item_id"] == ore)
            .unwrap()["quantity"],
        5
    );
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l2id}/bid"),
        Some(&buyer),
        Some(json!({"amount": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "below min bid");
    let (st, b) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l2id}/bid"),
        Some(&buyer),
        Some(json!({"amount": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&buyer), None).await;
    assert_eq!(w["balances"][0]["balance"], 690, "bid escrowed");
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l2id}/bid"),
        Some(&other),
        Some(json!({"amount": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "must beat current bid");
    let (st, b) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l2id}/bid"),
        Some(&other),
        Some(json!({"amount": 40})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["bidder_id"], other_id);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&buyer), None).await;
    assert_eq!(w["balances"][0]["balance"], 700, "outbid refunded");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&other), None).await;
    assert_eq!(w["balances"][0]["balance"], 960);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l2id}/cancel"),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "cannot cancel with bids");
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{l2id}/bid"),
        Some(&buyer),
        Some(json!({"amount": 50})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "expired");
    let (st, settled) = call_server(&app, "POST", "/v1/server/market/settle", k, None).await;
    assert_eq!(st, StatusCode::OK, "{settled}");
    assert!(settled["listings"].as_i64().unwrap() >= 1);
    let (_, l2) = call(
        &app,
        "GET",
        &format!("/v1/market/listings/{l2id}"),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(l2["status"], "sold");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&other), None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["item_id"] == ore)
            .unwrap()["quantity"],
        5
    );
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&seller), None).await;
    assert_eq!(w["balances"][0]["balance"], 285 + 38, "40 minus 5% fee");
    let (_, sales) = call(&app, "GET", "/v1/market/sales", Some(&seller), None).await;
    assert_eq!(sales["sales"].as_array().unwrap().len(), 2);
    // a listing without bids expires back into the inventory
    let (st, l3) = call(&app, "POST", "/v1/market/listings", Some(&seller), Some(json!({"inventory_id": ore_entry, "quantity": 2, "currency": gold, "buyout": 1, "duration_secs": 1}))).await;
    assert_eq!(st, StatusCode::OK, "{l3}");
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (st, _) = call_server(&app, "POST", "/v1/server/market/settle", k, None).await;
    assert_eq!(st, StatusCode::OK);
    let (_, l3) = call(
        &app,
        "GET",
        &format!("/v1/market/listings/{}", l3["id"].as_str().unwrap()),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(l3["status"], "expired");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&seller), None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["item_id"] == ore)
            .unwrap()["quantity"],
        5
    );
    // cancel an open listing without bids
    let (st, l4) = call(
        &app,
        "POST",
        "/v1/market/listings",
        Some(&seller),
        Some(json!({"inventory_id": ore_entry, "quantity": 1, "currency": gold, "buyout": 9})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l4}");
    let (st, l4) = call(
        &app,
        "POST",
        &format!("/v1/market/listings/{}/cancel", l4["id"].as_str().unwrap()),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l4}");
    assert_eq!(l4["status"], "cancelled");

    // direct trade: buyer offers the sword + 100 gold for 3 ore; the offer is escrowed at once
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&buyer), None).await;
    let sword_now = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["item_id"] == sword)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, t) = call(
        &app,
        "POST",
        "/v1/trades",
        Some(&buyer),
        Some(json!({
            "to_account": seller_id,
            "offer_items": [{"inventory_id": sword_now}],
            "offer_currency": {gold.clone(): 100},
            "want_items": [{"item_id": ore, "quantity": 3}],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    let tid = t["id"].as_str().unwrap().to_string();
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&buyer), None).await;
    assert_eq!(w["balances"][0]["balance"], 600, "offered gold escrowed");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&buyer), None).await;
    assert!(inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["item_id"] != sword));
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/trades/{tid}/accept"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "only the recipient accepts");
    let (_, pending) = call(&app, "GET", "/v1/trades", Some(&seller), None).await;
    assert_eq!(pending["trades"].as_array().unwrap().len(), 1);
    let (st, t) = call(
        &app,
        "POST",
        &format!("/v1/trades/{tid}/accept"),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["status"], "accepted");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&seller), None).await;
    assert_eq!(w["balances"][0]["balance"], 285 + 38 + 100);
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&seller), None).await;
    let items = inv["items"].as_array().unwrap();
    assert_eq!(
        items.iter().find(|e| e["item_id"] == ore).unwrap()["quantity"],
        2
    );
    assert_eq!(
        items.iter().find(|e| e["item_id"] == sword).unwrap()["instance"]["enchant"],
        3
    );
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&buyer), None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["item_id"] == ore)
            .unwrap()["quantity"],
        3
    );
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/trades/{tid}/accept"),
        Some(&seller),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "no double accept");
    // a declined offer refunds the escrow; accepting without the wanted goods fails atomically
    let (st, t2) = call(&app, "POST", "/v1/trades", Some(&buyer), Some(json!({
        "to_account": other_id, "offer_currency": {gold.clone(): 50}, "want_items": [{"item_id": sword, "quantity": 1}],
    }))).await;
    assert_eq!(st, StatusCode::OK, "{t2}");
    let t2id = t2["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/trades/{t2id}/accept"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "other has no sword");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&buyer), None).await;
    assert_eq!(
        w["balances"][0]["balance"], 550,
        "still escrowed after failed accept"
    );
    let (st, t2) = call(
        &app,
        "POST",
        &format!("/v1/trades/{t2id}/decline"),
        Some(&other),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t2}");
    assert_eq!(t2["status"], "declined");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&buyer), None).await;
    assert_eq!(w["balances"][0]["balance"], 600, "refunded");
    let (_, notes) = call(&app, "GET", "/v1/notifications", Some(&seller), None).await;
    assert!(
        notes["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["code"] == 111),
        "{notes}"
    );
}

#[tokio::test]
async fn metrics_endpoint_exposes_prometheus_text() {
    let Some(app) = app().await else {
        return;
    };
    let res = app
        .clone()
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = app
        .clone()
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(res.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    let body =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(
        body.contains("musubi_http_requests_total{") && body.contains("route=\"/healthz\""),
        "{body}"
    );
    assert!(body.contains("musubi_realtime_sessions "), "{body}");
    assert!(
        body.contains("musubi_http_request_duration_seconds_bucket"),
        "{body}"
    );
}

#[tokio::test]
async fn admin_ban_unban_storage_export_purge_and_config_redaction() {
    let Some(app) = app().await else {
        return;
    };
    let srv = |req: axum::http::request::Builder| req.header("x-musubi-server-key", "defaultkey");
    let device = unique("admin-dev-");
    let auth = |body: Value| {
        Request::post("/v1/auth/device")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let res = app
        .clone()
        .oneshot(auth(json!({"id": device})))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let sess: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let refresh = sess["refresh_token"].as_str().unwrap().to_string();

    // search finds it by id
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!("/v1/admin/accounts?q={acc}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["accounts"][0]["id"], acc);
    // no server key → 401
    let res = app
        .clone()
        .oneshot(
            Request::get("/v1/admin/accounts")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 401);

    // admin writes a storage object owned by the player, lists and reads it back
    let res = app
        .clone()
        .oneshot(
            srv(Request::put("/v1/admin/storage/admincol/k1"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"owner_id": acc, "value": {"gold": 5}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!(
                "/v1/admin/storage/admincol?owner_id={acc}"
            )))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["objects"][0]["value"]["gold"], 5);
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!(
                "/v1/admin/storage/admincol/k1?owner_id={acc}"
            )))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // ban: refresh and re-auth are refused, status shows disabled_at
    let res = app
        .clone()
        .oneshot(
            srv(Request::post(format!("/v1/admin/accounts/{acc}/ban")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/refresh")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"refresh_token": refresh}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 401, "refresh sessions revoked on ban");
    let res = app
        .clone()
        .oneshot(auth(json!({"id": device})))
        .await
        .unwrap();
    assert_eq!(res.status(), 403, "banned account cannot log in");
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!("/v1/admin/accounts/{acc}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert!(v["status"]["disabled_at"].is_string(), "{v}");
    // unban → login works again
    let res = app
        .clone()
        .oneshot(
            srv(Request::post(format!("/v1/admin/accounts/{acc}/unban")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = app
        .clone()
        .oneshot(auth(json!({"id": device})))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);

    // export contains the account, its identity (no secret) and the storage object
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!("/v1/admin/accounts/{acc}/export")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["account"]["id"], acc);
    assert_eq!(v["tables"]["identities"][0]["subject"], device);
    assert!(v["tables"]["identities"][0].get("secret_hash").is_none());
    assert_eq!(v["tables"]["storage_objects"][0]["key"], "k1");
    assert!(v["tables"]["sessions"].as_array().unwrap().len() >= 2);

    // config dump redacts secrets
    let res = app
        .clone()
        .oneshot(
            srv(Request::get("/v1/admin/config"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["http"]["server_key"], "***");
    assert_eq!(v["session"]["secret"], "***");
    assert!(v["http"]["listen"].is_string());

    // purge: account and every row about it are gone
    let res = app
        .clone()
        .oneshot(
            srv(Request::delete(format!("/v1/admin/accounts/{acc}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!("/v1/admin/accounts/{acc}")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!(
                "/v1/admin/storage/admincol?owner_id={acc}"
            )))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["objects"].as_array().unwrap().len(), 0);
    // the device is free again: logging in creates a fresh account
    let res = app
        .clone()
        .oneshot(auth(json!({"id": device})))
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let sess: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_ne!(sess["account_id"], acc);
    assert_eq!(sess["created"], true);
}

#[tokio::test]
async fn rpc_host_api_reaches_storage_and_accounts() {
    let Some(app) = app().await else {
        return;
    };
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/device")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"id": unique("host-dev-")}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let sess: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let (tok, acc) = (
        sess["access_token"].as_str().unwrap().to_string(),
        sess["account_id"].as_str().unwrap().to_string(),
    );
    let rpc = |body: Value| {
        Request::post("/v1/rpc/host")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let run = |req: Request<Body>| {
        let app = app.clone();
        async move {
            let res = app.oneshot(req).await.unwrap();
            assert_eq!(res.status(), 200);
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap();
            v
        }
    };
    let v = run(rpc(
        json!({"op": "storage.write", "collection": "rt", "key": "k1", "value": {"a": 1}}),
    ))
    .await;
    assert!(v["ok"].is_array(), "{v}");
    // owner defaults to the calling account, so the player can read it back through the API
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/storage/read")
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"objects": [{"collection": "rt", "key": "k1"}]}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let v = run(rpc(
        json!({"op": "storage.read", "collection": "rt", "key": "k1"}),
    ))
    .await;
    assert_eq!(v["ok"]["value"]["a"], 1, "{v}");
    assert_eq!(v["ok"]["owner_id"], acc);
    let v = run(rpc(json!({"op": "account.get"}))).await;
    assert_eq!(v["ok"]["id"], acc, "{v}");
    let v = run(rpc(
        json!({"op": "notify.send", "subject": "hi", "code": 7}),
    ))
    .await;
    assert_eq!(v["ok"]["sent"], true, "{v}");
    let v = run(rpc(json!({"op": "nope"}))).await;
    assert!(
        v["error"].as_str().unwrap().contains("unknown host op"),
        "{v}"
    );
    let v = run(rpc(
        json!({"op": "storage.delete", "collection": "rt", "key": "k1"}),
    ))
    .await;
    assert_eq!(v["ok"]["deleted"], true);
    let v = run(rpc(
        json!({"op": "storage.read", "collection": "rt", "key": "k1"}),
    ))
    .await;
    assert!(v["ok"].is_null(), "{v}");
}

#[tokio::test]
async fn audit_log_records_admin_and_server_mutations_with_redaction() {
    let Some(app) = app().await else {
        return;
    };
    let srv = |req: axum::http::request::Builder| req.header("x-musubi-server-key", "defaultkey");
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/device")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"id": unique("audit-dev-")}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let sess: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let actor = unique("ops-");
    // admin mutation with an operator identity and a body carrying a secret-looking field
    let res = app
        .clone()
        .oneshot(
            srv(Request::put(format!("/v1/admin/storage/auditcol/{acc}")))
                .header("x-musubi-actor", &actor)
                .header("x-forwarded-for", "203.0.113.7, 10.0.0.1")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"owner_id": acc, "value": {"password": "hunter2", "level": 3}})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    // server mutation without an actor header → server-key; a 4xx is recorded too
    let res = app
        .clone()
        .oneshot(
            srv(Request::post(format!(
                "/v1/admin/accounts/{}/ban",
                uuid::Uuid::now_v7()
            )))
            .header("x-musubi-actor", &actor)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 404);
    // reads are not audited
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!("/v1/admin/accounts/{acc}")))
                .header("x-musubi-actor", &actor)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    // audit rows are written asynchronously; poll instead of guessing a delay
    let mut v = Value::Null;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let res = app
            .clone()
            .oneshot(
                srv(Request::get(format!("/v1/admin/audit?actor={actor}")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        v = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
        if v["entries"].as_array().is_some_and(|e| e.len() >= 2) {
            break;
        }
    }
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{v}");
    let ban = &entries[0];
    assert_eq!(ban["route"], "/v1/admin/accounts/{id}/ban");
    assert_eq!(ban["status"], 404);
    assert_eq!(ban["method"], "POST");
    let put = &entries[1];
    assert_eq!(put["route"], "/v1/admin/storage/{collection}/{key}");
    assert_eq!(put["ip"], "203.0.113.7");
    assert_eq!(put["body"]["value"]["password"], "***");
    assert_eq!(put["body"]["value"]["level"], 3);
    assert_eq!(put["body"]["owner_id"], acc, "ids are not redacted");
    // pagination by cursor
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!(
                "/v1/admin/audit?actor={actor}&limit=1"
            )))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["entries"].as_array().unwrap().len(), 1);
    let cursor = v["cursor"].as_str().unwrap().to_string();
    let res = app
        .clone()
        .oneshot(
            srv(Request::get(format!(
                "/v1/admin/audit?actor={actor}&limit=1&cursor={cursor}"
            )))
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(
        v["entries"][0]["route"],
        "/v1/admin/storage/{collection}/{key}"
    );
    // the audit endpoint itself is read-only and server-key gated
    let res = app
        .clone()
        .oneshot(Request::get("/v1/admin/audit").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn webhooks_deliver_signed_events_with_backoff_and_pull() {
    let Some(app) = app().await else {
        return;
    };
    // receiver: /ok records, /bad answers 500
    #[derive(Default)]
    struct Recv {
        hits: std::sync::Mutex<Vec<(axum::http::HeaderMap, String)>>,
    }
    let recv = Arc::new(Recv::default());
    let r2 = recv.clone();
    let hook = Router::new()
        .route(
            "/ok",
            axum::routing::post(move |headers: axum::http::HeaderMap, body: String| {
                let r = r2.clone();
                async move {
                    r.hits.lock().unwrap().push((headers, body));
                    axum::http::StatusCode::NO_CONTENT
                }
            }),
        )
        .route(
            "/bad",
            axum::routing::post(|| async { axum::http::StatusCode::INTERNAL_SERVER_ERROR }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, hook).await.unwrap() });

    let srv = |req: axum::http::request::Builder| req.header("x-musubi-server-key", "defaultkey");
    let post = |path: String, body: Value| {
        srv(Request::post(path))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let read = |res: axum::response::Response| async move {
        let st = res.status();
        let v: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
            .unwrap_or(Value::Null);
        (st, v)
    };
    let secret = "whsec_0123456789abcdef";
    let (st, v) = read(app.clone().oneshot(post("/v1/admin/webhooks".into(), json!({"name": "ok", "url": format!("http://{addr}/ok"), "secret": secret, "kinds": ["account.banned"]}))).await.unwrap()).await;
    assert_eq!(st, 200, "{v}");
    let good = v["id"].as_str().unwrap().to_string();
    let (st, v) = read(
        app.clone()
            .oneshot(post(
                "/v1/admin/webhooks".into(),
                json!({"name": "bad", "url": format!("http://{addr}/bad"), "secret": secret}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let bad = v["id"].as_str().unwrap().to_string();
    let (st, _) = read(
        app.clone()
            .oneshot(post(
                "/v1/admin/webhooks".into(),
                json!({"name": "x", "url": "ftp://nope", "secret": secret}),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 400);

    // an account gets banned → account.banned event fans out to both hooks
    let (_, sess) = read(
        app.clone()
            .oneshot(
                Request::post("/v1/auth/device")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json!({"id": unique("wh-dev-")}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let (st, _) = read(
        app.clone()
            .oneshot(
                srv(Request::post(format!("/v1/admin/accounts/{acc}/ban")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200);
    let (st, v) = read(
        app.clone()
            .oneshot(post("/v1/server/webhooks/dispatch".into(), json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200);
    assert!(
        v["delivered"].as_u64().unwrap() >= 1 && v["failed"].as_u64().unwrap() >= 1,
        "{v}"
    );

    // the good receiver got a correctly signed body
    let hits = recv.hits.lock().unwrap().clone();
    let (headers, body) = hits
        .iter()
        .find(|(h, _)| {
            h.get("x-musubi-event")
                .map(|v| v == "account.banned")
                .unwrap_or(false)
        })
        .expect("delivered");
    let ev: Value = serde_json::from_str(body).unwrap();
    assert_eq!(ev["kind"], "account.banned");
    assert_eq!(ev["account_id"], acc);
    let sig = headers["x-musubi-signature"].to_str().unwrap();
    let ts: i64 = sig
        .split(',')
        .next()
        .unwrap()
        .trim_start_matches("t=")
        .parse()
        .unwrap();
    assert_eq!(sig, musubi_server::events::sign(secret, ts, body));
    assert_eq!(
        headers["x-musubi-event-id"].to_str().unwrap(),
        ev["id"].as_str().unwrap()
    );

    // delivery bookkeeping: good delivered, bad retried with backoff
    let (_, v) = read(
        app.clone()
            .oneshot(
                srv(Request::get(format!(
                    "/v1/admin/webhooks/{good}/deliveries"
                )))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let d = v["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "account.banned")
        .unwrap()
        .clone();
    assert!(d["delivered_at"].is_string(), "{d}");
    assert_eq!(d["last_status"], 204);
    let (_, v) = read(
        app.clone()
            .oneshot(
                srv(Request::get(format!("/v1/admin/webhooks/{bad}/deliveries")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let d = v["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "account.banned")
        .unwrap()
        .clone();
    assert!(d["delivered_at"].is_null());
    assert_eq!(d["attempts"], 1);
    assert_eq!(d["last_status"], 500);
    assert_eq!(d["dead"], false);
    // not due again yet → a second dispatch leaves it alone (the global counters may include
    // other tests' events landing on this all-kinds hook, so check the row itself)
    let (_, _) = read(
        app.clone()
            .oneshot(post("/v1/server/webhooks/dispatch".into(), json!({})))
            .await
            .unwrap(),
    )
    .await;
    let (_, v) = read(
        app.clone()
            .oneshot(
                srv(Request::get(format!("/v1/admin/webhooks/{bad}/deliveries")))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    let d = v["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "account.banned")
        .unwrap()
        .clone();
    assert_eq!(d["attempts"], 1, "{d}");
    assert!(d["delivered_at"].is_null());

    // listing hides secrets; test endpoint delivers immediately; pull returns the event
    let (_, v) = read(
        app.clone()
            .oneshot(
                srv(Request::get("/v1/admin/webhooks"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(v["webhooks"]
        .as_array()
        .unwrap()
        .iter()
        .all(|w| w.get("secret").is_none()));
    let (st, v) = read(
        app.clone()
            .oneshot(post(format!("/v1/admin/webhooks/{good}/test"), json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(st, 200);
    assert_eq!(v["delivered"], 1, "{v}");
    let (_, v) = read(
        app.clone()
            .oneshot(
                srv(Request::get(
                    "/v1/server/events?kinds=account.banned&limit=1000",
                ))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(
        v["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["account_id"] == acc),
        "{v}"
    );
    let cursor = v["cursor"].as_str().unwrap().to_string();
    let (_, v) = read(
        app.clone()
            .oneshot(
                srv(Request::get(format!(
                    "/v1/server/events?after={cursor}&kinds=account.banned"
                )))
                .body(Body::empty())
                .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["events"].as_array().unwrap().len(), 0);
    // cleanup
    for id in [good, bad] {
        let (st, _) = read(
            app.clone()
                .oneshot(
                    srv(Request::delete(format!("/v1/admin/webhooks/{id}")))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(st, 200);
    }
}

#[tokio::test]
async fn ban_takes_effect_on_the_next_request_despite_valid_jwt() {
    let Some(app) = app().await else {
        return;
    };
    let srv = |req: axum::http::request::Builder| req.header("x-musubi-server-key", "defaultkey");
    let res = app
        .clone()
        .oneshot(
            Request::post("/v1/auth/device")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"id": unique("ban-dev-")}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let sess: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let me = |app: axum::Router, tok: String| async move {
        app.oneshot(
            Request::get("/v1/account")
                .header(header::AUTHORIZATION, format!("Bearer {tok}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    };
    assert_eq!(me(app.clone(), tok.clone()).await, 200);
    let res = app
        .clone()
        .oneshot(
            srv(Request::post(format!("/v1/admin/accounts/{acc}/ban")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        me(app.clone(), tok.clone()).await,
        403,
        "same JWT, banned now"
    );
    let res = app
        .clone()
        .oneshot(
            srv(Request::post(format!("/v1/admin/accounts/{acc}/unban")))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(me(app.clone(), tok).await, 200);
}

#[tokio::test]
async fn metrics_endpoint_can_be_locked_down() {
    let Some(open) = app().await else {
        return;
    };
    let scrape = |app: Router, key: Option<&str>, ip: &str| {
        let mut req = Request::get("/metrics").header("x-forwarded-for", ip);
        if let Some(k) = key {
            req = req.header("x-musubi-server-key", k);
        }
        let req = req.body(Body::empty()).unwrap();
        async move { app.oneshot(req).await.unwrap().status().as_u16() }
    };
    assert_eq!(
        scrape(open.clone(), None, "1.2.3.4").await,
        200,
        "open by default"
    );

    let keyed = app_with(|c| c.metrics.require_server_key = true)
        .await
        .unwrap();
    assert_eq!(scrape(keyed.clone(), None, "1.2.3.4").await, 401);
    assert_eq!(scrape(keyed.clone(), Some("nope"), "1.2.3.4").await, 401);
    assert_eq!(
        scrape(keyed.clone(), Some("defaultkey"), "1.2.3.4").await,
        200
    );

    let ranged = app_with(|c| c.metrics.allow_ips = vec!["10.".into(), "192.168.1.7".into()])
        .await
        .unwrap();
    assert_eq!(scrape(ranged.clone(), None, "10.4.5.6").await, 200);
    assert_eq!(scrape(ranged.clone(), None, "192.168.1.7").await, 200);
    assert_eq!(scrape(ranged.clone(), None, "192.168.1.70").await, 403);
    assert_eq!(scrape(ranged.clone(), None, "1.2.3.4").await, 403);

    let off = app_with(|c| c.metrics.enabled = false).await.unwrap();
    assert_eq!(scrape(off, Some("defaultkey"), "10.0.0.1").await, 404);
}

/// WASM modules uploaded through the admin API go live immediately, persist in the database for
/// nodes that start later, and can be removed again.
#[tokio::test]
async fn modules_hot_load_over_admin_api() {
    let Some(app) = app().await else {
        return;
    };
    let put = |app: Router, name: &str, bytes: &'static [u8]| {
        let req = Request::put(format!("/v1/admin/runtime/modules/{name}"))
            .header("x-musubi-server-key", "defaultkey")
            .header(header::CONTENT_TYPE, "application/wasm")
            .body(Body::from(bytes))
            .unwrap();
        async move {
            let res = app.oneshot(req).await.unwrap();
            let st = res.status();
            let v: Value =
                serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
                    .unwrap_or(Value::Null);
            (st, v)
        }
    };
    let (st, v) = put(app.clone(), "hot", musubi_runtime::EXAMPLE_WAT.as_bytes()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["rpcs"].as_array().unwrap().iter().any(|r| r == "echo"),
        "{v}"
    );
    assert_eq!(v["size"], musubi_runtime::EXAMPLE_WAT.len());
    let (st, v) = put(app.clone(), "bad.name", b"(module)").await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = put(app.clone(), "broken", b"(module (func))").await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "needs memory+alloc: {v}");

    let (st, list) =
        call_server(&app, "GET", "/v1/admin/runtime/modules", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let hot = list["stored"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "hot")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(hot["loaded"], true, "{list}");
    assert!(list["loaded"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m == "hot"));

    // a node starting later picks the module up from the database
    let mut cfg = Config::default();
    cfg.database.url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    cfg.database.max_connections = 3;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    let fresh = Arc::new(musubi_runtime::Runtime::new(musubi_runtime::Limits::default()).unwrap());
    let m = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    assert!(!fresh.has_rpc("echo"));
    let n = musubi_server::modules::load_all(&db, &fresh, &m)
        .await
        .unwrap();
    assert!(n >= 1, "{n}");
    assert!(fresh.has_rpc("echo"));

    let (st, v) = call_server(
        &app,
        "DELETE",
        "/v1/admin/runtime/modules/hot",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call_server(
        &app,
        "DELETE",
        "/v1/admin/runtime/modules/hot",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, list) = call_server(&app, "GET", "/v1/admin/runtime/modules", "defaultkey", None).await;
    assert!(
        !list["loaded"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m == "hot"),
        "{list}"
    );
    assert!(
        !list["stored"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "hot"),
        "{list}"
    );
}

/// Social graph, groups and leaderboard writes leave events in the outbox.
#[tokio::test]
async fn social_and_leaderboard_events_reach_the_outbox() {
    // uuid v7 ids are time-ordered: everything this test produces sorts after this cursor
    let after = uuid::Uuid::now_v7();
    let Some(app) = app().await else {
        return;
    };
    let auth = |app: Router, tag: &'static str| async move {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique(tag)})),
        )
        .await;
        (
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        )
    };
    let (ta, _) = auth(app.clone(), "ev-a-").await;
    let (_, b_id) = auth(app.clone(), "ev-b-").await;
    let (st, v) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(&ta),
        Some(json!({"ids": [b_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "DELETE",
        "/v1/friends",
        Some(&ta),
        Some(json!({"ids": [b_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(&ta),
        Some(json!({"name": unique("ev-group-"), "description": "", "open": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/leave"),
        Some(&ta),
        Some(json!({})),
    )
    .await;
    assert!(
        st == StatusCode::OK || st == StatusCode::BAD_REQUEST || st == StatusCode::CONFLICT,
        "{v}"
    );
    let lb = unique("ev-lb-");
    let (st, b) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        "defaultkey",
        Some(json!({"id": lb, "sort_order": "desc", "operator": "best"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&ta),
        Some(json!({"score": 42})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");

    let (st, ev) = call_server(
        &app,
        "GET",
        &format!("/v1/server/events?kinds=friend.added,friend.removed,group.created,leaderboard.record&limit=1000&after={after}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ev}");
    let kinds: Vec<&str> = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    for k in [
        "friend.added",
        "friend.removed",
        "group.created",
        "leaderboard.record",
    ] {
        assert!(kinds.contains(&k), "missing {k}: {kinds:?}");
    }
    let lb_ev = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "leaderboard.record" && e["payload"]["leaderboard_id"] == lb)
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(lb_ev["payload"]["record"]["score"], 42, "{lb_ev}");
}

/// Deterministic test match: state = counter, +1 per tick and +1 per input; label mirrors it.
struct Counter;

impl musubi_actor::MatchHandler for Counter {
    fn init(&self, _params: &Value) -> musubi_core::Result<musubi_actor::Init> {
        Ok(musubi_actor::Init {
            state: 0u64.to_le_bytes().to_vec(),
            tick_rate: 30,
            label: "0".into(),
        })
    }
    fn join_attempt(
        &self,
        state: Vec<u8>,
        _presence: &musubi_proto::UserPresence,
        _metadata: &std::collections::HashMap<String, String>,
        _members: &[musubi_proto::UserPresence],
    ) -> musubi_core::Result<(Vec<u8>, musubi_core::Result<()>)> {
        Ok((state, Ok(())))
    }
    fn join(
        &self,
        state: Vec<u8>,
        _joined: &[musubi_proto::UserPresence],
        _members: &[musubi_proto::UserPresence],
    ) -> musubi_core::Result<Vec<u8>> {
        Ok(state)
    }
    fn leave(
        &self,
        state: Vec<u8>,
        _left: &[musubi_proto::UserPresence],
        _members: &[musubi_proto::UserPresence],
    ) -> musubi_core::Result<Vec<u8>> {
        Ok(state)
    }
    fn tick(
        &self,
        state: Vec<u8>,
        _tick: u64,
        inputs: Vec<musubi_actor::Input>,
        _members: &[musubi_proto::UserPresence],
    ) -> musubi_core::Result<musubi_actor::TickResult> {
        let n = u64::from_le_bytes(state.try_into().unwrap_or([0; 8])) + 1 + inputs.len() as u64;
        Ok(musubi_actor::TickResult {
            state: n.to_le_bytes().to_vec(),
            outbound: vec![],
            label: Some(n.to_string()),
            terminate: false,
        })
    }
    fn terminate(&self, state: Vec<u8>, _grace_seconds: u32) -> musubi_core::Result<Vec<u8>> {
        Ok(state)
    }
}

/// Failover replays the journal on top of the last snapshot: the resumed match is at (about) the
/// tick the dead node reached, not at the snapshot taken when the player joined.
#[tokio::test]
async fn match_failover_replays_the_journal() {
    let Ok(url) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let mut cfg = Config::default();
    cfg.database.url = url;
    cfg.database.max_connections = 3;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let node_a = uuid::Uuid::now_v7();
    let hub = Arc::new(musubi_server::realtime::Hub::new());
    let mut a = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), node_a);
    a.snapshot_every = 100_000;
    a.journal_every = 3;
    a.register("counter", Arc::new(Counter));
    let h = a.create(hub.clone(), "counter", json!({})).unwrap();
    let me: musubi_proto::UserPresence = serde_json::from_value(json!({
        "account_id": uuid::Uuid::now_v7(), "session_id": uuid::Uuid::now_v7(), "username": "p1", "status": null
    }))
    .unwrap();
    a.join(&me, &h.id.to_string(), Default::default(), None)
        .await
        .unwrap();
    for _ in 0..4 {
        h.data(musubi_actor::Input {
            presence: me.clone(),
            op_code: 1,
            data: vec![1],
            reliable: true,
        })
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let before: u64 = h.info().await.label.parse().unwrap();
    assert!(before >= 15, "{before}");
    let rows: i64 =
        sqlx::query_scalar("select count(*) from musubi.match_journal where match_id = $1")
            .bind(h.id.0)
            .fetch_one(&db)
            .await
            .unwrap();
    assert!(rows >= 2, "journal chunks: {rows}");

    let mut b = musubi_server::realtime::MatchRegistry::new(Some(db.clone()), uuid::Uuid::now_v7());
    b.snapshot_every = 100_000;
    b.register("counter", Arc::new(Counter));
    let resumed = b.recover(hub.clone(), &[node_a]).await.unwrap();
    assert_eq!(resumed, vec![h.id.to_string()]);
    let hb = b.get(&h.id.to_string()).unwrap();
    let after: u64 = hb.info().await.label.parse().unwrap();
    // snapshot was taken at the join (counter ≈ 0); the journal brings it back to within one
    // flush interval of where node A was, inputs included
    assert!(
        after + 4 >= before,
        "resumed at {after}, node A was at {before}"
    );
    assert!(after >= 10, "{after}");
}

fn assert_required(doc: &Value, schema: &str, v: &Value) {
    let req = doc["components"]["schemas"][schema]["required"]
        .as_array()
        .unwrap();
    for k in req {
        assert!(
            v.get(k.as_str().unwrap()).is_some(),
            "{schema}.{k} missing in {v}"
        );
    }
}

/// Typed OpenAPI schemas describe what the server actually returns.
#[tokio::test]
async fn openapi_schemas_match_real_responses() {
    let Some(app) = app().await else {
        return;
    };
    let (st, doc) = call(&app, "GET", "/openapi.json", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(doc["openapi"], "3.1.0");
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("oa-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    assert_required(&doc, "Session", &sess);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let (st, acc) = call(&app, "GET", "/v1/account", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{acc}");
    assert_required(&doc, "Account", &acc);
    let lb = unique("oa-lb-");
    let (st, board) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        "defaultkey",
        Some(json!({"id": lb, "sort_order": "desc", "operator": "best"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{board}");
    assert_required(&doc, "Leaderboard", &board);
    let (st, rec) = call(
        &app,
        "POST",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&tok),
        Some(json!({"score": 7})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rec}");
    assert_required(&doc, "Record", &rec);
    let (st, list) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/records"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_required(&doc, "RecordList", &list);
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(&tok),
        Some(json!({"name": unique("oa-g-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    assert_required(&doc, "Group", &g);
    let (st, friends) = call(&app, "GET", "/v1/friends", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{friends}");
    assert_required(&doc, "FriendList", &friends);
    let (st, v) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "Wallet", &v);
    let (st, v) = call(&app, "GET", "/v1/wallet/ledger", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "Ledger", &v);
    let (st, v) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "Inventory", &v);
    let (st, v) = call(&app, "GET", "/v1/store", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "Store", &v);
    for it in v["items"].as_array().unwrap() {
        assert_required(&doc, "StoreItem", it);
    }
    let (st, v) = call(&app, "GET", "/v1/market/listings", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "ListingPage", &v);
    let (st, v) = call_server(&app, "GET", "/v1/server/fleets", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "FleetList", &v);
    for f in v["fleets"].as_array().unwrap() {
        assert_required(&doc, "Fleet", f);
    }
    let (st, v) = call_server(&app, "GET", "/v1/server/worlds", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_required(&doc, "WorldList", &v);
}

/// Issued keys carry a role: `server` keys reach /v1/server but not /v1/admin; revocation is
/// immediate; the config key stays the bootstrap admin.
#[tokio::test]
async fn server_keys_are_scoped_and_revocable() {
    let Some(app) = app().await else {
        return;
    };
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/admin/keys",
        "defaultkey",
        Some(json!({"name": unique("ci-"), "role": "server"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let srv = v["secret"].as_str().unwrap().to_string();
    let srv_id = v["id"].as_str().unwrap().to_string();
    assert!(srv.starts_with("msk_"), "{srv}");
    let (st, _) = call_server(&app, "GET", "/v1/server/events", &srv, None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, e) = call_server(&app, "GET", "/v1/admin/keys", &srv, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{e}");

    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/admin/keys",
        "defaultkey",
        Some(json!({"name": unique("ops-"), "role": "admin"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let adm = v["secret"].as_str().unwrap().to_string();
    let (st, list) = call_server(&app, "GET", "/v1/admin/keys", &adm, None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert!(list["keys"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["id"] == srv_id && k["revoked_at"].is_null()));
    assert!(
        list.to_string().find("msk_").is_none(),
        "secrets must not be listed"
    );

    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/admin/keys/{srv_id}"),
        &adm,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "GET", "/v1/server/events", &srv, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "revoked key must be rejected");
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/admin/keys/{srv_id}"),
        &adm,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call_server(&app, "GET", "/v1/server/events", "nope", None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// Retention removes rows older than the configured windows (and orphan journals) but keeps
/// recent ones and tables whose window is 0.
#[tokio::test]
async fn retention_purges_old_rows_only() {
    let Some(app) = app().await else {
        return;
    };
    let Ok(url) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let tag = unique("ret-");
    for (kind, age) in [("old", "40 days"), ("new", "1 day")] {
        sqlx::query(&format!(
            "insert into musubi.events (id, kind, account_id, payload, created_at) values ($1, $2, null, '{{}}', now() - interval '{age}')"
        ))
        .bind(uuid::Uuid::now_v7())
        .bind(format!("{tag}.{kind}"))
        .execute(&db)
        .await
        .unwrap();
    }
    sqlx::query("insert into musubi.match_journal (match_id, tick_from, tick_to, entries) values ($1, 1, 5, '[]')")
        .bind(uuid::Uuid::now_v7())
        .execute(&db)
        .await
        .unwrap();
    sqlx::query(
        "insert into musubi.audit_log (id, actor, method, route, path, status, ip, body, duration_ms, at) values ($1, 't', 'POST', '/x', '/x', 200, '1.1.1.1', '{}', 1, now() - interval '100 days')",
    )
    .bind(uuid::Uuid::now_v7())
    .execute(&db)
    .await
    .unwrap();
    let (st, v) = call_server(&app, "POST", "/v1/admin/retention/run", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["removed"]["events"].as_u64().unwrap() >= 1, "{v}");
    assert!(v["removed"]["match_journal"].as_u64().unwrap() >= 1, "{v}");
    assert!(v["removed"]["audit_log"].as_u64().unwrap() >= 1, "{v}");
    let kinds: Vec<String> =
        sqlx::query_scalar("select kind from musubi.events where kind like $1")
            .bind(format!("{tag}.%"))
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(kinds, vec![format!("{tag}.new")]);
}

/// `?where=` lists only objects whose value contains the given JSON (served by a GIN index).
#[tokio::test]
async fn storage_list_where_filters_by_value() {
    let Some(app) = app().await else {
        return;
    };
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("sw-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let col = unique("inv");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&tok),
        Some(json!({"objects": [
            {"collection": col, "key": "sword", "value": {"kind": "weapon", "level": 5, "tags": ["fire"]}},
            {"collection": col, "key": "shield", "value": {"kind": "armor", "level": 5}},
            {"collection": col, "key": "potion", "value": {"kind": "consumable", "level": 1}}
        ]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let q = |w: &str| format!("/v1/storage/{col}?where={}", urlencoding(w));
    let (st, v) = call(&app, "GET", &q(r#"{"level":5}"#), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let keys: Vec<&str> = v["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["shield", "sword"], "{v}");
    let (st, v) = call(&app, "GET", &q(r#"{"tags":["fire"]}"#), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["objects"].as_array().unwrap().len(), 1);
    let (st, _) = call(&app, "GET", &q("[1,2]"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&app, "GET", &q("not json"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Email verification and password reset flow driven by outbox events (the game's mailer reads
/// the token from `account.email_verification` / `account.password_reset`).
#[tokio::test]
async fn email_verification_and_password_reset() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("{}@example.test", unique("u"));
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "Passw0rd!long"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"]
        .as_str()
        .map(str::to_string)
        .unwrap_or_default();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email/verify/request",
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["verified"], false);
    let token_for = |kind: &'static str| {
        let app = app.clone();
        let email = email.clone();
        async move {
            let (st, ev) = call_server(
                &app,
                "GET",
                &format!("/v1/server/events?kinds={kind}&limit=1000"),
                "defaultkey",
                None,
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{ev}");
            ev["events"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["payload"]["email"] == email)
                .map(|e| e["payload"]["token"].as_str().unwrap().to_string())
                .unwrap_or_else(|| panic!("no {kind} event for {email}: {ev}"))
        }
    };
    let vt = token_for("account.email_verification").await;
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email/verify",
        None,
        Some(json!({"token": vt})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email/verify",
        None,
        Some(json!({"token": vt})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "token must be single-use");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email/verify/request",
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["verified"], true, "{v}");

    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email/reset/request",
        None,
        Some(json!({"email": "nobody-here@example.test"})),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "unknown email must not be distinguishable"
    );
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email/reset/request",
        None,
        Some(json!({"email": email})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let rt = token_for("account.password_reset").await;
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email/reset",
        None,
        Some(json!({"token": rt, "new_password": "An0ther!long-pass"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "Passw0rd!long"})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "old password must fail");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "An0ther!long-pass"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": sess["refresh_token"]})),
    )
    .await;
    assert_ne!(
        st,
        StatusCode::OK,
        "old refresh sessions must be revoked: {v}"
    );
    let _ = acc;
}

/// Steam tickets and Google/Apple ID tokens become identities; verified against a mock Steam Web
/// API and a mock JWKS signed with the RSA test fixture.
#[tokio::test]
async fn social_logins_verify_and_link() {
    use axum::extract::Query as AxQuery;
    use axum::routing::get as ax_get;
    let jwks: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/google_test_jwks.json")).unwrap();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        % 1_000_000_000;
    let (s1, s2, s3) = (
        format!("76561198{nonce:09}"),
        format!("76561197{nonce:09}"),
        format!("76561196{nonce:09}"),
    );
    let mock = Router::new()
        .route("/jwks", ax_get(move || { let j = jwks.clone(); async move { axum::Json(j) } }))
        .route(
            "/ISteamUserAuth/AuthenticateUserTicket/v1/",
            ax_get(move |AxQuery(q): AxQuery<std::collections::HashMap<String, String>>| { let (s1, s2, s3) = (s1.clone(), s2.clone(), s3.clone()); async move {
                let id = match q.get("ticket").map(String::as_str) {
                    Some("0a0a") => s1.as_str(),
                    Some("0b0b") => s2.as_str(),
                    Some("0bad") => return axum::Json(json!({"response": {"params": {"result": "OK", "steamid": s3, "vacbanned": true}}})),
                    _ => return axum::Json(json!({"response": {"error": {"errorcode": 101, "errordesc": "Invalid ticket"}}})),
                };
                axum::Json(json!({"response": {"params": {"result": "OK", "steamid": id, "ownersteamid": id, "vacbanned": false, "publisherbanned": false}}}))
            } }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let b = base.clone();
    let Some(app) = app_with(move |c| {
        c.social.steam.url = b.clone();
        c.social.steam.web_api_key = "k".into();
        c.social.steam.app_id = 480;
        c.social.google.jwks_url = format!("{b}/jwks");
        c.social.google.issuers = vec!["https://accounts.google.com".into()];
        c.social.google.client_ids = vec!["my-client".into()];
        c.social.apple.jwks_url = format!("{b}/jwks");
        c.social.apple.issuers = vec!["https://appleid.apple.com".into()];
    })
    .await
    else {
        return;
    };
    let key =
        jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("fixtures/google_test_rsa.pem"))
            .unwrap();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("t1".into());
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 600;
    let sign = |iss: &str, aud: &str, sub: &str| {
        jsonwebtoken::encode(
            &header,
            &json!({"iss": iss, "aud": aud, "sub": sub, "exp": exp, "email": "g@example.test"}),
            &key,
        )
        .unwrap()
    };
    let gsub = unique("gsub-");
    let tok = sign("https://accounts.google.com", "my-client", &gsub);
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/google",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/google",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        s1["account_id"], s2["account_id"],
        "same sub → same account"
    );
    let bad_aud = sign("https://accounts.google.com", "other-client", &gsub);
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/google",
        None,
        Some(json!({"id_token": bad_aud})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    let bad_iss = sign("https://evil.example", "my-client", &gsub);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/google",
        None,
        Some(json!({"id_token": bad_iss})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let apple = sign(
        "https://appleid.apple.com",
        "com.example.game",
        &unique("asub-"),
    );
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/apple",
        None,
        Some(json!({"id_token": apple})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/steam",
        None,
        Some(json!({"ticket": "0a0a"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/steam",
        None,
        Some(json!({"ticket": "ffff"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/steam",
        None,
        Some(json!({"ticket": "0bad"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "vac banned: {v}");

    // link a steam id to the google account, then sign in with steam lands on the same account
    let gtok = s1["access_token"].as_str().unwrap().to_string();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/link/steam",
        Some(&gtok),
        Some(json!({"ticket": "0b0b"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/steam",
        None,
        Some(json!({"ticket": "0b0b"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["account_id"], s1["account_id"]);
}

/// Rotating the signing key does not log anyone out, new tokens carry the kid, the public key is
/// published for third parties, and a retired key stops signing but keeps verifying.
#[tokio::test]
async fn jwt_keys_rotate_and_publish_jwks() {
    let Some(app) = app().await else {
        return;
    };
    let (st, s0) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("jwt-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s0}");
    let t0 = s0["access_token"].as_str().unwrap().to_string();
    assert!(
        jsonwebtoken::decode_header(&t0).unwrap().kid.is_none(),
        "legacy token has no kid"
    );

    let (st, v) = call_server(&app, "POST", "/v1/admin/jwt/rotate", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let kid1 = v["kid"].as_str().unwrap().to_string();
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("jwt-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let t1 = s1["access_token"].as_str().unwrap().to_string();
    let h = jsonwebtoken::decode_header(&t1).unwrap();
    assert_eq!(h.kid.as_deref(), Some(kid1.as_str()));
    assert_eq!(h.alg, jsonwebtoken::Algorithm::ES256);
    let (st, jwks) = call(&app, "GET", "/.well-known/jwks.json", None, None).await;
    assert_eq!(st, StatusCode::OK);
    let set: jsonwebtoken::jwk::JwkSet = serde_json::from_value(jwks.clone()).unwrap();
    let jwk = set.find(&kid1).expect("kid published");
    let mut val = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
    val.validate_aud = false;
    val.set_required_spec_claims(&["exp", "sub"]);
    let claims = jsonwebtoken::decode::<serde_json::Value>(
        &t1,
        &jsonwebtoken::DecodingKey::from_jwk(jwk).unwrap(),
        &val,
    )
    .unwrap()
    .claims;
    assert_eq!(claims["sub"], s1["account_id"]);
    let (st, _) = call(&app, "GET", "/v1/account", Some(&t0), None).await;
    assert_eq!(st, StatusCode::OK, "legacy token still accepted");
    let (st, _) = call(&app, "GET", "/v1/account", Some(&t1), None).await;
    assert_eq!(st, StatusCode::OK);

    let (st, v) = call_server(&app, "POST", "/v1/admin/jwt/rotate", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK);
    let kid2 = v["kid"].as_str().unwrap().to_string();
    let (st, list) = call_server(&app, "GET", "/v1/admin/jwt/keys", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["active"], kid2);
    let (st, _) = call(&app, "GET", "/v1/account", Some(&t1), None).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "tokens of the previous key still verify"
    );
    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("jwt-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        jsonwebtoken::decode_header(s2["access_token"].as_str().unwrap())
            .unwrap()
            .kid
            .as_deref(),
        Some(kid2.as_str())
    );

    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/admin/jwt/keys/{kid1}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", "/v1/account", Some(&t1), None).await;
    assert_eq!(
        st,
        StatusCode::OK,
        "retired key still verifies until pruned"
    );
    let (st, jwks) = call(&app, "GET", "/.well-known/jwks.json", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(jwks["keys"].as_array().unwrap().len() >= 2, "{jwks}");
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/admin/jwt/keys/{kid1}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "already retired");
}

/// With a tracer installed, every response carries `X-Trace-Id` and error bodies a `trace_id`
/// that matches the exported span, so a client-side bug report points at one trace.
#[tokio::test]
async fn trace_id_is_returned_on_errors() {
    use opentelemetry::trace::TracerProvider as _;
    use tracing::instrument::WithSubscriber;
    use tracing_subscriber::prelude::*;
    let Some(app) = app().await else {
        return;
    };
    let exporter = opentelemetry_sdk::trace::InMemorySpanExporter::default();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let sub = tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test"))),
    );
    // Other tests in this binary may have hit the request-span callsite under the no-op
    // subscriber while our dispatcher was being registered, caching it as disabled; force a
    // recomputation so the scoped subscriber definitely sees the span.
    tracing::callsite::rebuild_interest_cache();
    let (st, v) = call(&app, "GET", "/v1/account", Some("not-a-token"), None)
        .with_subscriber(sub)
        .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    let trace_id = v["trace_id"]
        .as_str()
        .expect("trace_id in error body")
        .to_string();
    assert_eq!(trace_id.len(), 32, "{trace_id}");
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    assert!(
        spans
            .iter()
            .any(|s| s.span_context.trace_id().to_string() == trace_id
                && s.name.contains("/v1/account")),
        "exported spans: {:?}",
        spans
            .iter()
            .map(|s| (&s.name, s.span_context.trace_id().to_string()))
            .collect::<Vec<_>>()
    );
    // without a tracer the field is simply absent
    let (st, v) = call(&app, "GET", "/v1/account", Some("not-a-token"), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(v.get("trace_id").is_none(), "{v}");
}

/// Same request + same Idempotency-Key → the stored response (same storage version), not a second
/// write; another key or another caller executes afresh.
#[tokio::test]
async fn idempotency_key_replays_mutations() {
    let Some(app) = app().await else {
        return;
    };
    async fn call_h(app: &Router, tok: &str, key: &str, body: Value) -> (StatusCode, bool, Value) {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/storage/write")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let replayed = res.headers().get("idempotent-replayed").is_some();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            replayed,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("idem-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let col = unique("idem");
    let body = json!({"objects": [{"collection": col, "key": "k", "value": {"n": 1}}]});
    let key = unique("key");
    let (st, replayed, first) = call_h(&app, &tok, &key, body.clone()).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    assert!(!replayed);
    let (st, replayed, second) = call_h(&app, &tok, &key, body.clone()).await;
    assert_eq!(st, StatusCode::OK);
    assert!(replayed, "second call must be served from the store");
    assert_eq!(first, second, "identical response incl. storage version");
    let body2 = json!({"objects": [{"collection": col, "key": "k", "value": {"n": 2}}]});
    let (st, replayed, third) = call_h(&app, &tok, &unique("key"), body2).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!replayed);
    assert_ne!(
        first["acks"][0]["version"], third["acks"][0]["version"],
        "a new key performs a new write"
    );
    // another caller with the same key is independent
    let (_, sess2) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("idem-")})),
    )
    .await;
    let tok2 = sess2["access_token"].as_str().unwrap().to_string();
    let (st, replayed, _) = call_h(&app, &tok2, &key, body).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!replayed);
    // errors are stored too (a 4xx replays), server errors are not
    let (st, _, _) = call_h(&app, &tok, &unique("bad"), json!({"objects": "nope"})).await;
    assert!(st.is_client_error(), "{st}");
}

/// A notification for an account with no live session is pushed to its FCM and APNs tokens;
/// a token the provider reports as gone is removed.
#[tokio::test]
async fn push_tokens_receive_offline_notifications() {
    use axum::extract::Path as AxPath;
    use axum::routing::post as ax_post;
    let seen: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (s1, s2) = (seen.clone(), seen.clone());
    let mock = Router::new()
        .route(
            "/token",
            ax_post(|| async {
                axum::Json(json!({"access_token": "fcm-access", "expires_in": 3600}))
            }),
        )
        .route(
            "/v1/projects/proj/messages:send",
            ax_post(
                move |headers: axum::http::HeaderMap, axum::Json(b): axum::Json<Value>| {
                    let s1 = s1.clone();
                    async move {
                        assert_eq!(headers.get("authorization").unwrap(), "Bearer fcm-access");
                        s1.lock().unwrap().push(json!({"kind": "fcm", "msg": b}));
                        axum::Json(json!({"name": "projects/proj/messages/1"}))
                    }
                },
            ),
        )
        .route(
            "/3/device/{tok}",
            ax_post(
                move |AxPath(tok): AxPath<String>,
                      headers: axum::http::HeaderMap,
                      axum::Json(b): axum::Json<Value>| {
                    let s2 = s2.clone();
                    async move {
                        assert!(headers
                            .get("authorization")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .starts_with("bearer "));
                        assert_eq!(headers.get("apns-topic").unwrap(), "com.example.game");
                        if tok == "dead" {
                            return (
                                StatusCode::GONE,
                                axum::Json(json!({"reason": "Unregistered"})),
                            );
                        }
                        s2.lock()
                            .unwrap()
                            .push(json!({"kind": "apns", "token": tok, "msg": b}));
                        (StatusCode::OK, axum::Json(json!({})))
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let b = base.clone();
    let Some(app) = app_with(move |c| {
        c.push.fcm.project_id = "proj".into();
        c.push.fcm.client_email = "svc@proj.iam.gserviceaccount.com".into();
        c.push.fcm.private_key_pem = include_str!("fixtures/google_test_rsa.pem").into();
        c.push.fcm.token_url = format!("{b}/token");
        c.push.fcm.base_url = b.clone();
        c.push.apns.team_id = "TEAM".into();
        c.push.apns.key_id = "KEY1".into();
        c.push.apns.private_key_pem = include_str!("fixtures/apple_test_ec.pem").into();
        c.push.apns.bundle_id = "com.example.game".into();
        c.push.apns.base_url = b.clone();
    })
    .await
    else {
        return;
    };
    let (_, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("push-a")})),
    )
    .await;
    let (_, bb) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("push-b")})),
    )
    .await;
    let (ta, tb) = (
        a["access_token"].as_str().unwrap().to_string(),
        bb["access_token"].as_str().unwrap().to_string(),
    );
    for (p, t) in [("fcm", "fcm-1"), ("apns", "apns-1"), ("apns", "dead")] {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/account/push_tokens",
            Some(&tb),
            Some(json!({"platform": p, "token": t})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/push_tokens",
        Some(&tb),
        Some(json!({"platform": "sms", "token": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    // B has no socket: a friend request from A becomes pushes
    let (st, v) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(&ta),
        Some(json!({"ids": [bb["account_id"]]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let got = seen.lock().unwrap().clone();
        if got.iter().any(|g| g["kind"] == "fcm") && got.iter().any(|g| g["kind"] == "apns") {
            let fcm = got.iter().find(|g| g["kind"] == "fcm").unwrap();
            assert_eq!(fcm["msg"]["message"]["token"], "fcm-1");
            assert!(
                fcm["msg"]["message"]["notification"]["title"]
                    .as_str()
                    .unwrap()
                    .contains("friend"),
                "{fcm}"
            );
            assert!(fcm["msg"]["message"]["data"]["code"].is_string());
            let apns = got.iter().find(|g| g["kind"] == "apns").unwrap();
            assert_eq!(apns["token"], "apns-1");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pushes not delivered: {got:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (st, v) = call(&app, "GET", "/v1/account/push_tokens", Some(&tb), None).await;
        assert_eq!(st, StatusCode::OK);
        let toks: Vec<&str> = v["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["token"].as_str().unwrap())
            .collect();
        if !toks.contains(&"dead") {
            assert_eq!(toks.len(), 2, "{v}");
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "dead token not removed: {v}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let (st, v) = call(
        &app,
        "DELETE",
        "/v1/account/push_tokens",
        Some(&tb),
        Some(json!({"token": "fcm-1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["removed"], true);
}

/// Objects written with `ttl_secs` vanish from reads and lists once expired and are purged by retention.
#[tokio::test]
async fn storage_objects_expire() {
    let Some(app) = app().await else { return };
    let (_, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev")})),
    )
    .await;
    let tok = a["access_token"].as_str().unwrap().to_string();
    let col = unique("ttl");
    let (st, w) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&tok),
        Some(json!({"objects": [
            {"collection": col, "key": "short", "value": {"n": 1}, "ttl_secs": 1},
            {"collection": col, "key": "long", "value": {"n": 2}, "ttl_secs": 3600},
            {"collection": col, "key": "forever", "value": {"n": 3}}
        ]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");
    let (st, l) = call(&app, "GET", &format!("/v1/storage/{col}"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["objects"].as_array().unwrap().len(), 3, "{l}");
    let long = l["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["key"] == "long")
        .unwrap();
    assert!(long["expires_at"].is_string(), "{long}");
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (st, l) = call(&app, "GET", &format!("/v1/storage/{col}"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let keys: Vec<&str> = l["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert!(!keys.contains(&"short") && keys.len() == 2, "{keys:?}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/storage/read",
        Some(&tok),
        Some(json!({"objects": [{"collection": col, "key": "short"}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r["objects"].as_array().unwrap().is_empty(), "{r}");
    let (st, v) = call_server(&app, "POST", "/v1/admin/retention/run", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["removed"]["storage_objects"].as_u64().unwrap() >= 1,
        "{v}"
    );
}

/// Five wrong passwords lock the address (even for the right password afterwards); unknown
/// addresses lock the same way so the response never reveals whether an account exists; an
/// admin can lift the lock.
#[tokio::test]
async fn email_login_lockout() {
    let Some(app) = app().await else {
        return;
    };
    let email = format!("{}@example.test", unique("lock"));
    let body = |pw: &str| json!({"email": email, "password": pw});
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(body("Passw0rd!long")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    for i in 1..=4 {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/auth/email",
            None,
            Some(body("Wrong0rd!long")),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "attempt {i}: {v}");
    }
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(body("Wrong0rd!long")),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(body("Passw0rd!long")),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "locked: {v}");
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/auth/locks/{email}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["failures"], 5, "{v}");
    let (st, v) = call_server(
        &app,
        "DELETE",
        &format!("/v1/admin/auth/locks/{email}"),
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["cleared"], true);
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(body("Passw0rd!long")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "unlocked: {v}");

    let ghost = format!("{}@example.test", unique("ghost"));
    for _ in 0..5 {
        call(
            &app,
            "POST",
            "/v1/auth/email",
            None,
            Some(json!({"email": ghost, "password": "Wrong0rd!long", "create": false})),
        )
        .await;
    }
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": ghost, "password": "Wrong0rd!long", "create": false})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "ghost: {v}");
}

/// Notifications carry read state: `?unread=true` filters, `unread_count` counts, `POST /read`
/// marks a batch or everything.
#[tokio::test]
async fn notifications_read_state() {
    let Some(app) = app().await else { return };
    let (_, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev")})),
    )
    .await;
    let tok = a["access_token"].as_str().unwrap().to_string();
    let acc = a["account_id"].as_str().unwrap().to_string();
    for i in 0..3 {
        let (st, v) = call_server(
            &app,
            "POST",
            "/v1/server/notifications",
            "defaultkey",
            Some(json!({"user_id": acc, "subject": format!("n{i}"), "content": {}, "code": 1})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, l) = call(&app, "GET", "/v1/notifications", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["unread_count"], 3, "{l}");
    assert!(l["notifications"][0].get("read_time").is_none(), "{l}");
    let first = l["notifications"][0]["id"].as_str().unwrap().to_string();
    let (st, r) = call(
        &app,
        "POST",
        "/v1/notifications/read",
        Some(&tok),
        Some(json!({"ids": [first]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["read"], 1);
    let (_, l) = call(
        &app,
        "GET",
        "/v1/notifications?unread=true",
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(l["unread_count"], 2, "{l}");
    assert_eq!(l["notifications"].as_array().unwrap().len(), 2, "{l}");
    let (_, l) = call(&app, "GET", "/v1/notifications", Some(&tok), None).await;
    assert_eq!(l["notifications"].as_array().unwrap().len(), 3, "{l}");
    assert!(l["notifications"][0]["read_time"].is_string(), "{l}");
    let (_, r) = call(
        &app,
        "POST",
        "/v1/notifications/read",
        Some(&tok),
        Some(json!({"all": true})),
    )
    .await;
    assert_eq!(r["read"], 2, "{r}");
    let (_, l) = call(
        &app,
        "GET",
        "/v1/notifications?unread=true",
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(l["unread_count"], 0, "{l}");
    assert!(l["notifications"].as_array().unwrap().is_empty(), "{l}");
}

/// `GET /v1/users/search?q=` finds usernames by case-insensitive prefix, shortest first.
#[tokio::test]
async fn users_search_by_prefix() {
    let Some(app) = app().await else { return };
    let raw: String = unique("zq")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let tag = format!("zq{}", &raw[raw.len() - 8..]).to_lowercase();
    let mut tok = String::new();
    for suffix in ["alpha", "alphabet", "beta"] {
        let (st, a) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("dev"), "username": format!("{tag}{suffix}")})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{a}");
        tok = a["access_token"].as_str().unwrap().to_string();
    }
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/users/search?q={}", format!("{tag}AL").to_uppercase()),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let names: Vec<&str> = v["users"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["username"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![format!("{tag}alpha"), format!("{tag}alphabet")],
        "{v}"
    );
    let (st, v) = call(&app, "GET", "/v1/users/search?q=", Some(&tok), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/users/search?q={tag}%25"),
        Some(&tok),
        None,
    )
    .await;
    assert!(
        v["users"].as_array().unwrap().is_empty(),
        "wildcards are literal: {v}"
    );
}

/// A player can dump and delete their own account; the token stops working afterwards.
#[tokio::test]
async fn account_self_export_and_delete() {
    let Some(app) = app().await else { return };
    let (_, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev")})),
    )
    .await;
    let tok = a["access_token"].as_str().unwrap().to_string();
    let acc = a["account_id"].as_str().unwrap().to_string();
    call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&tok),
        Some(json!({"objects": [{"collection": "saves", "key": "slot1", "value": {"hp": 7}}]})),
    )
    .await;
    let (st, dump) = call(&app, "GET", "/v1/account/export", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{dump}");
    let text = dump.to_string();
    assert!(text.contains(&acc), "{text}");
    assert!(
        text.contains("slot1"),
        "export should include storage: {text}"
    );
    let (st, r) = call(&app, "POST", "/v1/account/delete", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["deleted"], true);
    let (st, _) = call(&app, "GET", "/v1/account", Some(&tok), None).await;
    assert!(
        st == StatusCode::UNAUTHORIZED
            || st == StatusCode::FORBIDDEN
            || st == StatusCode::NOT_FOUND,
        "deleted account should not be usable: {st}"
    );
    let (_, v) = call(&app, "GET", &format!("/v1/users?ids={acc}"), None, None).await;
    assert!(
        v["users"].as_array().map(|u| u.is_empty()).unwrap_or(true),
        "{v}"
    );
}

/// Configured schedules show up with their export name and next fire; a forced run without a
/// module exporting the hook is a clean 404, as is an unknown schedule.
#[tokio::test]
async fn runtime_schedules_status_and_forced_run() {
    let Some(app) = app_with(|cfg| {
        cfg.runtime
            .schedules
            .push(musubi_core::config::ScheduleConfig {
                name: "nightly".into(),
                cron: "0 3 * * *".into(),
            });
        cfg.runtime
            .schedules
            .push(musubi_core::config::ScheduleConfig {
                name: "broken".into(),
                cron: "99 * * * *".into(),
            });
    })
    .await
    else {
        return;
    };
    let (st, v) = call_server(
        &app,
        "GET",
        "/v1/admin/runtime/schedules",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let list = v["schedules"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0]["name"], "nightly");
    assert_eq!(list[0]["export"], "schedule_nightly");
    assert!(list[0]["next_run"].is_i64(), "{v}");
    assert_eq!(list[0]["runs"], 0);
    assert_eq!(v["problems"].as_array().unwrap().len(), 1, "{v}");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/admin/runtime/schedules/nightly/run",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("schedule_nightly"),
        "{v}"
    );
    let (_, v) = call_server(
        &app,
        "GET",
        "/v1/admin/runtime/schedules",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(v["schedules"][0]["runs"], 1, "{v}");
    assert_eq!(v["schedules"][0]["last_ok"], false, "{v}");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/admin/runtime/schedules/nope/run",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// `GET /v1/storage/{collection}?prefix=` lists only keys starting with the prefix; wildcards are
/// literal.
#[tokio::test]
async fn storage_list_by_key_prefix() {
    let Some(app) = app().await else { return };
    let (_, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev")})),
    )
    .await;
    let tok = a["access_token"].as_str().unwrap().to_string();
    let col = unique("pfx");
    call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&tok),
        Some(json!({"objects": [
            {"collection": col, "key": "save:1", "value": {}},
            {"collection": col, "key": "save:2", "value": {}},
            {"collection": col, "key": "save_x", "value": {}},
            {"collection": col, "key": "settings", "value": {}},
        ]})),
    )
    .await;
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?prefix=save%3A"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let keys: Vec<&str> = v["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["save:1", "save:2"], "{v}");
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?prefix=save_"),
        Some(&tok),
        None,
    )
    .await;
    let keys: Vec<&str> = v["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["save_x"], "underscore is literal: {v}");
}

#[tokio::test]
async fn edge_proxy_routes_allocations_by_connect_token() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let name = unique("edge-fleet");
    let (st, f) = call_server(
        &app,
        "POST",
        "/v1/server/fleets",
        k,
        Some(json!({
            "name": name, "provider": "local", "buffer": 1, "max_servers": 2,
            "port_start": 48000, "port_end": 48100,
            "spec": {"command": ["sleep", "60"]}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let fuid = uuid::Uuid::parse_str(f["id"].as_str().unwrap()).unwrap();

    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let plain =
        musubi_fleet::FleetService::new(db.clone(), uuid::Uuid::now_v7(), "http://127.0.0.1:0", k);
    let proxy = musubi_edge::UdpProxy::start(
        "127.0.0.1:0".parse().unwrap(),
        std::sync::Arc::new(musubi_server::edge::FleetRouter(std::sync::Arc::new(
            plain.clone(),
        ))),
        std::time::Duration::from_secs(5),
    )
    .await
    .unwrap();
    let edge = proxy.local_addr();
    let svc = plain.clone().with_edge(Some(edge));
    assert_eq!(svc.edge(), Some(edge));
    svc.reconcile().await.unwrap();
    for s in svc.servers(fuid, Some("starting")).await.unwrap() {
        svc.sdk_ready(s.id).await.unwrap();
    }
    assert!(svc.route_token("not-a-token").await.is_none());

    // clients are told the proxy's address, never the game server's
    let alloc = svc
        .allocate(fuid, json!({"match": "edge"}), false)
        .await
        .unwrap();
    assert_eq!(alloc.address, edge.ip().to_string());
    assert_eq!(alloc.port, edge.port() as i32);
    let real = svc.server(alloc.server_id).await.unwrap();
    assert_ne!(
        (real.address.as_str(), real.port),
        (alloc.address.as_str(), alloc.port)
    );
    let routed = svc.route_token(&alloc.connect_token).await.unwrap();
    assert_eq!(
        routed.to_string(),
        format!("{}:{}", real.address, real.port)
    );

    // the "game server" (the local provider only runs `sleep`): a UDP echo on its advertised port
    let gs = tokio::net::UdpSocket::bind(routed).await.unwrap();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        loop {
            let (n, from) = gs.recv_from(&mut buf).await.unwrap();
            let mut out = b"gs:".to_vec();
            out.extend_from_slice(&buf[..n]);
            gs.send_to(&out, from).await.unwrap();
        }
    });
    let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    client.connect(edge).await.unwrap();
    client
        .send(&musubi_edge::with_header(
            alloc.connect_token.as_bytes(),
            b"hello",
        ))
        .await
        .unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&buf[..n], b"gs:hello");
    // a stranger's token goes nowhere
    client
        .send(&musubi_edge::with_header(b"stranger", b"hello"))
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), client.recv(&mut buf))
            .await
            .is_err()
    );
    assert_eq!(proxy.counters(), (1, 1));

    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/fleets/{fuid}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn http_hooks_rewrite_and_reject_calls_by_nakama_names() {
    let Some(app) = app().await else {
        return;
    };
    let hooked = unique("hooked");
    let rewrite = format!(r#"{{"body":{{"id":"{hooked}"}}}}"#);
    let escaped = rewrite.replace('"', "\\\"");
    let wat = format!(
        r#"(module
  (import "musubi" "error" (func $error (param i32 i32)))
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))
  (func (export "alloc") (param $n i32) (result i32) (local $p i32)
    global.get $heap local.set $p
    global.get $heap local.get $n i32.add global.set $heap
    local.get $p)
  (data (i32.const 16) "{escaped}")
  (func (export "before_authenticate_device") (param i32 i32 i32 i32) (result i64)
    i64.const 16 i64.const 32 i64.shl i64.const {} i64.or)
  (func (export "after_authenticate_device") (param i32 i32 i32 i32) (result i64) i64.const 0)
  (data (i32.const 256) "nope")
  (func (export "before_list_friends") (param i32 i32 i32 i32) (result i64)
    i32.const 256 i32.const 4 call $error
    i64.const 0))"#,
        rewrite.len()
    );
    let req = Request::put("/v1/admin/runtime/modules/hooks")
        .header("x-musubi-server-key", "defaultkey")
        .header(header::CONTENT_TYPE, "application/wasm")
        .body(Body::from(wat))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let st = res.status();
    let v: Value = serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes())
        .unwrap_or(Value::Null);
    assert_eq!(st, StatusCode::OK, "{v}");

    // the before-hook swaps every device id for the hooked one: two strangers land on one account
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("a")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    assert_eq!(s1["created"], true);
    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("b")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(s2["created"], false, "{s2}");
    let tok = s1["access_token"].as_str().unwrap();
    let (st, me) = call(&app, "GET", "/v1/account/me", Some(tok), None).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    // the hooked device is the only identity
    let (st, acc) = call(&app, "GET", "/v1/account", Some(tok), None).await;
    assert_eq!(st, StatusCode::OK, "{acc}");
    assert!(acc.to_string().contains(&hooked), "{acc}");

    // a before-hook error rejects the call with the guest's message
    let (st, v) = call(&app, "GET", "/v1/friends", Some(tok), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["message"].as_str().unwrap().ends_with("nope"), "{v}");
    // other routes are untouched
    let (st, v) = call(&app, "GET", "/v1/notifications", Some(tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

fn balance_of(v: &Value, cur: &str) -> i64 {
    v["balances"]
        .as_array()
        .and_then(|a| a.iter().find(|w| w["currency"] == cur))
        .and_then(|w| w["balance"].as_i64())
        .unwrap_or(0)
}

fn held_of(inv: &Value, item: &str) -> i64 {
    inv["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|e| e["item_id"] == item)
                .map(|e| e["quantity"].as_i64().unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

#[tokio::test]
async fn order_book_matches_price_time_with_partial_fills_and_trade_hold() {
    let Some(app) = app_with(|c| c.market.trade_hold_secs = 1).await else {
        return;
    };
    let k = "defaultkey";
    let mut users = Vec::new();
    for _ in 0..3 {
        let (st, sess) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("book")})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        users.push((
            sess["access_token"].as_str().unwrap().to_string(),
            sess["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (alice, alice_id) = users[0].clone();
    let (bob, bob_id) = users[1].clone();
    let (carol, carol_id) = users[2].clone();
    let gold = unique("gold");
    let ore = unique("ore");
    let relic = unique("relic");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for (id, stackable) in [(&ore, true), (&relic, false)] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/items",
            k,
            Some(json!({"id": id, "name": id, "stackable": stackable, "max_stack": 1000})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    for (acc, amount) in [(&bob_id, 10_000), (&carol_id, 1_000)] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/wallet/adjust",
            k,
            Some(json!({"account_id": acc, "currency": gold, "delta": amount})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    for (acc, item, qty) in [
        (&alice_id, &ore, 100),
        (&carol_id, &ore, 50),
        (&alice_id, &relic, 1),
    ] {
        let (st, g) = call_server(
            &app,
            "POST",
            "/v1/server/inventory/grant",
            k,
            Some(json!({"account_id": acc, "item_id": item, "quantity": qty})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{g}");
    }
    let place = |tok: String, side: &'static str, price: i64, qty: i64| {
        let app = app.clone();
        let ore = ore.clone();
        let gold = gold.clone();
        async move {
            call(
                &app,
                "POST",
                "/v1/market/orders",
                Some(&tok),
                Some(json!({"item_id": ore, "currency": gold, "side": side, "price": price, "quantity": qty})),
            )
            .await
        }
    };

    // Unique items and unknown items are refused up front.
    let (st, v) = call(
        &app,
        "POST",
        "/v1/market/orders",
        Some(&alice),
        Some(
            json!({"item_id": relic, "currency": gold, "side": "sell", "price": 5, "quantity": 1}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/market/orders",
        Some(&alice),
        Some(json!({"item_id": "no-such-item", "currency": gold, "side": "sell", "price": 5, "quantity": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Three asks: alice 40 @ 10, carol 50 @ 12, alice 30 @ 10 (behind her first one in time).
    let (st, ask1) = place(alice.clone(), "sell", 10, 40).await;
    assert_eq!(st, StatusCode::OK, "{ask1}");
    assert_eq!(ask1["fills"].as_array().unwrap().len(), 0);
    let (st, ask2) = place(carol.clone(), "sell", 12, 50).await;
    assert_eq!(st, StatusCode::OK, "{ask2}");
    let (st, ask3) = place(alice.clone(), "sell", 10, 30).await;
    assert_eq!(st, StatusCode::OK, "{ask3}");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&alice), None).await;
    assert_eq!(held_of(&inv, &ore), 30, "70 ore sit in escrow: {inv}");
    let (st, book) = call(
        &app,
        "GET",
        &format!("/v1/market/book/{ore}?currency={gold}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{book}");
    assert_eq!(book["bids"].as_array().unwrap().len(), 0);
    assert_eq!(book["asks"][0]["price"], 10);
    assert_eq!(book["asks"][0]["quantity"], 70);
    assert_eq!(book["asks"][0]["orders"], 2);
    assert_eq!(book["asks"][1]["price"], 12);
    assert!(book["last_price"].is_null());

    // bob buys 60 @ 15: 40 from ask1 then 20 from ask3, both at 10; he pays 600, not 900.
    let (st, buy1) = place(bob.clone(), "buy", 15, 60).await;
    assert_eq!(st, StatusCode::OK, "{buy1}");
    let fills = buy1["fills"].as_array().unwrap();
    assert_eq!(fills.len(), 2, "{buy1}");
    assert_eq!(fills[0]["quantity"], 40);
    assert_eq!(fills[0]["price"], 10);
    assert_eq!(fills[0]["sell_order_id"], ask1["order"]["id"]);
    assert_eq!(fills[1]["quantity"], 20);
    assert_eq!(fills[1]["sell_order_id"], ask3["order"]["id"]);
    assert_eq!(fills[0]["fee"], 20, "5% of 400");
    assert_eq!(buy1["order"]["status"], "filled");
    assert_eq!(buy1["order"]["remaining"], 0);
    assert_eq!(buy1["order"]["escrow"], 0);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&bob), None).await;
    assert_eq!(balance_of(&w, &gold), 10_000 - 600, "{w}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&alice), None).await;
    assert_eq!(
        balance_of(&w, &gold),
        600 - 30,
        "seller gets 600 minus 5%: {w}"
    );
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&bob), None).await;
    assert_eq!(held_of(&inv, &ore), 60, "{inv}");
    let (_, a3) = call(
        &app,
        "GET",
        &format!(
            "/v1/market/orders/{}",
            ask3["order"]["id"].as_str().unwrap()
        ),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(a3["status"], "open");
    assert_eq!(a3["remaining"], 10);

    // bob buys 100 @ 11: 10 more from ask3, the other 90 rest as the best bid with 990 escrowed.
    let (st, buy2) = place(bob.clone(), "buy", 11, 100).await;
    assert_eq!(st, StatusCode::OK, "{buy2}");
    assert_eq!(buy2["fills"].as_array().unwrap().len(), 1);
    assert_eq!(buy2["fills"][0]["quantity"], 10);
    assert_eq!(buy2["fills"][0]["price"], 10);
    assert_eq!(buy2["order"]["status"], "open");
    assert_eq!(buy2["order"]["remaining"], 90);
    assert_eq!(buy2["order"]["escrow"], 990);
    let bid_id = buy2["order"]["id"].as_str().unwrap().to_string();
    let (_, book) = call(
        &app,
        "GET",
        &format!("/v1/market/book/{ore}?currency={gold}"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(book["bids"][0]["price"], 11);
    assert_eq!(book["bids"][0]["quantity"], 90);
    assert_eq!(book["asks"][0]["price"], 12);
    assert_eq!(book["last_price"], 10);
    assert_eq!(book["volume_24h"], 70);
    assert_eq!(book["fills_24h"], 3);

    // carol's 50 ore are escrowed in ask2, so she cannot sell more until she cancels it.
    let (st, v) = place(carol.clone(), "sell", 11, 20).await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (st, c) = call(
        &app,
        "POST",
        &format!(
            "/v1/market/orders/{}/cancel",
            ask2["order"]["id"].as_str().unwrap()
        ),
        Some(&carol),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "cancelled");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&carol), None).await;
    assert_eq!(held_of(&inv, &ore), 50);
    let (st, _) = call(
        &app,
        "POST",
        &format!(
            "/v1/market/orders/{}/cancel",
            ask2["order"]["id"].as_str().unwrap()
        ),
        Some(&carol),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // Now she hits bob's bid: 20 @ 11 (the maker's price), bob's bid shrinks to 70 / 770.
    let (st, sell) = place(carol.clone(), "sell", 11, 20).await;
    assert_eq!(st, StatusCode::OK, "{sell}");
    assert_eq!(sell["fills"][0]["quantity"], 20);
    assert_eq!(sell["fills"][0]["price"], 11);
    assert_eq!(sell["fills"][0]["buy_order_id"], bid_id);
    assert_eq!(sell["order"]["status"], "filled");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&carol), None).await;
    assert_eq!(balance_of(&w, &gold), 1_000 + 220 - 11, "{w}");

    // No self-trading: bob's own cheap ask does not cross his bid.
    let (st, own) = place(bob.clone(), "sell", 5, 10).await;
    assert_eq!(st, StatusCode::OK, "{own}");
    assert_eq!(own["fills"].as_array().unwrap().len(), 0);
    assert_eq!(own["order"]["status"], "open");
    let (st, _) = call(
        &app,
        "POST",
        &format!(
            "/v1/market/orders/{}/cancel",
            own["order"]["id"].as_str().unwrap()
        ),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Not his order.
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/market/orders/{bid_id}/cancel"),
        Some(&carol),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, c) = call(
        &app,
        "POST",
        &format!("/v1/market/orders/{bid_id}/cancel"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&bob), None).await;
    assert_eq!(
        balance_of(&w, &gold),
        10_000 - 400 - 200 - 100 - 220,
        "escrow refunded, only fills paid: {w}"
    );
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&bob), None).await;
    assert_eq!(held_of(&inv, &ore), 90, "{inv}");
    let (_, f) = call(&app, "GET", "/v1/market/fills", Some(&bob), None).await;
    assert_eq!(f["fills"].as_array().unwrap().len(), 4, "{f}");
    let (_, mine) = call(
        &app,
        "GET",
        "/v1/market/orders?status=filled",
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(mine["orders"].as_array().unwrap().len(), 1, "{mine}");
    let (_, mine) = call(&app, "GET", "/v1/market/orders", Some(&bob), None).await;
    assert_eq!(mine["orders"].as_array().unwrap().len(), 3, "{mine}");

    // Direct trade with the anti-scam hold: alice offers 5 ore for 50 gold.
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&alice), None).await;
    let stack = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["item_id"] == ore)
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let propose = || {
        let app = app.clone();
        let alice = alice.clone();
        let body = json!({"to_account": bob_id, "offer_items": [{"inventory_id": stack, "quantity": 5}], "want_currency": {gold.clone(): 50}});
        async move { call(&app, "POST", "/v1/trades", Some(&alice), Some(body)).await }
    };
    let (st, t) = propose().await;
    assert_eq!(st, StatusCode::OK, "{t}");
    let tid = t["id"].as_str().unwrap().to_string();
    let (st, t) = call(
        &app,
        "POST",
        &format!("/v1/trades/{tid}/accept"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["status"], "hold", "{t}");
    assert!(t["settle_at"].is_string());
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&bob), None).await;
    assert_eq!(
        balance_of(&w, &gold),
        9_080 - 50,
        "bob's 50 are escrowed: {w}"
    );
    // Either side may back out during the hold; everything returns.
    let (st, t) = call(
        &app,
        "POST",
        &format!("/v1/trades/{tid}/cancel"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["status"], "cancelled");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&bob), None).await;
    assert_eq!(balance_of(&w, &gold), 9_080, "{w}");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&alice), None).await;
    assert_eq!(held_of(&inv, &ore), 30, "{inv}");
    // Accept again and let the hold run out: the sweep settles it.
    let (st, t) = propose().await;
    assert_eq!(st, StatusCode::OK, "{t}");
    let tid = t["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/trades/{tid}/accept"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let (st, r) = call_server(&app, "POST", "/v1/server/market/settle", k, None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (_, t) = call(&app, "GET", &format!("/v1/trades/{tid}"), Some(&bob), None).await;
    assert_eq!(t["status"], "accepted", "{t}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&alice), None).await;
    assert_eq!(
        balance_of(&w, &gold),
        665 + 50,
        "70 ore sold at 10 minus 5%, plus the trade: {w}"
    );
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&bob), None).await;
    assert_eq!(held_of(&inv, &ore), 95, "{inv}");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&alice), None).await;
    assert_eq!(held_of(&inv, &ore), 25, "{inv}");
}

#[tokio::test]
async fn rating_queues_elo_glicko_openskill_seasons_and_decay() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..4 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("rt")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, a) = (&users[0].0, &users[0].1);
    let (_, b) = (&users[1].0, &users[1].1);
    let (_, c) = (&users[2].0, &users[2].1);
    let (_, d) = (&users[3].0, &users[3].1);
    let key = "defaultkey";
    let q = unique("elo");

    // queue needs a server key; bad params are refused
    let (st, _) = call(
        &app,
        "PUT",
        "/v1/server/rating/queues",
        Some(ta),
        Some(json!({"id": q})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/rating/queues",
        key,
        Some(json!({"id": q, "params": {"glicko_tau": 9.0}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call_server(
        &app,
        "PUT",
        "/v1/server/rating/queues",
        key,
        Some(json!({"id": q, "system": "elo", "params": {"elo_k": 32.0, "placement_games": 2}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["season"], 1);

    // never played → initial rating, not placed
    let (st, v) = call(&app, "GET", &format!("/v1/rating/{q}"), Some(ta), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["display"], 1500.0);
    assert_eq!(v["placed"], false);
    let (st, _) = call(&app, "GET", "/v1/rating/no-such-queue", Some(ta), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // a beats b: placement K (32 × 2) × 0.5 expected = ±32
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/rating/queues/{q}/results"),
        key,
        Some(json!({"teams": [[a], [b]], "match_id": "m1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let ch = v["changes"].as_array().unwrap();
    assert_eq!(ch[0]["after"], 1532.0);
    assert_eq!(ch[1]["after"], 1468.0);
    assert_eq!(ch[0]["rating"]["wins"], 1);
    assert_eq!(ch[1]["rating"]["losses"], 1);
    // a player in two teams is refused
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/rating/queues/{q}/results"),
        key,
        Some(json!({"teams": [[a], [a]]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // 2v2 draw between fresh c,d and a,b: a,b are 1500 on average → nothing moves
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/rating/queues/{q}/results"),
        key,
        Some(json!({"teams": [[a, b], [c, d]], "ranks": [1, 1]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    for ch in v["changes"].as_array().unwrap() {
        assert_eq!(ch["before"], ch["after"], "{ch}");
        assert_eq!(ch["rating"]["draws"], 1);
    }
    // ladder lists only placed players (2 games): a and b, a first
    let (st, v) = call(&app, "GET", &format!("/v1/rating/{q}/top"), Some(ta), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let top = v["ratings"].as_array().unwrap();
    assert_eq!(top.len(), 2, "{v}");
    assert_eq!(top[0]["account_id"], *a);
    assert_eq!(top[0]["placed"], true);
    let (st, v) = call(&app, "GET", &format!("/v1/rating/{q}/{c}"), Some(ta), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["games"], 1);
    assert_eq!(v["placed"], false);

    // Glicko-2 and OpenSkill queues: winner up, loser down, uncertainty shrinks
    for (system, id) in [("glicko2", unique("gl")), ("openskill", unique("os"))] {
        let (st, _) = call_server(
            &app,
            "PUT",
            "/v1/server/rating/queues",
            key,
            Some(json!({"id": id, "system": system})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, v) = call_server(
            &app,
            "POST",
            &format!("/v1/server/rating/queues/{id}/results"),
            key,
            Some(json!({"teams": [[a], [b]]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let ch = v["changes"].as_array().unwrap();
        assert!(ch[0]["after"].as_f64().unwrap() > 1500.0, "{system}: {v}");
        assert!(ch[1]["after"].as_f64().unwrap() < 1500.0, "{system}: {v}");
        let init_sigma = if system == "glicko2" {
            350.0
        } else {
            25.0 / 3.0
        };
        assert!(
            ch[0]["rating"]["sigma"].as_f64().unwrap() < init_sigma,
            "{system}: {v}"
        );
        assert!(
            ch[0]["rating"]["conservative"].as_f64().unwrap()
                < ch[0]["rating"]["display"].as_f64().unwrap()
        );
    }

    // season rollover carries half of the distance from 1500; decay has nothing to do yet
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/rating/queues/{q}/season"),
        key,
        Some(json!({"carry": 0.5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["season"], 2);
    let (_, v) = call(&app, "GET", &format!("/v1/rating/{q}"), Some(ta), None).await;
    assert_eq!(v["season"], 2);
    assert_eq!(v["display"], 1516.0);
    assert_eq!(v["games"], 0);
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/rating/queues/{q}/decay"),
        key,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["decayed"], 0);
    let (st, v) = call_server(&app, "GET", "/v1/server/rating/queues", key, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v["queues"].as_array().unwrap().iter().any(|x| x["id"] == q));
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/rating/queues/{q}"),
        key,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", &format!("/v1/rating/{q}"), Some(ta), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn bucketed_league_leaderboard_scopes_ranks_and_rolls_over() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..4 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("lg")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let key = "defaultkey";
    let id = unique("league");
    // invalid shapes
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        key,
        Some(json!({"id": id, "bucket_size": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        key,
        Some(
            json!({"id": id, "league": {"tiers": ["a", "b"], "promote_pct": 10, "demote_pct": 10}}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "league without reset schedule");
    let (st, board) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        key,
        Some(json!({
            "id": id, "reset_schedule": "0 0 * * *", "bucket_size": 2,
            "league": {"tiers": ["bronze", "silver", "gold"], "promote_pct": 50, "demote_pct": 50}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{board}");
    assert_eq!(board["bucket_size"], 2);
    assert_eq!(board["league"]["tiers"][1], "silver");

    // four scores → two buckets of two, in order of first score
    for (i, (tok, _)) in users.iter().enumerate() {
        let (st, r) = call(
            &app,
            "POST",
            &format!("/v1/leaderboards/{id}/records"),
            Some(tok),
            Some(json!({"score": (i as i64 + 1) * 10})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        assert_eq!(r["tier"], 0);
        assert_eq!(r["bucket"], (i / 2 + 1) as i64, "{r}");
        assert_eq!(
            r["rank"], 1,
            "newest score of the bucket is its best so far: {r}"
        );
    }
    // user 0 sees only bucket 1: user 1 (20) above user 0 (10)
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{id}/records?owner_ids={}", users[0].1),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let recs = v["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2, "{v}");
    assert_eq!(recs[0]["owner_id"], users[1].1);
    assert_eq!(recs[1]["owner_id"], users[0].1);
    assert_eq!(v["owner_records"][0]["rank"], 2);
    // user 3 sees bucket 2 and is first there
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{id}/around/{}", users[3].1),
        Some(&users[3].0),
        None,
    )
    .await;
    assert_eq!(v["records"][0]["owner_id"], users[3].1, "{v}");
    assert_eq!(v["records"].as_array().unwrap().len(), 2);
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{id}/tier"),
        Some(&users[1].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "bronze");

    // end the season now: top 50 % of each bucket (users 1 and 3) go up, nobody below bronze
    let expiry = board["next_reset"].as_i64().unwrap();
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/leaderboards/{id}/rollover"),
        key,
        Some(json!({"expiry": expiry})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["rollover"]["promoted"], 2, "{v}");
    assert_eq!(v["rollover"]["demoted"], 0, "{v}");
    let (_, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/leaderboards/{id}/rollover"),
        key,
        Some(json!({"expiry": expiry})),
    )
    .await;
    assert!(
        v["rollover"].is_null(),
        "second rollover of the same window is a no-op: {v}"
    );
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{id}/tier"),
        Some(&users[1].0),
        None,
    )
    .await;
    assert_eq!(v["name"], "silver", "{v}");
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{id}/tier"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(v["name"], "bronze", "{v}");
    // a rollover on a board without a league is refused
    let plain = unique("plain");
    call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        key,
        Some(json!({"id": plain})),
    )
    .await;
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/leaderboards/{plain}/rollover"),
        key,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn private_tournament_requires_join_code() {
    // the shared test database accumulates tournaments: list by a category only this run uses
    let cat = 1000
        + (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
            % 1_000_000) as i32;
    let Some(app) = app().await else { return };
    let mut toks = vec![];
    for _ in 0..2 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("pt")})),
        )
        .await;
        toks.push(s["access_token"].as_str().unwrap().to_string());
    }
    let id = unique("private");
    let (st, t) = call_server(
        &app,
        "POST",
        "/v1/server/tournaments",
        "defaultkey",
        Some(json!({"id": id, "duration": 3600, "join_code": "s3cret", "join_required": false, "category": cat})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["private"], true);
    assert!(
        t.get("join_code").is_none(),
        "code never leaves the server: {t}"
    );
    let (_, t) = call(
        &app,
        "GET",
        &format!("/v1/tournaments/{id}"),
        Some(&toks[0]),
        None,
    )
    .await;
    assert_eq!(t["private"], true);
    let path = format!("/v1/tournaments/{id}/join");
    let (st, _) = call(&app, "POST", &path, Some(&toks[0]), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "POST",
        &path,
        Some(&toks[0]),
        Some(json!({"code": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        &app,
        "POST",
        &path,
        Some(&toks[0]),
        Some(json!({"code": "s3cret"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // joined player scores; a stranger cannot score their way in even though join is optional
    let rec = format!("/v1/tournaments/{id}/records");
    let (st, v) = call(
        &app,
        "POST",
        &rec,
        Some(&toks[0]),
        Some(json!({"score": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        &rec,
        Some(&toks[1]),
        Some(json!({"score": 9})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, l) = call(
        &app,
        "GET",
        &format!("/v1/tournaments?limit=100&category_start={cat}&category_end={cat}"),
        Some(&toks[1]),
        None,
    )
    .await;
    let mine = l["tournaments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == id)
        .cloned();
    if let Some(m) = mine {
        assert_eq!(m["private"], true, "{m}");
    }
}

#[tokio::test]
async fn lobbies_lifecycle_teams_password_and_start() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..5 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("lb")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, a) = users[0].clone();
    let (tb, b) = users[1].clone();
    let (tc, c) = users[2].clone();
    let (td, d) = users[3].clone();
    let (te, _e) = users[4].clone();
    let mode = unique("ctf");

    // shape validation
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies",
        Some(&ta),
        Some(json!({"name": "x", "team_count": 2, "team_size": 1, "max_members": 4})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

    let (st, l) = call(
        &app,
        "POST",
        "/v1/lobbies",
        Some(&ta),
        Some(json!({
            "name": "Alpha", "visibility": "private", "password": "pw",
            "team_count": 2, "team_size": 2, "metadata": {"mode": mode}, "region": "eu"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let id = l["id"].as_str().unwrap().to_string();
    let code = l["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 6);
    assert_eq!(l["has_password"], true);
    assert_eq!(l["max_members"], 4);
    assert_eq!(l["member_count"], 1);
    assert_eq!(l["members"][0]["account_id"], a);
    assert_eq!(l["host_id"], a);
    // a second lobby for the same host is refused
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies",
        Some(&ta),
        Some(json!({"name": "Beta"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");

    // private: not listed, not joinable by id, code hidden from outsiders
    let filter = json!({"mode": mode})
        .to_string()
        .replace('{', "%7B")
        .replace('}', "%7D")
        .replace('"', "%22")
        .replace(':', "%3A");
    let path = format!("/v1/lobbies?metadata={filter}");
    let (_, v) = call(&app, "GET", &path, Some(&tb), None).await;
    assert_eq!(v["lobbies"].as_array().unwrap().len(), 0, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&tb),
        Some(json!({"id": id, "password": "pw"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (_, v) = call(&app, "GET", &format!("/v1/lobbies/{id}"), Some(&tb), None).await;
    assert!(v.get("code").is_none(), "{v}");
    // wrong / missing password
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&tb),
        Some(json!({"code": code.to_lowercase(), "password": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    // B joins by code, picks team 0 (host is there, room for one more)
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&tb),
        Some(json!({"code": code.to_lowercase(), "password": "pw", "team": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["member_count"], 2);
    assert_eq!(v["code"], code);

    // host flips it public: now listed with filters; C auto-lands on the emptier team 1
    let (st, v) = call(
        &app,
        "PATCH",
        &format!("/v1/lobbies/{id}"),
        Some(&ta),
        Some(json!({"visibility": "public"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v.get("code").is_none(), "{v}");
    let (_, v) = call(&app, "GET", &path, Some(&tc), None).await;
    assert_eq!(v["lobbies"][0]["id"], id, "{v}");
    assert_eq!(v["lobbies"][0]["member_count"], 2);
    assert!(v["lobbies"][0]["members"].as_array().unwrap().is_empty());
    let (_, v) = call(
        &app,
        "GET",
        "/v1/lobbies?name=alp&region=eu",
        Some(&tc),
        None,
    )
    .await;
    assert!(
        v["lobbies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == id),
        "{v}"
    );
    let (_, v) = call(
        &app,
        "GET",
        "/v1/lobbies?name=alp&region=us",
        Some(&tc),
        None,
    )
    .await;
    assert!(
        !v["lobbies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == id),
        "{v}"
    );
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&tc),
        Some(json!({"id": id, "password": "pw"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let c_team = v["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["account_id"] == c)
        .unwrap()["team"]
        .clone();
    assert_eq!(c_team, 1);
    // D: team 0 is full, team 1 fits; then the lobby is full and drops out of the listing
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&td),
        Some(json!({"id": id, "password": "pw", "team": 0})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&td),
        Some(json!({"id": id, "password": "pw", "team": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&te),
        Some(json!({"id": id, "password": "pw"})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (_, v) = call(&app, "GET", &path, Some(&te), None).await;
    assert_eq!(v["lobbies"].as_array().unwrap().len(), 0, "{v}");
    let (_, v) = call(
        &app,
        "GET",
        &format!("{path}&has_slots=false"),
        Some(&te),
        None,
    )
    .await;
    assert_eq!(v["lobbies"][0]["id"], id, "{v}");

    // ready gate, kick, team switch
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/start"),
        Some(&ta),
        Some(json!({"connect": {"host": "1.2.3.4"}, "require_ready": true})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    for t in [&tb, &tc] {
        let (st, v) = call(
            &app,
            "POST",
            &format!("/v1/lobbies/{id}/ready"),
            Some(t),
            Some(json!({"ready": true})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/kick"),
        Some(&tb),
        Some(json!({"account_id": d})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/kick"),
        Some(&ta),
        Some(json!({"account_id": d})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, v) = call(&app, "GET", "/v1/lobbies/mine", Some(&td), None).await;
    assert!(v["lobby"].is_null(), "{v}");
    let (_, v) = call(&app, "GET", "/v1/notifications", Some(&td), None).await;
    let kicked =
        v["notifications"].as_array().unwrap().iter().any(|n| {
            n["subject"] == "lobby" && n["content"]["event"] == "kicked" && n["code"] == 20
        });
    assert!(kicked, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/team"),
        Some(&tc),
        Some(json!({"team": 0})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "team 0 is full: {v}");
    // non-member cannot start or heartbeat
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/heartbeat"),
        Some(&td),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/heartbeat"),
        Some(&tc),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // start with the host's own connect info; everyone gets the persisted `started`
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/start"),
        Some(&ta),
        Some(json!({"connect": {"host": "1.2.3.4"}, "require_ready": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["state"], "started");
    assert_eq!(v["started"]["kind"], "custom");
    assert_eq!(v["started"]["connect"]["host"], "1.2.3.4");
    let (_, v) = call(&app, "GET", "/v1/notifications", Some(&tc), None).await;
    let started = v["notifications"].as_array().unwrap().iter().any(|n| {
        n["subject"] == "lobby"
            && n["content"]["event"] == "started"
            && n["content"]["started"]["connect"]["host"] == "1.2.3.4"
    });
    assert!(started, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/start"),
        Some(&ta),
        Some(json!({"connect": {}})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies/join",
        Some(&te),
        Some(json!({"id": id, "password": "pw"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");

    // host leaves → longest-standing member (B) takes over; B closes
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/lobbies/{id}/leave"),
        Some(&ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = call(&app, "GET", &format!("/v1/lobbies/{id}"), Some(&tb), None).await;
    assert_eq!(v["host_id"], b, "{v}");
    assert_eq!(v["member_count"], 2);
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/lobbies/{id}"),
        Some(&tc),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/lobbies/{id}"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", &format!("/v1/lobbies/{id}"), Some(&tc), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, v) = call(&app, "GET", "/v1/lobbies/mine", Some(&tc), None).await;
    assert!(v["lobby"].is_null(), "{v}");
    // A is free again
    let (st, v) = call(
        &app,
        "POST",
        "/v1/lobbies",
        Some(&ta),
        Some(json!({"name": "Beta"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn friends_notes_favorites_and_suggestions() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..4 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("fs")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, a) = users[0].clone();
    let (tb, b) = users[1].clone();
    let (tc, c) = users[2].clone();
    let (td, d) = users[3].clone();
    // A–B, A–C, B–D accepted friendships (invite + accept)
    for (x, y, yid, xid) in [(&ta, &tb, &b, &a), (&ta, &tc, &c, &a), (&tb, &td, &d, &b)] {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/friends",
            Some(x),
            Some(json!({"ids": [yid]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        let (st, v) = call(
            &app,
            "POST",
            "/v1/friends",
            Some(y),
            Some(json!({"ids": [xid]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    // D is a friend of A's friend B → suggested to A with one mutual friend; C is already a friend
    let (st, v) = call(&app, "GET", "/v1/friends/suggestions", Some(&ta), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let sugg = v["suggestions"].as_array().unwrap();
    assert_eq!(sugg.len(), 1, "{v}");
    assert_eq!(sugg[0]["user"]["id"], d);
    assert_eq!(sugg[0]["mutual_friends"], 1);
    assert_eq!(sugg[0]["reason"], "mutual");
    // for D: A (via B)
    let (_, v) = call(&app, "GET", "/v1/friends/suggestions", Some(&td), None).await;
    assert_eq!(v["suggestions"][0]["user"]["id"], a, "{v}");
    // D blocks A → A no longer sees D
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends/block",
        Some(&td),
        Some(json!({"ids": [a]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = call(&app, "GET", "/v1/friends/suggestions", Some(&ta), None).await;
    assert!(v["suggestions"].as_array().unwrap().is_empty(), "{v}");

    // note + favourite on B; not on a stranger
    let (st, v) = call(
        &app,
        "PATCH",
        &format!("/v1/friends/{b}"),
        Some(&ta),
        Some(json!({"note": "healer main", "favorite": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/v1/friends/{d}"),
        Some(&ta),
        Some(json!({"favorite": true})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, l) = call(&app, "GET", "/v1/friends", Some(&ta), None).await;
    let fb = l["friends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["user"]["id"] == b)
        .unwrap();
    assert_eq!(fb["note"], "healer main");
    assert_eq!(fb["favorite"], true);
    let fc = l["friends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["user"]["id"] == c)
        .unwrap();
    assert!(fc["note"].is_null());
    assert_eq!(fc["favorite"], false);
    // null clears the note, favourite untouched
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/v1/friends/{b}"),
        Some(&ta),
        Some(json!({"note": null})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, l) = call(&app, "GET", "/v1/friends", Some(&ta), None).await;
    let fb = l["friends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["user"]["id"] == b)
        .unwrap();
    assert!(fb["note"].is_null(), "{l}");
    assert_eq!(fb["favorite"], true);
    // the note is private: B's view of A carries B's own (empty) note
    let (_, l) = call(&app, "GET", "/v1/friends", Some(&tb), None).await;
    assert!(l["friends"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["note"].is_null()));
    // too long
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/v1/friends/{b}"),
        Some(&ta),
        Some(json!({"note": "x".repeat(300)})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let _ = tc;
}

#[tokio::test]
async fn groups_bans_announcements_and_events() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("gb")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, _a) = users[0].clone();
    let (tb, b) = users[1].clone();
    let (tc, _c) = users[2].clone();
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(&ta),
        Some(json!({"name": unique("clan"), "open": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    assert!(g["announcement"].is_null());
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // member cannot ban; admin bans B with a reason → B is out and cannot rejoin
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/ban"),
        Some(&tb),
        Some(json!({"ids": [b], "reason": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/ban"),
        Some(&ta),
        Some(json!({"ids": [b], "reason": "toxic"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, m) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/members"),
        Some(&ta),
        None,
    )
    .await;
    assert!(!m["items"].to_string().contains(&b), "{m}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/bans"),
        Some(&ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["bans"][0]["user_id"], b);
    assert_eq!(v["bans"][0]["reason"], "toxic");
    assert!(v["bans"][0]["until"].is_null());
    // lift → join works again; a timed ban blocks too
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/groups/{gid}/ban"),
        Some(&ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/ban"),
        Some(&ta),
        Some(json!({"ids": [b], "until_secs": 3600})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/bans"),
        Some(&ta),
        None,
    )
    .await;
    assert!(v["bans"][0]["until"].is_i64(), "{v}");
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/groups/{gid}/ban"),
        Some(&ta),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // announcement: shown on the group, members notified
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/announcement"),
        Some(&tb),
        Some(json!({"text": "raid at 8"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/announcement"),
        Some(&ta),
        Some(json!({"text": "raid at 8"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["announcement"], "raid at 8");
    assert!(v["announcement_at"].is_i64());
    let (_, n) = call(&app, "GET", "/v1/notifications", Some(&tb), None).await;
    let got = n["notifications"].as_array().unwrap().iter().any(|n| {
        n["subject"] == "group_announcement"
            && n["code"] == 30
            && n["content"]["text"] == "raid at 8"
            && n["content"]["group_id"] == gid
    });
    assert!(got, "{n}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/announcement"),
        Some(&ta),
        Some(json!({"text": null})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["announcement"].is_null());

    // journal: members only, newest first
    let (st, _) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/events"),
        Some(&tc),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/events"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let kinds: Vec<&str> = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "announcement",
            "announcement",
            "joined",
            "unbanned",
            "banned",
            "joined",
            "unbanned",
            "banned",
            "joined"
        ],
        "{v}"
    );
    let banned = v["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "banned" && e["payload"]["reason"] == "toxic")
        .unwrap();
    assert_eq!(banned["subject_id"], b);
    // cursor pages
    let (_, p) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/events?limit=2"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(p["events"].as_array().unwrap().len(), 2);
    let cur = p["cursor"].as_str().unwrap();
    let (_, p2) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/events?limit=2&cursor={cur}"),
        Some(&tb),
        None,
    )
    .await;
    assert_eq!(p2["events"][0]["kind"], "joined", "{p2}");
}

#[tokio::test]
async fn storage_patch_sort_count_and_quota() {
    let Some(app) = app_with(|c| c.storage.max_objects_per_user = 3).await else {
        return;
    };
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("st")})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap().to_string();
    let col = unique("scores");
    let objects: Vec<Value> = [("a", 10, "x"), ("b", 30, "y"), ("c", 20, "x")]
        .iter()
        .map(|(k, score, tag)| {
            json!({"collection": col, "key": k, "value": {"score": score, "tag": tag, "stats": {"wins": score / 10}}})
        })
        .collect();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&t),
        Some(json!({"objects": objects})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // a 4th object breaks the quota of 3 and nothing of the batch lands
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "d", "value": {"score": 1}}]})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    assert!(v.to_string().contains("quota"), "{v}");
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}/count"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(v["count"], 3, "{v}");
    // deleting frees the slot
    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/delete",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "c"}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "c", "value": {"score": 20, "tag": "x", "stats": {"wins": 2}}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    // sort asc / desc / nested, with a cursor
    let keys = |v: &Value| -> Vec<String> {
        v["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["key"].as_str().unwrap().to_string())
            .collect()
    };
    let (st, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?sort=score"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(keys(&v), vec!["a", "c", "b"]);
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?sort=-score"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(keys(&v), vec!["b", "c", "a"]);
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?sort=-stats.wins&limit=2"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(keys(&v), vec!["b", "c"]);
    let cur = v["cursor"].as_str().unwrap().to_string();
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}?sort=-stats.wins&limit=2&cursor={cur}"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(keys(&v), vec!["a"]);
    assert!(v["cursor"].is_null());
    // count with where
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}/count?where=%7B%22tag%22%3A%22x%22%7D"),
        Some(&t),
        None,
    )
    .await;
    assert_eq!(v["count"], 2, "{v}");

    // merge patch: nested merge, null removes, OCC
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/patch",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "a", "patch": {"score": 15, "stats": {"losses": 4}, "tag": null}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let ver = v["acks"][0]["version"].as_str().unwrap().to_string();
    let (_, r) = call(
        &app,
        "POST",
        "/v1/storage/read",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "a"}]})),
    )
    .await;
    assert_eq!(
        r["objects"][0]["value"],
        json!({"score": 15, "stats": {"wins": 1, "losses": 4}}),
        "{r}"
    );
    assert_eq!(r["objects"][0]["version"], ver);
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/patch",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "a", "patch": {"score": 1}, "version": "stale"}]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/patch",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "zzz", "patch": {"score": 1}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/patch",
        Some(&t),
        Some(json!({"objects": [{"collection": col, "key": "a", "patch": {"score": 16}, "version": ver}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn account_sessions_list_and_revoke_one_device() {
    let Some(app) = app().await else { return };
    let dev = unique("dv");
    let (_, s1) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev})),
    )
    .await;
    let t1 = s1["access_token"].as_str().unwrap().to_string();
    // same account from a second device
    let (_, s2) = call(
        &app,
        "POST",
        "/v1/account/link/device",
        Some(&t1),
        Some(json!({"id": unique("dv2")})),
    )
    .await;
    let _ = s2;
    let (_, s2) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev})),
    )
    .await;
    let t2 = s2["access_token"].as_str().unwrap().to_string();
    let r2 = s2["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(s1["account_id"], s2["account_id"]);
    let (st, v) = call(&app, "GET", "/v1/account/sessions", Some(&t1), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let list = v["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{v}");
    assert_eq!(list.iter().filter(|s| s["current"] == true).count(), 1);
    let other = list.iter().find(|s| s["current"] == false).unwrap();
    assert_eq!(other["device_id"], dev);
    let other_id = other["id"].as_str().unwrap().to_string();
    // revoke the other device: it cannot refresh any more, the caller still can
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/account/sessions/{other_id}"),
        Some(&t1),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": r2})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (_, v) = call(&app, "GET", "/v1/account/sessions", Some(&t1), None).await;
    assert_eq!(v["sessions"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["sessions"][0]["current"], true);
    // revoking an unknown / foreign session is 404
    let (st, _) = call(
        &app,
        "DELETE",
        &format!("/v1/account/sessions/{}", uuid::Uuid::now_v7()),
        Some(&t1),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let _ = t2;
}

#[tokio::test]
async fn account_merge_folds_guest_into_main() {
    let Some(app) = app().await else { return };
    let mut tok = vec![];
    let mut ids = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("mg")})),
        )
        .await;
        tok.push(s["access_token"].as_str().unwrap().to_string());
        ids.push(s["account_id"].as_str().unwrap().to_string());
    }
    let (ta, tb, tc) = (&tok[0], &tok[1], &tok[2]);
    let (a, b, c) = (&ids[0], &ids[1], &ids[2]);
    let guest_dev = unique("gdev");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/account/link/device",
        Some(tb),
        Some(json!({"id": guest_dev})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // guest B: storage (one clashing key, one unique), friend C, a group
    let col = unique("save");
    for (t, k, v) in [(tb, "slot1", 1), (tb, "slot2", 2), (ta, "slot1", 10)] {
        let (st, _) = call(
            &app,
            "POST",
            "/v1/storage/write",
            Some(t),
            Some(json!({"objects": [{"collection": col, "key": k, "value": {"v": v}}]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(tb),
        Some(json!({"ids": [c]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(tc),
        Some(json!({"ids": [b]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(tb),
        Some(json!({"name": unique("mgrp")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();

    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/merge",
        Some(ta),
        Some(json!({"access_token": ta})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/merge",
        Some(ta),
        Some(json!({"access_token": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/merge",
        Some(ta),
        Some(json!({"access_token": tb})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["merged"], *b);
    assert_eq!(v["report"]["storage_moved"], 1, "{v}");
    assert_eq!(v["report"]["storage_kept"], 1, "{v}");
    assert_eq!(v["report"]["friends"], 1, "{v}");
    assert_eq!(v["report"]["groups"], 1, "{v}");
    assert!(v["report"]["identities"].as_u64().unwrap() >= 2, "{v}");

    // A now owns slot2, kept its own slot1, is friends with C, owns the group; B's device logs into A
    let (_, r) = call(&app, "POST", "/v1/storage/read", Some(ta),
        Some(json!({"objects": [{"collection": col, "key": "slot1"}, {"collection": col, "key": "slot2"}]}))).await;
    let vals: Vec<i64> = r["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"]["v"].as_i64().unwrap())
        .collect();
    assert_eq!(vals.len(), 2, "{r}");
    assert!(vals.contains(&10) && vals.contains(&2), "{r}");
    let (_, f) = call(&app, "GET", "/v1/friends", Some(ta), None).await;
    assert!(f.to_string().contains(c), "{f}");
    let (_, m) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/members"),
        Some(ta),
        None,
    )
    .await;
    assert!(m.to_string().contains(a), "{m}");
    assert!(!m.to_string().contains(b), "{m}");
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": guest_dev, "create": false})),
    )
    .await;
    assert_eq!(s["account_id"], *a, "{s}");
    // B is gone
    let (st, _) = call(&app, "GET", "/v1/account/me", Some(tb), None).await;
    assert_ne!(st, StatusCode::OK);
}

#[tokio::test]
async fn liveops_receives_bus_events_and_flags() {
    use axum::body::Bytes;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    // fake Zanshin: records every request, answers flags
    let seen: StdArc<StdMutex<Vec<(String, String, Value)>>> = Default::default();
    let seen2 = seen.clone();
    let mock = axum::Router::new().fallback(
        move |method: axum::http::Method, uri: axum::http::Uri, body: Bytes| {
            let seen = seen2.clone();
            async move {
                let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                seen.lock()
                    .unwrap()
                    .push((method.to_string(), uri.to_string(), v));
                if uri.path().ends_with("/flags") {
                    axum::Json(json!({ "flags": [{ "name": "new_ui", "enabled": true }] }))
                } else {
                    axum::Json(json!({ "accepted": true }))
                }
            }
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let Some(app) = app_with(|c| {
        c.zanshin.url = Some(url.clone());
        c.zanshin.server_key = "k".into();
    })
    .await
    else {
        return;
    };
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("lo")})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap().to_string();
    let acc = s["account_id"].as_str().unwrap().to_string();
    // the bus event reaches Zanshin as an analytics event for that account
    let mut got = None;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let found = seen
            .lock()
            .unwrap()
            .iter()
            .find(|(m, p, b)| {
                m == "POST"
                    && p.ends_with("/server/v1/events")
                    && b["external_id"] == acc
                    && b["events"]
                        .as_array()
                        .is_some_and(|e| e.iter().any(|e| e["name"] == "account.authenticated"))
            })
            .cloned();
        if found.is_some() {
            got = found;
            break;
        }
    }
    let (_, _, body) = got.unwrap_or_else(|| {
        panic!(
            "account.authenticated not forwarded to liveops; seen: {:?}",
            seen.lock().unwrap()
        )
    });
    let ev = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "account.authenticated")
        .unwrap();
    assert_eq!(ev["metadata"]["provider"], "device", "{body}");
    // flags still answered through the proxy endpoint
    let (st, v) = call(
        &app,
        "GET",
        "/v1/liveops/flags?names=new_ui",
        Some(&t),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["flags"][0]["name"], "new_ui", "{v}");
    assert!(seen
        .lock()
        .unwrap()
        .iter()
        .any(|(m, p, _)| m == "GET" && p.contains("/server/v1/flags") && p.contains(&acc)));
}

#[tokio::test]
async fn totp_mfa_gates_email_login() {
    let Some(app) = app().await else { return };
    let email = format!("{}@example.com", unique("mfa"));
    let creds = json!({"email": email, "password": "Str0ng-Passw0rd!"});
    let (st, s) = call(&app, "POST", "/v1/auth/email", None, Some(creds.clone())).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    let t = s["access_token"].as_str().unwrap().to_string();
    let acc = s["account_id"].as_str().unwrap().to_string();
    let now = || time::OffsetDateTime::now_utc().unix_timestamp();

    let (st, v) = call(&app, "POST", "/v1/account/mfa/totp", Some(&t), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let secret = v["secret"].as_str().unwrap().to_string();
    assert!(
        v["otpauth"]
            .as_str()
            .unwrap()
            .starts_with("otpauth://totp/Musubi:"),
        "{v}"
    );
    let recovery: Vec<String> = v["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    assert_eq!(recovery.len(), 8);
    // not enforced until verified
    let (st, s2) = call(&app, "POST", "/v1/auth/email", None, Some(creds.clone())).await;
    assert_eq!(st, StatusCode::OK);
    assert!(s2["access_token"].is_string(), "{s2}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/account/mfa/totp/verify",
        Some(&t),
        Some(json!({"code": "000000"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let code = musubi_auth::totp::code_at(&secret, now()).unwrap();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/mfa/totp/verify",
        Some(&t),
        Some(json!({"code": code})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    // password login is now a challenge; the pending token is not a bearer
    let (st, c) = call(&app, "POST", "/v1/auth/email", None, Some(creds.clone())).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["mfa_required"], true, "{c}");
    assert_eq!(c["account_id"], acc);
    let mfa_token = c["mfa_token"].as_str().unwrap().to_string();
    let (st, _) = call(&app, "GET", "/v1/account/me", Some(&mfa_token), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/mfa",
        None,
        Some(json!({"mfa_token": mfa_token, "code": "123456"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/mfa",
        None,
        Some(json!({"mfa_token": t, "code": code})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let code = musubi_auth::totp::code_at(&secret, now()).unwrap();
    let (st, s3) = call(
        &app,
        "POST",
        "/v1/auth/mfa",
        None,
        Some(json!({"mfa_token": mfa_token, "code": code})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s3}");
    assert_eq!(s3["account_id"], acc);
    let (st, _) = call(
        &app,
        "GET",
        "/v1/account/me",
        Some(s3["access_token"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // a recovery code works exactly once
    let (st, c) = call(&app, "POST", "/v1/auth/email", None, Some(creds.clone())).await;
    assert_eq!(st, StatusCode::OK);
    let mfa_token = c["mfa_token"].as_str().unwrap().to_string();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/mfa",
        None,
        Some(json!({"mfa_token": mfa_token, "code": recovery[0]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, c) = call(&app, "POST", "/v1/auth/email", None, Some(creds.clone())).await;
    assert_eq!(st, StatusCode::OK);
    let mfa_token = c["mfa_token"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/mfa",
        None,
        Some(json!({"mfa_token": mfa_token, "code": recovery[0]})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // disable with a code → plain logins again
    let code = musubi_auth::totp::code_at(&secret, now()).unwrap();
    let (st, _) = call(
        &app,
        "DELETE",
        "/v1/account/mfa/totp",
        Some(&t),
        Some(json!({"code": code})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, s4) = call(&app, "POST", "/v1/auth/email", None, Some(creds)).await;
    assert_eq!(st, StatusCode::OK);
    assert!(s4["access_token"].is_string(), "{s4}");
}

#[tokio::test]
async fn fleet_builds_canary_promote_and_usage() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let name = unique("bfleet");
    let (st, f) = call_server(&app, "POST", "/v1/server/fleets", k,
        Some(json!({"name": name, "provider": "local", "buffer": 1, "max_servers": 2,
            "port_start": 48000, "port_end": 48100, "spec": {"command": ["sleep", "60"], "vcpu": 2}}))).await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let fid = f["id"].as_str().unwrap().to_string();
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/builds"),
        k,
        Some(json!({"version": "bad version!", "spec": {"command": ["sleep", "60"]}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v1) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/builds"),
        k,
        Some(json!({"version": "1.0.0", "spec": {"command": ["sleep", "60"], "vcpu": 2}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v1}");
    assert_eq!(v1["state"], "active");
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/builds"),
        k,
        Some(json!({"version": "1.0.0", "spec": {}})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, v2) = call_server(&app, "POST", &format!("/v1/server/fleets/{fid}/builds"), k,
        Some(json!({"version": "1.1.0", "spec": {"command": ["sleep", "60"], "vcpu": 4}, "canary_percent": 100}))).await;
    assert_eq!(st, StatusCode::OK, "{v2}");
    assert_eq!(v2["state"], "canary");
    // a new server picks the 100 % canary
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let svc =
        musubi_fleet::FleetService::new(db.clone(), uuid::Uuid::now_v7(), "http://127.0.0.1:0", k);
    let _ = svc.reconcile().await.unwrap();
    let (_, list) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/servers"),
        k,
        None,
    )
    .await;
    let servers = list["servers"].as_array().unwrap();
    assert!(!servers.is_empty(), "{list}");
    assert!(
        servers.iter().all(|s| s["build_version"] == "1.1.0"),
        "{list}"
    );
    // promote the canary: it is active, 1.0.0 retired; the active build cannot be retired
    let (st, p) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/builds/1.1.0/promote"),
        k,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["state"], "active");
    let (_, b) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/builds"),
        k,
        None,
    )
    .await;
    let state = |v: &str| {
        b["builds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["version"] == v)
            .unwrap()["state"]
            .clone()
    };
    assert_eq!(state("1.1.0"), "active");
    assert_eq!(state("1.0.0"), "retired");
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/fleets/{fid}/builds/1.1.0"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // rollback = promote the old build
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/builds/1.0.0/promote"),
        k,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/fleets/{fid}/builds/1.1.0"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/builds/nope/promote"),
        k,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // usage: the running server counts with the canary's 4 vCPU
    let (st, u) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/usage"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    let rows = u["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{u}");
    assert_eq!(rows[0]["build_version"], "1.1.0");
    assert_eq!(rows[0]["servers"], servers.len() as i64);
    assert!(rows[0]["vcpu_hours"].as_f64().unwrap() >= 0.0);
    assert_eq!(u["vcpu_hours"], rows[0]["vcpu_hours"]);
    let (st, _) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/usage?from=2030-01-01T00:00:00Z"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // stop what we started
    let (_, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/scale"),
        k,
        Some(json!({"buffer": 0, "min_servers": 0})),
    )
    .await;
    let _ = svc.reconcile().await;
}

/// Any OIDC issuer from `[social.oidc.<name>]`: JWKS found through discovery, tokens become
/// `oidc:<name>` identities, link/unlink like the built-in providers.
#[tokio::test]
async fn generic_oidc_login_and_link() {
    use axum::routing::get as ax_get;
    let jwks: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/google_test_jwks.json")).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let issuer = base.clone();
    let mock = Router::new()
        .route(
            "/.well-known/openid-configuration",
            ax_get(move || {
                let iss = issuer.clone();
                async move { axum::Json(json!({"issuer": iss, "jwks_uri": format!("{iss}/jwks")})) }
            }),
        )
        .route(
            "/jwks",
            ax_get(move || {
                let j = jwks.clone();
                async move { axum::Json(j) }
            }),
        );
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let b = base.clone();
    let Some(app) = app_with(move |c| {
        c.social.oidc.insert(
            "discord".into(),
            musubi_core::config::OidcLoginConfig {
                issuer: b.clone(),
                client_ids: vec!["cid".into()],
                ..Default::default()
            },
        );
    })
    .await
    else {
        return;
    };
    let key =
        jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("fixtures/google_test_rsa.pem"))
            .unwrap();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("t1".into());
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 600;
    let sign = |aud: &str, sub: &str| {
        jsonwebtoken::encode(
            &header,
            &json!({"iss": base, "aud": aud, "sub": sub, "exp": exp}),
            &key,
        )
        .unwrap()
    };
    let sub = unique("dsub-");
    let tok = sign("cid", &sub);
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/oidc/discord",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/oidc/discord",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(
        s1["account_id"], s2["account_id"],
        "same sub → same account"
    );

    // wrong audience, unknown provider, bad name
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/oidc/discord",
        None,
        Some(json!({"id_token": sign("other", &sub)})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/oidc/twitch",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/oidc/Bad%20Name",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // link a fresh subject to a device account, then unlink it
    let (st, dev) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dev-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dev}");
    let dt = dev["access_token"].as_str().unwrap();
    let sub2 = unique("dsub-");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/account/link/oidc/discord",
        Some(dt),
        Some(json!({"id_token": sign("cid", &sub2)})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, s3) = call(
        &app,
        "POST",
        "/v1/auth/oidc/discord",
        None,
        Some(json!({"id_token": sign("cid", &sub2)})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s3}");
    assert_eq!(
        s3["account_id"], dev["account_id"],
        "linked sub signs into the device account"
    );
    let (st, r) = call(
        &app,
        "DELETE",
        "/v1/account/unlink/oidc/discord",
        Some(dt),
        Some(json!({"subject": sub2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, s4) = call(
        &app,
        "POST",
        "/v1/auth/oidc/discord",
        None,
        Some(json!({"id_token": sign("cid", &sub2)})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s4}");
    assert_ne!(
        s4["account_id"], dev["account_id"],
        "after unlink the sub is a new account"
    );
}

/// Passkeys: register a P-256 credential (attestation `none`), sign in with it, replayed
/// assertion (counter did not advance) and a removed key are refused.
#[tokio::test]
async fn passkeys_register_and_login() {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
    use base64::Engine;
    use musubi_auth::passkey::{
        build_assertion_auth_data, build_none_attestation, FLAG_UP, FLAG_UV,
    };
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
    use sha2::{Digest, Sha256};
    let Some(app) = app_with(|c| {
        c.social.passkeys.rp_id = "localhost".into();
        c.social.passkeys.origins = vec!["http://localhost:3000".into()];
    })
    .await
    else {
        return;
    };
    let (st, dev) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("pk-dev-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dev}");
    let dt = dev["access_token"].as_str().unwrap();

    let (st, opts) = call(&app, "POST", "/v1/account/passkeys/options", Some(dt), None).await;
    assert_eq!(st, StatusCode::OK, "{opts}");
    assert_eq!(opts["publicKey"]["rp"]["id"], "localhost");
    let reg_token = opts["token"].as_str().unwrap().to_string();
    let challenge = opts["publicKey"]["challenge"].as_str().unwrap().to_string();
    // the challenge token is not an access token
    let (st, _) = call(&app, "GET", "/v1/account", Some(&reg_token), None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
    let kp =
        EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).unwrap();
    let cred_id = unique("cred-").into_bytes();
    let client_data = |kind: &str, ch: &str| {
        B64.encode(
            json!({"type": kind, "challenge": ch, "origin": "http://localhost:3000"}).to_string(),
        )
    };
    let att = build_none_attestation(
        "localhost",
        &cred_id,
        kp.public_key().as_ref(),
        0,
        FLAG_UP | FLAG_UV,
    );
    let credential = json!({
        "id": B64.encode(&cred_id),
        "response": {"clientDataJSON": client_data("webauthn.create", &challenge), "attestationObject": B64.encode(&att)}
    });
    let (st, r) = call(
        &app,
        "POST",
        "/v1/account/passkeys",
        Some(dt),
        Some(json!({"token": reg_token, "name": "YubiKey", "credential": credential})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["id"], B64.encode(&cred_id));
    // the same challenge cannot register twice (credential already linked)
    let (st, _) = call(
        &app,
        "POST",
        "/v1/account/passkeys",
        Some(dt),
        Some(json!({"token": reg_token, "credential": credential})),
    )
    .await;
    assert_ne!(st, StatusCode::OK);
    let (st, l) = call(&app, "GET", "/v1/account/passkeys", Some(dt), None).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["passkeys"].as_array().unwrap().len(), 1);
    assert_eq!(l["passkeys"][0]["name"], "YubiKey");

    // sign in with the key
    async fn assertion(
        app: &Router,
        kp: &EcdsaKeyPair,
        cred_id: &[u8],
        count: u32,
    ) -> serde_json::Value {
        let (st, o) = call(app, "POST", "/v1/auth/passkey/options", None, None).await;
        assert_eq!(st, StatusCode::OK, "{o}");
        let cdj = B64.encode(
            json!({"type": "webauthn.get", "challenge": o["publicKey"]["challenge"], "origin": "http://localhost:3000"}).to_string(),
        );
        let ad = build_assertion_auth_data("localhost", count, FLAG_UP);
        let mut msg = ad.clone();
        msg.extend_from_slice(&Sha256::digest(B64.decode(&cdj).unwrap()));
        let sig = kp.sign(&ring::rand::SystemRandom::new(), &msg).unwrap();
        json!({
            "token": o["token"],
            "credential": {"id": B64.encode(cred_id), "response": {"clientDataJSON": cdj, "authenticatorData": B64.encode(&ad), "signature": B64.encode(sig.as_ref())}}
        })
    }
    let body = assertion(&app, &kp, &cred_id, 1).await;
    let (st, s1) = call(&app, "POST", "/v1/auth/passkey", None, Some(body.clone())).await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    assert_eq!(s1["account_id"], dev["account_id"]);
    // replay: same assertion, counter did not advance
    let (st, _) = call(&app, "POST", "/v1/auth/passkey", None, Some(body)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/passkey",
        None,
        Some(assertion(&app, &kp, &cred_id, 2).await),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    let (st, l) = call(&app, "GET", "/v1/account/passkeys", Some(dt), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(l["passkeys"][0]["sign_count"], 2);

    // remove → the key no longer signs in
    let (st, r) = call(
        &app,
        "DELETE",
        &format!("/v1/account/passkeys/{}", B64.encode(&cred_id)),
        Some(dt),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/passkey",
        None,
        Some(assertion(&app, &kp, &cred_id, 3).await),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

/// Bans carry a player-visible message and an expiry; shadow bans keep the account working.
#[tokio::test]
async fn account_bans_message_expiry_and_shadow() {
    let Some(app) = app().await else {
        return;
    };
    let k = "defaultkey";
    let dev_id = unique("ban-dev-");
    let (st, a) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev_id})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    let at = a["access_token"].as_str().unwrap();
    let aid = a["account_id"].as_str().unwrap().to_string();

    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        Some(json!({"reason": "aimbot", "message": "cheating is not allowed", "until_secs": 3600})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["shadow"], false);
    assert!(r["until"].is_i64());
    // the player sees the message on requests and at login
    let (st, e) = call(&app, "GET", "/v1/account", Some(at), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{e}");
    let msg = e["message"].as_str().unwrap();
    assert!(
        msg.contains("cheating is not allowed") && msg.contains("until"),
        "{msg}"
    );
    let (st, e) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev_id})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("cheating is not allowed"),
        "{e}"
    );
    let (st, b) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["reason"], "aimbot");
    assert_eq!(b["active"], true);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{aid}/unban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", "/v1/account", Some(at), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // shadow: nothing is denied
    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        Some(json!({"shadow": true, "reason": "spam"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, _) = call(&app, "GET", "/v1/account", Some(at), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["shadow"], true);
    assert_eq!(b["until"], serde_json::Value::Null);

    // a short ban expires on its own (in-memory check; the leader sweep also clears the row)
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        Some(json!({"until_secs": 3})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // a loaded CI runner may take most of a second between the ban and this check
    let (st, _) = call(&app, "GET", "/v1/account", Some(at), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    tokio::time::sleep(std::time::Duration::from_millis(3300)).await;
    let (st, _) = call(&app, "GET", "/v1/account", Some(at), None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, b) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["active"], false);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{aid}/unban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // legacy: bare POST = permanent hard ban
    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{aid}/ban"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["until"], serde_json::Value::Null);
    let (st, _) = call(&app, "GET", "/v1/account", Some(at), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

/// Store depth: windows, bundles, level gates, timed discounts, coupons and daily rotation.
#[tokio::test]
async fn store_windows_bundles_discounts_coupons_and_rotation() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("store")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(json!({"account_id": acc, "currency": gold, "delta": 10_000, "reason": "test", "idempotency_key": unique("g")}))).await;
    assert_eq!(st, StatusCode::OK);
    let mk_item = |id: &str| json!({"id": id, "name": id, "category": "misc", "stackable": true});
    let (potion, gem, key) = (unique("potion"), unique("gem"), unique("key"));
    for it in [&potion, &gem, &key] {
        let (st, r) = call_server(&app, "POST", "/v1/server/items", k, Some(mk_item(it))).await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let now = time::OffsetDateTime::now_utc().unix_timestamp();

    // bundle with a 50 % discount still running
    let bundle = unique("bundle");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/store",
        k,
        Some(json!({
            "id": bundle, "item_id": potion, "quantity": 2, "prices": {gold.clone(): 1000},
            "bundle": [{"item_id": gem, "quantity": 3}, {"item_id": key}],
            "discount_percent": 50, "discount_until": now + 3600
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // not yet available, and expired
    let later = unique("later");
    let (st, _) = call_server(&app, "POST", "/v1/server/store", k, Some(json!({"id": later, "item_id": potion, "prices": {gold.clone(): 1}, "available_from": now + 3600}))).await;
    assert_eq!(st, StatusCode::OK);
    let gone = unique("gone");
    let (st, _) = call_server(&app, "POST", "/v1/server/store", k, Some(json!({"id": gone, "item_id": potion, "prices": {gold.clone(): 1}, "available_from": now - 7200, "available_until": now - 3600}))).await;
    assert_eq!(st, StatusCode::OK);
    // level gate
    let track = unique("track");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(
            json!({"id": track, "name": "Main", "levels": [{"xp": 100}, {"xp": 200}, {"xp": 300}]}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let gated = unique("gated");
    let (st, _) = call_server(&app, "POST", "/v1/server/store", k, Some(json!({"id": gated, "item_id": potion, "prices": {gold.clone(): 10}, "min_level": 3, "level_track": track}))).await;
    assert_eq!(st, StatusCode::OK);
    // rotation: 3 listings, 1 slot per day
    let rot = unique("rot");
    let rot_ids: Vec<String> = (0..3).map(|i| format!("{rot}-{i}")).collect();
    for id in &rot_ids {
        let (st, _) = call_server(&app, "POST", "/v1/server/store", k, Some(json!({"id": id, "item_id": potion, "prices": {gold.clone(): 5}, "rotation": rot, "rotation_slots": 1}))).await;
        assert_eq!(st, StatusCode::OK);
    }
    // validation
    let (st, _) = call_server(&app, "POST", "/v1/server/store", k, Some(json!({"id": unique("bad"), "item_id": potion, "prices": {}, "bundle": [{"item_id": "nope"}]}))).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, list) = call(&app, "GET", "/v1/store", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let items = list["items"].as_array().unwrap();
    let find = |id: &str| items.iter().find(|i| i["id"] == id);
    assert!(
        find(&later).is_none() && find(&gone).is_none(),
        "windows hide listings"
    );
    assert_eq!(find(&bundle).unwrap()["effective_prices"][&gold], 500);
    assert_eq!(find(&gated).unwrap()["locked"], "level");
    assert_eq!(
        rot_ids.iter().filter(|id| find(id).is_some()).count(),
        1,
        "one rotation slot"
    );
    let on_sale = rot_ids
        .iter()
        .find(|id| find(id).is_some())
        .unwrap()
        .clone();
    let off_sale = rot_ids
        .iter()
        .find(|id| find(id).is_none())
        .unwrap()
        .clone();
    // server view shows everything
    let (_, all) = call_server(&app, "GET", "/v1/server/store", k, None).await;
    assert!(all["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["id"] == gone));

    // buying: window, rotation and level gates bite
    for (id, code) in [
        (&later, StatusCode::FORBIDDEN),
        (&off_sale, StatusCode::FORBIDDEN),
        (&gated, StatusCode::FORBIDDEN),
    ] {
        let (st, r) = call(
            &app,
            "POST",
            "/v1/store/buy",
            Some(&tok),
            Some(json!({"store_item_id": id, "currency": gold})),
        )
        .await;
        assert_eq!(st, code, "{id}: {r}");
    }
    let (st, r) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": on_sale, "currency": gold})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // level up → gated listing opens
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc, "track_id": track, "delta": 300, "reason": "test"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": gated, "currency": gold})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");

    // coupon on top of the discount: 1000 → 500 → 400; once per account; restricted to the bundle
    let code = unique("SAVE");
    let (st, r) = call_server(
        &app,
        "PUT",
        "/v1/server/store/coupons",
        k,
        Some(json!({"code": code, "store_item_id": bundle, "percent": 20, "uses_left": 5})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": on_sale, "currency": gold, "coupon": code})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, b) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": bundle, "currency": gold, "coupon": code})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    assert_eq!(b["amount"], 400);
    // potion is stackable: 2 from this bundle on top of the two singles bought above
    assert_eq!(b["granted"]["item_id"], potion);
    assert_eq!(b["granted"]["quantity"], 4);
    assert_eq!(b["bundle"].as_array().unwrap().len(), 2);
    assert_eq!(b["bundle"][0]["quantity"], 3);
    let (st, r) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": bundle, "currency": gold, "coupon": code})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{r}");
    let (_, cs) = call_server(&app, "GET", "/v1/server/store/coupons", k, None).await;
    let c = cs["coupons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == code)
        .unwrap();
    assert_eq!(c["uses_left"], 4);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": bundle, "currency": gold, "coupon": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/store/coupons/{code}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/store/coupons/{code}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // without the coupon the discount alone applies
    let (st, b2) = call(
        &app,
        "POST",
        "/v1/store/buy",
        Some(&tok),
        Some(json!({"store_item_id": bundle, "currency": gold})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b2}");
    assert_eq!(b2["amount"], 500);
}

/// Game-server SDK: occupancy reports, backfill allocations onto a live server, WatchGameServer.
#[tokio::test]
async fn gameserver_watch_and_backfill() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let name = unique("wbfleet");
    let (st, f) = call_server(
        &app,
        "POST",
        "/v1/server/fleets",
        k,
        Some(
            json!({"name": name, "provider": "local", "buffer": 1, "max_servers": 2,
            "port_start": 48200, "port_end": 48300, "spec": {"command": ["sleep", "60"]}}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{f}");
    let fid = f["id"].as_str().unwrap().to_string();
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let svc =
        musubi_fleet::FleetService::new(db.clone(), uuid::Uuid::now_v7(), "http://127.0.0.1:0", k);
    let _ = svc.reconcile().await.unwrap();
    let (_, list) = call_server(
        &app,
        "GET",
        &format!("/v1/server/fleets/{fid}/servers"),
        k,
        None,
    )
    .await;
    let sid = list["servers"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{list}"))
        .to_string();

    // SDK calls carry the server key and the server's own id
    let sdk = |method: &str, path: &str, body: Option<Value>| {
        let app = app.clone();
        let sid = sid.clone();
        let (method, path) = (method.to_string(), path.to_string());
        async move {
            let mut req = Request::builder()
                .method(method.as_str())
                .uri(path)
                .header("x-musubi-server-key", "defaultkey")
                .header("x-musubi-server-id", sid);
            let req = match body {
                Some(b) => req
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(b.to_string()))
                    .unwrap(),
                None => {
                    req = req.header(header::CONTENT_TYPE, "application/json");
                    req.body(Body::empty()).unwrap()
                }
            };
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            let v: Value = serde_json::from_slice(&bytes)
                .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
            (status, v)
        }
    };
    let (st, gs) = sdk("POST", "/v1/gameserver/ready", None).await;
    assert_eq!(st, StatusCode::OK, "{gs}");
    assert_eq!(gs["state"], "ready");
    let (st, gs) = sdk(
        "POST",
        "/v1/gameserver/players",
        Some(json!({"players": 1, "capacity": 4})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{gs}");
    assert_eq!(gs["capacity"], 4);
    let v0 = gs["version"].as_i64().unwrap();

    // first allocation takes the ready server; a backfill one lands on the same live server
    let (st, a1) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/allocate"),
        k,
        Some(json!({"payload": {"match": "m1"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a1}");
    assert_eq!(a1["server_id"], sid);
    assert_eq!(a1["backfill"], false);
    let (st, a2) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/allocate"),
        k,
        Some(json!({"payload": {"match": "m1", "late": true}, "backfill": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a2}");
    assert_eq!(a2["server_id"], sid);
    assert_eq!(a2["backfill"], true);
    assert_eq!(
        a2["connect_token"], a1["connect_token"],
        "newcomers share the live match's token"
    );
    // a full server is not backfilled: no ready server left either → exhausted
    let (st, _) = sdk(
        "POST",
        "/v1/gameserver/players",
        Some(json!({"players": 4})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/fleets/{fid}/allocate"),
        k,
        Some(json!({"payload": {}, "backfill": true})),
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);

    // watch: the version moved past v0 → immediate; from the latest version → times out unchanged;
    // a label change wakes it up
    let (st, w) = sdk(
        "GET",
        &format!("/v1/gameserver/watch?since={v0}&timeout_secs=5"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");
    assert_eq!(w["changed"], true);
    let v1 = w["server"]["version"].as_i64().unwrap();
    assert!(v1 > v0);
    let started = std::time::Instant::now();
    let (st, w) = sdk(
        "GET",
        &format!("/v1/gameserver/watch?since={v1}&timeout_secs=1"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");
    assert_eq!(w["changed"], false);
    assert!(started.elapsed() >= std::time::Duration::from_millis(900));
    let waiter = tokio::spawn(sdk(
        "GET",
        &format!("/v1/gameserver/watch?since={v1}&timeout_secs=10"),
        None,
    ));
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    let (st, _) = sdk("POST", "/v1/gameserver/label", Some(json!({"map": "dust"}))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, w) = waiter.await.unwrap();
    assert_eq!(st, StatusCode::OK, "{w}");
    assert_eq!(w["changed"], true);
    assert_eq!(w["server"]["labels"]["map"], "dust");
    let (_, _) = sdk("POST", "/v1/gameserver/shutdown", None).await;
}

/// `POST /v1/server/liveops/changed` drops the cached flag answers: the next read goes to Zanshin.
#[tokio::test]
async fn liveops_changed_drops_cache() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;
    let hits: StdArc<AtomicUsize> = Default::default();
    let hits2 = hits.clone();
    let mock = axum::Router::new().fallback(move |uri: axum::http::Uri| {
        let hits = hits2.clone();
        async move {
            if uri.path().ends_with("/flags") {
                hits.fetch_add(1, Ordering::SeqCst);
                axum::Json(json!({ "flags": [{ "name": "x", "enabled": true }] }))
            } else {
                axum::Json(json!({ "accepted": true }))
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let Some(app) = app_with(|c| {
        c.zanshin.url = Some(url.clone());
        c.zanshin.server_key = "k".into();
    })
    .await
    else {
        return;
    };
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("loc")})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap().to_string();
    for _ in 0..2 {
        let (st, v) = call(&app, "GET", "/v1/liveops/flags?names=x", Some(&t), None).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert_eq!(v["flags"][0]["name"], "x");
    }
    assert_eq!(
        hits.load(Ordering::SeqCst),
        1,
        "second read served from cache"
    );
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/liveops/changed",
        "defaultkey",
        Some(json!({"flags": ["x"]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, _) = call(&app, "GET", "/v1/liveops/flags?names=x", Some(&t), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        hits.load(Ordering::SeqCst),
        2,
        "cache dropped by the change webhook"
    );
}

#[tokio::test]
async fn email_login_captcha_gate_and_session_ips() {
    // siteverify stand-in: Turnstile/hCaptcha/reCAPTCHA shape, "good" is the only valid token
    let mock = axum::Router::new().fallback(
        |axum::Form(f): axum::Form<std::collections::HashMap<String, String>>| async move {
            let ok = f.get("secret").map(String::as_str) == Some("s")
                && f.get("response").map(String::as_str) == Some("good");
            axum::Json(json!({ "success": ok, "remoteip": f.get("remoteip") }))
        },
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let Some(app) = app_with(|c| {
        c.session.captcha_after = 2;
        c.session.captcha_verify_url = Some(url.clone());
        c.session.captcha_secret = Some("s".into());
    })
    .await
    else {
        return;
    };
    let from = |ip: &'static str, body: Value| {
        let app = app.clone();
        async move {
            let req = Request::builder()
                .method("POST")
                .uri("/v1/auth/email")
                .header("x-forwarded-for", ip)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            (status, v)
        }
    };
    let email = format!("{}@example.com", unique("cap"));
    let good = json!({ "email": email, "password": "Str0ngPassw0rd!" });
    let (st, v) = from("10.0.0.1", good.clone()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    for _ in 0..2 {
        let (st, _) = from(
            "10.0.0.1",
            json!({ "email": email, "password": "wrong-password-1", "create": false }),
        )
        .await;
        assert_eq!(st, StatusCode::UNAUTHORIZED);
    }
    // two failures: the right password alone is no longer enough
    let (st, v) = from("10.0.0.1", good.clone()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert!(v.to_string().contains("captcha_required"), "{v}");
    let mut with_bad = good.clone();
    with_bad["captcha"] = json!("bad");
    let (st, v) = from("10.0.0.1", with_bad).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert!(v.to_string().contains("captcha_invalid"), "{v}");
    let mut with_good = good.clone();
    with_good["captcha"] = json!("good");
    let (st, v) = from("10.0.0.1", with_good).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // success clears the counter: plain password works again, now from a new address
    let (st, v) = from("10.0.0.2", good.clone()).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let t = v["access_token"].as_str().unwrap().to_string();
    // IPs are stamped on sessions (asynchronously) and show in the device list
    let mut ips = Vec::new();
    for _ in 0..40 {
        let (st, v) = call(&app, "GET", "/v1/account/sessions", Some(&t), None).await;
        assert_eq!(st, StatusCode::OK, "{v}");
        ips = v["sessions"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|s| s["ip"].as_str().map(str::to_string))
            .collect();
        if ips.iter().any(|i| i == "10.0.0.2") && ips.iter().any(|i| i == "10.0.0.1") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert!(
        ips.iter().any(|i| i == "10.0.0.2") && ips.iter().any(|i| i == "10.0.0.1"),
        "{ips:?}"
    );
}

#[tokio::test]
async fn groups_roles_bitmask_and_ranks() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("gr")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, _a) = &users[0];
    let (tb, b) = &users[1];
    let (tc, c) = &users[2];
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(ta),
        Some(json!({"name": unique("clan"), "open": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    for t in [tb, tc] {
        let (st, _) = call(
            &app,
            "POST",
            &format!("/v1/groups/{gid}/join"),
            Some(t),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    // defaults: eight ranks, members may only pay into the treasury
    let (st, r) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/roles"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["roles"].as_array().unwrap().len(), 8);
    assert_eq!(r["roles"][2]["permissions"], 64 | 256);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/kick"),
        Some(tb),
        Some(json!({"ids": [c]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "plain member cannot kick");
    // only the owner defines roles: members may kick and announce, a junior "recruit" tier (3) may do nothing
    let officer = json!({"roles": [{"rank": 2, "name": "member", "permissions": 2 | 8}, {"rank": 3, "name": "recruit", "permissions": 0}]});
    let (st, _) = call(
        &app,
        "PUT",
        &format!("/v1/groups/{gid}/roles"),
        Some(tb),
        Some(officer.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, r) = call(
        &app,
        "PUT",
        &format!("/v1/groups/{gid}/roles"),
        Some(ta),
        Some(officer),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["roles"][3]["name"], "recruit");
    assert_eq!(r["roles"][2]["permissions"], 10);
    let (st, v) = call(
        &app,
        "PUT",
        &format!("/v1/groups/{gid}/roles"),
        Some(ta),
        Some(json!({"roles": [{"rank": 0, "name": "x", "permissions": 1}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/rank"),
        Some(ta),
        Some(json!({"ids": [c], "rank": 3})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, m) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/members"),
        Some(ta),
        None,
    )
    .await;
    let rank_b = m["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["user"]["id"] == json!(c))
        .map(|x| x["rank"].clone());
    assert_eq!(rank_b, Some(json!(3)), "{m}");
    // a member may kick the junior recruit now, but cannot hand out ranks (no PROMOTE)
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/rank"),
        Some(tb),
        Some(json!({"ids": [c], "rank": 4})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/kick"),
        Some(tb),
        Some(json!({"ids": [c]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // the owner cannot be out-ranked: rank 0 is not grantable, and nobody can change the owner
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/rank"),
        Some(ta),
        Some(json!({"ids": [b], "rank": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // rank 1 also makes an admin in the legacy sense
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/rank"),
        Some(ta),
        Some(json!({"ids": [b], "rank": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, m) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/members?state=1"),
        Some(ta),
        None,
    )
    .await;
    assert!(
        m["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["user"]["id"] == json!(b)),
        "{m}"
    );
}

#[tokio::test]
async fn groups_treasury_deposit_and_withdraw() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut users = vec![];
    for _ in 0..2 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("tr")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, _a) = &users[0];
    let (tb, b) = &users[1];
    let gold = unique("gold");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(json!({"account_id": b, "currency": gold, "delta": 500, "reason": "seed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(ta),
        Some(json!({"name": unique("bank"), "open": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // a member pays in from their own wallet; more than they hold is refused
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/treasury/deposit"),
        Some(tb),
        Some(json!({"currency": gold, "amount": 300, "reason": "tax"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["entry"]["balance_after"], 300);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/treasury/deposit"),
        Some(tb),
        Some(json!({"currency": gold, "amount": 1000})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(tb), None).await;
    let bal = |w: &Value| {
        w["balances"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["currency"] == json!(gold))
            .map(|x| x["balance"].clone())
    };
    assert_eq!(bal(&w), Some(json!(200)), "{w}");
    let (st, t) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/treasury"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["balances"][0]["balance"], 300);
    // members may not pay out; the owner pays a member, never below zero
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/treasury/withdraw"),
        Some(tb),
        Some(json!({"currency": gold, "amount": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/treasury/withdraw"),
        Some(ta),
        Some(json!({"currency": gold, "amount": 100, "to": b, "reason": "salary"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["entry"]["balance_after"], 200);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/treasury/withdraw"),
        Some(ta),
        Some(json!({"currency": gold, "amount": 1000, "to": b})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(tb), None).await;
    assert_eq!(bal(&w), Some(json!(300)), "{w}");
    let (st, l) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/treasury/ledger"),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let entries = l["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{l}");
    assert_eq!(entries[0]["delta"], -100);
    assert_eq!(entries[0]["reason"], "salary");
    assert_eq!(entries[1]["delta"], 300);
}

#[tokio::test]
async fn groups_xp_levels_and_alliances() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut owners = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("al")})),
        )
        .await;
        let t = s["access_token"].as_str().unwrap().to_string();
        let (st, g) = call(
            &app,
            "POST",
            "/v1/groups",
            Some(&t),
            Some(json!({"name": unique("clan"), "open": true})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{g}");
        owners.push((t, g["id"].as_str().unwrap().to_string()));
    }
    let (ta, ga) = &owners[0];
    let (tb, gb) = &owners[1];
    let (tc, gc) = &owners[2];
    // XP: 1500 crosses the first threshold (1000), another 100 does not reach the next (3000)
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/groups/{ga}/xp"),
        k,
        Some(json!({"delta": 1500, "reason": "war"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["group"]["level"], 2);
    assert_eq!(v["leveled_up"], true);
    let (_, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/groups/{ga}/xp"),
        k,
        Some(json!({"delta": 100})),
    )
    .await;
    assert_eq!(v["group"]["xp"], 1600);
    assert_eq!(v["leveled_up"], false);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/groups/{ga}/xp"),
        k,
        Some(json!({"delta": -5})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // alliance: A founds, invites B; B accepts; C cannot accept without an invitation
    let (st, a) = call(
        &app,
        "POST",
        "/v1/alliances",
        Some(ta),
        Some(json!({"group_id": ga, "name": unique("pact")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    let aid = a["id"].as_str().unwrap().to_string();
    assert_eq!(a["members"].as_array().unwrap().len(), 1);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/alliances/{aid}/invite"),
        Some(tb),
        Some(json!({"group_id": gc})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "only the leader's owner invites");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/alliances/{aid}/invite"),
        Some(ta),
        Some(json!({"group_id": gb})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["members"][1]["state"], 1);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/alliances/{aid}/accept"),
        Some(tc),
        Some(json!({"group_id": gc})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/alliances/{aid}/accept"),
        Some(tb),
        Some(json!({"group_id": gb})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(
        v["members"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["state"] == 0),
        "{v}"
    );
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gb}/alliance"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(v["alliance"]["id"], json!(aid));
    // a group sits in one alliance at a time
    let (st, _) = call(
        &app,
        "POST",
        "/v1/alliances",
        Some(tb),
        Some(json!({"group_id": gb, "name": unique("pact")})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // B leaves; the leader leaving dissolves the alliance
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/alliances/{aid}/leave"),
        Some(tb),
        Some(json!({"group_id": gb})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, v) = call(&app, "GET", &format!("/v1/alliances/{aid}"), Some(ta), None).await;
    assert_eq!(v["members"].as_array().unwrap().len(), 1, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/alliances/{aid}/leave"),
        Some(ta),
        Some(json!({"group_id": ga})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", &format!("/v1/alliances/{aid}"), Some(ta), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, v) = call(
        &app,
        "GET",
        &format!("/v1/groups/{ga}/alliance"),
        Some(ta),
        None,
    )
    .await;
    assert!(v["alliance"].is_null(), "{v}");
}

#[tokio::test]
async fn groups_inventory_deposit_and_withdraw() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut users = vec![];
    for _ in 0..2 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("gi")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, a) = &users[0];
    let (tb, b) = &users[1];
    let ore = unique("ore");
    let relic = unique("relic");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": ore, "name": "Ore", "category": "material", "max_stack": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": relic, "name": "Relic", "category": "artifact", "stackable": false})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, ore_entry) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": b, "item_id": ore, "quantity": 50})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ore_entry}");
    let ore_entry = ore_entry["id"].as_str().unwrap().to_string();
    let (_, relic_entry) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(json!({"account_id": b, "item_id": relic})),
    )
    .await;
    let relic_entry = relic_entry["id"].as_str().unwrap().to_string();
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(ta),
        Some(json!({"name": unique("depot"), "open": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // a member pools from their own entry; more than they hold and non-stackables are refused
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/deposit"),
        Some(tb),
        Some(json!({"entry_id": ore_entry, "quantity": 30, "reason": "mining"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["entry"]["quantity_after"], 30);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/deposit"),
        Some(tb),
        Some(json!({"entry_id": ore_entry, "quantity": 100})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/deposit"),
        Some(tb),
        Some(json!({"entry_id": relic_entry, "quantity": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(tb), None).await;
    let left = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_id"] == json!(ore))
        .map(|x| x["quantity"].clone());
    assert_eq!(left, Some(json!(20)), "{inv}");
    let (st, t) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/inventory"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["items"][0]["item_id"], json!(ore));
    assert_eq!(t["items"][0]["quantity"], 30);
    // members may not hand out; the owner hands out to a member, never below the stock
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/withdraw"),
        Some(tb),
        Some(json!({"item_id": ore, "quantity": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/withdraw"),
        Some(ta),
        Some(json!({"item_id": ore, "quantity": 10, "to": a, "reason": "crafting"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["entry"]["quantity_after"], 20);
    assert_eq!(v["entry"]["delta"], -10);
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/withdraw"),
        Some(ta),
        Some(json!({"item_id": ore, "quantity": 100})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(ta), None).await;
    let got = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["item_id"] == json!(ore))
        .map(|x| x["quantity"].clone());
    assert_eq!(got, Some(json!(10)), "{inv}");
    let (st, l) = call(
        &app,
        "GET",
        &format!("/v1/groups/{gid}/inventory/ledger"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{l}");
    assert_eq!(l["entries"].as_array().unwrap().len(), 2);
    assert_eq!(l["entries"][0]["delta"], -10);
    assert_eq!(l["entries"][1]["delta"], 30);
    // hand-outs go to any member, not to outsiders
    let (st, v) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/withdraw"),
        Some(ta),
        Some(json!({"item_id": ore, "quantity": 5, "to": b})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["entry"]["quantity_after"], 15);
    let (_, s3) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("gi-out")})),
    )
    .await;
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/inventory/withdraw"),
        Some(ta),
        Some(json!({"item_id": ore, "quantity": 5, "to": s3["account_id"]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tournaments_prizes_paid_by_mail() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("tp")})),
        )
        .await;
        users.push(s["access_token"].as_str().unwrap().to_string());
    }
    let medal = unique("medal");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": medal, "name": "Medal", "category": "trophy", "max_stack": 99})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let table = unique("podium");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({"id": table, "name": "Podium", "kind": "weighted",
            "entries": [{"weight": 1, "item_id": medal, "amount": 1}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let id = unique("cup");
    // overlapping bands are refused
    let (st, v) = call_server(&app, "POST", "/v1/server/tournaments", k, Some(json!({
        "id": id, "title": "Cup", "duration": 3600,
        "prizes": [{"from_rank": 1, "to_rank": 2, "reward_table": table}, {"from_rank": 2, "to_rank": 3, "reward_table": table}]
    }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, t) = call_server(
        &app,
        "POST",
        "/v1/server/tournaments",
        k,
        Some(json!({
            "id": id, "title": "Cup", "duration": 3600,
            "prizes": [{"from_rank": 1, "to_rank": 1, "reward_table": table, "title": "Champion"},
                       {"from_rank": 2, "to_rank": 3, "reward_table": table}]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["prizes"].as_array().unwrap().len(), 2);
    let (_, g) = call(
        &app,
        "GET",
        &format!("/v1/tournaments/{id}"),
        Some(&users[0]),
        None,
    )
    .await;
    assert_eq!(g["prizes"][0]["title"], "Champion");
    for (i, tok) in users.iter().enumerate() {
        let (st, v) = call(
            &app,
            "POST",
            &format!("/v1/tournaments/{id}/records"),
            Some(tok),
            Some(json!({"score": 30 - 10 * i as i64})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, e) = call_server(
        &app,
        "POST",
        &format!("/v1/server/tournaments/{id}/end"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    assert_eq!(e["paid"], 3, "{e}");
    for (i, tok) in users.iter().enumerate() {
        let (st, m) = call(&app, "GET", "/v1/mail", Some(tok), None).await;
        assert_eq!(st, StatusCode::OK, "{m}");
        let mail = m["mail"].as_array().unwrap();
        assert_eq!(mail.len(), 1, "{m}");
        assert_eq!(mail[0]["rewards"], json!(table));
        assert_eq!(mail[0]["metadata"]["rank"], i as i64 + 1);
        if i == 0 {
            assert_eq!(mail[0]["title"], "Champion");
        }
    }
    // a second close of the same window pays nothing
    let (st, e) = call_server(
        &app,
        "POST",
        &format!("/v1/server/tournaments/{id}/end"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    assert_eq!(e["paid"], 0);
}

#[tokio::test]
async fn tournaments_team_records_and_treasury_prizes() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut users = vec![];
    for _ in 0..3 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("tt")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (ta, _) = &users[0];
    let (tb, _) = &users[1];
    let (tc, _) = &users[2];
    let gold = unique("gold");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let mut groups = vec![];
    for (tok, nm) in [(ta, "alpha"), (tc, "beta")] {
        let (st, g) = call(
            &app,
            "POST",
            "/v1/groups",
            Some(tok),
            Some(json!({"name": unique(nm), "open": true})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{g}");
        groups.push(g["id"].as_str().unwrap().to_string());
    }
    // b is a plain member of alpha: deposit-only role, no ANNOUNCE bit
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/groups/{}/join", groups[0]),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let id = unique("clan-cup");
    let (st, v) = call_server(&app, "POST", "/v1/server/tournaments", k, Some(json!({
        "id": id, "title": "Clan Cup", "duration": 3600, "operator": "incr", "team": {},
        "prizes": [{"from_rank": 1, "to_rank": 1, "reward_table": "x", "currency": gold, "amount": 500}]
    }))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "mixed prize kinds: {v}");
    let (st, t) = call_server(
        &app,
        "POST",
        "/v1/server/tournaments",
        k,
        Some(json!({
            "id": id, "title": "Clan Cup", "duration": 3600, "operator": "incr", "team": {},
            "prizes": [{"from_rank": 1, "to_rank": 1, "currency": gold, "amount": 500},
                       {"from_rank": 2, "to_rank": 2, "currency": gold, "amount": 100}]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["team"]["perm"], 8);
    // no group → 400; member without the role bit → 403; owner writes for the group
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(ta),
        Some(json!({"score": 10})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(tb),
        Some(json!({"score": 10, "group_id": groups[0]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    for score in [10, 15] {
        let (st, r) = call(
            &app,
            "POST",
            &format!("/v1/tournaments/{id}/records"),
            Some(ta),
            Some(json!({"score": score, "group_id": groups[0]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/tournaments/{id}/records"),
        Some(tc),
        Some(json!({"score": 20, "group_id": groups[1]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, recs) = call(
        &app,
        "GET",
        &format!("/v1/tournaments/{id}/records"),
        Some(tb),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{recs}");
    let list = recs["records"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{recs}");
    assert_eq!(
        list[0]["owner_id"],
        json!(groups[0]),
        "alpha summed 10+15 = 25 leads: {recs}"
    );
    assert_eq!(list[0]["score"], 25);
    let (st, e) = call_server(
        &app,
        "POST",
        &format!("/v1/server/tournaments/{id}/end"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    assert_eq!(e["paid"], 2);
    let (st, tr) = call(
        &app,
        "GET",
        &format!("/v1/groups/{}/treasury", groups[0]),
        Some(ta),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{tr}");
    assert_eq!(tr["balances"][0]["balance"], 500, "{tr}");
    let (_, tr) = call(
        &app,
        "GET",
        &format!("/v1/groups/{}/treasury", groups[1]),
        Some(tc),
        None,
    )
    .await;
    assert_eq!(tr["balances"][0]["balance"], 100, "{tr}");
}

#[tokio::test]
async fn admin_patch_account_profile() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut ids = vec![];
    for _ in 0..2 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("ap")})),
        )
        .await;
        ids.push(s["account_id"].as_str().unwrap().to_string());
    }
    let name = format!("adm_{}", &uuid::Uuid::now_v7().simple().to_string()[20..]);
    let (st, v) = call_server(
        &app,
        "PATCH",
        &format!("/v1/admin/accounts/{}", ids[0]),
        k,
        Some(json!({"username": name, "display_name": "Ren", "metadata": {"vip": true}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["username"], json!(name));
    let (st, a) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{}", ids[0]),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["account"]["display_name"], "Ren", "{a}");
    assert_eq!(a["account"]["metadata"]["vip"], true, "{a}");
    // a taken username is refused
    let (st, v) = call_server(
        &app,
        "PATCH",
        &format!("/v1/admin/accounts/{}", ids[1]),
        k,
        Some(json!({"username": name})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
}

#[tokio::test]
async fn admin_finds_account_by_identity() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let dev = unique("dev-lookup");
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": dev})),
    )
    .await;
    let acc = s["account_id"].as_str().unwrap().to_string();
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts?identity=device:{dev}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let list = v["accounts"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{v}");
    assert_eq!(list[0]["id"], json!(acc));
    let (st, v) = call_server(
        &app,
        "GET",
        "/v1/admin/accounts?identity=device:nobody-has-this",
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["accounts"].as_array().unwrap().len(), 0);
    let (st, _) = call_server(&app, "GET", "/v1/admin/accounts?identity=garbage", k, None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn webhooks_redeliver_resets_a_delivery() {
    let Some(app) = app().await else { return };
    let Ok(url) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
        return;
    };
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let k = "defaultkey";
    let (st, h) = call_server(
        &app,
        "POST",
        "/v1/admin/webhooks",
        k,
        Some(json!({
            "name": unique("rd"),
            "url": "http://127.0.0.1:9/nobody",
            "secret": "s3cret-s3cret-s3cret-16",
            "kinds": ["account.updated"]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    let hook = h["id"].as_str().unwrap().to_string();
    // disabled: neither fan-out nor the dispatcher touch it, so nothing races with other tests
    let (st, _) = call_server(
        &app,
        "PATCH",
        &format!("/v1/admin/webhooks/{hook}"),
        k,
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("rd-dev")})),
    )
    .await;
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let (st, _) = call_server(
        &app,
        "PATCH",
        &format!("/v1/admin/accounts/{acc}"),
        k,
        Some(json!({"display_name": "Redeliver"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // a dead-lettered delivery of this account's latest event, as twelve failed retries leave it
    let hid: uuid::Uuid = hook.parse().unwrap();
    let aid: uuid::Uuid = acc.parse().unwrap();
    let event: uuid::Uuid = sqlx::query_scalar(
        "insert into musubi.webhook_deliveries (webhook_id, event_id, attempts, dead, next_at, last_status, last_error)
         select $1, id, 12, true, now() + interval '1 day', 500, 'boom'
         from musubi.events where account_id = $2 order by id desc limit 1
         returning event_id",
    )
    .bind(hid)
    .bind(aid)
    .fetch_one(&db)
    .await
    .unwrap();
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/webhooks/{hook}/deliveries"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let row = |v: &serde_json::Value| {
        v["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["event_id"] == json!(event.to_string()))
            .cloned()
            .unwrap()
    };
    let d = row(&v);
    assert_eq!(d["dead"], true, "{d}");
    assert_eq!(d["attempts"], 12, "{d}");
    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/webhooks/{hook}/deliveries/{event}/redeliver"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["queued"], true);
    let (_, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/webhooks/{hook}/deliveries"),
        k,
        None,
    )
    .await;
    let d = row(&v);
    assert_eq!(d["attempts"], 0, "{d}");
    assert_eq!(d["dead"], false, "{d}");
    assert!(
        d["last_error"].is_null() && d["last_status"].is_null(),
        "{d}"
    );
    let (st, _) = call_server(
        &app,
        "POST",
        &format!(
            "/v1/admin/webhooks/{hook}/deliveries/{}/redeliver",
            uuid::Uuid::now_v7()
        ),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/admin/webhooks/{hook}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// Console sign-in (5.1): Xbox XSTS against a mock profile service, PSN / Nintendo id tokens
/// against the RSA test JWKS; identities are provider-scoped and found by the admin lookup.
#[tokio::test]
async fn console_logins_xbox_psn_nintendo() {
    use axum::http::HeaderMap;
    use axum::routing::get as ax_get;
    let jwks: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/google_test_jwks.json")).unwrap();
    let mock = Router::new()
        .route(
            "/jwks",
            ax_get(move || {
                let j = jwks.clone();
                async move { axum::Json(j) }
            }),
        )
        .route(
            "/users/me/profile/settings",
            ax_get(|headers: HeaderMap| async move {
                let auth = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let ver = headers
                    .get("x-xbl-contract-version")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if ver != "2" {
                    return (axum::http::StatusCode::BAD_REQUEST, axum::Json(json!({})));
                }
                match auth {
                    "XBL3.0 x=uhs1;xsts-good" => (
                        axum::http::StatusCode::OK,
                        axum::Json(json!({"profileUsers": [{"id": "2535400000000001",
                            "settings": [{"id": "Gamertag", "value": "MusubiFan"}]}]})),
                    ),
                    _ => (
                        axum::http::StatusCode::UNAUTHORIZED,
                        axum::Json(json!({"code": 401})),
                    ),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let b = base.clone();
    let Some(app) = app_with(move |c| {
        c.social.xbox.url = b.clone();
        c.social.psn.jwks_url = format!("{b}/jwks");
        c.social.psn.issuers = vec!["https://auth.api.sonyentertainmentnetwork.com".into()];
        c.social.psn.client_ids = vec!["psn-client".into()];
        c.social.nintendo.jwks_url = format!("{b}/jwks");
        c.social.nintendo.issuers = vec!["https://accounts.nintendo.com".into()];
    })
    .await
    else {
        return;
    };
    let k = "defaultkey";

    // Xbox: good token → session, identity xbox:<xuid>; forged → 401; malformed → 401
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/xbox",
        None,
        Some(json!({"user_hash": "uhs1", "token": "xsts-good"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    let acc = s1["account_id"].as_str().unwrap().to_string();
    let (st, v) = call_server(
        &app,
        "GET",
        "/v1/admin/accounts?identity=xbox:2535400000000001",
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["accounts"][0]["id"], json!(acc), "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/xbox",
        None,
        Some(json!({"user_hash": "uhs1", "token": "xsts-forged"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/xbox",
        None,
        Some(json!({"user_hash": "uhs;1", "token": "x"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // same token again → same account
    let (_, again) = call(
        &app,
        "POST",
        "/v1/auth/xbox",
        None,
        Some(json!({"user_hash": "uhs1", "token": "xsts-good"})),
    )
    .await;
    assert_eq!(again["account_id"], json!(acc));

    // PSN / Nintendo id tokens
    let key =
        jsonwebtoken::EncodingKey::from_rsa_pem(include_bytes!("fixtures/google_test_rsa.pem"))
            .unwrap();
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = Some("t1".into());
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + 600;
    let sign = |claims: serde_json::Value| jsonwebtoken::encode(&header, &claims, &key).unwrap();
    let psn_sub = unique("psn-");
    let tok = sign(
        json!({"iss": "https://auth.api.sonyentertainmentnetwork.com", "aud": "psn-client",
        "sub": psn_sub, "exp": exp, "online_id": "ps_player", "env": "np"}),
    );
    let (st, p) = call(
        &app,
        "POST",
        "/v1/auth/psn",
        None,
        Some(json!({"id_token": tok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts?identity=psn:{psn_sub}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["accounts"][0]["id"], p["account_id"], "{v}");
    // wrong audience → 401
    let bad = sign(
        json!({"iss": "https://auth.api.sonyentertainmentnetwork.com", "aud": "other",
        "sub": psn_sub, "exp": exp}),
    );
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/psn",
        None,
        Some(json!({"id_token": bad})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // Nintendo token linked to the Xbox account; linking it to a second account → conflict
    let nsa = unique("nsa-");
    let ntok =
        sign(json!({"iss": "https://accounts.nintendo.com", "aud": "any", "sub": nsa, "exp": exp}));
    let bearer = s1["access_token"].as_str().unwrap();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/link/nintendo",
        Some(bearer),
        Some(json!({"id_token": ntok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/link/nintendo",
        Some(p["access_token"].as_str().unwrap()),
        Some(json!({"id_token": ntok})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    // and signing in with it lands on the Xbox account
    let (st, n) = call(
        &app,
        "POST",
        "/v1/auth/nintendo",
        None,
        Some(json!({"id_token": ntok})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{n}");
    assert_eq!(n["account_id"], json!(acc));
    // link xbox returns the gamertag
    let (st, v) = call(
        &app,
        "POST",
        "/v1/account/link/xbox",
        Some(p["access_token"].as_str().unwrap()),
        Some(json!({"user_hash": "uhs1", "token": "xsts-good"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
}

#[tokio::test]
async fn friends_import_maps_platform_ids_to_accounts() {
    let Some(app) = app().await else { return };
    let (d1, d2, d3) = (unique("imp-a-"), unique("imp-b-"), unique("imp-c-"));
    let mut toks = vec![];
    for d in [&d1, &d2, &d3] {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": d})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        toks.push((
            v["access_token"].as_str().unwrap().to_string(),
            v["account_id"].as_str().unwrap().to_string(),
        ));
    }
    // a's friend list on the platform: b, c, itself and a stranger
    let (st, v) = call(
        &app,
        "POST",
        "/v1/friends/import",
        Some(&toks[0].0),
        Some(json!({"provider": "device", "subjects": [d2, d3, d1, "nobody-here"], "add": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let m = v["matches"].as_array().unwrap();
    assert_eq!(m.len(), 2, "{v}");
    assert_eq!(m[0]["subject"], json!(d2));
    assert_eq!(m[0]["account_id"], json!(toks[1].1));
    assert_eq!(m[1]["account_id"], json!(toks[2].1));
    assert_eq!(v["added"], 2);
    // requests went out: b sees a pending request from a
    let (st, f) = call(&app, "GET", "/v1/friends", Some(&toks[1].0), None).await;
    assert_eq!(st, StatusCode::OK, "{f}");
    assert!(f.to_string().contains(&toks[0].1), "{f}");
    // without add: just the mapping
    let (st, v) = call(
        &app,
        "POST",
        "/v1/friends/import",
        Some(&toks[1].0),
        Some(json!({"provider": "device", "subjects": [d3]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["matches"].as_array().unwrap().len(), 1);
    assert_eq!(v["added"], 0);
    // limits
    let many: Vec<String> = (0..501).map(|i| format!("x{i}")).collect();
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends/import",
        Some(&toks[1].0),
        Some(json!({"provider": "device", "subjects": many})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// Entitlements (5.11): Steam DLC ownership from a mock publisher API → reward table once,
/// refund → revoked, re-own → granted again; console entitlements via the server routes.
#[tokio::test]
async fn entitlements_sync_grants_once_and_revokes() {
    use axum::extract::Query as AxQuery;
    use axum::routing::get as ax_get;
    let owned: Arc<std::sync::Mutex<std::collections::HashSet<String>>> = Arc::new(
        std::sync::Mutex::new(["1001".to_string()].into_iter().collect()),
    );
    let o2 = owned.clone();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        % 1_000_000_000;
    let s7 = format!("76561198{nonce:09}");
    let (s7a, s7b) = (s7.clone(), s7.clone());
    let mock = Router::new()
        .route(
            "/ISteamUserAuth/AuthenticateUserTicket/v1/",
            ax_get(move |AxQuery(q): AxQuery<std::collections::HashMap<String, String>>| { let s7 = s7a.clone(); async move {
                match q.get("ticket").map(String::as_str) {
                    Some("0e0e") => axum::Json(json!({"response": {"params": {"result": "OK", "steamid": s7, "ownersteamid": s7, "vacbanned": false, "publisherbanned": false}}})),
                    _ => axum::Json(json!({"response": {"error": {"errorcode": 101, "errordesc": "Invalid ticket"}}})),
                }
            } }),
        )
        .route(
            "/ISteamUser/CheckAppOwnership/v4/",
            ax_get(move |AxQuery(q): AxQuery<std::collections::HashMap<String, String>>| {
                let o = o2.clone();
                async move {
                    assert_eq!(q.get("key").map(String::as_str), Some("pubkey"));
                    assert_eq!(q.get("steamid").map(String::as_str), Some(s7b.as_str()));
                    let owns = o.lock().unwrap().contains(q.get("appid").unwrap());
                    axum::Json(json!({"appownership": {"ownsapp": owns, "permanent": owns, "result": "OK"}}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let b = base.clone();
    let Some(app) = app_with(move |c| {
        c.social.steam.url = b.clone();
        c.social.steam.web_api_key = "k".into();
        c.social.steam.app_id = 480;
        c.iap.steam.url = b.clone();
        c.iap.steam.key = "pubkey".into();
    })
    .await
    else {
        return;
    };
    let k = "defaultkey";
    let (dlc1, dlc2) = (unique("dlc1-"), unique("dlc2-"));
    for (it, t) in [(&dlc1, "t1"), (&dlc2, "t2")] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/items",
            k,
            Some(json!({"id": it, "name": it, "category": "dlc", "stackable": true})),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        let (st, v) = call_server(
            &app,
            "POST",
            "/v1/server/rewards/tables",
            k,
            Some(json!({
                "id": format!("{it}-{t}"), "name": t, "kind": "weighted",
                "entries": [{"weight": 1, "rarity": "common", "item_id": it, "amount": 1}]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/entitlements",
        k,
        Some(json!({"provider": "steam", "external_id": "1001", "reward_table": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    for (ext, it, t) in [("1001", &dlc1, "t1"), ("1002", &dlc2, "t2")] {
        let (st, v) = call_server(&app, "PUT", "/v1/server/entitlements", k, Some(json!({"provider": "steam", "external_id": ext, "reward_table": format!("{it}-{t}"), "label": "DLC"}))).await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, v) = call_server(&app, "PUT", "/v1/server/entitlements", k, Some(json!({"provider": "xbox", "external_id": "9NBLGGH-SEASON", "reward_table": format!("{dlc2}-t2")}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, v) = call_server(&app, "GET", "/v1/server/entitlements", k, None).await;
    assert!(v["entitlements"].as_array().unwrap().len() >= 3, "{v}");

    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/steam",
        None,
        Some(json!({"ticket": "0e0e"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let (st, v) = call(
        &app,
        "POST",
        "/v1/entitlements/sync",
        Some(&tok),
        Some(json!({"provider": "steam"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["owned"], json!(["1001"]));
    assert_eq!(v["granted"], json!(["1001"]));
    assert_eq!(v["revoked"], json!([]));
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    assert!(inv.to_string().contains(&dlc1), "{inv}");
    assert!(!inv.to_string().contains(&dlc2), "{inv}");
    let (_, v) = call(
        &app,
        "POST",
        "/v1/entitlements/sync",
        Some(&tok),
        Some(json!({"provider": "steam"})),
    )
    .await;
    assert_eq!(v["granted"], json!([]), "{v}");
    let (_, inv2) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    assert_eq!(inv2, inv);
    {
        let mut o = owned.lock().unwrap();
        o.remove("1001");
        o.insert("1002".into());
    }
    let (_, v) = call(
        &app,
        "POST",
        "/v1/entitlements/sync",
        Some(&tok),
        Some(json!({"provider": "steam"})),
    )
    .await;
    assert_eq!(v["granted"], json!(["1002"]), "{v}");
    assert_eq!(v["revoked"], json!(["1001"]), "{v}");
    let (_, mine) = call(&app, "GET", "/v1/entitlements", Some(&tok), None).await;
    let m = mine["entitlements"].as_array().unwrap();
    assert_eq!(m.len(), 2, "{mine}");
    assert_eq!(
        m.iter().find(|e| e["external_id"] == "1001").unwrap()["active"],
        false
    );
    assert_eq!(
        m.iter().find(|e| e["external_id"] == "1002").unwrap()["active"],
        true
    );
    owned.lock().unwrap().insert("1001".into());
    let (_, v) = call(
        &app,
        "POST",
        "/v1/entitlements/sync",
        Some(&tok),
        Some(json!({"provider": "steam"})),
    )
    .await;
    assert_eq!(v["granted"], json!(["1001"]), "{v}");
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/entitlements/grant",
        k,
        Some(json!({"account_id": acc, "provider": "xbox", "external_id": "9NBLGGH-SEASON"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["granted"], true);
    let (_, v) = call_server(
        &app,
        "POST",
        "/v1/server/entitlements/grant",
        k,
        Some(json!({"account_id": acc, "provider": "xbox", "external_id": "9NBLGGH-SEASON"})),
    )
    .await;
    assert_eq!(v["granted"], false);
    let (_, v) = call_server(&app, "POST", "/v1/server/entitlements/revoke", k, Some(json!({"account_id": acc, "provider": "xbox", "external_id": "9NBLGGH-SEASON", "reason": "refund"}))).await;
    assert_eq!(v["revoked"], true);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/entitlements/grant",
        k,
        Some(json!({"account_id": acc, "provider": "psn", "external_id": "UNKNOWN"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/entitlements/sync",
        Some(&tok),
        Some(json!({"provider": "xbox"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(
        &app,
        "DELETE",
        "/v1/server/entitlements/xbox/9NBLGGH-SEASON",
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn player_reports_queue_and_ban_from_decision() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut acc = vec![];
    for p in ["rep-a-", "rep-b-", "rep-c-"] {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique(p)})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        acc.push((
            v["access_token"].as_str().unwrap().to_string(),
            v["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (a, b, c) = (&acc[0], &acc[1], &acc[2]);
    // self / bad category / ok
    let (st, _) = call(
        &app,
        "POST",
        "/v1/reports",
        Some(&a.0),
        Some(json!({"target_id": a.1, "category": "cheating"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/reports",
        Some(&a.0),
        Some(json!({"target_id": b.1, "category": "being_rude"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, r1) = call(
        &app,
        "POST",
        "/v1/reports",
        Some(&a.0),
        Some(json!({
            "target_id": b.1, "category": "cheating", "details": "aimbot in match",
            "evidence": {"match_id": "m-123", "tick": 4410, "url": "https://clips.example/1"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r1}");
    let r1 = r1["id"].as_str().unwrap().to_string();
    // duplicate open report → 409; second reporter fine
    let (st, _) = call(
        &app,
        "POST",
        "/v1/reports",
        Some(&a.0),
        Some(json!({"target_id": b.1, "category": "griefing"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let (st, r2) = call(
        &app,
        "POST",
        "/v1/reports",
        Some(&c.0),
        Some(json!({"target_id": b.1, "category": "harassment"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r2}");
    let r2 = r2["id"].as_str().unwrap().to_string();
    // queue + summary
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/reports?status=open&target_id={}", b.1),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["reports"].as_array().unwrap().len(), 2, "{v}");
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{}/reports", b.1),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["distinct_reporters"], 2, "{v}");
    let (st, v) = call_server(&app, "GET", &format!("/v1/admin/reports/{r1}"), k, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["evidence"]["match_id"], "m-123");
    // ban requires actioned
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/reports/{r1}"),
        k,
        Some(json!({"status": "reviewing", "ban": {"until_secs": 60}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/reports/{r1}"),
        k,
        Some(json!({"status": "reviewing", "by": "mod1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "reviewing");
    // actioned with a timed ban: target locked out, the other open report closed too
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/reports/{r1}"),
        k,
        Some(json!({
            "status": "actioned", "decision": "confirmed aimbot", "by": "mod1",
            "ban": {"until_secs": 3600, "reason": "report", "message": "Cheating"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "actioned");
    assert_eq!(v["ban"]["banned"], true, "{v}");
    assert_eq!(v["also_closed"], 1, "{v}");
    let (st, _) = call(&app, "GET", "/v1/account", Some(&b.0), None).await;
    assert_ne!(st, StatusCode::OK);
    let (_, v) = call_server(&app, "GET", &format!("/v1/admin/reports/{r2}"), k, None).await;
    assert_eq!(v["status"], "actioned", "{v}");
    assert!(v["decision"].as_str().unwrap().contains(&r1), "{v}");
    // reporter sees outcome, not the moderator
    let (_, mine) = call(&app, "GET", "/v1/reports", Some(&a.0), None).await;
    let m = mine["reports"].as_array().unwrap();
    assert_eq!(m.len(), 1, "{mine}");
    assert_eq!(m[0]["status"], "actioned");
    assert!(m[0]["decided_by"].is_null());
    // dismiss path
    let (_, r3) = call(
        &app,
        "POST",
        "/v1/reports",
        Some(&c.0),
        Some(json!({"target_id": a.1, "category": "spam"})),
    )
    .await;
    let r3 = r3["id"].as_str().unwrap().to_string();
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/reports/{r3}"),
        k,
        Some(json!({"status": "dismissed", "decision": "no evidence"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "dismissed");
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/reports/{}", uuid::Uuid::now_v7()),
        k,
        Some(json!({"status": "dismissed"})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn limited_grants_cap_server_grants_per_window() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (_, v) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("lg-")})),
    )
    .await;
    let acc = v["account_id"].as_str().unwrap().to_string();
    let (cur, item) = (unique("lgc"), unique("lgi-"));
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": cur, "name": "Gold", "max_balance": 1_000_000})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": item, "name": "Key", "category": "misc", "stackable": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // bad kinds / values
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/grant_limits",
        k,
        Some(json!({"kind": "xp", "key": cur, "window_secs": 60, "max_per_account": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/grant_limits",
        k,
        Some(json!({"kind": "currency", "key": cur, "window_secs": 60, "max_per_account": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/grant_limits",
        k,
        Some(json!({"kind": "item", "key": item, "window_secs": 60, "max_per_account": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // 60 ok, +50 → 429, +40 ok (exactly 100), negative deltas never limited
    let adj = |d: i64| json!({"account_id": acc, "currency": cur, "delta": d, "reason": "test"});
    let (st, v) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(adj(60))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(adj(50))).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{v}");
    assert_eq!(v["code"], "resource_exhausted");
    let (st, _) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(adj(40))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(adj(-30))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(adj(1))).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    // items: one per window
    let gi = json!({"account_id": acc, "item_id": item, "quantity": 1});
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/inventory/grant",
        k,
        Some(gi.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/inventory/grant", k, Some(gi)).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS);
    // usage visible, limit removable → grants flow again
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/accounts/{acc}/grants"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["grants"].as_array().unwrap().len(), 3, "{v}");
    let (_, v) = call_server(&app, "GET", "/v1/server/grant_limits", k, None).await;
    assert!(v["limits"].as_array().unwrap().len() >= 2, "{v}");
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/grant_limits/currency/{cur}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(adj(1000))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/grant_limits/currency/{cur}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn match_results_report_fans_out_once() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut acc = vec![];
    for i in 0..4 {
        let (_, v) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique(&format!("mr{i}-"))})),
        )
        .await;
        acc.push((
            v["access_token"].as_str().unwrap().to_string(),
            v["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (q, cur, win, played, track) = (
        unique("mrq-"),
        unique("mrc"),
        unique("mrw-"),
        unique("mrp-"),
        unique("mrt-"),
    );
    let (st, v) = call_server(
        &app,
        "PUT",
        "/v1/server/rating/queues",
        k,
        Some(json!({"id": q})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": cur, "name": "Gold", "max_balance": 1_000_000})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for (t, amount) in [(&win, 100), (&played, 10)] {
        let (st, v) = call_server(
            &app,
            "POST",
            "/v1/server/rewards/tables",
            k,
            Some(json!({
                "id": t, "name": t, "kind": "weighted",
                "entries": [{"weight": 1, "rarity": "common", "currency": cur, "amount": amount}]
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, v) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(json!({"id": track, "name": "Account", "levels": [{"xp": 50}, {"xp": 200}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let mid = unique("match-");
    let body = json!({
        "queue": q,
        "teams": [
            {"rank": 1, "players": [{"account_id": acc[0].1, "stats": {"kills": 7}}, {"account_id": acc[1].1}]},
            {"rank": 2, "players": [{"account_id": acc[2].1}, {"account_id": acc[3].1}]}
        ],
        "rewards": {"1": win, "default": played},
        "quest_event": {"name": "matches_played"},
        "progression": {"track_id": track, "xp_by_rank": {"1": 60, "default": 20}},
        "metadata": {"map": "dust"}
    });
    let (st, v) = call_server(
        &app,
        "POST",
        &format!("/v1/server/matches/{mid}/results"),
        k,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["replayed"], false);
    assert_eq!(v["players"].as_array().unwrap().len(), 4, "{v}");
    assert!(
        v["rating"]["changes"]
            .as_array()
            .map(|c| c.len() == 4)
            .unwrap_or(false),
        "{v}"
    );
    assert_eq!(v["players"][0]["rewards"]["table"], json!(win), "{v}");
    assert_eq!(v["players"][2]["rewards"]["table"], json!(played), "{v}");
    assert!(v["players"][0]["rewards_error"].is_null(), "{v}");
    assert!(v["players"][0]["xp_error"].is_null(), "{v}");
    // winners got 100, losers 10; winner reached level 2 with 60 xp
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&acc[0].0), None).await;
    assert!(w.to_string().contains("100"), "{w}");
    let (_, l) = call(&app, "GET", "/v1/wallet", Some(&acc[2].0), None).await;
    assert!(l.to_string().contains("10"), "{l}");
    assert!(!v["players"][0]["xp"].is_null(), "{v}");
    assert!(
        !v["players"][0]["quests"].is_null() || !v["players"][0]["quests_error"].is_null(),
        "{v}"
    );
    // repeat → stored outcome, no second payout
    let (st, v2) = call_server(
        &app,
        "POST",
        &format!("/v1/server/matches/{mid}/results"),
        k,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v2}");
    assert_eq!(v2["replayed"], true);
    let (_, w2) = call(&app, "GET", "/v1/wallet", Some(&acc[0].0), None).await;
    assert_eq!(w2, w);
    let (st, g) = call_server(
        &app,
        "GET",
        &format!("/v1/server/matches/{mid}/results"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(g["payload"]["metadata"]["map"], "dust", "{g}");
    assert_eq!(g["result"]["players"].as_array().unwrap().len(), 4);
    let (st, _) = call_server(&app, "GET", "/v1/server/matches/nope/results", k, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/server/matches/{}/results", unique("m-")),
        k,
        Some(json!({"teams": []})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// Content bundles: dry-run shows the diff without touching anything, import applies it and
/// records a version, an unchanged re-import is a no-op, rollback restores the earlier state,
/// bad bundles are refused and a foreign-key violation rolls the whole import back.
#[tokio::test]
async fn content_bundle_export_import_diff_rollback() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let cur = format!("cb{sfx}");
    let item = format!("sword-{sfx}");
    let find = |b: &Value, table: &str, key: &str, id: &str| -> Value {
        b["tables"][table]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r[key] == id)
            .cloned()
            .unwrap_or(Value::Null)
    };
    let bundle1 = json!({"format": 1, "tables": {
        "currencies": [{"code": cur, "name": "Bundle Gold", "max_balance": 1000}],
        "items": [{"id": item, "name": "Sword", "category": "weapon"}],
    }});
    // dry run: diff only
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": bundle1, "dry_run": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["applied"], false);
    assert_eq!(r["diff"]["currencies"]["added"], 1, "{r}");
    assert_eq!(r["diff"]["items"]["added"], 1, "{r}");
    let (st, b) = call_server(&app, "GET", "/v1/admin/content", k, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(b["format"], 1);
    assert!(
        find(&b, "currencies", "code", &cur).is_null(),
        "dry run must not write"
    );
    // apply
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": bundle1, "note": "v1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["applied"], true);
    let v1 = r["version"].as_i64().expect("version id");
    let (_, b) = call_server(&app, "GET", "/v1/admin/content", k, None).await;
    let row = find(&b, "currencies", "code", &cur);
    assert_eq!(row["name"], "Bundle Gold", "{b}");
    assert_eq!(row["max_balance"], 1000);
    assert_eq!(find(&b, "items", "id", &item)["category"], "weapon");
    // v2 changes one field; the dry run reports a change, not an addition
    let bundle2 = json!({"format": 1, "tables": {
        "currencies": [{"code": cur, "name": "Bundle Gold II"}],
    }});
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": bundle2, "dry_run": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["diff"]["currencies"]["changed"], 1, "{r}");
    assert_eq!(r["diff"]["currencies"]["added"], 0, "{r}");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": bundle2, "note": "v2"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["applied"], true);
    let v2 = r["version"].as_i64().expect("version id");
    assert!(v2 > v1);
    let (_, b) = call_server(&app, "GET", "/v1/admin/content", k, None).await;
    let row = find(&b, "currencies", "code", &cur);
    assert_eq!(row["name"], "Bundle Gold II");
    assert_eq!(row["max_balance"], 1000, "omitted columns keep their value");
    // same bundle again: nothing to do, no version
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": bundle2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["applied"], false);
    assert!(r["version"].is_null());
    // history
    let (st, l) = call_server(&app, "GET", "/v1/admin/content/versions?limit=100", k, None).await;
    assert_eq!(st, StatusCode::OK);
    let ids: Vec<i64> = l["versions"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v["id"].as_i64())
        .collect();
    assert!(ids.contains(&v1) && ids.contains(&v2), "{l}");
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/admin/content/versions/{v1}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["note"], "v1");
    assert_eq!(
        find(&v["bundle"], "currencies", "code", &cur)["name"],
        "Bundle Gold"
    );
    // rollback to v1 restores the name
    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/content/versions/{v1}/rollback"),
        k,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["applied"], true);
    let (_, b) = call_server(&app, "GET", "/v1/admin/content", k, None).await;
    assert_eq!(find(&b, "currencies", "code", &cur)["name"], "Bundle Gold");
    // refusals
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": {"format": 2, "tables": {}}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": {"format": 1, "tables": {"wallets": []}}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": {"format": 1, "tables": {"items": [{"name": "no key"}]}}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // a store item pointing at an unknown item violates a foreign key: 409, nothing applied
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        k,
        Some(json!({"bundle": {"format": 1, "tables": {
            "currencies": [{"code": format!("{cur}-x"), "name": "Never"}],
            "store_items": [{"id": format!("si-{sfx}"), "item_id": format!("missing-{sfx}")}],
        }}})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{r}");
    let (_, b) = call_server(&app, "GET", "/v1/admin/content", k, None).await;
    assert!(
        find(&b, "currencies", "code", &format!("{cur}-x")).is_null(),
        "rolled back"
    );
    let (st, _) = call_server(&app, "GET", "/v1/admin/content/versions/999999999", k, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// Operator roles: a support key moderates players but cannot touch content; a liveops key
/// imports content but cannot ban; neither issues keys; a server key stays out of `/v1/admin/`.
#[tokio::test]
async fn operator_roles_scope_admin_writes() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut keys = std::collections::HashMap::new();
    for role in ["support", "liveops", "ops", "server"] {
        let (st, r) = call_server(
            &app,
            "POST",
            "/v1/admin/keys",
            k,
            Some(json!({"name": format!("{role}-key"), "role": role})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        keys.insert(role, r["secret"].as_str().unwrap().to_string());
    }
    let (st, v) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": format!("role-target-{}", uuid::Uuid::now_v7().simple())})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, me) = call(
        &app,
        "GET",
        "/v1/account/me",
        v["access_token"].as_str(),
        None,
    )
    .await;
    let target = me["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{me}"))
        .to_string();
    // every operator role reads
    for role in ["support", "liveops", "ops"] {
        let (st, _) =
            call_server(&app, "GET", "/v1/admin/accounts?q=role", &keys[role], None).await;
        assert_eq!(st, StatusCode::OK, "{role} must read");
        let (st, _) = call_server(&app, "GET", "/v1/admin/content", &keys[role], None).await;
        assert_eq!(st, StatusCode::OK, "{role} must read");
    }
    // support: ban yes, content no, keys no
    let (st, r) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{target}/ban"),
        &keys["support"],
        Some(json!({"until_secs": 60, "reason": "test"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        &keys["support"],
        Some(json!({"bundle": {"format": 1, "tables": {}}, "dry_run": true})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{r}");
    assert!(
        r["message"].as_str().unwrap_or("").contains("support"),
        "{r}"
    );
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/admin/keys",
        &keys["support"],
        Some(json!({"name": "escalate", "role": "admin"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // liveops: content yes, ban no
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/admin/content/import",
        &keys["liveops"],
        Some(json!({"bundle": {"format": 1, "tables": {}}, "dry_run": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{target}/ban"),
        &keys["liveops"],
        Some(json!({"until_secs": 60})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // ops: worlds yes, ban no
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/worlds",
        &keys["ops"],
        Some(json!({"name": format!("ops-{}", uuid::Uuid::now_v7().simple())})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, _) = call_server(
        &app,
        "POST",
        &format!("/v1/admin/accounts/{target}/ban"),
        &keys["ops"],
        Some(json!({"until_secs": 60})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // server keys never enter the admin surface
    let (st, _) = call_server(&app, "GET", "/v1/admin/accounts", &keys["server"], None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, r) = call_server(&app, "GET", "/v1/admin/keys", k, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(r.to_string().contains("liveops"), "{r}");
}

/// Leaderboard queries (5.6): friends-only and group-only pages keep full-board ranks, a rank
/// range replaces cursor paging, and `windows` lists the board's slices.
#[tokio::test]
async fn leaderboard_friends_group_rank_range_and_windows() {
    let Some(app) = app().await else { return };
    let mut users = vec![];
    for _ in 0..4 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("lbq")})),
        )
        .await;
        users.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let lb = unique("alltime");
    let (st, b) = call_server(
        &app,
        "POST",
        "/v1/server/leaderboards",
        "defaultkey",
        Some(json!({"id": lb, "sort_order": "desc", "operator": "best"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    // ranks: u0=400 (1), u1=300 (2), u2=200 (3), u3=100 (4)
    for (i, (t, _)) in users.iter().enumerate() {
        let (st, r) = call(
            &app,
            "POST",
            &format!("/v1/leaderboards/{lb}/records"),
            Some(t),
            Some(json!({"score": 400 - 100 * i as i64})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    // rank range 2..3
    let (st, r) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/records?rank_from=2&rank_to=3"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let recs = r["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0]["rank"], 2);
    assert_eq!(recs[0]["owner_id"], json!(users[1].1));
    assert_eq!(recs[1]["rank"], 3);
    // u3 befriends u1 (mutual add) → friends page = u3 + u1 with board ranks 2 and 4
    for (t, other) in [(&users[3].0, &users[1].1), (&users[1].0, &users[3].1)] {
        let (st, r) = call(
            &app,
            "POST",
            "/v1/friends",
            Some(t),
            Some(json!({"ids": [other]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let (st, r) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/records?friends=true"),
        Some(&users[3].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let recs = r["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2, "{r}");
    assert_eq!(recs[0]["owner_id"], json!(users[1].1));
    assert_eq!(recs[0]["rank"], 2);
    assert_eq!(recs[1]["owner_id"], json!(users[3].1));
    assert_eq!(recs[1]["rank"], 4);
    // group of u0 + u2 → group page ranks 1 and 3
    let (st, g) = call(
        &app,
        "POST",
        "/v1/groups",
        Some(&users[0].0),
        Some(json!({"name": unique("lbclan"), "open": true})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    let gid = g["id"].as_str().unwrap().to_string();
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/groups/{gid}/join"),
        Some(&users[2].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/records?group_id={gid}"),
        Some(&users[3].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let recs = r["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2, "{r}");
    assert_eq!(recs[0]["rank"], 1);
    assert_eq!(recs[1]["rank"], 3);
    assert_eq!(recs[1]["owner_id"], json!(users[2].1));
    // windows: one (current, never-expiring) slice with 4 records
    let (st, w) = call(
        &app,
        "GET",
        &format!("/v1/leaderboards/{lb}/windows"),
        Some(&users[0].0),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");
    let ws = w["windows"].as_array().unwrap();
    assert_eq!(ws.len(), 1, "{w}");
    assert_eq!(ws[0]["records"], 4);
    assert_eq!(ws[0]["current"], true);
}

/// Per-platform storefronts: a listing limited to some platforms is hidden from the others and
/// from nobody when no platform is given.
#[tokio::test]
async fn store_listings_filter_by_platform() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("plat")})),
    )
    .await;
    let tok = s["access_token"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold", "max_balance": 1000})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let skin = unique("skin");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/items",
        k,
        Some(json!({"id": skin, "name": "Skin", "category": "cosmetic", "stackable": false})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (every, ios_only) = (unique("every"), unique("ios"));
    for (id, platforms) in [
        (&every, json!([])),
        (&ios_only, json!(["iOS", " android "])),
    ] {
        let (st, r) = call_server(
            &app,
            "POST",
            "/v1/server/store",
            k,
            Some(json!({"id": id, "item_id": skin, "quantity": 1, "prices": {gold.clone(): 10}, "platforms": platforms})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let ids = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let (_, all) = call(&app, "GET", "/v1/store", Some(&tok), None).await;
    assert!(
        ids(&all).contains(&every) && ids(&all).contains(&ios_only),
        "{all}"
    );
    let (_, steam) = call(&app, "GET", "/v1/store?platform=steam", Some(&tok), None).await;
    assert!(
        ids(&steam).contains(&every) && !ids(&steam).contains(&ios_only),
        "{steam}"
    );
    let (_, ios) = call(&app, "GET", "/v1/store?platform=IOS", Some(&tok), None).await;
    assert!(ids(&ios).contains(&ios_only), "{ios}");
    let (_, android) = call(&app, "GET", "/v1/store?platform=android", Some(&tok), None).await;
    assert!(ids(&android).contains(&ios_only), "{android}");
    let item = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == ios_only)
        .unwrap();
    assert_eq!(item["platforms"], json!(["ios", "android"]));
}

/// Quest rerolls: a rerollable quest can be hidden for the period, limited per period and kind;
/// completed and non-rerollable quests cannot be rerolled.
#[tokio::test]
async fn quest_rerolls_are_limited_per_period() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("reroll")})),
    )
    .await;
    let tok = s["access_token"].as_str().unwrap().to_string();
    let (a, b, fixed) = (unique("qa"), unique("qb"), unique("qf"));
    // own event name: quests are global and other tests fire match_won
    let evt = unique("evt");
    for (id, rerollable) in [(&a, true), (&b, true), (&fixed, false)] {
        let (st, r) = call_server(
            &app,
            "POST",
            "/v1/server/quests",
            k,
            Some(json!({"id": id, "name": id, "kind": "daily", "rerollable": rerollable, "rerolls_per_period": 1,
                "objectives": [{"event": evt, "target": 1}]})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let (_, q) = call(&app, "GET", "/v1/quests", Some(&tok), None).await;
    let view = |v: &Value, id: &str| -> Option<Value> {
        v["quests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["quest_id"] == id)
            .cloned()
    };
    assert_eq!(view(&q, &a).unwrap()["rerolls_left"], 1, "{q}");
    assert_eq!(view(&q, &fixed).unwrap()["rerollable"], false);
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/quests/{fixed}/reroll"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/quests/{a}/reroll"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["rerolls_left"], 0);
    let (_, q) = call(&app, "GET", "/v1/quests", Some(&tok), None).await;
    assert!(view(&q, &a).is_none(), "{q}");
    assert_eq!(view(&q, &b).unwrap()["rerolls_left"], 0);
    // the kind's budget is spent
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/quests/{b}/reroll"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{r}");
    // rerolling the same quest again is a no-op, not a second reroll
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/quests/{a}/reroll"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // a completed quest cannot be rerolled
    let (_, me) = call(&app, "GET", "/v1/account", Some(&tok), None).await;
    let acc = me["account"]["id"]
        .as_str()
        .unwrap_or(me["id"].as_str().unwrap_or(""))
        .to_string();
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/quests/event",
        k,
        Some(json!({"account_id": acc, "event": evt, "count": 1})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/quests/{b}/reroll"),
        Some(&tok),
        None,
    )
    .await;
    assert!(
        st == StatusCode::CONFLICT || st == StatusCode::TOO_MANY_REQUESTS,
        "{st} {r}"
    );
    // leave nothing enabled behind for the other quest tests
    for id in [&a, &b, &fixed] {
        let (st, _) = call_server(
            &app,
            "POST",
            "/v1/server/quests",
            k,
            Some(
                json!({"id": id, "name": id, "kind": "daily", "enabled": false,
                "objectives": [{"event": evt, "target": 1}]}),
            ),
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
}

/// Reward modifiers: a global currency x2 doubles table grants and level rewards, a per-account
/// xp x2 only affects that player, an expired modifier does nothing, items are never multiplied.
#[tokio::test]
async fn reward_modifiers_scale_xp_and_currency() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let mut players = Vec::new();
    for _ in 0..2 {
        let (_, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": unique("mod")})),
        )
        .await;
        players.push((
            s["access_token"].as_str().unwrap().to_string(),
            s["account_id"].as_str().unwrap().to_string(),
        ));
    }
    let (tok_a, acc_a) = players[0].clone();
    let (tok_b, acc_b) = players[1].clone();
    let gems = unique("gems");
    let bonus = unique("bonus");
    let track = unique("xp");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gems})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/rewards/tables", k, Some(json!({"id": bonus, "kind": "guaranteed", "entries": [{"currency": gems, "amount": 50}]}))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(json!({"id": track, "levels": [{"xp": 100, "rewards": bonus}, {"xp": 250}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // invalid modifiers
    for body in [
        json!({"id": unique("m"), "scope": "gold", "factor": 2.0}),
        json!({"id": unique("m"), "scope": "xp", "factor": 0.0}),
        json!({"id": unique("m"), "scope": "xp", "factor": 2.0, "starts_at": "2030-01-01T00:00:00Z", "ends_at": "2029-01-01T00:00:00Z"}),
    ] {
        let (st, r) = call_server(&app, "PUT", "/v1/server/rewards/modifiers", k, Some(body)).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    }
    let (m_cur, m_xp, m_old) = (unique("m-cur"), unique("m-xp"), unique("m-old"));
    let (st, r) = call_server(&app, "PUT", "/v1/server/rewards/modifiers", k, Some(json!({"id": m_cur, "scope": format!("currency:{gems}"), "factor": 2.0, "note": "weekend"}))).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/rewards/modifiers",
        k,
        Some(json!({"id": m_xp, "scope": "xp", "factor": 2.0, "account_id": acc_a})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "PUT", "/v1/server/rewards/modifiers", k, Some(json!({"id": m_old, "scope": format!("table:{bonus}"), "factor": 10.0, "starts_at": "2020-01-01T00:00:00Z", "ends_at": "2020-01-02T00:00:00Z"}))).await;
    assert_eq!(st, StatusCode::OK);
    // what each player sees as active
    let (_, a) = call(&app, "GET", "/v1/rewards/modifiers", Some(&tok_a), None).await;
    let ids = |v: &Value| -> Vec<String> {
        v["modifiers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    let a_ids = ids(&a);
    assert!(
        a_ids.contains(&m_cur) && a_ids.contains(&m_xp) && !a_ids.contains(&m_old),
        "{a}"
    );
    let (_, b) = call(&app, "GET", "/v1/rewards/modifiers", Some(&tok_b), None).await;
    let b_ids = ids(&b);
    assert!(b_ids.contains(&m_cur) && !b_ids.contains(&m_xp), "{b}");
    // table grant: 50 gems x2 (expired table x10 ignored)
    let (st, g) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/grant",
        k,
        Some(json!({"account_id": acc_b, "table_id": bonus, "reference": "r1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    assert_eq!(g["rewards"][0]["amount"], 100);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok_b), None).await;
    assert_eq!(w["balances"][0]["balance"], 100, "{w}");
    // xp: A has x2 → 60 becomes 120 → level 1 with doubled level reward; B stays at 60
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc_a, "track_id": track, "delta": 60})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["xp_factor"], 2.0);
    assert_eq!(r["delta_applied"], 120);
    assert_eq!(r["level_ups"], json!([1]));
    assert_eq!(r["rewards"][0]["amount"], 100);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok_a), None).await;
    assert_eq!(w["balances"][0]["balance"], 100, "{w}");
    let (_, r) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc_b, "track_id": track, "delta": 60})),
    )
    .await;
    assert_eq!(r["xp_factor"], 1.0);
    assert_eq!(r["delta_applied"], 60);
    assert_eq!(r["progress"]["level"], 0);
    // list / delete
    let (_, all) = call_server(&app, "GET", "/v1/server/rewards/modifiers", k, None).await;
    assert!(ids(&all).contains(&m_old));
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/rewards/modifiers/{m_cur}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "DELETE",
        &format!("/v1/server/rewards/modifiers/{m_cur}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    for id in [&m_xp, &m_old] {
        call_server(
            &app,
            "DELETE",
            &format!("/v1/server/rewards/modifiers/{id}"),
            k,
            None,
        )
        .await;
    }
    // without the modifier the grant is back to 50
    let (_, g) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/grant",
        k,
        Some(json!({"account_id": acc_b, "table_id": bonus, "reference": "r2"})),
    )
    .await;
    assert_eq!(g["rewards"][0]["amount"], 50);
}

/// Progress graphs: prerequisites and level gates, paid instant unlocks with rewards, ranks,
/// a timed node that can be skipped for currency or claimed when the timer ran out, reset
/// with a partial refund.
#[tokio::test]
async fn progress_graphs_skill_tree_and_timed_unlockables() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (_, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("gr")})),
    )
    .await;
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let bonus = unique("bonus");
    let track = unique("xp");
    let g = unique("tree");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/rewards/tables", k, Some(json!({"id": bonus, "kind": "guaranteed", "entries": [{"currency": gold, "amount": 5}]}))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/tracks",
        k,
        Some(json!({"id": track, "levels": [{"xp": 100}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/wallet/adjust",
        k,
        Some(json!({"account_id": acc, "currency": gold, "delta": 1000, "reason": "seed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // invalid graphs: cycle, unknown prerequisite
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/graphs",
        k,
        Some(json!({"id": unique("bad"), "nodes": [
        {"id": "a", "requires": ["b"]}, {"id": "b", "requires": ["a"]}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/graphs",
        k,
        Some(json!({"id": unique("bad"), "nodes": [{"id": "a", "requires": ["zz"]}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, r) = call_server(&app, "POST", "/v1/server/graphs", k, Some(json!({"id": g, "name": "Tree", "reset_cost": {gold.clone(): 10}, "refund_pct": 50, "nodes": [
        {"id": "root", "cost": {gold.clone(): 100}, "rewards": bonus, "max_rank": 2},
        {"id": "gated", "requires": ["root"], "min_level": {"track": track, "level": 1}, "cost": {gold.clone(): 50}},
        {"id": "slow", "requires": ["root"], "cost": {gold.clone(): 20}, "unlock_seconds": 7200, "skip_cost_per_hour": {gold.clone(): 30}},
        {"id": "blink", "requires": ["root"], "unlock_seconds": 1, "grants": {"speed": 2}}
    ]}))).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let node = |v: &Value, id: &str| -> Value {
        v["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .cloned()
            .unwrap()
    };
    let (st, v) = call(&app, "GET", &format!("/v1/graphs/{g}"), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(node(&v, "root")["state"], "available");
    assert_eq!(node(&v, "gated")["state"], "locked");
    assert_eq!(node(&v, "gated")["blocked_by"], json!("requires root"));
    // prerequisite not met → 409
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/gated/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{r}");
    // root rank 1: pays 100, gets the 5-gold reward
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/root/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["node"]["rank"], 1);
    assert_eq!(r["node"]["state"], "available");
    assert_eq!(r["paid"][&gold], 100);
    assert_eq!(r["rewards"][0]["amount"], 5);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], 905, "{w}");
    // root rank 2 → maxed, third attempt 409
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/root/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["node"]["state"], "maxed");
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/root/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    // level gate: blocked until level 1
    let (_, v) = call(&app, "GET", &format!("/v1/graphs/{g}"), Some(&tok), None).await;
    assert_eq!(
        node(&v, "gated")["blocked_by"],
        json!(format!("requires level 1 on {track}"))
    );
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/progression/xp",
        k,
        Some(json!({"account_id": acc, "track_id": track, "delta": 100})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/gated/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    // timed node: unlocking with a skip price of 2 h × 30
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/slow/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["node"]["state"], "unlocking");
    assert_eq!(r["node"]["skip_cost"][&gold], 60);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/slow/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "already unlocking");
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/slow/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/slow/skip"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["paid"][&gold], 60);
    assert_eq!(r["node"]["state"], "maxed");
    // 1-second node: cannot be skipped (no price), claimable once the timer ran out
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/blink/unlock"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/blink/skip"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (_, v) = call(&app, "GET", &format!("/v1/graphs/{g}"), Some(&tok), None).await;
    assert_eq!(node(&v, "blink")["state"], "ready");
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/nodes/blink/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["node"]["grants"]["speed"], 2);
    // server view of the player's graph
    let (st, v) = call_server(
        &app,
        "GET",
        &format!("/v1/server/graphs/{g}/progress/{acc}"),
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(node(&v, "root")["rank"], 2);
    // reset: pay 10, refund 50% of 100+100+50+20+60 = 165, everything locked again
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    let before = w["balances"][0]["balance"].as_i64().unwrap();
    let (st, r) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/reset"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["refunded"][&gold], 165);
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    assert_eq!(w["balances"][0]["balance"], before - 10 + 165);
    let (_, v) = call(&app, "GET", &format!("/v1/graphs/{g}"), Some(&tok), None).await;
    assert_eq!(node(&v, "root")["rank"], 0);
    assert_eq!(node(&v, "gated")["state"], "locked");
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/graphs/{g}/reset"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "nothing to reset");
}

/// Magic-link login (passwordless, verifies the address), custom auth minted by the server key,
/// and the per-account session cap.
#[tokio::test]
async fn magic_link_custom_auth_and_session_cap() {
    let Some(app) = app_with(|c| c.session.max_sessions_per_account = 2).await else {
        return;
    };
    let k = "defaultkey";
    let email = format!("{}@example.test", unique("magic"));
    // unknown address with create:false → 200 but no link
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email/magic/request",
        None,
        Some(json!({"email": email, "create": false})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token_for = |email: String| {
        let app = app.clone();
        async move {
            let (st, ev) = call_server(
                &app,
                "GET",
                "/v1/server/events?kinds=account.magic_link&limit=1000",
                k,
                None,
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{ev}");
            ev["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    if e["payload"].is_object() {
                        &e["payload"]
                    } else {
                        &e["data"]
                    }
                })
                .filter(|p| p["email"] == email)
                .map(|p| p["token"].as_str().unwrap().to_string())
                .next_back()
        }
    };
    assert!(
        token_for(email.clone()).await.is_none(),
        "create:false must not mint a link"
    );
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email/magic/request",
        None,
        Some(json!({"email": email})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token = token_for(email.clone()).await.expect("magic link token");
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/email/magic",
        None,
        Some(json!({"token": token, "vars": {"via": "magic"}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/email/magic",
        None,
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "single use: {r}");
    let (_, me) = call(&app, "GET", "/v1/account", Some(&tok), None).await;
    assert!(
        me["identities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["provider"] == "email" && i["verified"] == true),
        "{me}"
    );
    // the same address asks again → same account
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/email/magic/request",
        None,
        Some(json!({"email": email})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let token2 = token_for(email.clone()).await.unwrap();
    assert_ne!(token, token2);
    let (_, sess2) = call(
        &app,
        "POST",
        "/v1/auth/email/magic",
        None,
        Some(json!({"token": token2})),
    )
    .await;
    assert_eq!(sess2["account_id"], sess["account_id"]);
    // custom auth: server key only, stable mapping id → account
    let id = unique("launcher");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/server/auth/custom",
        None,
        Some(json!({"id": id})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, c1) = call_server(
        &app,
        "POST",
        "/v1/server/auth/custom",
        k,
        Some(json!({"id": id})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c1}");
    let (_, c2) = call_server(
        &app,
        "POST",
        "/v1/server/auth/custom",
        k,
        Some(json!({"id": id})),
    )
    .await;
    assert_eq!(c1["account_id"], c2["account_id"]);
    assert!(c2["created"].is_null() || c2["created"] == false);
    // session cap 2: a third login evicts the oldest session
    let dev = unique("capdevice00");
    let mut sessions = Vec::new();
    for _ in 0..3 {
        let (st, s) = call(
            &app,
            "POST",
            "/v1/auth/device",
            None,
            Some(json!({"id": dev})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{s}");
        sessions.push(s);
    }
    let newest = sessions[2]["access_token"].as_str().unwrap().to_string();
    let (st, list) = call(&app, "GET", "/v1/account/sessions", Some(&newest), None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["sessions"].as_array().unwrap().len(), 2, "{list}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": sessions[0]["refresh_token"]})),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "evicted session must not refresh: {r}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/refresh",
        None,
        Some(json!({"refresh_token": sessions[2]["refresh_token"]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

/// RFC 6902 JSON Patch next to merge-patch, numeric aggregates and full-text `$q`.
#[tokio::test]
async fn storage_json_patch_aggregate_and_fulltext() {
    let Some(app) = app().await else {
        return;
    };
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("sj-")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sess}");
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let col = unique("inv");
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(&tok),
        Some(json!({"objects": [
            {"collection": col, "key": "sword", "value": {"kind": "weapon", "name": "Flaming sword of dawn", "level": 5, "tags": ["fire"]}},
            {"collection": col, "key": "shield", "value": {"kind": "armor", "name": "Tower shield", "level": 7}},
            {"collection": col, "key": "potion", "value": {"kind": "consumable", "name": "Healing potion", "level": 1}},
            {"collection": col, "key": "note", "value": {"kind": "misc", "name": "Note about a sword", "level": "n/a"}}
        ]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // JSON Patch: test guards, add/replace/remove/move/copy, array append
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/patch",
        Some(&tok),
        Some(
            json!({"objects": [{"collection": col, "key": "sword", "ops": [
                {"op": "test", "path": "/level", "value": 5},
                {"op": "replace", "path": "/level", "value": 6},
                {"op": "add", "path": "/tags/-", "value": "legendary"},
                {"op": "add", "path": "/stats", "value": {"atk": 10}},
                {"op": "copy", "from": "/stats/atk", "path": "/stats/def"},
                {"op": "move", "from": "/kind", "path": "/slot"},
                {"op": "remove", "path": "/name"}
            ]}]}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, r) = call(
        &app,
        "POST",
        "/v1/storage/read",
        Some(&tok),
        Some(json!({"objects": [{"collection": col, "key": "sword"}]})),
    )
    .await;
    let sword = &r["objects"][0]["value"];
    assert_eq!(
        sword,
        &json!({"level": 6, "tags": ["fire", "legendary"], "stats": {"atk": 10, "def": 10}, "slot": "weapon"}),
        "{r}"
    );
    // failing test → 409 and nothing applied
    let (st, v) = call(
        &app,
        "POST",
        "/v1/storage/patch",
        Some(&tok),
        Some(
            json!({"objects": [{"collection": col, "key": "sword", "ops": [
                {"op": "replace", "path": "/level", "value": 99},
                {"op": "test", "path": "/level", "value": 5}
            ]}]}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (_, r) = call(
        &app,
        "POST",
        "/v1/storage/read",
        Some(&tok),
        Some(json!({"objects": [{"collection": col, "key": "sword"}]})),
    )
    .await;
    assert_eq!(r["objects"][0]["value"]["level"], 6, "{r}");
    for bad in [
        json!([{"op": "remove", "path": "/nope"}]),
        json!([{"op": "frobnicate", "path": "/level"}]),
        json!([{"op": "add", "path": "level", "value": 1}]),
        json!([{"op": "move", "from": "/stats", "path": "/stats/inner"}]),
    ] {
        let (st, v) = call(
            &app,
            "POST",
            "/v1/storage/patch",
            Some(&tok),
            Some(json!({"objects": [{"collection": col, "key": "sword", "ops": bad}]})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    }
    // aggregates: numeric levels only (the note's "n/a" is skipped)
    let (st, a) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}/aggregate?field=level"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    let g = &a["groups"][0];
    assert_eq!(g["count"], 3, "{a}");
    assert_eq!(g["sum"], 14.0, "{a}");
    assert_eq!(g["min"], 1.0, "{a}");
    assert_eq!(g["max"], 7.0, "{a}");
    let (st, a) = call(
        &app,
        "GET",
        &format!(
            "/v1/storage/{col}/aggregate?field=level&where={}",
            urlencoding(r#"{"$where":[["level",">=",6]]}"#)
        ),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["groups"][0]["count"], 2, "{a}");
    assert_eq!(a["groups"][0]["avg"], 6.5, "{a}");
    let (st, a) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}/aggregate?field=level&group=slot"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    let groups = a["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2, "{a}");
    assert!(
        groups
            .iter()
            .any(|g| g["group"] == "weapon" && g["count"] == 1 && g["sum"] == 6.0),
        "{a}"
    );
    assert!(
        groups
            .iter()
            .any(|g| g["group"].is_null() && g["count"] == 2),
        "{a}"
    );
    let (st, _) = call(
        &app,
        "GET",
        &format!("/v1/storage/{col}/aggregate"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // full-text over the value, combinable with containment / $where
    let q = |w: &str| format!("/v1/storage/{col}?where={}", urlencoding(w));
    let keys = |v: &serde_json::Value| -> Vec<String> {
        v["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["key"].as_str().unwrap().to_string())
            .collect()
    };
    let (st, v) = call(&app, "GET", &q(r#"{"$q":"sword"}"#), Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(keys(&v), vec!["note"], "{v}");
    let (st, v) = call(
        &app,
        "GET",
        &q(r#"{"$q":"shield OR potion"}"#),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(keys(&v), vec!["potion", "shield"], "{v}");
    let (st, v) = call(
        &app,
        "GET",
        &q(r#"{"$q":"shield OR potion","kind":"armor"}"#),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(keys(&v), vec!["shield"], "{v}");
    let (st, v) = call(
        &app,
        "GET",
        &format!(
            "/v1/storage/{col}/count?where={}",
            urlencoding(r#"{"$q":"sword","$where":[["level",">",1]]}"#)
        ),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["count"], 0, "{v}");
    let (st, _) = call(&app, "GET", &q(r#"{"$q":""}"#), Some(&tok), None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn migrate_playfab_and_ugs_exports() {
    let Some(app) = app().await else { return };
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").unwrap();
    let db = sqlx::PgPool::connect(&url).await.unwrap();
    let k = "defaultkey";
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let pf = format!("PF{}", sfx[..14].to_ascii_uppercase());
    let ug = format!("ugs-{sfx}");
    let dir = std::env::temp_dir().join(format!("musubi-pf-{sfx}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("players.json"),
        json!({"Players": [{
            "PlayFabId": pf,
            "InfoResultPayload": {
                "PlayerProfile": {
                    "PlayerId": pf, "DisplayName": "Pf Hero",
                    "LinkedAccounts": [{"Platform": "Steam", "PlatformUserId": format!("7656{}", &sfx[..10])}],
                    "Statistics": [{"Name": "wins", "Value": 12}]
                },
                "AccountInfo": {
                    "Username": format!("pf_{}", &sfx[..12]),
                    "PrivateInfo": {"Email": format!("{}@example.test", pf.to_ascii_lowercase())}
                },
                "UserData": {
                    "loadout": {"Value": "{\"primary\":\"ak\"}", "Permission": "Public"},
                    "note": {"Value": "hi", "Permission": "Private"}
                },
                "UserReadOnlyData": {"vip": {"Value": "true"}},
                "UserVirtualCurrency": {"GC": 150, "GM": -5},
                "UserInventory": [{"ItemId": "sword_pf", "ItemInstanceId": "inst-1", "RemainingUses": 3,
                                   "ItemClass": "weapon", "DisplayName": "Sword", "CustomData": {"lvl": "2"}}],
                "PlayerStatistics": [{"StatisticName": "wins", "Value": 12}]
            }
        }]})
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("ugs.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "playerId": ug, "username": format!("ugs_{}", &sfx[..12]),
                "externalIds": [{"providerId": "google-play-games", "externalId": format!("g-{ug}")}],
                "cloudSave": {"progress": {"value": {"level": 7}}},
                "currencies": {"GEM": 20},
                "inventory": [{"inventoryItemId": "shield_ugs", "playersInventoryItemId": "pi-1", "quantity": 2}],
                "leaderboards": {"wins": 3}
            }),
            json!({"garbage": true})
        ),
    )
    .unwrap();

    let report = musubi_server::playfab::import_dir(&dir, &db, false)
        .await
        .unwrap();
    let get = |n: &str| report.iter().find(|(t, _)| *t == n).unwrap().1;
    assert_eq!(get("accounts").written, 2, "{report:?}");
    assert_eq!(get("skipped_records").kept, 1);
    assert_eq!(get("wallets").written, 3);
    assert_eq!(get("inventory").written, 2);
    assert_eq!(get("leaderboard_records").written, 2);
    assert_eq!(get("storage_objects").written, 4);
    // identities: steam + custom + email for PlayFab, google + custom for UGS
    assert_eq!(get("identities").written, 5);
    // re-running the same export writes nothing
    let again = musubi_server::playfab::import_dir(&dir, &db, false)
        .await
        .unwrap();
    for (t, c) in &again {
        if *t != "skipped_records" {
            assert_eq!(c.written, 0, "{t}: {again:?}");
        }
    }

    // the old PlayFab id still logs in (custom auth) and lands on the imported account
    let (st, s) = call_server(
        &app,
        "POST",
        "/v1/server/auth/custom",
        k,
        Some(json!({"id": format!("playfab:{pf}")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(
        s["account_id"],
        musubi_server::playfab::stable_id(&format!("playfab:{pf}")).to_string()
    );
    let tok = s["access_token"].as_str().unwrap().to_string();
    let (st, me) = call(&app, "GET", "/v1/account", Some(&tok), None).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert_eq!(me["display_name"], "Pf Hero");
    let ms = me.to_string();
    assert!(ms.contains("\"steam\"") && ms.contains("\"email\""), "{me}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    let ws = w.to_string();
    assert!(
        ws.contains("\"GC\"") && ws.contains("150") && !ws.contains("-5"),
        "{w}"
    );
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    assert!(inv.to_string().contains("sword_pf"), "{inv}");
    let (st, so) = call(
        &app,
        "GET",
        "/v1/storage/playfab_user_data",
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{so}");
    assert!(so.to_string().contains("\"primary\":\"ak\""), "{so}");
    let (_, ro) = call(
        &app,
        "GET",
        "/v1/storage/playfab_readonly",
        Some(&tok),
        None,
    )
    .await;
    assert!(ro.to_string().contains("vip"), "{ro}");
    let score: Option<(i64,)> = sqlx::query_as(
        "select score from musubi.leaderboard_records where leaderboard_id = 'wins' and owner_id = $1",
    )
    .bind(musubi_server::playfab::stable_id(&format!("playfab:{pf}")))
    .fetch_optional(&db)
    .await
    .unwrap();
    assert_eq!(score, Some((12,)));

    // UGS player
    let (st, s2) = call_server(
        &app,
        "POST",
        "/v1/server/auth/custom",
        k,
        Some(json!({"id": format!("ugs:{ug}")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    let tok2 = s2["access_token"].as_str().unwrap().to_string();
    let (_, cs) = call(&app, "GET", "/v1/storage/cloud_save", Some(&tok2), None).await;
    assert!(cs.to_string().contains("\"level\":7"), "{cs}");
    let (_, w2) = call(&app, "GET", "/v1/wallet", Some(&tok2), None).await;
    assert!(w2.to_string().contains("\"GEM\""), "{w2}");
    let (_, inv2) = call(&app, "GET", "/v1/inventory", Some(&tok2), None).await;
    assert!(inv2.to_string().contains("shield_ugs"), "{inv2}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn age_gate_and_parental_consent() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let login = |p: &str| {
        let app = app.clone();
        let id = unique(p);
        async move {
            let (st, s) = call(
                &app,
                "POST",
                "/v1/auth/device",
                None,
                Some(json!({"id": id})),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{s}");
            (
                s["access_token"].as_str().unwrap().to_string(),
                s["account_id"].as_str().unwrap().to_string(),
            )
        }
    };
    let (kt, kid_id) = login("kid").await;
    let (ot, other_id) = login("grown").await;
    let today = time::OffsetDateTime::now_utc().date();
    let born = |years: i32| {
        format!(
            "{}-{:02}-{:02}",
            today.year() - years,
            today.month() as u8,
            today.day().min(28)
        )
    };

    // no age yet: nothing is gated
    let (st, a) = call(&app, "GET", "/v1/account/age", Some(&kt), None).await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["bracket"], "unknown");
    assert_eq!(a["restrictions"], json!([]));

    // ten years old → child, the default list is closed
    let (st, a) = call(
        &app,
        "PUT",
        "/v1/account/age",
        Some(&kt),
        Some(json!({"birthdate": born(10)})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["bracket"], "child");
    for f in ["chat", "friends", "purchases", "ugc", "public_profile"] {
        assert!(
            a["restrictions"].as_array().unwrap().contains(&json!(f)),
            "{a}"
        );
    }
    // a child cannot promote itself
    let (st, e) = call(
        &app,
        "PUT",
        "/v1/account/age",
        Some(&kt),
        Some(json!({"bracket": "adult"})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{e}");
    // friend requests are closed for the child, open for the adult
    let (st, e) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(&kt),
        Some(json!({"ids": [other_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{e}");
    assert!(e["message"].as_str().unwrap().contains("friends"), "{e}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(&ot),
        Some(json!({"ids": [kid_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // public profile of a child is reduced to id / username / avatar
    let (st, u) = call(
        &app,
        "GET",
        &format!("/v1/users?ids={kid_id},{other_id}"),
        Some(&ot),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{u}");
    let users = u["users"].as_array().unwrap();
    let kid = users.iter().find(|x| x["id"] == kid_id).unwrap();
    let grown = users.iter().find(|x| x["id"] == other_id).unwrap();
    assert_eq!(kid["restricted"], true, "{kid}");
    assert!(
        kid.get("lang").is_none() && kid.get("created_at").is_none(),
        "{kid}"
    );
    assert!(kid["username"].is_string());
    assert!(
        grown.get("restricted").is_none() && grown["lang"].is_string(),
        "{grown}"
    );

    // parental consent: request → token in the event → consent lifts all but purchases
    let (st, e) = call(
        &app,
        "POST",
        "/v1/account/parental/request",
        Some(&kt),
        Some(json!({"parent_email": "nope"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let parent = format!("{}@example.test", unique("parent"));
    let (st, r) = call(
        &app,
        "POST",
        "/v1/account/parental/request",
        Some(&kt),
        Some(json!({"parent_email": parent})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, ev) = call_server(
        &app,
        "GET",
        "/v1/server/events?kinds=account.parental_consent_request&limit=1000",
        k,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ev}");
    let token = ev["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            if e["payload"].is_object() {
                &e["payload"]
            } else {
                &e["data"]
            }
        })
        .filter(|p| p["account_id"] == kid_id)
        .map(|p| p["token"].as_str().unwrap().to_string())
        .next_back()
        .expect("consent token event");
    let (st, c) = call(
        &app,
        "POST",
        "/v1/auth/parental/consent",
        None,
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["parental_consent"], true);
    assert_eq!(c["restrictions"], json!(["purchases"]));
    let (st, _) = call(
        &app,
        "POST",
        "/v1/auth/parental/consent",
        None,
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "token is single-use");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/friends",
        Some(&kt),
        Some(json!({"ids": [other_id]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, u) = call(
        &app,
        "GET",
        &format!("/v1/users?ids={kid_id}"),
        Some(&ot),
        None,
    )
    .await;
    assert!(u["users"][0].get("restricted").is_none(), "{u}");

    // the game server (platform age signal, support) can raise the bracket
    let (st, a) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/accounts/{kid_id}/age"),
        k,
        Some(json!({"bracket": "adult"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["bracket"], "adult");
    assert_eq!(a["restrictions"], json!([]));

    // teen: nothing closed by default; bad input
    let (tt, _) = login("teen").await;
    let (st, a) = call(
        &app,
        "PUT",
        "/v1/account/age",
        Some(&tt),
        Some(json!({"birthdate": born(15)})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["bracket"], "teen");
    assert_eq!(a["restrictions"], json!([]));
    let (st, _) = call(
        &app,
        "PUT",
        "/v1/account/age",
        Some(&tt),
        Some(json!({"birthdate": "2099-01-01"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&app, "PUT", "/v1/account/age", Some(&tt), Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn facebook_gamecenter_logins_and_password_policy() {
    use axum::extract::Query as AxQuery;
    use axum::routing::get as ax_get;
    use base64::Engine;
    let uid = unique("fbuid");
    let cert = include_bytes!("fixtures/gamecenter_test_cert.der").to_vec();
    let mock = Router::new()
        .route(
            "/debug_token",
            ax_get(move |AxQuery(q): AxQuery<std::collections::HashMap<String, String>>| {
                let uid = uid.clone();
                async move {
                    assert_eq!(q.get("access_token").map(String::as_str), Some("app1|sec"));
                    match q.get("input_token").map(String::as_str) {
                        Some("fb-ok") => axum::Json(json!({"data": {"app_id": "app1", "is_valid": true, "user_id": uid}})),
                        Some("fb-other-app") => axum::Json(json!({"data": {"app_id": "app2", "is_valid": true, "user_id": "x"}})),
                        _ => axum::Json(json!({"data": {"app_id": "app1", "is_valid": false, "error": {"code": 190, "message": "Invalid OAuth access token."}}})),
                    }
                }
            }),
        )
        .route("/gc/public.cer", ax_get(move || { let c = cert.clone(); async move { c } }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let b = base.clone();
    let Some(app) = app_with(move |c| {
        c.social.facebook.url = b.clone();
        c.social.facebook.app_id = "app1".into();
        c.social.facebook.app_secret = "sec".into();
        c.social.gamecenter.bundle_ids = vec!["com.example.game".into()];
        c.social.gamecenter.public_key_url_prefixes = vec![format!("{b}/gc/")];
        c.session.password_min_length = 10;
        c.session.password_require_classes = 2;
    })
    .await
    else {
        return;
    };

    // Facebook access tokens: valid → session, same uid → same account; other app / invalid → 401.
    let (st, s1) = call(
        &app,
        "POST",
        "/v1/auth/facebook",
        None,
        Some(json!({"token": "fb-ok"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    let (st, s2) = call(
        &app,
        "POST",
        "/v1/auth/facebook",
        None,
        Some(json!({"token": "fb-ok"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(s1["account_id"], s2["account_id"]);
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/facebook",
        None,
        Some(json!({"token": "fb-other-app"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/facebook",
        None,
        Some(json!({"token": "fb-bad"})),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{r}");
    let (st, r) = call(
        &app,
        "GET",
        "/v1/account",
        Some(s1["access_token"].as_str().unwrap()),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(
        r["identities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["provider"] == "facebook"),
        "{r}"
    );

    // Game Center: sign player_id ‖ bundle_id ‖ timestamp(be) ‖ salt with the fixture key.
    let key = ring::signature::RsaKeyPair::from_pkcs8(include_bytes!(
        "fixtures/gamecenter_test_key.p8.der"
    ))
    .unwrap();
    let b64 = base64::engine::general_purpose::STANDARD;
    let player = unique("G:");
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let salt = [7u8, 8, 9, 10];
    let sign = |player: &str, bundle: &str, ts: u64| {
        let mut payload = Vec::new();
        payload.extend_from_slice(player.as_bytes());
        payload.extend_from_slice(bundle.as_bytes());
        payload.extend_from_slice(&ts.to_be_bytes());
        payload.extend_from_slice(&salt);
        let mut sig = vec![0u8; key.public().modulus_len()];
        key.sign(
            &ring::signature::RSA_PKCS1_SHA256,
            &ring::rand::SystemRandom::new(),
            &payload,
            &mut sig,
        )
        .unwrap();
        b64.encode(sig)
    };
    let req = |player: &str, bundle: &str, ts: u64, sig: String, url: &str| json!({"player_id": player, "bundle_id": bundle, "timestamp": ts, "salt": b64.encode(salt), "signature": sig, "public_key_url": url});
    let good_url = format!("{base}/gc/public.cer");
    let (st, g1) = call(
        &app,
        "POST",
        "/v1/auth/gamecenter",
        None,
        Some(req(
            &player,
            "com.example.game",
            ts,
            sign(&player, "com.example.game", ts),
            &good_url,
        )),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g1}");
    let (st, g2) = call(
        &app,
        "POST",
        "/v1/auth/gamecenter",
        None,
        Some(req(
            &player,
            "com.example.game",
            ts + 1,
            sign(&player, "com.example.game", ts + 1),
            &good_url,
        )),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g2}");
    assert_eq!(g1["account_id"], g2["account_id"]);
    assert_ne!(g1["account_id"], s1["account_id"]);
    // tampered player id, wrong bundle, stale timestamp, foreign certificate host → refused
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/gamecenter",
        None,
        Some(req(
            "G:someone-else",
            "com.example.game",
            ts,
            sign(&player, "com.example.game", ts),
            &good_url,
        )),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/gamecenter",
        None,
        Some(req(
            &player,
            "com.evil.game",
            ts,
            sign(&player, "com.evil.game", ts),
            &good_url,
        )),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{r}");
    let old = ts - 3600 * 1000;
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/gamecenter",
        None,
        Some(req(
            &player,
            "com.example.game",
            old,
            sign(&player, "com.example.game", old),
            &good_url,
        )),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/gamecenter",
        None,
        Some(req(
            &player,
            "com.example.game",
            ts,
            sign(&player, "com.example.game", ts),
            "https://evil.example/public.cer",
        )),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "{r}");

    // Password policy applies when a password is set, not to existing logins.
    let email = format!("{}@example.test", unique("pp"));
    for weak in ["short1", "alllowercaseonly", "Password123", "qwertyuiop12"] {
        let (st, r) = call(
            &app,
            "POST",
            "/v1/auth/email",
            None,
            Some(json!({"email": email, "password": weak})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{weak}: {r}");
    }
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "Correct-horse-9"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/auth/email",
        None,
        Some(json!({"email": email, "password": "Correct-horse-9"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
}

/// `POST /v1/server/tx`: storage write + wallet debit + inventory grant land together; a CAS
/// miss or an overdrawn wallet rolls everything back.
#[tokio::test]
async fn server_tx_all_or_nothing() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("tx")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let sword = unique("sword");
    let coll = unique("txc");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/items", k, Some(json!({"id": sword, "name": "Sword", "category": "weapon", "stackable": false, "properties": {}}))).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call_server(&app, "POST", "/v1/server/wallet/adjust", k, Some(json!({"account_id": acc, "currency": gold, "delta": 150, "reason": "seed", "reference": "t"}))).await;
    assert_eq!(st, StatusCode::OK);

    // 1) buy: -100 gold, +sword, quest object created (create-only) — all land
    let ops = |version: &str, price: i64| {
        json!({"ops": [
            {"type": "storage_write", "owner_id": acc, "collection": coll, "key": "quest", "value": {"state": "done"}, "version": version},
            {"type": "wallet_adjust", "account_id": acc, "currency": gold, "delta": -price, "reason": "buy", "reference": sword},
            {"type": "inventory_grant", "account_id": acc, "item_id": sword, "quantity": 1},
        ]})
    };
    let (st, r) = call_server(&app, "POST", "/v1/server/tx", k, Some(ops("*", 100))).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let res = r["results"].as_array().unwrap();
    assert_eq!(res.len(), 3);
    assert!(res[0]["ack"]["version"].is_string(), "{r}");
    assert_eq!(res[1]["entry"]["delta"], -100, "{r}");
    let entry_id = res[2]["entry"]["id"].as_str().unwrap().to_string();
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    let bal = w["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["currency"] == json!(gold))
        .map(|b| b["balance"].as_i64().unwrap());
    assert_eq!(bal, Some(50), "{w}");

    // 2) same create-only write again → 409 and the wallet/inventory untouched
    let (st, r) = call_server(&app, "POST", "/v1/server/tx", k, Some(ops("*", 10))).await;
    assert_eq!(st, StatusCode::CONFLICT, "{r}");
    // 3) wallet cannot go negative → everything rolled back, including the storage write
    let (st, r) = call_server(&app, "POST", "/v1/server/tx", k, Some(json!({"ops": [
        {"type": "storage_write", "owner_id": acc, "collection": coll, "key": "quest", "value": {"state": "again"}},
        {"type": "wallet_adjust", "account_id": acc, "currency": gold, "delta": -500, "reason": "buy", "reference": "x"},
    ]}))).await;
    assert_ne!(st, StatusCode::OK, "{r}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    let bal = w["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["currency"] == json!(gold))
        .map(|b| b["balance"].as_i64().unwrap());
    assert_eq!(bal, Some(50), "{w}");
    let (st, o) = call(
        &app,
        "POST",
        "/v1/storage/read",
        Some(&tok),
        Some(json!({"objects": [{"collection": coll, "key": "quest", "owner_id": acc}]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{o}");
    assert_eq!(o["objects"][0]["value"]["state"], "done", "{o}");
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    assert_eq!(
        inv["items"]
            .as_array()
            .map(|e| e.iter().filter(|e| e["item_id"] == json!(sword)).count()),
        Some(1),
        "{inv}"
    );

    // 4) consume the sword and refund in one go
    let (st, r) = call_server(&app, "POST", "/v1/server/tx", k, Some(json!({"ops": [
        {"type": "inventory_consume", "account_id": acc, "entry_id": entry_id, "quantity": 1},
        {"type": "wallet_adjust", "account_id": acc, "currency": gold, "delta": 100, "reason": "refund", "reference": sword},
    ]}))).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r["results"][0]["entry"].is_null(), "{r}");
    let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
    let bal = w["balances"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["currency"] == json!(gold))
        .map(|b| b["balance"].as_i64().unwrap());
    assert_eq!(bal, Some(150), "{w}");
    // limits: 0 and 101 ops
    let (st, _) = call_server(&app, "POST", "/v1/server/tx", k, Some(json!({"ops": []}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn inventory_rules_unique_bind_equip_limits() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("inv")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let helmet = unique("helmet");
    let ring = unique("ring");
    let badge = unique("badge");
    let gem = unique("gem");
    let gem_cat = unique("gemcat");
    for body in [
        json!({"id": helmet, "name": "Helmet", "category": "armor", "stackable": false, "unique": true, "bind": "equip",
               "slots": ["head"], "instance_schema": {"type": "object", "properties": {"durability": {"type": "integer", "minimum": 0, "maximum": 100}}}}),
        json!({"id": ring, "name": "Ring", "category": "armor", "stackable": false, "slots": ["ring1", "ring2"]}),
        json!({"id": badge, "name": "Badge", "category": "badge", "stackable": false, "tradeable": false}),
        json!({"id": gem, "name": "Gem", "category": gem_cat, "stackable": false}),
    ] {
        let (st, r) = call_server(&app, "POST", "/v1/server/items", k, Some(body)).await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let (_, def) = call_server(&app, "GET", &format!("/v1/server/items/{helmet}"), k, None).await;
    assert_eq!(def["unique"], true);
    assert_eq!(def["bind"], "equip");
    assert_eq!(def["slots"], json!(["head"]));
    assert_eq!(def["tradeable"], true);
    // a re-upsert without the rule fields keeps them
    let (st, def) = call_server(&app, "POST", "/v1/server/items", k, Some(json!({"id": helmet, "name": "Helmet", "category": "armor", "stackable": false, "instance_schema": def["instance_schema"]}))).await;
    assert_eq!(st, StatusCode::OK, "{def}");
    assert_eq!(def["bind"], "equip");
    assert_eq!(def["slots"], json!(["head"]));

    let grant = |item: String, instance: Value| {
        let app = app.clone();
        let acc = acc.clone();
        async move {
            call_server(&app, "POST", "/v1/server/inventory/grant", k, Some(json!({"account_id": acc, "item_id": item, "quantity": 1, "instance": instance}))).await
        }
    };
    // unique: one per account
    let (st, h) = grant(helmet.clone(), json!({"durability": 100})).await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(h["bound"], false);
    assert!(h["slot"].is_null());
    let helmet_id = h["id"].as_str().unwrap().to_string();
    let (st, r) = grant(helmet.clone(), json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "{r}");

    // equip binds (bind = equip) and refuses foreign slots
    let (st, r) = call(
        &app,
        "POST",
        "/v1/inventory/equip",
        Some(&tok),
        Some(json!({"id": helmet_id, "slot": "ring1"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/inventory/equip",
        Some(&tok),
        Some(json!({"id": helmet_id, "slot": "head"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["slot"], "head");
    assert_eq!(r["bound"], true);

    // slots are exclusive: equipping a third ring into ring1 takes the first one off
    let mut rings = Vec::new();
    for _ in 0..3 {
        let (st, r) = grant(ring.clone(), json!({})).await;
        assert_eq!(st, StatusCode::OK, "{r}");
        rings.push(r["id"].as_str().unwrap().to_string());
    }
    for (id, slot) in [
        (&rings[0], "ring1"),
        (&rings[1], "ring2"),
        (&rings[2], "ring1"),
    ] {
        let (st, r) = call(
            &app,
            "POST",
            "/v1/inventory/equip",
            Some(&tok),
            Some(json!({"id": id, "slot": slot})),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{r}");
        assert_eq!(r["slot"], slot);
        assert_eq!(r["bound"], false, "rings do not bind");
    }
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    let slot_of = |id: &str| {
        inv["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == id)
            .map(|e| e["slot"].clone())
            .unwrap()
    };
    assert!(slot_of(&rings[0]).is_null());
    assert_eq!(slot_of(&rings[1]), "ring2");
    assert_eq!(slot_of(&rings[2]), "ring1");
    assert_eq!(slot_of(&helmet_id), "head");

    // bound / equipped / untradeable entries stay off the market
    let list = |id: String| {
        let app = app.clone();
        let tok = tok.clone();
        let gold = gold.clone();
        async move {
            call(
                &app,
                "POST",
                "/v1/market/listings",
                Some(&tok),
                Some(json!({"inventory_id": id, "currency": gold, "buyout": 10})),
            )
            .await
        }
    };
    let (st, r) = list(helmet_id.clone()).await;
    assert_eq!(st, StatusCode::CONFLICT, "bound: {r}");
    let (st, r) = list(rings[2].clone()).await;
    assert_eq!(st, StatusCode::CONFLICT, "equipped: {r}");
    let (st, r) = call(
        &app,
        "POST",
        "/v1/inventory/unequip",
        Some(&tok),
        Some(json!({"id": rings[2]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r["slot"].is_null());
    let (st, r) = list(rings[2].clone()).await;
    assert_eq!(st, StatusCode::OK, "unequipped ring lists: {r}");
    let (st, b) = grant(badge.clone(), json!({})).await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (st, r) = list(b["id"].as_str().unwrap().to_string()).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "untradeable: {r}");
    // the helmet stays bound after unequip
    let (st, r) = call(
        &app,
        "POST",
        "/v1/inventory/unequip",
        Some(&tok),
        Some(json!({"id": helmet_id})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["bound"], true);
    assert!(r["slot"].is_null());

    // server-side instance patch honours the schema
    let path = format!("/v1/server/inventory/{helmet_id}/instance");
    let (st, r) = call_server(
        &app,
        "POST",
        &path,
        k,
        Some(json!({"account_id": acc, "merge": {"durability": 40}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["instance"]["durability"], 40);
    let (st, r) = call_server(
        &app,
        "POST",
        &path,
        k,
        Some(json!({"account_id": acc, "merge": {"durability": 500}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, _) = call_server(
        &app,
        "POST",
        &path,
        k,
        Some(json!({"account_id": uuid::Uuid::now_v7().to_string(), "merge": {"durability": 1}})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, inv) = call(&app, "GET", "/v1/inventory", Some(&tok), None).await;
    let h = inv["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == helmet_id)
        .unwrap();
    assert_eq!(h["instance"]["durability"], 40);

    // per-category entry limits
    let (st, r) = call_server(
        &app,
        "PUT",
        "/v1/server/inventory/limits",
        k,
        Some(json!({"category": gem_cat, "max_entries": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(r["limits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["category"] == gem_cat && l["max_entries"] == 2));
    for _ in 0..2 {
        let (st, r) = grant(gem.clone(), json!({})).await;
        assert_eq!(st, StatusCode::OK, "{r}");
    }
    let (st, r) = grant(gem.clone(), json!({})).await;
    assert_eq!(st, StatusCode::TOO_MANY_REQUESTS, "{r}");
    let (st, r) = call_server(
        &app,
        "PUT",
        "/v1/server/inventory/limits",
        k,
        Some(json!({"category": gem_cat, "max_entries": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert!(!r["limits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["category"] == gem_cat));
    let (st, r) = grant(gem.clone(), json!({})).await;
    assert_eq!(st, StatusCode::OK, "{r}");
}

#[tokio::test]
async fn deferred_rewards_go_through_the_mailbox() {
    let Some(app) = app().await else { return };
    let k = "defaultkey";
    let (st, sess) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("dr")})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let tok = sess["access_token"].as_str().unwrap().to_string();
    let acc = sess["account_id"].as_str().unwrap().to_string();
    let gold = unique("gold");
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/server/currencies",
        k,
        Some(json!({"code": gold, "name": "Gold"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let now_t = unique("now");
    let later_t = unique("later");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({
            "id": now_t, "name": "Daily", "kind": "guaranteed",
            "entries": [{"weight": 1, "rarity": "common", "currency": gold, "amount": 50}],
            "deferred": {"delay_secs": 0, "title": "Daily gift", "body": "claim me"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["deferred"]["title"], "Daily gift");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({
            "id": later_t, "name": "Later", "kind": "guaranteed",
            "entries": [{"weight": 1, "rarity": "common", "currency": gold, "amount": 500}],
            "deferred": {"delay_secs": 3600, "ttl_secs": 7200, "title": "Tomorrow"}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/tables",
        k,
        Some(json!({
            "id": unique("bad"), "name": "Bad", "kind": "guaranteed",
            "entries": [{"weight": 1, "rarity": "common", "currency": gold, "amount": 1}],
            "deferred": {"delay_secs": -1}
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");

    let balance = |app: axum::Router| {
        let tok = tok.clone();
        let gold = gold.clone();
        async move {
            let (_, w) = call(&app, "GET", "/v1/wallet", Some(&tok), None).await;
            w["balances"]
                .as_array()
                .unwrap()
                .iter()
                .find(|b| b["currency"] == gold)
                .map(|b| b["balance"].as_i64().unwrap())
                .unwrap_or(0)
        }
    };

    // a roll on a deferred table freezes the outcome into a mail instead of paying out
    let (st, roll) = call(
        &app,
        "POST",
        &format!("/v1/rewards/{now_t}/roll"),
        Some(&tok),
        Some(json!({"client_seed": "s1", "idempotency_key": "d1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{roll}");
    assert_eq!(roll["rewards"][0]["amount"], 50);
    let mail_id = roll["deferred"]
        .as_str()
        .expect("deferred mail id")
        .to_string();
    assert_eq!(balance(app.clone()).await, 0, "nothing paid out yet");
    // replay reports the same mail
    let (st, again) = call(
        &app,
        "POST",
        &format!("/v1/rewards/{now_t}/roll"),
        Some(&tok),
        Some(json!({"client_seed": "s1", "idempotency_key": "d1"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(again["replayed"], true);
    assert_eq!(again["deferred"], mail_id);
    let (_, inbox) = call(&app, "GET", "/v1/mail", Some(&tok), None).await;
    let m = inbox["mail"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == mail_id)
        .expect("mail listed")
        .clone();
    assert_eq!(m["title"], "Daily gift");
    assert_eq!(m["reward_entries"][0]["amount"], 50);
    assert!(m["available_at"].is_string());
    let (st, c) = call(
        &app,
        "POST",
        &format!("/v1/mail/{mail_id}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(balance(app.clone()).await, 50);
    let (st, _) = call(
        &app,
        "POST",
        &format!("/v1/mail/{mail_id}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // server grant of a delayed table: mail exists, claim refused until available_at
    let (st, g) = call_server(
        &app,
        "POST",
        "/v1/server/rewards/grant",
        k,
        Some(json!({"account_id": acc, "table_id": later_t, "reference": "login"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{g}");
    assert_eq!(g["rewards"][0]["amount"], 500);
    let late_id = g["mail_id"].as_str().expect("mail id").to_string();
    let (st, c) = call(
        &app,
        "POST",
        &format!("/v1/mail/{late_id}/claim"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{c}");
    assert_eq!(balance(app.clone()).await, 50);
    let (st, c) = call(
        &app,
        "DELETE",
        &format!("/v1/mail/{late_id}"),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "unclaimed inline rewards block delete: {c}"
    );

    // inline reward entries on a server mail
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/mail",
        k,
        Some(json!({
            "account_ids": [acc], "title": "Compensation", "body": "sorry",
            "reward_entries": [{"currency": gold, "amount": 0}]
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{r}");
    let (st, r) = call_server(
        &app,
        "POST",
        "/v1/server/mail",
        k,
        Some(json!({
            "account_ids": [acc], "title": "Compensation", "body": "sorry",
            "reward_entries": [{"currency": gold, "amount": 7}], "available_in_seconds": 0
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (_, inbox) = call(&app, "GET", "/v1/mail", Some(&tok), None).await;
    let comp = inbox["mail"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["title"] == "Compensation")
        .expect("compensation mail")
        .clone();
    assert_eq!(comp["reward_entries"][0]["amount"], 7);
    let (st, c) = call(
        &app,
        "POST",
        &format!("/v1/mail/{}/claim", comp["id"].as_str().unwrap()),
        Some(&tok),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(balance(app.clone()).await, 57);
}

/// One IDL document (plan 3.8) drives storage contracts, `/v1/rpc/{name}` input validation and
/// auth, and `/openapi.json`.
#[tokio::test]
async fn idl_deploys_contracts_rpc_validation_and_openapi() {
    let Some(app) = app().await else { return };
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("idl")})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap();
    let col = format!("idlp{}", uuid::Uuid::now_v7().simple());
    let source = format!(
        r#"
package = "t"

[types.Profile]
n = {{ type = "int", min = 0 }}
nick = "string?"

[types.EchoIn]
x = "int?"

[collections."{col}"]
type = "Profile"

[rpcs.echo]
input = "EchoIn"
output = "EchoIn"

[rpcs.admin_only]
auth = "server"
"#
    );
    let (st, sum) = call_server(
        &app,
        "PUT",
        "/v1/admin/idl",
        "defaultkey",
        Some(json!({ "source": source })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sum}");
    assert_eq!(sum["collections"][0]["collection"], col);
    assert_eq!(sum["collections"][0]["version"], 1);
    assert!(sum["rpcs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["name"] == "echo"));

    // the collection contract came from the IDL: `n` must be an int >= 0
    let (st, e) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(t),
        Some(json!({"objects": [{"collection": col, "key": "k", "value": {"n": "x"}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, e) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(t),
        Some(json!({"objects": [{"collection": col, "key": "k", "value": {"n": -1}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, w) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(t),
        Some(
            json!({"objects": [{"collection": col, "key": "k", "value": {"n": 3, "nick": null}}]}),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{w}");

    // rpc input is validated against `EchoIn` before the module runs
    let (st, out) = call(&app, "POST", "/v1/rpc/echo", Some(t), Some(json!({"x": 7}))).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    assert_eq!(out["x"], 7);
    let (st, e) = call(
        &app,
        "POST",
        "/v1/rpc/echo",
        Some(t),
        Some(json!({"x": "no"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    assert!(e["message"].as_str().unwrap().contains("echo"), "{e}");
    let (st, e) = call(
        &app,
        "POST",
        "/v1/rpc/echo",
        Some(t),
        Some(json!({"x": 1, "y": 2})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    // declared server-only: a player is refused before the runtime is consulted
    let (st, _) = call(&app, "POST", "/v1/rpc/admin_only", Some(t), Some(json!({}))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call_server(
        &app,
        "POST",
        "/v1/rpc/admin_only",
        "defaultkey",
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // openapi carries the IDL types and rpc paths; other formats are served too
    let (st, doc) = call(&app, "GET", "/openapi.json", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(doc["components"]["schemas"]["t.EchoIn"]["type"], "object");
    assert_eq!(
        doc["paths"]["/v1/rpc/echo"]["post"]["requestBody"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/t.EchoIn"
    );
    let (st, summary) = call_server(
        &app,
        "GET",
        "/v1/admin/idl?format=summary",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(summary["types"]
        .as_array()
        .unwrap()
        .contains(&json!("Profile")));
    let (st, schemas) = call_server(
        &app,
        "GET",
        "/v1/admin/idl?format=schemas",
        "defaultkey",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        schemas["schemas"][0]["schema"]["properties"]["n"]["minimum"],
        0.0
    );
    let (st, _) = call_server(&app, "GET", "/v1/admin/idl?format=rust", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, e) = call_server(&app, "GET", "/v1/admin/idl?format=nope", "defaultkey", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, _) = call(&app, "GET", "/v1/admin/idl", None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    // a broken document is refused and the previous one stays active
    let (st, e) = call_server(
        &app,
        "PUT",
        "/v1/admin/idl",
        "defaultkey",
        Some(json!({ "source": "[types.A]\nx = \"Nope\"" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    assert!(e["message"].as_str().unwrap().contains("Nope"), "{e}");
    let (st, _) = call(
        &app,
        "POST",
        "/v1/rpc/echo",
        Some(t),
        Some(json!({"x": "no"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // removing the IDL lifts rpc gating; the storage contract it attached stays
    let (st, d) = call_server(&app, "DELETE", "/v1/admin/idl", "defaultkey", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["removed"], true);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/rpc/echo",
        Some(t),
        Some(json!({"x": "no"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/storage/write",
        Some(t),
        Some(json!({"objects": [{"collection": col, "key": "k2", "value": {"n": "x"}}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call_server(&app, "GET", "/v1/admin/idl", "defaultkey", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// Plan 3.8: `[matches.*]` types the creation params and the module's JSON state,
/// `[configs.*]` types server-written / client-read config documents.
#[tokio::test]
async fn idl_types_match_params_state_and_configs() {
    let Some(app) = app().await else { return };
    let (_, s) = call(
        &app,
        "POST",
        "/v1/auth/device",
        None,
        Some(json!({"id": unique("idlm")})),
    )
    .await;
    let t = s["access_token"].as_str().unwrap();
    let sfx = uuid::Uuid::now_v7().simple().to_string();
    let module = format!("tm{}", &sfx[..10]);
    let cfg = format!("tuning_{}", &sfx[..10]);
    // a match module whose init state is {"round":"x"} (base64) and whose other parts keep it
    let reply = |off: usize, name: &str, json: &str| {
        format!(
            "  (data (i32.const {off}) \"{}\")\n  (func (export \"match_{module}_{name}\") (param i32 i32 i32 i32) (result i64)\n    i64.const {off} i64.const 32 i64.shl i64.const {} i64.or)\n",
            json.replace('"', "\\\""),
            json.len()
        )
    };
    let mut wat = String::from(
        "(module\n  (memory (export \"memory\") 1)\n  (global $heap (mut i32) (i32.const 4096))\n  (func (export \"alloc\") (param $n i32) (result i32) (local $p i32)\n    global.get $heap local.set $p\n    global.get $heap local.get $n i32.add global.set $heap\n    local.get $p)\n",
    );
    wat += &reply(
        16,
        "init",
        r#"{"state":"eyJyb3VuZCI6IngifQ==","tick_rate":10,"label":"t"}"#,
    );
    wat += &reply(256, "join_attempt", r#"{"state":"","accept":true}"#);
    wat += &reply(512, "join", r#"{"state":""}"#);
    wat += &reply(768, "leave", r#"{"state":""}"#);
    wat += &reply(1024, "tick", r#"{"state":""}"#);
    wat += &reply(1280, "terminate", r#"{"state":""}"#);
    wat += ")";
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/v1/admin/runtime/modules/{module}"))
                .header("x-musubi-server-key", "defaultkey")
                .header(header::CONTENT_TYPE, "application/wasm")
                .body(Body::from(wat.into_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let idl = |typed_state: bool| {
        format!(
            r#"
[types.ArenaParams]
rounds = {{ type = "int", min = 1, max = 9 }}
[types.ArenaState]
round = "int"
[types.Tuning]
speed = {{ type = "float", min = 0 }}
[matches.{module}]
params = "ArenaParams"
{}
[configs.{cfg}]
type = "Tuning"
version = 2
"#,
            if typed_state {
                "state = \"ArenaState\""
            } else {
                ""
            }
        )
    };
    let (st, sum) = call_server(
        &app,
        "PUT",
        "/v1/admin/idl",
        "defaultkey",
        Some(json!({ "source": idl(true) })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sum}");
    assert_eq!(sum["matches"][0]["module"], module);
    assert_eq!(sum["configs"][0]["version"], 2);

    // params are validated before the match is launched
    let (st, e) = call_server(
        &app,
        "POST",
        "/v1/server/matches",
        "defaultkey",
        Some(json!({"module": module, "params": {"rounds": 0}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, e) = call_server(
        &app,
        "POST",
        "/v1/server/matches",
        "defaultkey",
        Some(json!({"module": module, "params": {"rounds": 3, "extra": 1}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    // valid params, but the module's state {"round":"x"} violates ArenaState
    let (st, e) = call_server(
        &app,
        "POST",
        "/v1/server/matches",
        "defaultkey",
        Some(json!({"module": module, "params": {"rounds": 3}})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{e}");
    assert!(e.to_string().contains("violates the IDL"), "{e}");
    // without a declared state the same module runs
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/admin/idl",
        "defaultkey",
        Some(json!({ "source": idl(false) })),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, m) = call_server(
        &app,
        "POST",
        "/v1/server/matches",
        "defaultkey",
        Some(json!({"module": module, "params": {"rounds": 3}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["label"], "t");

    // configs: declared ones are typed and carry the declared version
    let (st, e) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/configs/{cfg}"),
        "defaultkey",
        Some(json!({"value": {"speed": "fast"}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, e) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/configs/{cfg}"),
        "defaultkey",
        Some(json!({"value": {"speed": -1.0}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let (st, c) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/configs/{cfg}"),
        "defaultkey",
        Some(json!({"value": {"speed": 1.5}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["version"], 2);
    // players read them with a token
    let (st, c) = call(&app, "GET", &format!("/v1/configs/{cfg}"), Some(t), None).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["value"]["speed"], 1.5);
    let (st, _) = call(&app, "GET", &format!("/v1/configs/{cfg}"), None, None).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, l) = call(&app, "GET", "/v1/configs", Some(t), None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        l["configs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == cfg),
        "{l}"
    );
    // undeclared configs are free-form (version 1); names are checked
    let free = format!("free_{}", &sfx[..10]);
    let (st, c) = call_server(
        &app,
        "PUT",
        &format!("/v1/server/configs/{free}"),
        "defaultkey",
        Some(json!({"value": {"anything": [1, 2]}})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["version"], 1);
    let (st, _) = call_server(
        &app,
        "PUT",
        "/v1/server/configs/bad%20name",
        "defaultkey",
        Some(json!({"value": {}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    // the typed GET shows up in openapi
    let (_, doc) = call(&app, "GET", "/openapi.json", None, None).await;
    assert!(doc["paths"][format!("/v1/configs/{cfg}")]["get"].is_object());
    for name in [&cfg, &free] {
        let (st, d) = call_server(
            &app,
            "DELETE",
            &format!("/v1/server/configs/{name}"),
            "defaultkey",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(d["deleted"], true);
    }
    let (st, _) = call(&app, "GET", &format!("/v1/configs/{cfg}"), Some(t), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    call_server(&app, "DELETE", "/v1/admin/idl", "defaultkey", None).await;
}
