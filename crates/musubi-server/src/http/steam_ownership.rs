//! Steam app ownership and Family Sharing at login (competitive P1-24): refuse copies that are
//! not owned, optionally refuse borrowers, and propagate bans between a lender and the accounts
//! borrowing the game from it. The answer is stored on the steam identity
//! (`metadata.ownership`) and reused for `[social.steam] ownership_cache_secs`.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/accounts/{id}/steam", get(account_steam))
        .route("/v1/server/steam/app_ticket", post(verify_app_ticket))
}

const STEAM: &str = musubi_auth::provider::STEAM;

fn enabled(s: &AppState) -> bool {
    let c = &s.cfg.social.steam;
    c.require_ownership || c.family_sharing != "allow" || c.ban_propagation != "none"
}

async fn account_of(s: &AppState, steam_id: &str) -> Result<Option<Uuid>> {
    sqlx::query_scalar(
        "select account_id from musubi.identities where provider = $1 and subject = $2",
    )
    .bind(STEAM)
    .bind(steam_id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

async fn borrowers_of(s: &AppState, steam_id: &str) -> Result<Vec<(Uuid, String)>> {
    sqlx::query_as(
        "select account_id, subject from musubi.identities
         where provider = $1 and subject <> $2
           and metadata -> 'ownership' ->> 'owner_steamid' = $2",
    )
    .bind(STEAM)
    .bind(steam_id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)
}

/// Runs before the session is opened. `Ok(None)` = nothing to enforce or store; `Ok(Some(meta))`
/// = ownership metadata to persist once the identity exists; `Err` = refuse the login.
pub async fn gate(s: &AppState, steam_id: &str) -> Result<Option<Value>> {
    if !enabled(s) {
        return Ok(None);
    }
    let c = &s.cfg.social.steam;
    let cached: Option<Value> = sqlx::query_scalar(
        "select metadata -> 'ownership' from musubi.identities where provider = $1 and subject = $2",
    )
    .bind(STEAM)
    .bind(steam_id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .flatten();
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let fresh = cached.as_ref().filter(|m| {
        c.ownership_cache_secs > 0
            && m["checked_at"]
                .as_i64()
                .is_some_and(|t| now - t < c.ownership_cache_secs as i64)
    });
    // Steam is asked only when an answer is needed to admit the player (ownership or family
    // sharing rules); ban propagation alone works from whatever answer is already stored.
    let needs_api = c.require_ownership || c.family_sharing != "allow";
    let o: crate::logins::SteamOwnership = match (fresh, cached.as_ref()) {
        (Some(m), _) => serde_json::from_value(m.clone()).unwrap_or_default(),
        (None, _) if needs_api => s.logins.steam_ownership(steam_id).await?,
        (None, Some(m)) => serde_json::from_value(m.clone()).unwrap_or_default(),
        (None, None) => return Ok(None),
    };
    if c.require_ownership && !o.owns_app {
        return Err(Error::PermissionDenied(
            "this steam account does not own the game".into(),
        ));
    }
    if o.family_shared && c.family_sharing == "deny" {
        return Err(Error::PermissionDenied(
            "steam family sharing is not allowed for this game".into(),
        ));
    }
    if o.family_shared && c.ban_propagation != "none" {
        if let Some(owner) = account_of(s, &o.owner_steamid).await? {
            if s.banned.is_banned(AccountId(owner)) {
                return Err(Error::PermissionDenied(
                    "the steam account lending this game is banned".into(),
                ));
            }
        }
    }
    if !o.family_shared && c.ban_propagation == "both" {
        for (acc, _) in borrowers_of(s, steam_id).await? {
            if s.banned.is_banned(AccountId(acc)) {
                return Err(Error::PermissionDenied(
                    "an account borrowing this game from you via family sharing is banned".into(),
                ));
            }
        }
    }
    let mut meta = serde_json::to_value(&o).unwrap_or(json!({}));
    meta["checked_at"] = json!(fresh.and_then(|m| m["checked_at"].as_i64()).unwrap_or(now));
    Ok(Some(meta))
}

/// Persist the ownership answer on the (now existing) steam identity.
pub async fn store(s: &AppState, steam_id: &str, meta: &Value) -> Result<()> {
    sqlx::query(
        "update musubi.identities
         set metadata = metadata || jsonb_build_object('ownership', $3::jsonb), updated_at = now()
         where provider = $1 and subject = $2",
    )
    .bind(STEAM)
    .bind(steam_id)
    .bind(meta)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(())
}

/// `GET /v1/admin/accounts/{id}/steam` — the account's steam identities with the stored
/// ownership answer, the lender (when borrowed) and every account borrowing from it, each with
/// its ban state.
async fn account_steam(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<(String, Value, OffsetDateTime)> = sqlx::query_as(
        "select subject, coalesce(metadata -> 'ownership', 'null'::jsonb), created_at
         from musubi.identities where account_id = $1 and provider = $2 order by created_at",
    )
    .bind(id)
    .bind(STEAM)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    if rows.is_empty() {
        let (exists,): (bool,) =
            sqlx::query_as("select exists(select 1 from musubi.accounts where id = $1)")
                .bind(id)
                .fetch_one(&s.db)
                .await
                .map_err(Error::internal)?;
        if !exists {
            return Err(Error::NotFound("account".into()).into());
        }
    }
    let mut identities = Vec::new();
    for (subject, ownership, created_at) in rows {
        let lender = match ownership["owner_steamid"].as_str() {
            Some(owner) if ownership["family_shared"] == true => {
                let acc = account_of(&s, owner).await?;
                Some(json!({
                    "steam_id": owner,
                    "account_id": acc,
                    "banned": acc.map(|a| s.banned.is_banned(AccountId(a))).unwrap_or(false),
                }))
            }
            _ => None,
        };
        let borrowers: Vec<Value> = borrowers_of(&s, &subject)
            .await?
            .into_iter()
            .map(|(acc, sid)| {
                json!({"account_id": acc, "steam_id": sid, "banned": s.banned.is_banned(AccountId(acc))})
            })
            .collect();
        identities.push(json!({
            "steam_id": subject,
            "linked_at": created_at.unix_timestamp(),
            "ownership": if ownership.is_null() { Value::Null } else { ownership },
            "lender": lender,
            "borrowers": borrowers,
        }));
    }
    let c = &s.cfg.social.steam;
    Ok(Json(json!({
        "account_id": id,
        "policy": {
            "require_ownership": c.require_ownership,
            "family_sharing": c.family_sharing,
            "ban_propagation": c.ban_propagation,
            "ownership_cache_secs": c.ownership_cache_secs,
        },
        "identities": identities,
    })))
}

#[derive(serde::Deserialize)]
struct VerifyTicketBody {
    ticket: String,
}

/// `POST /v1/server/steam/app_ticket {ticket}` — a game server hands over the encrypted app
/// ticket a peer presented; the backend decrypts it with the SDK Auth key and returns who it is
/// (`steam_id`, `app_id`, licenses, dlc, `user_data`) plus the Musubi account linked to that
/// Steam identity, if any.
async fn verify_app_ticket(
    State(s): State<AppState>,
    _k: ServerKey,
    axum::Json(b): axum::Json<VerifyTicketBody>,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    let t = s.logins.steam_app_ticket(&b.ticket)?;
    let account: Option<(uuid::Uuid,)> = sqlx::query_as(
        "select account_id from musubi.identities where provider = $1 and subject = $2",
    )
    .bind(musubi_auth::provider::STEAM)
    .bind(&t.steam_id)
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let mut v = serde_json::to_value(&t).unwrap_or_default();
    v["account_id"] = serde_json::json!(account.map(|a| a.0));
    Ok(axum::Json(v))
}
