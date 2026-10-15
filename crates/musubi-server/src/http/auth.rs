use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_auth::{AuthOptions, Session};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/device", post(device))
        .route("/v1/auth/email", post(email))
        .route("/v1/auth/refresh", post(refresh))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/account/me", get(me))
}

#[derive(Deserialize)]
struct DeviceReq {
    id: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

#[derive(Deserialize)]
struct EmailReq {
    email: String,
    password: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

#[derive(Deserialize)]
struct RefreshReq {
    refresh_token: String,
}

#[derive(Deserialize, Default)]
struct LogoutReq {
    #[serde(default)]
    all_devices: bool,
}

async fn device(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<DeviceReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let sess = s.auth.authenticate_device(&r.id, r.opts).await?;
    authenticated(&s, &sess, musubi_auth::provider::DEVICE);
    Ok(Json(sess))
}

/// Fire the `authenticated` runtime event without delaying the response.
fn authenticated(s: &AppState, sess: &Session, provider: &'static str) {
    if let Some(l) = &s.liveops {
        l.track(
            &sess.account_id.to_string(),
            "musubi.authenticated",
            serde_json::json!({ "provider": provider, "created": sess.created }),
        );
    }
    if !s.runtime.has_hook("hook_authenticated") {
        return;
    }
    let rt = s.runtime.clone();
    let ctx = musubi_runtime::CallContext {
        account_id: Some(sess.account_id),
        session_id: Some(sess.session_id),
        ..Default::default()
    };
    let payload = serde_json::json!({ "provider": provider, "created": sess.created });
    tokio::spawn(async move { rt.event("authenticated", ctx, payload).await });
}

async fn email(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<EmailReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let sess = s
        .auth
        .authenticate_email(&r.email, &r.password, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::EMAIL);
    Ok(Json(sess))
}

async fn refresh(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<RefreshReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    Ok(Json(s.auth.refresh(&r.refresh_token).await?))
}

async fn logout(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    body: Option<Json<LogoutReq>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let all = body.map(|b| b.all_devices).unwrap_or(false);
    s.auth.logout(me.session_id, all).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn me(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let row = sqlx::query_as::<_, (String, Option<String>, serde_json::Value, time::OffsetDateTime)>(
        "select username, display_name, metadata, created_at from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?
    .ok_or_else(|| musubi_core::Error::NotFound("account".into()))?;
    Ok(Json(json!({
        "id": me.account_id,
        "username": row.0,
        "display_name": row.1,
        "metadata": row.2,
        "created_at": row.3.unix_timestamp(),
        "session_id": me.session_id,
        "vars": me.vars,
    })))
}
