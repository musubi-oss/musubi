use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_auth::{AuthOptions, Session};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::ServerKey;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/device", post(device))
        .route("/v1/auth/email", post(email))
        .route("/v1/auth/mfa", post(mfa))
        .route("/v1/auth/mfa/email/send", post(mfa_email_send))
        .route("/v1/auth/refresh", post(refresh))
        .route("/v1/auth/token/scope", post(token_scope))
        .route("/v1/server/accounts/{id}/token", post(server_token))
        .route("/v1/auth/queue", post(queue))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/auth/steam", post(steam))
        .route("/v1/auth/xbox", post(xbox))
        .route("/v1/auth/psn", post(psn))
        .route("/v1/auth/nintendo", post(nintendo))
        .route("/v1/auth/google", post(google))
        .route("/v1/auth/apple", post(apple))
        .route("/v1/auth/oidc/{provider}", post(oidc))
        .route("/v1/auth/facebook", post(facebook))
        .route("/v1/auth/gamecenter", post(gamecenter))
        .route("/v1/auth/email/verify/request", post(verify_request))
        .route("/v1/auth/email/verify", post(verify))
        .route("/v1/auth/email/reset/request", post(reset_request))
        .route("/v1/auth/email/reset", post(reset))
        .route("/v1/auth/email/magic/request", post(magic_request))
        .route("/v1/auth/email/magic", post(magic))
        .route("/v1/account/parental/request", post(parental_request))
        .route("/v1/auth/parental/consent", post(parental_consent))
        .route("/v1/server/auth/custom", post(custom))
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
    s.ban_rules.check_device(&r.id)?;
    let sess = s.auth.authenticate_device(&r.id, r.opts).await?;
    authenticated(&s, &sess, musubi_auth::provider::DEVICE, &ip.0).await?;
    Ok(Json(sess))
}

/// Fire the `authenticated` runtime event without delaying the response.
pub(super) async fn authenticated(
    s: &AppState,
    sess: &Session,
    provider: &str,
    ip: &str,
) -> Result<(), ApiError> {
    super::access::gate(s, sess, provider, ip).await?;
    let provider = provider.to_string();
    {
        // stamp the IP and flag a login from an address this account was never seen at
        let (st, sid, acc, ip, provider) = (
            s.clone(),
            sess.session_id,
            sess.account_id,
            ip.to_string(),
            provider.clone(),
        );
        tokio::spawn(async move {
            match st.auth.note_login(sid, &ip).await {
                Ok(true) => suspicious_login(&st, acc, sid, &provider, &ip).await,
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, "note_login failed"),
            }
            if let Err(e) = crate::bans::note_login(&st.db, sid.0, acc, &ip).await {
                tracing::warn!(error = %e, "device registry update failed");
            }
            // a banned device that logged in through another provider is cut off right away
            let dev: Option<String> =
                sqlx::query_scalar("select device_id from musubi.sessions where id = $1")
                    .bind(sid.0)
                    .fetch_optional(&st.db)
                    .await
                    .ok()
                    .flatten()
                    .flatten();
            if let Some(d) = dev {
                if let Err(e) = st.ban_rules.check_device(&d) {
                    if let Err(e) = st.auth.logout(sid, false).await {
                        tracing::warn!(error = %e, "revoking banned-device session failed");
                    }
                    crate::http::admin::kick_everywhere(&st, acc).await;
                    st.events
                        .emit(
                            "account.banned_device_login",
                            Some(acc),
                            serde_json::json!({ "device_id": d, "message": e.to_string() }),
                        )
                        .await;
                }
            }
        });
    }
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
        return Ok(());
    }
    let rt = s.runtime.clone();
    let ctx = musubi_runtime::CallContext {
        account_id: Some(sess.account_id),
        session_id: Some(sess.session_id),
        ..Default::default()
    };
    let payload = serde_json::json!({ "provider": provider, "created": sess.created });
    tokio::spawn(async move { rt.event("authenticated", ctx, payload).await });
    Ok(())
}

/// Plan 5.1 "suspicious login" hook: bus event for webhooks/LiveOps plus the module hook, which
/// may e.g. force a re-verification or notify the player.
async fn suspicious_login(
    s: &AppState,
    account: musubi_core::AccountId,
    session: musubi_core::SessionId,
    provider: &str,
    ip: &str,
) {
    let payload = serde_json::json!({ "provider": provider, "ip": ip, "session_id": session });
    s.events
        .emit("account.suspicious_login", Some(account), payload.clone())
        .await;
    if s.runtime.has_hook("hook_suspicious_login") {
        let ctx = musubi_runtime::CallContext {
            account_id: Some(account),
            session_id: Some(session),
            ..Default::default()
        };
        s.runtime.event("suspicious_login", ctx, payload).await;
    }
}

async fn email(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<EmailReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let login = match s
        .auth
        .authenticate_email(&r.email, &r.password, r.opts, Some(&ip.0))
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
            authenticated(&s, &sess, musubi_auth::provider::EMAIL, &ip.0).await?;
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
    authenticated(&s, &sess, musubi_auth::provider::EMAIL, &ip.0).await?;
    Ok(Json(sess))
}

/// Login queue (5.1): a login past the online capacity answered 202 with a ticket; the client
/// polls here and gets the session the moment its place is reached.
async fn queue(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<QueueReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let sess = s.auth.queue_poll(&r.ticket).await?;
    authenticated(&s, &sess, "queue", &ip.0).await?;
    Ok(Json(sess))
}

#[derive(Deserialize)]
struct QueueReq {
    ticket: String,
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

const MAGIC_TTL_SECS: i64 = 15 * 60;

#[derive(Deserialize)]
struct MagicReq {
    email: String,
    /// Create the account when the address is unknown (default true).
    #[serde(default = "default_true")]
    create: bool,
}

fn default_true() -> bool {
    true
}

/// Passwordless login, step 1: always 200 (no account enumeration); emits `account.magic_link`
/// with the token so the game's mailer sends the link. Unknown addresses get a fresh account
/// unless `create: false`.
async fn magic_request(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<MagicReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    if let Some(account) = s.auth.magic_account(&r.email, r.create).await? {
        let email = r.email.trim().to_lowercase();
        let token = mint_token(&s, account, "magic", &email, MAGIC_TTL_SECS).await?;
        s.events
            .emit(
                "account.magic_link",
                Some(account),
                json!({ "account_id": account, "email": email, "token": token, "expires_in_secs": MAGIC_TTL_SECS }),
            )
            .await;
    }
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct MagicLoginReq {
    token: String,
    #[serde(flatten)]
    opts: musubi_auth::AuthOptions,
}

/// Passwordless login, step 2: the single-use token from the link becomes a session and marks
/// the address verified.
async fn magic(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<MagicLoginReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let Some((account, email)) = consume_token(&s, &r.token, "magic").await? else {
        return Err(musubi_core::Error::NotFound("token unknown, used or expired".into()).into());
    };
    let sess = s.auth.authenticate_magic(account, &email, r.opts).await?;
    authenticated(&s, &sess, musubi_auth::provider::EMAIL, &ip.0).await?;
    Ok(Json(sess))
}

#[derive(Deserialize)]
struct CustomReq {
    /// The game's own stable id for the player (already verified by the caller).
    id: String,
    #[serde(flatten)]
    opts: musubi_auth::AuthOptions,
}

/// Custom authentication: a trusted game server (server key) mints a session for an identity it
/// verified itself — its own accounts, a launcher token, a publisher SSO…
async fn custom(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    ip: super::ClientIp,
    Json(r): Json<CustomReq>,
) -> Result<Json<Session>, ApiError> {
    let sess = s.auth.authenticate_custom(&r.id, r.opts).await?;
    authenticated(&s, &sess, musubi_auth::provider::CUSTOM, &ip.0).await?;
    Ok(Json(sess))
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
    let ownership = super::steam_ownership::gate(&s, &steam_id).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::STEAM, &steam_id, r.opts)
        .await?;
    if let Some(o) = ownership {
        super::steam_ownership::store(&s, &steam_id, &o).await?;
    }
    authenticated(&s, &sess, musubi_auth::provider::STEAM, &ip.0).await?;
    Ok(Json(sess))
}

#[derive(Deserialize)]
struct XboxReq {
    user_hash: String,
    token: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

#[derive(Deserialize)]
struct ConsoleIdTokenReq {
    id_token: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

async fn xbox(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<XboxReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let p = s.logins.xbox(&r.user_hash, &r.token).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::XBOX, &p.xuid, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::XBOX, &ip.0).await?;
    Ok(Json(sess))
}

async fn psn(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<ConsoleIdTokenReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let c = s.logins.psn(&r.id_token).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::PSN, &c.sub, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::PSN, &ip.0).await?;
    Ok(Json(sess))
}

async fn nintendo(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<ConsoleIdTokenReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let c = s.logins.nintendo(&r.id_token).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::NINTENDO, &c.sub, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::NINTENDO, &ip.0).await?;
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
    authenticated(&s, &sess, musubi_auth::provider::GOOGLE, &ip.0).await?;
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
    authenticated(&s, &sess, musubi_auth::provider::APPLE, &ip.0).await?;
    Ok(Json(sess))
}

#[derive(Deserialize)]
struct FacebookReq {
    /// Classic access token or Limited Login id token.
    token: String,
    #[serde(flatten)]
    opts: AuthOptions,
}

/// `POST /v1/auth/facebook` — `{token}`: an access token (checked with `debug_token`) or a
/// Limited Login id token; identity `facebook:<app-scoped user id>`.
async fn facebook(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<FacebookReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let uid = s.logins.facebook(&r.token).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::FACEBOOK, &uid, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::FACEBOOK, &ip.0).await?;
    Ok(Json(sess))
}

#[derive(Deserialize)]
struct GameCenterReq {
    #[serde(flatten)]
    sig: crate::logins::GameCenterSig,
    #[serde(flatten)]
    opts: AuthOptions,
}

/// `POST /v1/auth/gamecenter` — the identity verification signature from GameKit
/// (`player_id, bundle_id, timestamp, salt, signature, public_key_url`); identity
/// `gamecenter:<player_id>`.
async fn gamecenter(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<GameCenterReq>,
) -> Result<Json<Session>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let pid = s.logins.gamecenter(&r.sig).await?;
    let sess = s
        .auth
        .authenticate_external(musubi_auth::provider::GAMECENTER, &pid, r.opts)
        .await?;
    authenticated(&s, &sess, musubi_auth::provider::GAMECENTER, &ip.0).await?;
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
    authenticated(&s, &sess, &p, &ip.0).await?;
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

#[derive(Deserialize)]
struct ParentalReq {
    parent_email: String,
}

/// Parental consent, step 1: a child (or any account) names a parent; emits
/// `account.parental_consent_request` with a single-use token for your mailer. Always 200.
async fn parental_request(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    ip: super::ClientIp,
    Json(r): Json<ParentalReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let email = r.parent_email.trim().to_lowercase();
    if email.len() > 254 || !email.contains('@') || email.starts_with('@') || email.ends_with('@') {
        return Err(musubi_core::Error::InvalidArgument("bad parent_email".into()).into());
    }
    let ttl = s.cfg.compliance.consent_ttl_secs.max(60);
    sqlx::query("update musubi.accounts set parent_email = $2 where id = $1")
        .bind(me.account_id.0)
        .bind(&email)
        .execute(&s.db)
        .await
        .map_err(musubi_core::Error::internal)?;
    let token = mint_token(&s, me.account_id, "consent", &email, ttl).await?;
    s.events
        .emit(
            "account.parental_consent_request",
            Some(me.account_id),
            json!({ "account_id": me.account_id, "parent_email": email, "token": token, "expires_in_secs": ttl }),
        )
        .await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ConsentReq {
    token: String,
}

/// Parental consent, step 2 (no session — the parent clicks the link): marks the account as
/// consented, which switches a child to `consented_child_restrictions`.
async fn parental_consent(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<ConsentReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let Some((account, _email)) = consume_token(&s, &r.token, "consent").await? else {
        return Err(musubi_core::Error::NotFound("token unknown, used or expired".into()).into());
    };
    let cur = s.age.get(account);
    let info = crate::compliance::AgeInfo {
        bracket: cur.bracket,
        consented: true,
    };
    s.age
        .set(&s.db, account, info, None, s.cluster.as_ref())
        .await?;
    s.events
        .emit(
            "account.parental_consent",
            Some(account),
            json!({ "account_id": account }),
        )
        .await;
    Ok(Json(json!({
        "account_id": account,
        "bracket": info.bracket,
        "parental_consent": true,
        "restrictions": s.age.restrictions(account),
    })))
}

#[derive(Deserialize)]
struct MfaSendReq {
    mfa_token: String,
}

/// E-mail second factor, step 1: mint a code for the pending login and hand it to the game's
/// mailer through `account.mfa_code` (like `account.magic_link`). Always `{ok: true}`.
async fn mfa_email_send(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<MfaSendReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let (account, email, code) = s.auth.email_mfa_send(&r.mfa_token).await?;
    s.events
        .emit(
            "account.mfa_code",
            Some(account),
            serde_json::json!({
                "account_id": account, "email": email, "code": code,
                "expires_in_secs": musubi_auth::EMAIL_CODE_TTL_SECS,
            }),
        )
        .await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(serde::Deserialize)]
pub struct ScopeBody {
    pub scopes: Vec<String>,
    /// Lifetime in seconds (default: the rest of the current token).
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

/// `POST /v1/auth/token/scope {scopes, ttl_secs?}` — a down-scoped access token for the
/// caller's own session (hand it to a companion app, overlay or third-party service). No
/// refresh token; scopes must be within the caller's own.
async fn token_scope(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<ScopeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::scopes::validate(&b.scopes)?;
    if b.scopes.is_empty() {
        return Err(Error::InvalidArgument("scopes required".into()).into());
    }
    if !crate::scopes::within(&b.scopes, &me.scopes) {
        return Err(
            Error::PermissionDenied("requested scopes exceed the token's own".into()).into(),
        );
    }
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let remaining = (me.exp - now).max(1);
    let ttl = b.ttl_secs.unwrap_or(remaining).clamp(1, remaining);
    let (token, exp) = s.auth.sign_scoped(
        me.account_id,
        me.session_id,
        me.username.clone(),
        b.scopes.clone(),
        ttl,
    )?;
    Ok(Json(serde_json::json!({
        "access_token": token, "expires_at": exp, "scopes": b.scopes, "account_id": me.account_id,
    })))
}

/// `POST /v1/server/accounts/{id}/token {scopes, ttl_secs}` — the game mints a scoped token for a
/// player (bots, spectator overlays, partner integrations) without their credentials.
async fn server_token(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<uuid::Uuid>,
    Json(b): Json<ScopeBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    crate::scopes::validate(&b.scopes)?;
    if b.scopes.is_empty() {
        return Err(Error::InvalidArgument("scopes required".into()).into());
    }
    let exists: Option<(Option<String>,)> =
        sqlx::query_as("select username from musubi.accounts where id = $1 and deleted_at is null")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((username,)) = exists else {
        return Err(Error::NotFound("account".into()).into());
    };
    let ttl = b.ttl_secs.unwrap_or(3600).clamp(1, 30 * 86_400);
    let account = musubi_core::AccountId(id);
    let (token, exp) = s.auth.sign_scoped(
        account,
        musubi_core::SessionId(uuid::Uuid::now_v7()),
        username,
        b.scopes.clone(),
        ttl,
    )?;
    s.events
        .emit(
            "account.scoped_token",
            Some(account),
            serde_json::json!({ "scopes": b.scopes, "expires_at": exp }),
        )
        .await;
    Ok(Json(serde_json::json!({
        "access_token": token, "expires_at": exp, "scopes": b.scopes, "account_id": account,
    })))
}
