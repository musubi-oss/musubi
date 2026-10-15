//! OpenAPI 3.1 description of the HTTP API, served at `GET /openapi.json`.
//!
//! Level 1: every route with method, path parameters, tag and the security scheme implied by its
//! prefix (`/v1/server/*`, `/v1/admin/*` → `X-Musubi-Server-Key`; `/v1/auth/*`, health, metrics
//! and store notification endpoints → public; everything else → bearer access token). Bodies are
//! documented as free-form JSON; typed schemas come with the SDK generation work. A test keeps
//! this table in sync with the router sources.

use serde_json::{json, Map, Value};

/// (method, path, tag). Generated from the `.route(...)` declarations; keep sorted by path.
pub const ROUTES: &[(&str, &str, &str)] = &[
    ("GET", "/healthz", "system"),
    ("GET", "/metrics", "system"),
    ("GET", "/openapi.json", "system"),
    ("GET", "/readyz", "system"),
    ("DELETE", "/v1/account", "account"),
    ("GET", "/v1/account", "account"),
    ("PATCH", "/v1/account", "account"),
    ("POST", "/v1/account/link/device", "account"),
    ("POST", "/v1/account/link/email", "account"),
    ("GET", "/v1/account/me", "auth"),
    ("DELETE", "/v1/account/unlink/device", "account"),
    ("DELETE", "/v1/account/unlink/email", "account"),
    ("GET", "/v1/achievements", "economy"),
    ("GET", "/v1/admin/accounts", "admin"),
    ("DELETE", "/v1/admin/accounts/{id}", "admin"),
    ("GET", "/v1/admin/accounts/{id}", "admin"),
    ("POST", "/v1/admin/accounts/{id}/ban", "admin"),
    ("GET", "/v1/admin/accounts/{id}/export", "admin"),
    (
        "DELETE",
        "/v1/admin/accounts/{id}/identities/{provider}/{subject}",
        "admin",
    ),
    ("POST", "/v1/admin/accounts/{id}/kick", "admin"),
    ("GET", "/v1/admin/accounts/{id}/sessions", "admin"),
    ("POST", "/v1/admin/accounts/{id}/unban", "admin"),
    ("GET", "/v1/admin/audit", "admin"),
    ("GET", "/v1/admin/cluster/matches", "admin"),
    ("POST", "/v1/admin/cluster/nodes/{id}/drain", "admin"),
    ("GET", "/v1/admin/cluster/worlds", "admin"),
    ("GET", "/v1/admin/config", "admin"),
    ("GET", "/v1/admin/runtime/modules", "admin"),
    ("DELETE", "/v1/admin/runtime/modules/{name}", "admin"),
    ("PUT", "/v1/admin/runtime/modules/{name}", "admin"),
    ("GET", "/v1/admin/storage/{collection}", "admin"),
    ("DELETE", "/v1/admin/storage/{collection}/{key}", "admin"),
    ("GET", "/v1/admin/storage/{collection}/{key}", "admin"),
    ("PUT", "/v1/admin/storage/{collection}/{key}", "admin"),
    ("GET", "/v1/admin/webhooks", "webhooks"),
    ("POST", "/v1/admin/webhooks", "webhooks"),
    ("DELETE", "/v1/admin/webhooks/{id}", "webhooks"),
    ("PATCH", "/v1/admin/webhooks/{id}", "webhooks"),
    ("GET", "/v1/admin/webhooks/{id}/deliveries", "webhooks"),
    ("POST", "/v1/admin/webhooks/{id}/test", "webhooks"),
    ("POST", "/v1/auth/device", "auth"),
    ("POST", "/v1/auth/email", "auth"),
    ("POST", "/v1/auth/logout", "auth"),
    ("POST", "/v1/auth/refresh", "auth"),
    ("GET", "/v1/channel/{id}/messages", "chat"),
    ("GET", "/v1/energy", "economy"),
    ("POST", "/v1/energy/{id}/spend", "economy"),
    ("DELETE", "/v1/friends", "friends"),
    ("GET", "/v1/friends", "friends"),
    ("POST", "/v1/friends", "friends"),
    ("POST", "/v1/friends/block", "friends"),
    ("GET", "/v1/gameserver", "fleets"),
    ("POST", "/v1/gameserver/allocate", "fleets"),
    ("POST", "/v1/gameserver/connect", "fleets"),
    ("POST", "/v1/gameserver/health", "fleets"),
    ("POST", "/v1/gameserver/label", "fleets"),
    ("POST", "/v1/gameserver/ready", "fleets"),
    ("POST", "/v1/gameserver/shutdown", "fleets"),
    ("GET", "/v1/groups", "groups"),
    ("POST", "/v1/groups", "groups"),
    ("DELETE", "/v1/groups/{id}", "groups"),
    ("GET", "/v1/groups/{id}", "groups"),
    ("PATCH", "/v1/groups/{id}", "groups"),
    ("POST", "/v1/groups/{id}/add", "groups"),
    ("POST", "/v1/groups/{id}/demote", "groups"),
    ("POST", "/v1/groups/{id}/join", "groups"),
    ("POST", "/v1/groups/{id}/kick", "groups"),
    ("POST", "/v1/groups/{id}/leave", "groups"),
    ("GET", "/v1/groups/{id}/members", "groups"),
    ("POST", "/v1/groups/{id}/promote", "groups"),
    ("POST", "/v1/iap/apple", "economy"),
    ("POST", "/v1/iap/apple/notifications", "economy"),
    ("POST", "/v1/iap/google", "economy"),
    ("POST", "/v1/iap/google/notifications", "economy"),
    ("GET", "/v1/iap/receipts", "economy"),
    ("POST", "/v1/iap/steam/finalize", "economy"),
    ("POST", "/v1/iap/steam/init", "economy"),
    ("GET", "/v1/inventory", "economy"),
    ("POST", "/v1/inventory/consume", "economy"),
    ("GET", "/v1/leaderboards", "leaderboards"),
    ("GET", "/v1/leaderboards/{id}", "leaderboards"),
    (
        "GET",
        "/v1/leaderboards/{id}/around/{owner}",
        "leaderboards",
    ),
    ("DELETE", "/v1/leaderboards/{id}/records", "leaderboards"),
    ("GET", "/v1/leaderboards/{id}/records", "leaderboards"),
    ("POST", "/v1/leaderboards/{id}/records", "leaderboards"),
    ("GET", "/v1/liveops/flags", "integrations"),
    ("GET", "/v1/liveops/live-events", "integrations"),
    ("GET", "/v1/mail", "economy"),
    ("DELETE", "/v1/mail/{id}", "economy"),
    ("POST", "/v1/mail/{id}/claim", "economy"),
    ("POST", "/v1/mail/{id}/read", "economy"),
    ("GET", "/v1/market/listings", "economy"),
    ("POST", "/v1/market/listings", "economy"),
    ("GET", "/v1/market/listings/mine", "economy"),
    ("GET", "/v1/market/listings/{id}", "economy"),
    ("POST", "/v1/market/listings/{id}/bid", "economy"),
    ("POST", "/v1/market/listings/{id}/buy", "economy"),
    ("POST", "/v1/market/listings/{id}/cancel", "economy"),
    ("GET", "/v1/market/sales", "economy"),
    ("GET", "/v1/matches", "matches"),
    ("GET", "/v1/node", "system"),
    ("DELETE", "/v1/notifications", "notifications"),
    ("GET", "/v1/notifications", "notifications"),
    ("GET", "/v1/progression", "economy"),
    ("GET", "/v1/quests", "economy"),
    ("POST", "/v1/quests/{id}/claim", "economy"),
    ("GET", "/v1/rewards/rolls", "economy"),
    ("GET", "/v1/rewards/tables", "economy"),
    ("POST", "/v1/rewards/{table}/roll", "economy"),
    ("GET", "/v1/rpc", "rpc"),
    ("POST", "/v1/rpc/{id}", "rpc"),
    ("GET", "/v1/seasons/current", "economy"),
    ("POST", "/v1/seasons/{id}/claim", "economy"),
    ("GET", "/v1/server/achievements", "economy"),
    ("POST", "/v1/server/achievements", "economy"),
    ("POST", "/v1/server/achievements/progress", "economy"),
    ("GET", "/v1/server/cluster/directory/{key}", "cluster"),
    ("GET", "/v1/server/cluster/nodes", "cluster"),
    ("GET", "/v1/server/cluster/raft", "cluster"),
    ("GET", "/v1/server/currencies", "economy"),
    ("POST", "/v1/server/currencies", "economy"),
    ("GET", "/v1/server/energy", "economy"),
    ("POST", "/v1/server/energy", "economy"),
    ("POST", "/v1/server/energy/grant", "economy"),
    ("GET", "/v1/server/events", "webhooks"),
    ("GET", "/v1/server/fleets", "fleets"),
    ("POST", "/v1/server/fleets", "fleets"),
    ("DELETE", "/v1/server/fleets/{id}", "fleets"),
    ("GET", "/v1/server/fleets/{id}", "fleets"),
    ("POST", "/v1/server/fleets/{id}/allocate", "fleets"),
    ("POST", "/v1/server/fleets/{id}/scale", "fleets"),
    ("GET", "/v1/server/fleets/{id}/servers", "fleets"),
    ("DELETE", "/v1/server/gameservers/{id}", "fleets"),
    ("GET", "/v1/server/gameservers/{id}", "fleets"),
    ("GET", "/v1/server/iap/products", "economy"),
    ("POST", "/v1/server/iap/products", "economy"),
    ("GET", "/v1/server/iap/receipts", "economy"),
    ("POST", "/v1/server/iap/steam/reconcile", "economy"),
    ("POST", "/v1/server/inventory/grant", "economy"),
    ("GET", "/v1/server/items", "economy"),
    ("POST", "/v1/server/items", "economy"),
    ("DELETE", "/v1/server/leaderboards/{id}", "leaderboards"),
    ("POST", "/v1/server/leaderboards", "leaderboards"),
    (
        "DELETE",
        "/v1/server/leaderboards/{id}/records/{owner}",
        "leaderboards",
    ),
    (
        "POST",
        "/v1/server/leaderboards/{id}/records/{owner}",
        "leaderboards",
    ),
    ("POST", "/v1/server/mail", "economy"),
    ("POST", "/v1/server/market/settle", "economy"),
    ("POST", "/v1/server/matches", "matches"),
    ("DELETE", "/v1/server/matches/{id}", "matches"),
    ("GET", "/v1/server/matches/{id}", "matches"),
    ("GET", "/v1/server/progression/tracks", "economy"),
    ("POST", "/v1/server/progression/tracks", "economy"),
    ("POST", "/v1/server/progression/xp", "economy"),
    ("GET", "/v1/server/quests", "economy"),
    ("POST", "/v1/server/quests", "economy"),
    ("POST", "/v1/server/quests/event", "economy"),
    ("POST", "/v1/server/rewards/grant", "economy"),
    ("GET", "/v1/server/rewards/tables", "economy"),
    ("POST", "/v1/server/rewards/tables", "economy"),
    ("GET", "/v1/server/seasons", "economy"),
    ("POST", "/v1/server/seasons", "economy"),
    ("POST", "/v1/server/seasons/{id}/premium", "economy"),
    ("GET", "/v1/server/store", "economy"),
    ("POST", "/v1/server/store", "economy"),
    ("GET", "/v1/server/streaks", "economy"),
    ("POST", "/v1/server/streaks", "economy"),
    ("POST", "/v1/server/tournaments", "tournaments"),
    ("DELETE", "/v1/server/tournaments/{id}", "tournaments"),
    (
        "POST",
        "/v1/server/tournaments/{id}/records/{owner}",
        "tournaments",
    ),
    ("POST", "/v1/server/wallet/adjust", "economy"),
    ("POST", "/v1/server/webhooks/dispatch", "webhooks"),
    ("GET", "/v1/server/worlds", "worlds"),
    ("POST", "/v1/server/worlds", "worlds"),
    ("DELETE", "/v1/server/worlds/{w}", "worlds"),
    ("GET", "/v1/server/worlds/{w}", "worlds"),
    ("POST", "/v1/server/worlds/{w}/apply", "worlds"),
    ("POST", "/v1/server/worlds/{w}/despawn", "worlds"),
    ("GET", "/v1/server/worlds/{w}/events", "worlds"),
    ("POST", "/v1/server/worlds/{w}/handover/ack", "worlds"),
    ("POST", "/v1/server/worlds/{w}/lease", "worlds"),
    ("POST", "/v1/server/worlds/{w}/renew", "worlds"),
    ("DELETE", "/v1/server/worlds/{w}/sims", "worlds"),
    ("POST", "/v1/server/worlds/{w}/sims", "worlds"),
    ("POST", "/v1/server/worlds/{w}/spawn", "worlds"),
    ("GET", "/v1/server/worlds/{w}/view/{x}/{y}", "worlds"),
    ("POST", "/v1/storage/delete", "storage"),
    ("POST", "/v1/storage/read", "storage"),
    ("POST", "/v1/storage/write", "storage"),
    ("GET", "/v1/storage/{collection}", "storage"),
    ("GET", "/v1/store", "economy"),
    ("POST", "/v1/store/buy", "economy"),
    ("GET", "/v1/streaks", "economy"),
    ("POST", "/v1/streaks/{id}/claim", "economy"),
    ("GET", "/v1/tournaments", "tournaments"),
    ("GET", "/v1/tournaments/{id}", "tournaments"),
    ("GET", "/v1/tournaments/{id}/around/{owner}", "tournaments"),
    ("POST", "/v1/tournaments/{id}/join", "tournaments"),
    ("GET", "/v1/tournaments/{id}/records", "tournaments"),
    ("POST", "/v1/tournaments/{id}/records", "tournaments"),
    ("GET", "/v1/trades", "economy"),
    ("POST", "/v1/trades", "economy"),
    ("GET", "/v1/trades/{id}", "economy"),
    ("POST", "/v1/trades/{id}/accept", "economy"),
    ("POST", "/v1/trades/{id}/cancel", "economy"),
    ("POST", "/v1/trades/{id}/decline", "economy"),
    ("GET", "/v1/users", "account"),
    ("GET", "/v1/users/{id}/groups", "groups"),
    ("POST", "/v1/voice/token", "integrations"),
    ("GET", "/v1/wallet", "economy"),
    ("GET", "/v1/wallet/ledger", "economy"),
    ("GET", "/ws", "ws"),
];

/// (method, path, request schema, response schema); "" = free-form JSON. Level 2 coverage: auth,
/// account, storage, friends, groups, leaderboards.
pub const BODIES: &[(&str, &str, &str, &str)] = &[
    ("POST", "/v1/auth/device", "DeviceAuthRequest", "Session"),
    ("POST", "/v1/auth/email", "EmailAuthRequest", "Session"),
    ("POST", "/v1/auth/refresh", "RefreshRequest", "Session"),
    ("POST", "/v1/auth/logout", "LogoutRequest", ""),
    ("GET", "/v1/account/me", "", "Account"),
    ("GET", "/v1/account", "", "Account"),
    ("PATCH", "/v1/account", "", "Account"),
    ("POST", "/v1/storage/read", "StorageReadRequest", ""),
    ("POST", "/v1/storage/write", "StorageWriteRequest", ""),
    ("POST", "/v1/storage/delete", "StorageDeleteRequest", ""),
    ("GET", "/v1/storage/{collection}", "", "StorageList"),
    ("GET", "/v1/friends", "", "FriendList"),
    ("POST", "/v1/friends", "FriendIds", ""),
    ("DELETE", "/v1/friends", "FriendIds", ""),
    ("POST", "/v1/friends/block", "FriendIds", ""),
    ("GET", "/v1/groups", "", "GroupPage"),
    ("POST", "/v1/groups", "CreateGroupRequest", "Group"),
    ("GET", "/v1/groups/{id}", "", "Group"),
    ("PATCH", "/v1/groups/{id}", "", "Group"),
    ("GET", "/v1/leaderboards/{id}", "", "Leaderboard"),
    ("GET", "/v1/leaderboards/{id}/records", "", "RecordList"),
    (
        "POST",
        "/v1/leaderboards/{id}/records",
        "WriteRecordRequest",
        "Record",
    ),
    (
        "GET",
        "/v1/leaderboards/{id}/around/{owner}",
        "",
        "RecordList",
    ),
    ("POST", "/v1/server/leaderboards", "", "Leaderboard"),
    (
        "POST",
        "/v1/server/leaderboards/{id}/records/{owner}",
        "WriteRecordRequest",
        "Record",
    ),
];

fn ts() -> Value {
    json!({ "type": "string", "format": "date-time" })
}
fn uuid() -> Value {
    json!({ "type": "string", "format": "uuid" })
}
fn opt(v: Value) -> Value {
    json!({ "anyOf": [v, { "type": "null" }] })
}
fn obj(props: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}
fn r(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}
fn arr(item: Value) -> Value {
    json!({ "type": "array", "items": item })
}

/// Hand-maintained mirrors of the serde structs behind the core endpoints; a test checks real
/// responses against the `required` lists.
pub fn schemas() -> Map<String, Value> {
    let s = |t: &str| json!({ "type": t });
    let auth_opts = json!({
        "create": { "type": "boolean", "default": true, "description": "Create the account if it does not exist." },
        "username": opt(s("string")),
        "vars": { "type": "object", "additionalProperties": true }
    });
    let mut device = auth_opts.clone();
    device["id"] = s("string");
    let mut email = auth_opts;
    email["email"] = json!({ "type": "string", "format": "email" });
    email["password"] = s("string");
    let mut m = Map::new();
    m.insert("DeviceAuthRequest".into(), obj(device, &["id"]));
    m.insert(
        "EmailAuthRequest".into(),
        obj(email, &["email", "password"]),
    );
    m.insert(
        "RefreshRequest".into(),
        obj(json!({ "refresh_token": s("string") }), &["refresh_token"]),
    );
    m.insert(
        "LogoutRequest".into(),
        obj(
            json!({ "all_devices": { "type": "boolean", "default": false } }),
            &[],
        ),
    );
    m.insert(
        "Session".into(),
        obj(
            json!({ "account_id": uuid(), "session_id": uuid(), "access_token": s("string"), "refresh_token": s("string"), "created": s("boolean") }),
            &["account_id", "session_id", "access_token", "refresh_token", "created"],
        ),
    );
    m.insert(
        "Identity".into(),
        obj(
            json!({ "provider": s("string"), "subject": s("string"), "verified": s("boolean") }),
            &["provider", "subject", "verified"],
        ),
    );
    m.insert(
        "Account".into(),
        obj(
            json!({ "id": uuid(), "username": s("string"), "display_name": opt(s("string")), "avatar_url": opt(s("string")), "lang": s("string"), "timezone": opt(s("string")), "location": opt(s("string")), "metadata": s("object"), "created_at": ts(), "updated_at": ts(), "identities": arr(r("Identity")) }),
            &["id", "username", "lang", "metadata", "created_at", "updated_at", "identities"],
        ),
    );
    m.insert(
        "User".into(),
        obj(
            json!({ "id": uuid(), "username": s("string"), "display_name": opt(s("string")), "avatar_url": opt(s("string")), "lang": s("string"), "location": opt(s("string")), "metadata": s("object"), "created_at": ts() }),
            &["id", "username", "lang", "metadata", "created_at"],
        ),
    );
    let perm = json!({ "type": "integer", "minimum": 0, "maximum": 2, "description": "0 = owner only, 1 = public read, 2 = (write) owner + server" });
    m.insert(
        "StorageObject".into(),
        obj(
            json!({ "collection": s("string"), "key": s("string"), "owner_id": uuid(), "value": {}, "version": s("string"), "read_permission": perm, "write_permission": perm, "created_at": ts(), "updated_at": ts() }),
            &["collection", "key", "owner_id", "value", "version", "read_permission", "write_permission", "created_at", "updated_at"],
        ),
    );
    m.insert(
        "StorageReadRequest".into(),
        obj(json!({ "objects": arr(obj(json!({ "collection": s("string"), "key": s("string"), "owner_id": opt(uuid()) }), &["collection", "key"])) }), &["objects"]),
    );
    m.insert(
        "StorageWriteRequest".into(),
        obj(json!({ "objects": arr(obj(json!({ "collection": s("string"), "key": s("string"), "value": {}, "version": opt(s("string")), "read_permission": perm, "write_permission": perm }), &["collection", "key", "value"])) }), &["objects"]),
    );
    m.insert(
        "StorageDeleteRequest".into(),
        obj(json!({ "objects": arr(obj(json!({ "collection": s("string"), "key": s("string"), "version": opt(s("string")) }), &["collection", "key"])) }), &["objects"]),
    );
    m.insert(
        "StorageList".into(),
        obj(
            json!({ "objects": arr(r("StorageObject")), "cursor": opt(s("string")) }),
            &["objects"],
        ),
    );
    m.insert(
        "Friend".into(),
        obj(
            json!({ "user": r("User"), "state": { "type": "integer", "enum": [0, 1, 2, 3], "description": "0 friend, 1 invite sent, 2 invite received, 3 blocked" }, "update_time": ts() }),
            &["user", "state", "update_time"],
        ),
    );
    m.insert(
        "FriendList".into(),
        obj(
            json!({ "friends": arr(r("Friend")), "cursor": opt(s("string")) }),
            &["friends"],
        ),
    );
    m.insert(
        "FriendIds".into(),
        obj(
            json!({ "ids": arr(uuid()), "usernames": arr(s("string")) }),
            &[],
        ),
    );
    m.insert(
        "Group".into(),
        obj(
            json!({ "id": uuid(), "creator_id": opt(uuid()), "name": s("string"), "description": s("string"), "avatar_url": opt(s("string")), "lang": s("string"), "metadata": s("object"), "open": s("boolean"), "edge_count": s("integer"), "max_count": s("integer"), "create_time": ts(), "update_time": ts() }),
            &["id", "name", "description", "lang", "metadata", "open", "edge_count", "max_count", "create_time", "update_time"],
        ),
    );
    m.insert(
        "CreateGroupRequest".into(),
        obj(json!({ "name": s("string"), "description": s("string"), "avatar_url": opt(s("string")), "lang": opt(s("string")), "metadata": opt(s("object")), "open": { "type": "boolean", "default": true } }), &["name"]),
    );
    m.insert(
        "GroupPage".into(),
        obj(
            json!({ "items": arr(r("Group")), "cursor": opt(s("string")) }),
            &["items"],
        ),
    );
    let operator = json!({ "type": "string", "enum": ["best", "set", "incr", "decr"] });
    m.insert(
        "Leaderboard".into(),
        obj(
            json!({ "id": s("string"), "authoritative": s("boolean"), "sort_order": { "type": "string", "enum": ["asc", "desc"] }, "operator": operator, "reset_schedule": opt(s("string")), "metadata": s("object"), "create_time": ts(), "next_reset": s("integer"), "prev_reset": s("integer") }),
            &["id", "authoritative", "sort_order", "operator", "metadata", "create_time", "next_reset", "prev_reset"],
        ),
    );
    m.insert(
        "Record".into(),
        obj(
            json!({ "leaderboard_id": s("string"), "owner_id": uuid(), "username": opt(s("string")), "score": s("integer"), "subscore": s("integer"), "num_score": s("integer"), "metadata": s("object"), "create_time": ts(), "update_time": ts(), "expiry_time": s("integer"), "rank": s("integer") }),
            &["leaderboard_id", "owner_id", "score", "subscore", "num_score", "metadata", "create_time", "update_time", "expiry_time", "rank"],
        ),
    );
    m.insert(
        "RecordList".into(),
        obj(json!({ "records": arr(r("Record")), "owner_records": arr(r("Record")), "next_cursor": opt(s("string")), "prev_cursor": opt(s("string")) }), &["records", "owner_records"]),
    );
    m.insert(
        "WriteRecordRequest".into(),
        obj(json!({ "score": s("integer"), "subscore": { "type": "integer", "default": 0 }, "metadata": opt(s("object")), "operator": opt(operator) }), &["score"]),
    );
    m
}

pub fn security_for(path: &str) -> Option<&'static str> {
    if path.starts_with("/v1/server/") || path.starts_with("/v1/admin/") {
        Some("serverKey")
    } else if path.starts_with("/v1/auth/")
        || path == "/healthz"
        || path == "/readyz"
        || path == "/metrics"
        || path == "/openapi.json"
        || path == "/v1/node"
        || path.ends_with("/notifications") && path.starts_with("/v1/iap/")
    {
        None
    } else {
        Some("bearer")
    }
}

fn operation_id(method: &str, path: &str) -> String {
    let mut id = method.to_ascii_lowercase();
    for seg in path.trim_start_matches('/').split('/') {
        if seg == "v1" {
            continue;
        }
        let seg = seg.trim_matches(|c| c == '{' || c == '}').replace('-', "_");
        id.push('_');
        id.push_str(&seg);
    }
    id
}

pub fn document(version: &str) -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for (method, path, tag) in ROUTES {
        let params: Vec<Value> = path
            .split('/')
            .filter(|s| s.starts_with('{'))
            .map(|s| {
                json!({ "name": s.trim_matches(|c| c == '{' || c == '}'), "in": "path", "required": true, "schema": { "type": "string" } })
            })
            .collect();
        let mut op = json!({
            "operationId": operation_id(method, path),
            "tags": [tag],
            "parameters": params,
            "responses": {
                "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } },
                "400": { "$ref": "#/components/responses/Error" },
                "401": { "$ref": "#/components/responses/Error" },
                "403": { "$ref": "#/components/responses/Error" },
                "404": { "$ref": "#/components/responses/Error" },
                "429": { "$ref": "#/components/responses/Error" }
            }
        });
        if matches!(*method, "POST" | "PUT" | "PATCH") {
            let content = if *path == "/v1/admin/runtime/modules/{name}" {
                json!({ "application/wasm": { "schema": { "type": "string", "format": "binary" } } })
            } else {
                json!({ "application/json": { "schema": { "type": "object" } } })
            };
            op["requestBody"] = json!({ "content": content });
        }
        if let Some((_, _, req, resp)) = BODIES.iter().find(|(m, p, _, _)| m == method && p == path)
        {
            if !req.is_empty() {
                op["requestBody"]["content"]["application/json"]["schema"] = r(req);
            }
            if !resp.is_empty() {
                op["responses"]["200"]["content"]["application/json"]["schema"] = r(resp);
            }
        }
        op["security"] = match security_for(path) {
            Some(s) => json!([{ s: [] }]),
            None => json!([]),
        };
        let entry = paths.entry(path.to_string()).or_insert_with(|| json!({}));
        entry[method.to_ascii_lowercase()] = op;
    }
    json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Musubi",
            "version": version,
            "description": "Game backend API: auth, accounts, storage, social, chat, leaderboards, matchmaker, economy, worlds, fleets, admin.",
            "license": { "name": "Apache-2.0" }
        },
        "servers": [{ "url": "/" }],
        "paths": paths,
        "components": {
            "schemas": schemas(),
            "securitySchemes": {
                "bearer": { "type": "http", "scheme": "bearer", "bearerFormat": "JWT" },
                "serverKey": { "type": "apiKey", "in": "header", "name": "X-Musubi-Server-Key" }
            },
            "responses": {
                "Error": {
                    "description": "Error",
                    "content": { "application/json": { "schema": {
                        "type": "object",
                        "properties": { "code": { "type": "integer" }, "message": { "type": "string" } },
                        "required": ["code", "message"]
                    } } }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `.route("...", ...)` in the HTTP sources must be described, and vice versa.
    #[test]
    fn route_table_matches_router_sources() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("http"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "rs"))
            .collect();
        files.push(root.join("events.rs"));
        let mut found = std::collections::BTreeSet::new();
        for f in files {
            let src = std::fs::read_to_string(&f).unwrap();
            let mut rest = src.as_str();
            while let Some(i) = rest.find(".route(") {
                rest = &rest[i + 7..];
                let Some(q1) = rest.find('"') else { break };
                let Some(q2) = rest[q1 + 1..].find('"') else {
                    break;
                };
                let path = &rest[q1 + 1..q1 + 1 + q2];
                // the handler chain ends where the `.route(` call closes
                let after = &rest[q1 + 1 + q2..];
                let mut depth = 1i32;
                let mut chain_end = after.len();
                for (i, c) in after.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                chain_end = i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let chain = &after[..chain_end];
                for m in ["get", "post", "put", "patch", "delete"] {
                    let pat = format!("{m}(");
                    let hit = chain.match_indices(&pat).any(|(i, _)| {
                        i == 0
                            || !chain.as_bytes()[i - 1].is_ascii_alphanumeric()
                                && chain.as_bytes()[i - 1] != b'_'
                    });
                    if hit {
                        found.insert((m.to_ascii_uppercase(), path.to_string()));
                    }
                }
            }
        }
        let table: std::collections::BTreeSet<(String, String)> = ROUTES
            .iter()
            .map(|(m, p, _)| (m.to_string(), p.to_string()))
            .collect();
        let missing: Vec<_> = found.difference(&table).collect();
        let stale: Vec<_> = table.difference(&found).collect();
        assert!(
            missing.is_empty(),
            "routes not in openapi::ROUTES: {missing:?}"
        );
        assert!(
            stale.is_empty(),
            "openapi::ROUTES entries without a route: {stale:?}"
        );
        let doc = document("test");
        assert_eq!(
            doc["paths"].as_object().unwrap().len(),
            table
                .iter()
                .map(|(_, p)| p)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
        );
        assert_eq!(
            doc["paths"]["/v1/account"]["get"]["security"][0]["bearer"],
            json!([])
        );
        assert_eq!(
            doc["paths"]["/v1/server/events"]["get"]["security"][0]["serverKey"],
            json!([])
        );
        assert_eq!(
            doc["paths"]["/v1/auth/device"]["post"]["security"],
            json!([])
        );
        assert_eq!(
            doc["paths"]["/v1/auth/device"]["post"]["requestBody"]["content"]["application/json"]
                ["schema"]["$ref"],
            "#/components/schemas/DeviceAuthRequest"
        );
    }

    /// Every typed body refers to a real route and an existing schema; every `$ref` resolves.
    #[test]
    fn typed_bodies_refer_to_routes_and_schemas() {
        let schemas = schemas();
        for (m, p, req, resp) in BODIES {
            assert!(
                ROUTES.iter().any(|(rm, rp, _)| rm == m && rp == p),
                "{m} {p} is not a route"
            );
            for name in [req, resp].into_iter().filter(|n| !n.is_empty()) {
                assert!(
                    schemas.contains_key(*name),
                    "schema {name} missing for {m} {p}"
                );
            }
        }
        fn refs(v: &Value, out: &mut Vec<String>) {
            match v {
                Value::Object(m) => {
                    if let Some(Value::String(r)) = m.get("$ref") {
                        if let Some(n) = r.strip_prefix("#/components/schemas/") {
                            out.push(n.to_string());
                        }
                    }
                    m.values().for_each(|v| refs(v, out));
                }
                Value::Array(a) => a.iter().for_each(|v| refs(v, out)),
                _ => {}
            }
        }
        let mut all = Vec::new();
        refs(&Value::Object(schemas.clone()), &mut all);
        for n in all {
            assert!(schemas.contains_key(&n), "dangling $ref {n}");
        }
    }
}
