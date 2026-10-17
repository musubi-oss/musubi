//! Before/after hooks on every HTTP API call (Nakama's `RegisterBefore*/RegisterAfter*`, plan M1
//! "хуки — маппинг 1:1 по именам"). A runtime module exports
//! `before_<operation>` / `after_<operation>` where `<operation>` is the OpenAPI operationId
//! (`post_account_authenticate_email`, `put_storage_objects`, …); for the APIs Nakama has, the
//! Nakama name works too (`before_authenticate_email`, `after_write_storage_objects`).
//!
//! Before: payload `{method, path, params, query, body}`; the hook may reject the call (error →
//! 4xx/5xx by error kind) or return `{"body": …}` to replace the request body (anything else keeps
//! it). After: `{method, path, params, status, request, response}`, fire-and-forget. With no module
//! exporting the hook the request path is untouched (two map lookups).

use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use musubi_core::Error;
use musubi_runtime::CallContext;
use serde_json::{json, Map, Value};

use crate::app::AppState;
use crate::http::ApiError;

const MAX_HOOK_BODY: usize = 4 << 20;

/// Nakama hook names for the endpoints that have a Nakama counterpart: (method, route, name).
pub const NAKAMA_NAMES: &[(&str, &str, &str)] = &[
    ("POST", "/v1/auth/device", "authenticate_device"),
    ("POST", "/v1/auth/email", "authenticate_email"),
    ("POST", "/v1/auth/steam", "authenticate_steam"),
    ("POST", "/v1/auth/google", "authenticate_google"),
    ("POST", "/v1/auth/apple", "authenticate_apple"),
    ("POST", "/v1/auth/refresh", "session_refresh"),
    ("POST", "/v1/auth/logout", "session_logout"),
    ("GET", "/v1/account", "get_account"),
    ("PATCH", "/v1/account", "update_account"),
    ("POST", "/v1/account/delete", "delete_account"),
    ("POST", "/v1/account/link/device", "link_device"),
    ("POST", "/v1/account/link/email", "link_email"),
    ("POST", "/v1/account/link/steam", "link_steam"),
    ("POST", "/v1/account/link/google", "link_google"),
    ("POST", "/v1/account/link/apple", "link_apple"),
    ("DELETE", "/v1/account/unlink/device", "unlink_device"),
    ("DELETE", "/v1/account/unlink/email", "unlink_email"),
    ("GET", "/v1/friends", "list_friends"),
    ("POST", "/v1/friends", "add_friends"),
    ("DELETE", "/v1/friends", "delete_friends"),
    ("POST", "/v1/friends/block", "block_friends"),
    ("GET", "/v1/groups", "list_groups"),
    ("POST", "/v1/groups", "create_group"),
    ("DELETE", "/v1/groups/{id}", "delete_group"),
    ("PATCH", "/v1/groups/{id}", "update_group"),
    ("POST", "/v1/groups/{id}/add", "add_group_users"),
    ("POST", "/v1/groups/{id}/demote", "demote_group_users"),
    ("POST", "/v1/groups/{id}/join", "join_group"),
    ("POST", "/v1/groups/{id}/kick", "kick_group_users"),
    ("POST", "/v1/groups/{id}/leave", "leave_group"),
    ("GET", "/v1/groups/{id}/members", "list_group_users"),
    ("POST", "/v1/groups/{id}/promote", "promote_group_users"),
    (
        "GET",
        "/v1/leaderboards/{id}/records",
        "list_leaderboard_records",
    ),
    (
        "POST",
        "/v1/leaderboards/{id}/records",
        "write_leaderboard_record",
    ),
    (
        "DELETE",
        "/v1/leaderboards/{id}/records",
        "delete_leaderboard_record",
    ),
    ("GET", "/v1/notifications", "list_notifications"),
    ("DELETE", "/v1/notifications", "delete_notifications"),
    ("POST", "/v1/storage/read", "read_storage_objects"),
    ("POST", "/v1/storage/write", "write_storage_objects"),
    ("POST", "/v1/storage/delete", "delete_storage_objects"),
    ("GET", "/v1/storage/{collection}", "list_storage_objects"),
    ("GET", "/v1/matches", "list_matches"),
    ("GET", "/v1/tournaments", "list_tournaments"),
    ("POST", "/v1/tournaments/{id}/join", "join_tournament"),
    (
        "POST",
        "/v1/tournaments/{id}/records",
        "write_tournament_record",
    ),
    (
        "GET",
        "/v1/tournaments/{id}/records",
        "list_tournament_records",
    ),
    (
        "GET",
        "/v1/tournaments/{id}/around/{owner}",
        "list_tournament_records_around_owner",
    ),
    ("POST", "/v1/iap/apple", "validate_purchase_apple"),
    ("POST", "/v1/iap/google", "validate_purchase_google"),
];

/// `(before, after)` export names for a route, generic first, Nakama alias second when it has one.
pub fn names(method: &str, route: &str) -> Vec<(String, String)> {
    let op = crate::openapi::operation_id(method, route);
    let mut v = vec![(format!("before_{op}"), format!("after_{op}"))];
    if let Some((_, _, n)) = NAKAMA_NAMES
        .iter()
        .find(|(m, r, _)| *m == method && *r == route)
    {
        v.push((format!("before_{n}"), format!("after_{n}")));
    }
    v
}

fn path_params(route: &str, path: &str) -> Map<String, Value> {
    route
        .split('/')
        .zip(path.split('/'))
        .filter(|(t, _)| t.starts_with('{'))
        .map(|(t, v)| {
            (
                t.trim_matches(|c| c == '{' || c == '}').to_string(),
                Value::String(v.to_string()),
            )
        })
        .collect()
}

fn context(s: &AppState, req: &Request) -> CallContext {
    let mut ctx = CallContext {
        server: req.headers().contains_key("x-musubi-server-key"),
        ..Default::default()
    };
    if let Some(token) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|h| {
            h.strip_prefix("Bearer ")
                .or_else(|| h.strip_prefix("bearer "))
        })
    {
        if let Ok(a) = s.auth.verify_access(token) {
            ctx.account_id = Some(a.account_id);
            ctx.session_id = Some(a.session_id);
        }
    }
    ctx
}

fn as_json(bytes: &[u8]) -> Value {
    if bytes.is_empty() {
        return Value::Null;
    }
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

pub async fn before_after(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let Some(route) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
    else {
        return next.run(req).await;
    };
    if !route.starts_with("/v1/") || req.headers().contains_key(header::UPGRADE) {
        return next.run(req).await;
    }
    let method = req.method().as_str().to_string();
    let candidates = names(&method, &route);
    let before = candidates
        .iter()
        .map(|(b, _)| b)
        .find(|n| s.runtime.has_hook(n))
        .cloned();
    let after = candidates
        .iter()
        .map(|(_, a)| a)
        .find(|n| s.runtime.has_hook(n))
        .cloned();
    if before.is_none() && after.is_none() {
        return next.run(req).await;
    }

    let ctx = context(&s, &req);
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let params = path_params(&route, &path);
    let (parts, body) = req.into_parts();
    let mut body = match axum::body::to_bytes(body, MAX_HOOK_BODY).await {
        Ok(b) => b,
        Err(_) => {
            return ApiError(Error::InvalidArgument("request body too large".into()))
                .into_response()
        }
    };
    if let Some(name) = &before {
        let payload = json!({
            "method": method, "path": path, "params": params, "query": query,
            "body": as_json(&body),
        });
        match s
            .runtime
            .hook(
                name,
                ctx.clone(),
                serde_json::to_vec(&payload).unwrap_or_default(),
            )
            .await
        {
            Some(Err(e)) => return ApiError(e).into_response(),
            Some(Ok(out)) => {
                if let Some(b) = serde_json::from_slice::<Value>(&out)
                    .ok()
                    .and_then(|v| v.get("body").cloned())
                {
                    body = serde_json::to_vec(&b).unwrap_or_default().into();
                }
            }
            None => {}
        }
    }
    let request_json = after.as_ref().map(|_| as_json(&body));
    let req = Request::from_parts(parts, Body::from(body));
    let res = next.run(req).await;
    let Some(name) = after else { return res };

    let (parts, body) = res.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_HOOK_BODY).await {
        Ok(b) => b,
        Err(e) => return ApiError(Error::internal(e)).into_response(),
    };
    let payload = json!({
        "method": method, "path": path, "params": params, "status": parts.status.as_u16(),
        "request": request_json, "response": as_json(&bytes),
    });
    let rt = s.runtime.clone();
    tokio::spawn(async move {
        if let Some(Err(e)) = rt
            .hook(&name, ctx, serde_json::to_vec(&payload).unwrap_or_default())
            .await
        {
            tracing::warn!(hook = name, error = %e, "after hook failed");
        }
    });
    Response::from_parts(parts, Body::from(bytes))
}

#[cfg(test)]
mod tests {
    #[test]
    fn nakama_aliases_point_at_real_routes_and_params_parse() {
        for (m, r, _) in super::NAKAMA_NAMES {
            assert!(
                crate::openapi::ROUTES
                    .iter()
                    .any(|(rm, rr, _)| rm == m && rr == r),
                "{m} {r} is not a route"
            );
        }
        let p = super::path_params("/v1/groups/{id}/members/{user}", "/v1/groups/g1/members/u2");
        assert_eq!(p["id"], "g1");
        assert_eq!(p["user"], "u2");
        let n = super::names("POST", "/v1/rpc/{id}");
        assert_eq!(n[0].0, "before_post_rpc_id");
    }
}
