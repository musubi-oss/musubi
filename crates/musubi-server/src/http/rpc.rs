use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_runtime::CallContext;
use serde_json::json;

use crate::app::AppState;
use crate::http::ApiError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/rpc/{id}", post(call))
        .route("/v1/rpc", get(list))
}

/// Server key or bearer token; the module sees which one.
async fn context(s: &AppState, headers: &HeaderMap) -> Result<CallContext, ApiError> {
    if let Some(k) = headers
        .get("x-musubi-server-key")
        .and_then(|v| v.to_str().ok())
    {
        let expected = s.cfg.http.server_key.as_bytes();
        let ok = k.len() == expected.len()
            && k.bytes().zip(expected).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
        if !ok {
            return Err(musubi_core::Error::Unauthenticated("bad server key".into()).into());
        }
        return Ok(CallContext {
            server: true,
            ..Default::default()
        });
    }
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or_else(|| {
            musubi_core::Error::Unauthenticated("missing bearer token or server key".into())
        })?;
    let me = s.auth.verify_access(token)?;
    let username = s
        .accounts
        .get_users(&[me.account_id], &[])
        .await?
        .pop()
        .map(|u| u.username);
    Ok(CallContext {
        account_id: Some(me.account_id),
        session_id: Some(me.session_id),
        username,
        server: false,
        vars: me.vars,
    })
}

async fn call(
    State(s): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let ctx = context(&s, &headers).await?;
    if let Some(acc) = ctx.account_id {
        s.limits.rpc(acc)?;
    }
    let out = s.runtime.call(&id, ctx, body.to_vec()).await?;
    let ct = headers
        .get(CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| "application/json".parse().unwrap());
    Ok((StatusCode::OK, [(CONTENT_TYPE, ct)], out).into_response())
}

async fn list(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, ApiError> {
    context(&s, &headers).await?;
    Ok(Json(
        json!({ "rpcs": s.runtime.rpc_ids(), "modules": s.runtime.module_names() }),
    ))
}
