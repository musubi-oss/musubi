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
        .route("/v1/auth/mfa", post(mfa))
        .route("/v1/auth/refresh", post(refresh))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/steam", post(steam))
        .route("/v1/auth/google", post(google))
        .route("/v1/auth/apple", post(apple))
        .route("/v1/auth/oidc/{provider}", post(oidc))
        .route("/v1/auth/email/verify/request", post(verify_request))
        .route("/v1/auth/email/verify", post(verify))
        .route("/v1/auth/email/reset/request", post(reset_request))
        .route("/v1/auth/email/reset", post(reset))
        .route("/v1/account/me", get(me))
        .route("/v1/account/sessions", get(sessions))
        .route(
            "/v1/account/sessions/{id}",
            axum::routing::delete(revoke_session),
        )
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
fn authenticated(s: &AppState, sess: &Session, provider: &str) {
    let provider = provider.to_string();
    {
        let (ev, acc, created) = (s.events.clone(), sess.account_id, sess.created);
        let provider = provider.clone();
        tokio::spawn(async move {
            ev.emit(
                "account.authenticated",
                Some(acc),
                serde_json::json!({ "provider": provider, "created": created }),
            )
            .await;
        });
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
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let login = match s
        .auth
        .authenticate_email(&r.email, &r.password, r.opts)
        .await
    {
        Ok(sess) => sess,
        Err(e @ musubi_core::Error::ResourceExhausted(_)) => {
            if let Ok(Some(lock)) = s.auth.login_lock(&r.email).await {
                let account = s.auth.account_by_email(&lock.email).await.ok().flatten();
                s.events
                    .emit(
                        "account.login_locked",
                        account,
                        json!({ "account_id": account, "email": lock.email, "failures": lock.failures, "locked_until": lock.locked_until, "ip": ip.0 }),
                    )
                    .await;
            }
            return Err(e.into());
        }
        Err(e) => return Err(e.into()),
    };
    match login {
        musubi_auth::Login::Session(sess) => {
            authenticated(&s, &sess, musubi_auth::provider::EMAIL);
            Ok(Json(
                serde_json::to_value(sess).map_err(musubi_core::Error::internal)?,
            ))
        }
        musubi_auth::Login::MfaRequired {
            account_id,
            mfa_token,
        } => Ok(Json(json!({
            "mfa_required": true,
            "account_id": account_id,
            "mfa_token": mfa_token,
            "expires_in": musubi_auth::MFA_TOKEN_SECS,
        }))),
    }
}

#[derive(Deserialize)]
struct MfaReq {
    mfa_token: String,
    code: String,
}

/// `POST /v1/auth/mfa` — second step of a password login on an account with an authenticator.
async fn mfa(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<MfaReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let sess = s.auth.mfa_complete(&r.mfa_token, &r.code).await?;
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

const VERIFY_TTL_SECS: i64 = 24 * 3600;
const RESET_TTL_SECS: i64 = 3600;

/// Mint a one-shot token: the raw value goes to the outbox event, only its hash is stored.
async fn mint_token(
    s: &AppState,
    account: musubi_core::AccountId,
    kind: &str,
    email: &str,
    ttl_secs: i64,
) -> Result<String, ApiError> {
    let raw = crate::keys::hash(&format!(
        "{}{}{:?}",
        uuid::Uuid::now_v7().simple(),
        uuid::Uuid::now_v7().simple(),
        std::time::SystemTime::now()
    ));
    sqlx::query(
        "insert into musubi.email_tokens (token_hash, account_id, kind, email, expires_at)
         values ($1, $2, $3, $4, now() + make_interval(secs => $5))",
    )
    .bind(crate::keys::hash(&raw))
    .bind(account.0)
    .bind(kind)
    .bind(email)
    .bind(ttl_secs as f64)
    .execute(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    Ok(raw)
}

/// Consume a token of the given kind; None when unknown, used or expired.
async fn consume_token(
    s: &AppState,
    raw: &str,
    kind: &str,
) -> Result<Option<(musubi_core::AccountId, String)>, ApiError> {
    let row: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "update musubi.email_tokens set used_at = now()
         where token_hash = $1 and kind = $2 and used_at is null and expires_at > now()
         returning account_id, email",
    )
    .bind(crate::keys::hash(raw))
    .bind(kind)
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    Ok(row.map(|(a, e)| (musubi_core::AccountId(a), e)))
}

/// Start email verification for the caller: emits `account.email_verification` with the token so
/// the game's mailer (webhook) can send the link. Already-verified addresses are a no-op.
async fn verify_request(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some((email, verified)) = s.auth.email_of(me.account_id).await? else {
        return Err(musubi_core::Error::NotFound("no email identity".into()).into());
    };
    if verified {
        return Ok(Json(json!({ "ok": true, "verified": true })));
    }
    let token = mint_token(&s, me.account_id, "verify", &email, VERIFY_TTL_SECS).await?;
    s.events
        .emit(
            "account.email_verification",
            Some(me.account_id),
            json!({ "account_id": me.account_id, "email": email, "token": token, "expires_in_secs": VERIFY_TTL_SECS }),
        )
        .await;
    Ok(Json(json!({ "ok": true, "verified": false })))
}

#[derive(Deserialize)]
struct TokenReq {
    token: String,
}

async fn verify(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<TokenReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let Some((account, email)) = consume_token(&s, &r.token, "verify").await? else {
        return Err(musubi_core::Error::NotFound("token unknown, used or expired".into()).into());
    };
    s.auth.mark_email_verified(account, &email).await?;
    s.events
        .emit(
            "account.email_verified",
            Some(account),
            json!({ "account_id": account, "email": email }),
        )
        .await;
    Ok(Json(json!({ "ok": true, "account_id": account })))
}

#[derive(Deserialize)]
struct ResetReq {
    email: String,
}

/// Always 200 (no account enumeration); emits `account.password_reset` when the email exists.
async fn reset_request(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<ResetReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    if let Some(account) = s.auth.account_by_email(&r.email).await? {
        let email = r.email.trim().to_lowercase();
        let token = mint_token(&s, account, "reset", &email, RESET_TTL_SECS).await?;
        s.events
            .emit(
                "account.password_reset",
                Some(account),
                json!({ "account_id": account, "email": email, "token": token, "expires_in_secs": RESET_TTL_SECS }),
            )
            .await;
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ResetConfirmReq {
    token: String,
    new_password: String,
}

/// Set the new password and revoke all refresh sessions of the account.
async fn reset(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<ResetConfirmReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let Some((account, email)) = consume_token(&s, &r.token, "reset").await? else {
        return Err(musubi_core::Error::NotFound("token unknown, used or expired".into()).into());
    };
    s.auth
        .set_email_password(account, &email, &r.new_password)
        .await?;
    s.events
        .emit(
            "account.password_changed",
            Some(account),
            json!({ "account_id": account, "email": email }),
        )
        .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct SteamReq {
    ticket: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

#[derive(Deserialize)]
struct IdTokenReq {
    id_token: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

async fn steam(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<SteamReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let steam_id = s.logins.steam(&r.ticket).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::STEAM, &steam_id, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::STEAM);
    Ok(Json(sess))
}

async fn google(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<IdTokenReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let claims = s.logins.google(&r.id_token).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::GOOGLE, &claims.sub, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::GOOGLE);
    Ok(Json(sess))
}

async fn apple(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<IdTokenReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let claims = s.logins.apple(&r.id_token).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::APPLE, &claims.sub, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::APPLE);
    Ok(Json(sess))
}

/// `POST /v1/auth/oidc/{provider}` — ID token from any issuer configured under `[social.oidc]`.
async fn oidc(
    State(s): State<AppState>,
    ip: super::ClientIp,
    axum::extract::Path(provider): axum::extract::Path<String>,
    Json(r): Json<IdTokenReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    if !crate::logins::valid_oidc_name(&provider) {
        return Err(musubi_core::Error::InvalidArgument("bad provider name".into()).into());
    }
    let claims = s.logins.oidc(&provider, &r.id_token).await?;
    let p = crate::logins::oidc_provider(&provider);
    let sess = s
        .auth
        .authenticate_external(&p, &claims.sub, r.opts)
        .await?;
    authenticated(&s, &sess, &p);
    Ok(Json(sess))
}

/// `GET /v1/account/sessions` — live logins (one per refresh family) with `current` marking the caller's.
async fn sessions(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let list = s.auth.sessions(me.account_id).await?;
    let mine =
        sqlx::query_scalar::<_, uuid::Uuid>("select family from musubi.sessions where id = $1")
            .bind(me.session_id.0)
            .fetch_optional(&s.db)
            .await
            .map_err(musubi_core::Error::internal)?;
    let sessions: Vec<serde_json::Value> = list
        .into_iter()
        .map(|si| {
            let mut v = serde_json::to_value(&si).unwrap_or_default();
            v["current"] = json!(mine == Some(si.family));
            v
        })
        .collect();
    Ok(Json(json!({ "sessions": sessions })))
}

/// `DELETE /v1/account/sessions/{id}` — sign that device out (its refresh family is revoked; the
/// short-lived access token it holds expires on its own).
async fn revoke_session(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(id): axum::extract::Path<musubi_core::SessionId>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !s.auth.revoke_family(me.account_id, id).await? {
        return Err(musubi_core::Error::NotFound("session".into()).into());
    }
    Ok(Json(json!({ "ok": true })))
}
