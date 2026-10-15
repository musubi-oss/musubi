use axum::extract::{Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_account::{Account, UpdateAccount, User};
use musubi_core::AccountId;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

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
        .route("/v1/account/link/google", post(link_google))
        .route("/v1/account/link/apple", post(link_apple))
        .route("/v1/account/unlink/device", delete(unlink_device))
        .route("/v1/account/unlink/email", delete(unlink_email))
        .route("/v1/users", get(get_users))
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
    s.accounts.update(me.account_id, u).await?;
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
    Bearer(_): Bearer,
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
    Ok(Json(json!({ "users": users })))
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
