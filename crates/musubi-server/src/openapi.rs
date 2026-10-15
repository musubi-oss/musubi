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
    }
}
