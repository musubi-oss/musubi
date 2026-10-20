//! Account privacy toggles (LootLocker public/private profile parity). `profile` governs what
//! other players see in `/v1/users*` cards (`public` = full card, `friends` = full card for
//! mutual friends, id/username/avatar for everyone else, `private` = id/username/avatar only);
//! `inventory` governs inspect links (`/v1/inspect/{token}` answers 404 to viewers the owner did
//! not allow); `show_online` hides presence in the friends-visible baggage card. Defaults are
//! public; `GET/PUT /v1/account/privacy`, operators read `GET /v1/server/accounts/{id}/privacy`.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/account/privacy", get(my_privacy).put(put_privacy))
        .route("/v1/server/accounts/{id}/privacy", get(server_privacy))
}

#[derive(Debug, Clone)]
pub struct Privacy {
    pub profile: String,
    pub inventory: String,
    pub show_online: bool,
}

impl Default for Privacy {
    fn default() -> Self {
        Self {
            profile: "public".into(),
            inventory: "public".into(),
            show_online: true,
        }
    }
}

impl Privacy {
    fn doc(&self, account: Uuid) -> Value {
        json!({
            "account_id": account, "profile": self.profile, "inventory": self.inventory,
            "show_online": self.show_online,
        })
    }
}

pub async fn load(s: &AppState, account: Uuid) -> Result<Privacy> {
    let row: Option<(String, String, bool)> = sqlx::query_as(
        "select profile, inventory, show_online from musubi.account_privacy where account_id = $1",
    )
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(row
        .map(|(profile, inventory, show_online)| Privacy {
            profile,
            inventory,
            show_online,
        })
        .unwrap_or_default())
}

/// Mutual friendship (state 0 = accepted) between two accounts.
pub async fn friends(s: &AppState, a: Uuid, b: Uuid) -> Result<bool> {
    sqlx::query_scalar(
        "select exists(select 1 from musubi.friends where user_id = $1 and friend_id = $2 and state = 0)",
    )
    .bind(a)
    .bind(b)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)
}

/// May `viewer` see the `level`-guarded part of `owner`'s data? Owners always may.
pub async fn allowed(s: &AppState, owner: Uuid, viewer: Option<Uuid>, level: &str) -> Result<bool> {
    if viewer == Some(owner) {
        return Ok(true);
    }
    Ok(match level {
        "public" => true,
        "friends" => match viewer {
            Some(v) => friends(s, owner, v).await?,
            None => false,
        },
        _ => false,
    })
}

/// Reduce a public profile card to id / username / avatar when the viewer is not allowed.
pub async fn filter_profiles(
    s: &AppState,
    viewer: Option<Uuid>,
    cards: &mut [Value],
) -> Result<()> {
    for v in cards.iter_mut() {
        let Some(id) = v["id"].as_str().and_then(|x| x.parse::<Uuid>().ok()) else {
            continue;
        };
        let p = load(s, id).await?;
        if p.profile == "public" || allowed(s, id, viewer, &p.profile).await? {
            continue;
        }
        if let Some(o) = v.as_object_mut() {
            o.retain(|k, _| matches!(k.as_str(), "id" | "username" | "avatar_url"));
            o.insert("private".into(), json!(true));
        }
    }
    Ok(())
}

async fn my_privacy(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(load(&s, me.account_id.0).await?.doc(me.account_id.0)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PutBody {
    profile: Option<String>,
    inventory: Option<String>,
    show_online: Option<bool>,
}

fn level_ok(l: &Option<String>) -> Result<()> {
    if l.as_deref()
        .is_some_and(|l| !["public", "friends", "private"].contains(&l))
    {
        return Err(Error::InvalidArgument(
            "visibility must be public|friends|private".into(),
        ));
    }
    Ok(())
}

async fn put_privacy(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    level_ok(&b.profile)?;
    level_ok(&b.inventory)?;
    let cur = load(&s, me.account_id.0).await?;
    let next = Privacy {
        profile: b.profile.unwrap_or(cur.profile),
        inventory: b.inventory.unwrap_or(cur.inventory),
        show_online: b.show_online.unwrap_or(cur.show_online),
    };
    sqlx::query(
        "insert into musubi.account_privacy (account_id, profile, inventory, show_online)
         values ($1, $2, $3, $4)
         on conflict (account_id) do update set profile = excluded.profile,
            inventory = excluded.inventory, show_online = excluded.show_online, updated_at = now()",
    )
    .bind(me.account_id.0)
    .bind(&next.profile)
    .bind(&next.inventory)
    .bind(next.show_online)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "account.privacy_updated",
            Some(me.account_id),
            json!({ "profile": next.profile, "inventory": next.inventory, "show_online": next.show_online }),
        )
        .await;
    Ok(Json(next.doc(me.account_id.0)))
}

async fn server_privacy(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n: i64 = sqlx::query_scalar("select count(*) from musubi.accounts where id = $1")
        .bind(id)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    if n == 0 {
        return Err(Error::NotFound(format!("account {id}")).into());
    }
    let _ = AccountId(id);
    Ok(Json(load(&s, id).await?.doc(id)))
}
