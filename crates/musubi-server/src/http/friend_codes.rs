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

const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

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

/// Take one use of a code atomically: the expiry / `max_uses` check and the increment are a single
/// conditional UPDATE, so N parallel callers of a `max_uses = 1` code cannot all get through.
pub(crate) async fn claim_friend_code(
    db: &sqlx::PgPool,
    code: &str,
) -> Result<Option<musubi_core::AccountId>, Error> {
    let r: Option<(uuid::Uuid,)> = sqlx::query_as(
        "update musubi.friend_codes set uses = uses + 1
         where code = $1 and (expires_at is null or expires_at > now())
           and (max_uses is null or uses < max_uses)
         returning account_id",
    )
    .bind(code)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?;
    Ok(r.map(|(id,)| musubi_core::AccountId(id)))
}

/// Give a claimed use back when the add itself failed (policy, block, batch error).
pub(crate) async fn release_friend_code(db: &sqlx::PgPool, code: &str) -> Result<(), Error> {
    sqlx::query("update musubi.friend_codes set uses = greatest(uses - 1, 0) where code = $1")
        .bind(code)
        .execute(db)
        .await
        .map_err(Error::internal)?;
    Ok(())
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
    let code = normalize(&code)?;
    let who = claim_friend_code(&s.db, &code).await?.ok_or_else(|| {
        Error::NotFound("no such friend code (or it expired / was used up)".into())
    })?;
    if who == me.account_id {
        release_friend_code(&s.db, &code).await?;
        return Err(Error::InvalidArgument("that is your own friend code".into()).into());
    }
    let out = match super::friends::add_targets(&s, me.account_id, vec![who], true).await {
        Ok(out) => out,
        Err(e) => {
            release_friend_code(&s.db, &code).await?;
            return Err(e);
        }
    };
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

#[cfg(test)]
mod audit_tests {
    use super::*;
    use crate::http::friends::test_db;

    #[test]
    fn normalize_maps_lookalikes_and_rejects_bad_lengths() {
        assert_eq!(normalize("abcd efgh").unwrap(), "ABCD-EFGH");
        assert_eq!(normalize("o1il-0000").unwrap(), "0111-0000");
        assert!(normalize("abc").is_err());
        assert!(normalize("abcdefghi").is_err());
    }

    /// A `max_uses = 1` code claimed by 16 parallel callers is consumed exactly once; a release
    /// makes it claimable again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn claim_is_atomic_under_concurrency() {
        let Some(db) = test_db::pool().await else {
            return;
        };
        let owner = test_db::account(&db).await;
        let code = fresh_code();
        sqlx::query(
            "insert into musubi.friend_codes (account_id, code, max_uses) values ($1, $2, 1)",
        )
        .bind(owner.0)
        .bind(&code)
        .execute(&db)
        .await
        .unwrap();
        let results =
            futures::future::join_all((0..16).map(|_| claim_friend_code(&db, &code))).await;
        let claimed: Vec<_> = results.into_iter().flat_map(|r| r.unwrap()).collect();
        assert_eq!(claimed, vec![owner]);
        let uses: i32 = sqlx::query_scalar("select uses from musubi.friend_codes where code = $1")
            .bind(&code)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(uses, 1);
        release_friend_code(&db, &code).await.unwrap();
        assert_eq!(claim_friend_code(&db, &code).await.unwrap(), Some(owner));
        assert_eq!(claim_friend_code(&db, &code).await.unwrap(), None);
    }

    #[tokio::test]
    async fn expired_code_is_not_claimable() {
        let Some(db) = test_db::pool().await else {
            return;
        };
        let owner = test_db::account(&db).await;
        let code = fresh_code();
        sqlx::query(
            "insert into musubi.friend_codes (account_id, code, expires_at) values ($1, $2, now() - interval '1 second')",
        )
        .bind(owner.0)
        .bind(&code)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(claim_friend_code(&db, &code).await.unwrap(), None);
    }
}
