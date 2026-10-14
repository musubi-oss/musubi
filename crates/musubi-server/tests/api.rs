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
    musubi_server::db::migrate(&db).await.unwrap();
    let auth = AuthService::new(db.clone(), &cfg.session).unwrap();
    let storage = StorageService::new(db.clone());
    Some(musubi_server::http::router(AppState {
        cfg: Arc::new(cfg),
        auth,
        storage,
        accounts: musubi_account::AccountService::new(db.clone()),
        hub: std::sync::Arc::new(musubi_server::realtime::Hub::new()),
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
