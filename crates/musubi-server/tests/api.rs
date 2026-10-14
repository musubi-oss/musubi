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
    let url = std::env::var("MUSUBI_TEST_DATABASE_URL").ok()?;
    let mut cfg = Config::default();
    cfg.database.url = url;
    let db = musubi_server::db::connect(&cfg.database).await.unwrap();
    migrate_once(&db).await;
    let auth = AuthService::new(db.clone(), &cfg.session).unwrap();
    let storage = StorageService::new(db.clone());
    let hub = Arc::new(musubi_server::realtime::Hub::new());
    Some(musubi_server::http::router(AppState {
        cfg: Arc::new(cfg),
        auth,
        storage,
        accounts: musubi_account::AccountService::new(db.clone()),
        social: musubi_social::SocialService::new(db.clone()),
        groups: musubi_social::GroupService::new(db.clone()),
        notify: musubi_server::notify::Notifier::new(
            musubi_social::NotificationService::new(db.clone()),
            hub.clone(),
        ),
        chat: musubi_chat::ChatService::new(db.clone()),
        leaderboards: musubi_leaderboard::LeaderboardService::new(db.clone()),
        tournaments: musubi_leaderboard::TournamentService::new(db.clone()),
        matchmaker: Arc::new(musubi_matchmaker::Matchmaker::default()),
        parties: Arc::new(musubi_server::realtime::PartyRegistry::new()),
        relayed: Arc::new(musubi_server::realtime::RelayedRegistry::new()),
        runtime: test_runtime(),
        hub,
        db,
        started_at: std::time::Instant::now(),
    }))
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
