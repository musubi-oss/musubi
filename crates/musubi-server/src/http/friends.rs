use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_social::{FriendList, FriendState};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/friends", get(list).post(add).delete(remove))
        .route("/v1/friends/block", post(block))
}

#[derive(Deserialize)]
struct Ids {
    #[serde(default)]
    ids: Vec<AccountId>,
    #[serde(default)]
    usernames: Vec<String>,
}

async fn resolve(s: &AppState, b: Ids) -> Result<Vec<AccountId>, ApiError> {
    let mut ids = b.ids;
    if !b.usernames.is_empty() {
        let users = s.accounts.get_users(&[], &b.usernames).await?;
        if users.len() != b.usernames.len() {
            return Err(musubi_core::Error::NotFound("unknown username".into()).into());
        }
        ids.extend(users.into_iter().map(|u| u.id));
    }
    Ok(ids)
}

async fn add(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = resolve(&s, b).await?;
    s.social.add(me.account_id, &ids).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn remove(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = resolve(&s, b).await?;
    s.social.delete(me.account_id, &ids).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn block(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<Ids>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let ids = resolve(&s, b).await?;
    s.social.block(me.account_id, &ids).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    state: Option<i16>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

fn default_limit() -> usize {
    50
}

async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<FriendList>, ApiError> {
    let state = match q.state {
        None => None,
        Some(0) => Some(FriendState::Friend),
        Some(1) => Some(FriendState::InviteSent),
        Some(2) => Some(FriendState::InviteReceived),
        Some(3) => Some(FriendState::Blocked),
        Some(_) => {
            return Err(musubi_core::Error::InvalidArgument("state must be 0..=3".into()).into())
        }
    };
    Ok(Json(
        s.social
            .list(me.account_id, state, q.limit, q.cursor.as_deref())
            .await?,
    ))
}
