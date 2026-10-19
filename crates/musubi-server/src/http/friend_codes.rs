//! Friend codes and friend-request privacy. Every player has a shareable `XXXX-XXXX` code
//! (`GET /v1/friends/code`; `POST …/rotate` replaces it, optionally with a use or time limit —
//! a code for one stream, one convention weekend); a friend looks the owner up with
//! `GET /v1/friends/code/{code}` and adds them with `POST /v1/friends/code/{code}/add`, which
//! bypasses the owner's request policy. `GET/PUT /v1/friends/settings {requests}` chooses who may
//! send requests: `anyone` (default), `friends_of_friends` (needs a mutual friend), `code_only`
//! or `nobody`; `POST /v1/friends` answers 403 when the policy refuses.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/friends/code", get(my_code))
        .route("/v1/friends/code/rotate", post(rotate))
        .route("/v1/friends/code/{code}", get(lookup))
        .route("/v1/friends/code/{code}/add", post(add_by_code))
        .route("/v1/friends/settings", get(get_settings).put(put_settings))
}

fn fresh_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(ALPHABET[rng.gen_range(0..ALPHABET.len())] as char);
    }
    s
}

/// `abcd efgh`, `ABCDEFGH`, `abcd-efgh` → `ABCD-EFGH` (O→0, I/L→1 like the device codes).
fn normalize(code: &str) -> Result<String, Error> {
    let s: String = code
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .map(|c| match c {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    if s.len() != 8 {
        return Err(Error::InvalidArgument("friend code: 8 characters".into()));
    }
    Ok(format!("{}-{}", &s[..4], &s[4..]))
}

async fn my_code(Bearer(me): Bearer, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let c = s.social.friend_code(me.account_id, &fresh_code()).await?;
    Ok(Json(serde_json::to_value(c).map_err(Error::internal)?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RotateReq {
    max_uses: Option<i32>,
    expires_in_secs: Option<i64>,
}

async fn rotate(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(r): Json<RotateReq>,
) -> Result<Json<Value>, ApiError> {
    let c = s
        .social
        .rotate_friend_code(me.account_id, &fresh_code(), r.max_uses, r.expires_in_secs)
        .await?;
    s.events
        .emit(
            "friend.code_rotated",
            Some(me.account_id),
            json!({ "max_uses": r.max_uses, "expires_in_secs": r.expires_in_secs }),
        )
        .await;
    Ok(Json(serde_json::to_value(c).map_err(Error::internal)?))
}

async fn owner(s: &AppState, code: &str) -> Result<(String, musubi_core::AccountId), ApiError> {
    let code = normalize(code)?;
    match s.social.resolve_friend_code(&code).await? {
        Some(a) => Ok((code, a)),
        None => {
            Err(Error::NotFound("no such friend code (or it expired / was used up)".into()).into())
        }
    }
}

/// Public card of the code's owner so the UI can confirm before adding.
async fn lookup(
    Bearer(_me): Bearer,
    State(s): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (_, who) = owner(&s, &code).await?;
    let user = s
        .accounts
        .get_users(&[who], &[])
        .await?
        .pop()
        .ok_or_else(|| Error::NotFound("account".into()))?;
    Ok(Json(json!({ "account_id": who, "user": user })))
}

async fn add_by_code(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (code, who) = owner(&s, &code).await?;
    if who == me.account_id {
        return Err(Error::InvalidArgument("that is your own friend code".into()).into());
    }
    let out = super::friends::add_targets(&s, me.account_id, vec![who], true).await?;
    s.social.consume_friend_code(&code).await?;
    s.events
        .emit(
            "friend.added_by_code",
            Some(me.account_id),
            json!({ "owner": who }),
        )
        .await;
    Ok(out)
}

async fn get_settings(
    Bearer(me): Bearer,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "requests": s.social.friend_request_policy(me.account_id).await?,
        "options": musubi_social::FRIEND_REQUEST_POLICIES,
    })))
}

#[derive(Deserialize)]
struct SettingsReq {
    requests: String,
}

async fn put_settings(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    Json(r): Json<SettingsReq>,
) -> Result<Json<Value>, ApiError> {
    s.social
        .set_friend_request_policy(me.account_id, &r.requests)
        .await?;
    s.events
        .emit(
            "friend.settings_updated",
            Some(me.account_id),
            json!({ "requests": r.requests }),
        )
        .await;
    Ok(Json(json!({ "requests": r.requests })))
}
