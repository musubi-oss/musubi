use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_account::{Account, UpdateAccount, User};
use musubi_core::AccountId;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/account",
            get(get_account)
                .patch(update_account)
                .delete(delete_account),
        )
        .route("/v1/account/link/device", post(link_device))
        .route("/v1/account/link/email", post(link_email))
        .route("/v1/account/link/steam", post(link_steam))
        .route(
            "/v1/account/link/steam/app_ticket",
            post(link_steam_app_ticket),
        )
        .route("/v1/account/link/xbox", post(link_xbox))
        .route("/v1/account/link/psn", post(link_psn))
        .route("/v1/account/link/nintendo", post(link_nintendo))
        .route("/v1/account/link/google", post(link_google))
        .route("/v1/account/link/apple", post(link_apple))
        .route("/v1/account/link/oidc/{provider}", post(link_oidc))
        .route("/v1/account/unlink/oidc/{provider}", delete(unlink_oidc))
        .route(
            "/v1/account/push_tokens",
            get(push_tokens).post(push_register).delete(push_unregister),
        )
        .route("/v1/account/unlink/device", delete(unlink_device))
        .route("/v1/account/unlink/email", delete(unlink_email))
        .route("/v1/account/export", get(export_self))
        .route("/v1/account/merge", post(merge))
        .route(
            "/v1/account/mfa/totp",
            post(totp_begin).delete(totp_disable),
        )
        .route("/v1/account/mfa/totp/verify", post(totp_verify))
        .route("/v1/account/mfa", get(mfa_status))
        .route(
            "/v1/account/mfa/email",
            post(email_mfa_enable).delete(email_mfa_disable),
        )
        .route("/v1/account/security_events", get(my_security_events))
        .route("/v1/account/delete", post(delete_self))
        .route("/v1/account/age", get(get_age).put(put_age))
        .route(
            "/v1/server/accounts/{id}/age",
            axum::routing::put(server_put_age),
        )
        .route("/v1/users", get(get_users))
        .route("/v1/users/search", get(search_users))
}

async fn get_account(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Account>, ApiError> {
    Ok(Json(s.accounts.get(me.account_id).await?))
}

async fn update_account(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(u): Json<UpdateAccount>,
) -> Result<Json<Account>, ApiError> {
    let changed = s
        .accounts
        .update_with_policy(me.account_id, u, time::OffsetDateTime::now_utc())
        .await?;
    if !changed.is_empty() {
        s.events
            .emit(
                "account.profile_updated",
                Some(me.account_id),
                serde_json::json!({ "fields": changed }),
            )
            .await;
    }
    Ok(Json(s.accounts.get(me.account_id).await?))
}

async fn delete_account(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.accounts.delete(me.account_id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct DeviceBody {
    id: String,
}

#[derive(Deserialize)]
struct EmailBody {
    email: String,
    #[serde(default)]
    password: Option<String>,
}

async fn link_device(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<DeviceBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth.link_device(me.account_id, &b.id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn link_email(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<EmailBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let pass = b
        .password
        .ok_or_else(|| musubi_core::Error::InvalidArgument("password required".into()))?;
    s.auth.link_email(me.account_id, &b.email, &pass).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn unlink_device(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<DeviceBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth
        .unlink(me.account_id, musubi_auth::provider::DEVICE, &b.id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn unlink_email(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<EmailBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth
        .unlink(
            me.account_id,
            musubi_auth::provider::EMAIL,
            &b.email.trim().to_ascii_lowercase(),
        )
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct UsersQuery {
    #[serde(default)]
    ids: Option<String>,
    #[serde(default)]
    usernames: Option<String>,
}

/// `GET /v1/users?ids=a,b&usernames=x,y`
async fn get_users(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<UsersQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = q
        .ids
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|x| !x.is_empty())
        .map(|x| {
            x.parse::<AccountId>()
                .map_err(|_| musubi_core::Error::InvalidArgument(format!("bad id {x}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let names: Vec<String> = q
        .usernames
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect();
    let users: Vec<User> = s.accounts.get_users(&ids, &names).await?;
    let mut cards = public_profiles(&s, users);
    super::privacy::filter_profiles(&s, Some(me.account_id.0), &mut cards).await?;
    Ok(Json(json!({ "users": cards })))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
    #[serde(default = "search_limit")]
    limit: usize,
}

fn search_limit() -> usize {
    20
}

/// `GET /v1/users/search?q=<username prefix>&limit=20` — case-insensitive prefix match.
async fn search_users(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let users: Vec<User> = s.accounts.search_users(&q.q, q.limit).await?;
    let mut cards = public_profiles(&s, users);
    super::privacy::filter_profiles(&s, Some(me.account_id.0), &mut cards).await?;
    Ok(Json(json!({ "users": cards })))
}

#[derive(Deserialize)]
struct LinkSteamBody {
    ticket: String,
}

#[derive(Deserialize)]
struct LinkIdTokenBody {
    id_token: String,
}

async fn link_steam(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkSteamBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = s.logins.steam(&b.ticket).await?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::STEAM, &id)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn link_steam_app_ticket(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkSteamBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let t = s.logins.steam_app_ticket(&b.ticket)?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::STEAM, &t.steam_id)
        .await?;
    Ok(Json(json!({ "ok": true, "steam_id": t.steam_id })))
}

#[derive(Deserialize)]
struct LinkXboxBody {
    user_hash: String,
    token: String,
}

async fn link_xbox(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkXboxBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let p = s.logins.xbox(&b.user_hash, &b.token).await?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::XBOX, &p.xuid)
        .await?;
    Ok(Json(json!({ "ok": true, "gamertag": p.gamertag })))
}

async fn link_psn(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkIdTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s.logins.psn(&b.id_token).await?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::PSN, &c.sub)
        .await?;
    Ok(Json(json!({ "ok": true, "online_id": c.online_id })))
}

async fn link_nintendo(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkIdTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s.logins.nintendo(&b.id_token).await?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::NINTENDO, &c.sub)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn link_google(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkIdTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s.logins.google(&b.id_token).await?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::GOOGLE, &c.sub)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn link_apple(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<LinkIdTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let c = s.logins.apple(&b.id_token).await?;
    s.auth
        .link_external(me.account_id, musubi_auth::provider::APPLE, &c.sub)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn link_oidc(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(provider): axum::extract::Path<String>,
    Json(b): Json<LinkIdTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !crate::logins::valid_oidc_name(&provider) {
        return Err(musubi_core::Error::InvalidArgument("bad provider name".into()).into());
    }
    let c = s.logins.oidc(&provider, &b.id_token).await?;
    s.auth
        .link_external(
            me.account_id,
            &crate::logins::oidc_provider(&provider),
            &c.sub,
        )
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct UnlinkSubjectBody {
    subject: String,
}

async fn unlink_oidc(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Path(provider): axum::extract::Path<String>,
    Json(b): Json<UnlinkSubjectBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth
        .unlink(
            me.account_id,
            &crate::logins::oidc_provider(&provider),
            &b.subject,
        )
        .await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct PushTokenBody {
    platform: String,
    token: String,
}

#[derive(Deserialize)]
struct PushTokenRef {
    token: String,
}

fn push_of(s: &AppState) -> Result<&std::sync::Arc<crate::push::Push>, ApiError> {
    s.notify.push().ok_or_else(|| {
        musubi_core::Error::Unavailable("push is not available on this node".into()).into()
    })
}

async fn push_tokens(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let push = push_of(&s)?;
    Ok(Json(json!({
        "tokens": push.list(me.account_id).await?,
        "vapid_public_key": push.vapid_public_key(),
    })))
}

/// Register a device token (`fcm`, `apns`) or a Web Push subscription (`webpush`, token = the
/// `PushSubscription` JSON) for the caller; re-registering moves it over.
async fn push_register(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<PushTokenBody>,
) -> Result<Json<serde_json::Value>, ApiError> {
    push_of(&s)?
        .register(me.account_id, &b.platform, &b.token)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn push_unregister(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<PushTokenRef>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let removed = push_of(&s)?.unregister(me.account_id, &b.token).await?;
    Ok(Json(json!({ "ok": true, "removed": removed })))
}

/// `GET /v1/account/export` — everything the server stores about the caller (GDPR-style dump).
async fn export_self(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(s.accounts.export(me.account_id).await?))
}

/// `POST /v1/account/delete` — soft-delete the caller: identities and sessions go, every live
/// session is kicked, the row stays for ledgers/leaderboards. Irreversible for the player.
async fn delete_self(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    super::admin::kick_everywhere(&s, me.account_id).await;
    s.accounts.delete(me.account_id).await?;
    s.events
        .emit(
            "account.deleted",
            Some(me.account_id),
            json!({ "by": "self" }),
        )
        .await;
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize)]
struct MergeReq {
    /// Access token of the account to fold into the caller's — proof that the caller owns it too.
    access_token: String,
}

/// `POST /v1/account/merge` — absorb another account (typically a guest) into the caller's.
async fn merge(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<MergeReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let other = s
        .auth
        .verify_access(&r.access_token)
        .map_err(|_| musubi_core::Error::Unauthenticated("invalid access_token".into()))?;
    if other.account_id == me.account_id {
        return Err(musubi_core::Error::InvalidArgument(
            "cannot merge an account into itself".into(),
        )
        .into());
    }
    let report = s.accounts.merge(me.account_id, other.account_id).await?;
    super::admin::kick_everywhere(&s, other.account_id).await;
    Ok(Json(
        json!({ "merged": other.account_id, "report": report }),
    ))
}

/// `POST /v1/account/mfa/totp` — start enrolling an authenticator app.
async fn totp_begin(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let label = match s.auth.email_of(me.account_id).await? {
        Some((email, _)) => email,
        None => me.account_id.to_string(),
    };
    let (secret, uri, recovery) = s.auth.totp_begin(me.account_id, "Musubi", &label).await?;
    Ok(Json(
        json!({ "secret": secret, "otpauth": uri, "recovery_codes": recovery }),
    ))
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

/// `POST /v1/account/mfa/totp/verify` — confirm with a code; password logins now need it.
async fn totp_verify(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<CodeReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth.totp_enable(me.account_id, &r.code).await?;
    s.events
        .emit("account.mfa_enabled", Some(me.account_id), json!({}))
        .await;
    Ok(Json(json!({ "enabled": true })))
}

/// `DELETE /v1/account/mfa/totp` — turn the authenticator off (code or recovery code required).
async fn totp_disable(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<CodeReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth.totp_disable(me.account_id, &r.code).await?;
    s.events
        .emit("account.mfa_disabled", Some(me.account_id), json!({}))
        .await;
    Ok(Json(json!({ "enabled": false })))
}

/// Public profiles: accounts whose age group closes `public_profile` show only id, username and
/// avatar.
fn public_profiles(s: &AppState, users: Vec<User>) -> Vec<serde_json::Value> {
    users
        .into_iter()
        .map(|u| {
            let mut v = serde_json::to_value(&u).unwrap_or_default();
            let id = v["id"].as_str().and_then(|x| x.parse::<AccountId>().ok());
            if let Some(id) = id {
                if s.age.restricted(id, "public_profile") {
                    if let Some(o) = v.as_object_mut() {
                        o.retain(|k, _| matches!(k.as_str(), "id" | "username" | "avatar_url"));
                        o.insert("restricted".into(), json!(true));
                    }
                }
            }
            v
        })
        .collect()
}

fn age_view(s: &AppState, id: AccountId) -> serde_json::Value {
    let info = s.age.get(id);
    json!({
        "bracket": info.bracket,
        "parental_consent": info.consented,
        "restrictions": s.age.restrictions(id),
    })
}

/// `GET /v1/account/age` — the caller's age group, consent state and closed features.
async fn get_age(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(age_view(&s, me.account_id)))
}

#[derive(Deserialize)]
struct AgeReq {
    /// `YYYY-MM-DD`; the bracket is derived from it.
    #[serde(default)]
    birthdate: Option<String>,
    /// Platform-provided bracket (`child` / `teen` / `adult`) when no birthdate is collected.
    #[serde(default)]
    bracket: Option<String>,
    /// Server route only.
    #[serde(default)]
    parental_consent: Option<bool>,
}

fn parse_birthdate(s: &str) -> Result<time::Date, musubi_core::Error> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    let d = time::Date::parse(s.trim(), &fmt)
        .map_err(|_| musubi_core::Error::InvalidArgument("birthdate must be YYYY-MM-DD".into()))?;
    let today = time::OffsetDateTime::now_utc().date();
    if d > today || today.year() - d.year() > 130 {
        return Err(musubi_core::Error::InvalidArgument(
            "birthdate out of range".into(),
        ));
    }
    Ok(d)
}

fn resolve_age(
    s: &AppState,
    r: &AgeReq,
) -> Result<(crate::compliance::Bracket, Option<time::Date>), musubi_core::Error> {
    if let Some(b) = r.birthdate.as_deref().filter(|b| !b.trim().is_empty()) {
        let d = parse_birthdate(b)?;
        let today = time::OffsetDateTime::now_utc().date();
        return Ok((s.age.bracket_for(d, today), Some(d)));
    }
    match r
        .bracket
        .as_deref()
        .and_then(crate::compliance::Bracket::parse)
    {
        Some(b) => Ok((b, None)),
        None => Err(musubi_core::Error::InvalidArgument(
            "give a birthdate (YYYY-MM-DD) or a bracket: child | teen | adult".into(),
        )),
    }
}

/// `PUT /v1/account/age {birthdate | bracket}` — a player states their age once; moving to an
/// older bracket afterwards needs the game server (or a parent) so a child cannot lift its own
/// restrictions.
async fn put_age(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<AgeReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (bracket, birthdate) = resolve_age(&s, &r)?;
    let cur = s.age.get(me.account_id);
    use crate::compliance::Bracket as B;
    let older = |a: B| matches!(a, B::Teen | B::Adult);
    if cur.bracket == B::Child && older(bracket) {
        return Err(musubi_core::Error::PermissionDenied(
            "age group can only be raised by the game server or a parent".into(),
        )
        .into());
    }
    if cur.bracket == B::Teen && bracket == B::Adult && birthdate.is_none() {
        return Err(musubi_core::Error::PermissionDenied(
            "age group can only be raised with a birthdate".into(),
        )
        .into());
    }
    let info = crate::compliance::AgeInfo {
        bracket,
        consented: cur.consented,
    };
    s.age
        .set(&s.db, me.account_id, info, birthdate, s.cluster.as_ref())
        .await?;
    Ok(Json(age_view(&s, me.account_id)))
}

/// `PUT /v1/server/accounts/{id}/age {bracket | birthdate, parental_consent?}` — trusted
/// source (platform age rating, support desk, verified parent flow of your own).
async fn server_put_age(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<AccountId>,
    Json(r): Json<AgeReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (bracket, birthdate) = resolve_age(&s, &r)?;
    let cur = s.age.get(id);
    let info = crate::compliance::AgeInfo {
        bracket,
        consented: r.parental_consent.unwrap_or(cur.consented),
    };
    s.age
        .set(&s.db, id, info, birthdate, s.cluster.as_ref())
        .await?;
    Ok(Json(age_view(&s, id)))
}

/// `GET /v1/account/mfa` — which second factors are on.
async fn mfa_status(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "totp": s.auth.totp_required(me.account_id).await?,
        "email_code": s.auth.email_mfa_enabled(me.account_id).await?,
    })))
}

/// `POST /v1/account/mfa/email` — e-mail codes as a second factor (needs a verified e-mail).
async fn email_mfa_enable(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    let email = s.auth.email_mfa_enable(me.account_id).await?;
    s.events
        .emit(
            "account.mfa_enabled",
            Some(me.account_id),
            serde_json::json!({ "method": "email_code" }),
        )
        .await;
    Ok(Json(serde_json::json!({ "enabled": true, "email": email })))
}

async fn email_mfa_disable(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.auth.email_mfa_disable(me.account_id).await?;
    s.events
        .emit(
            "account.mfa_disabled",
            Some(me.account_id),
            serde_json::json!({ "method": "email_code" }),
        )
        .await;
    Ok(Json(serde_json::json!({ "enabled": false })))
}

/// `GET /v1/account/security_events` — the player's own security timeline.
async fn my_security_events(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    axum::extract::Query(q): axum::extract::Query<crate::security::TimelineQ>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        crate::security::timeline(&s.db, me.account_id.0, &q).await?,
    ))
}
