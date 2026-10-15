//! HTTP integration tests. Run with `MUSUBI_TEST_DATABASE_URL=postgres://...`; skipped otherwise.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use musubi_auth::AuthService;
use musubi_core::config::Config;
use musubi_server::app::AppState;
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
    cfg.market.anti_snipe_secs = 0;
    tweak(&mut cfg);
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let auth = AuthService::new(db.clone(), &cfg.session).unwrap();
    let storage = StorageService::new(db.clone());
    let hub = Arc::new(musubi_server::realtime::Hub::new());
    let runtime = test_runtime();
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
    let state = AppState {
        cfg: Arc::new(cfg),
        auth,
        storage,
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        notify: musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            fanout.clone(),
        ),
        chat: musubi_chat::ChatService::new(db.clone()),
        leaderboards: musubi_leaderboard::LeaderboardService::new(db.clone()),
        tournaments: musubi_leaderboard::TournamentService::new(db.clone()),
        economy: musubi_economy::EconomyService::new(db.clone()),
        matchmaker,
        parties,
        relayed,
        matches: test_matches(&db, &runtime),
        worlds: worlds.clone(),
        liveops: None,
        voice: None,
        apple_iap: None,
        google_iap: None,
        steam_iap: None,
        runtime: runtime.clone(),
        cluster: cluster.clone(),
        fleets: fleets.clone(),
        fanout,
        hub,
        events: Arc::new(musubi_server::events::Events::new(db.clone())),
        banned: Arc::new(musubi_server::bans::Bans::default()),
        db,
        started_at: std::time::Instant::now(),
        metrics: musubi_server::metrics::install(),
        limits: Arc::new(musubi_server::limits::RateLimiter::new(
            &musubi_core::config::LimitsConfig {
                enabled: false,
                ..Default::default()
            },
        )),
    };
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
    assert_eq!(r["completed"], json!([once]));
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
    assert_eq!(r["completed"], json!([daily]));
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
    wa.create(&world, Default::default(), Default::default())
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
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
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
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
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
    // not due again yet → a second dispatch does nothing for it
    let (_, v) = read(
        app.clone()
            .oneshot(post("/v1/server/webhooks/dispatch".into(), json!({})))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(v["failed"], 0, "{v}");

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
