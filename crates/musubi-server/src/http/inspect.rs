//! Public inspect links for inventory items (Steamworks `InspectItem` parity). The owner mints
//! a link for one inventory entry (`POST /v1/inventory/{id}/share {ttl_secs?}` — reused while
//! live), anyone opens `GET /v1/inspect/{token}` without a session and sees the catalogue item,
//! the instance data and the owner's display name; the owner lists and revokes links. Links die
//! with the item (cascade), on expiry, on revoke, and when the item's catalogue entry says
//! `properties.inspectable = false`.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer};

const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const MAX_TTL: i64 = 365 * 86_400;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/inventory/shares", get(my_shares))
        .route("/v1/inventory/{id}/share", post(share).delete(revoke))
        .route("/v1/inspect/{token}", get(inspect))
}

fn fresh_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..22)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn ts(t: Option<OffsetDateTime>) -> Value {
    t.map(|t| {
        json!(t
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default())
    })
    .unwrap_or(Value::Null)
}

type LinkRow = (
    String,
    Uuid,
    OffsetDateTime,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    i64,
);

fn link_doc(r: &LinkRow) -> Value {
    json!({
        "token": r.0, "inventory_id": r.1, "path": format!("/v1/inspect/{}", r.0),
        "created_at": ts(Some(r.2)), "expires_at": ts(r.3), "revoked_at": ts(r.4), "views": r.5,
    })
}

async fn owned_entry(s: &AppState, me: Uuid, id: Uuid) -> Result<(String, i64, Value)> {
    let row: Option<(String, i64, Value)> = sqlx::query_as(
        "select item_id, quantity, instance from musubi.inventory where id = $1 and account_id = $2",
    )
    .bind(id)
    .bind(me)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    row.ok_or_else(|| Error::NotFound(format!("inventory entry {id}")))
}

fn inspectable(item: &musubi_economy::Item) -> bool {
    item.properties
        .get("inspectable")
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ShareBody {
    ttl_secs: Option<i64>,
}

/// `POST /v1/inventory/{id}/share {ttl_secs?}` — mint (or return the live) inspect link.
async fn share(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(b): Json<ShareBody>,
) -> Result<Json<Value>, ApiError> {
    if b.ttl_secs.is_some_and(|t| !(60..=MAX_TTL).contains(&t)) {
        return Err(Error::InvalidArgument(format!("ttl_secs must be 60..={MAX_TTL}")).into());
    }
    let (item_id, _, _) = owned_entry(&s, me.account_id.0, id).await?;
    let item = s.economy.item(&item_id).await?;
    if !inspectable(&item) {
        return Err(Error::PermissionDenied(format!("item {item_id} cannot be inspected")).into());
    }
    let live: Option<LinkRow> = sqlx::query_as(
        "select token, inventory_id, created_at, expires_at, revoked_at, views from musubi.item_share_links
         where inventory_id = $1 and revoked_at is null and (expires_at is null or expires_at > now())
         order by created_at desc limit 1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if let Some(r) = live {
        return Ok(Json(link_doc(&r)));
    }
    let expires = b
        .ttl_secs
        .map(|t| OffsetDateTime::now_utc() + time::Duration::seconds(t));
    let row: LinkRow = sqlx::query_as(
        "insert into musubi.item_share_links (token, inventory_id, account_id, expires_at)
         values ($1, $2, $3, $4)
         returning token, inventory_id, created_at, expires_at, revoked_at, views",
    )
    .bind(fresh_token())
    .bind(id)
    .bind(me.account_id.0)
    .bind(expires)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "inventory.shared",
            Some(me.account_id),
            json!({ "inventory_id": id, "item_id": item_id, "token": row.0 }),
        )
        .await;
    Ok(Json(link_doc(&row)))
}

/// `DELETE /v1/inventory/{id}/share` — revoke every live link of the entry.
async fn revoke(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    owned_entry(&s, me.account_id.0, id).await?;
    let n = sqlx::query(
        "update musubi.item_share_links set revoked_at = now()
         where inventory_id = $1 and account_id = $2 and revoked_at is null",
    )
    .bind(id)
    .bind(me.account_id.0)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("no live share link for this entry".into()).into());
    }
    s.events
        .emit(
            "inventory.share_revoked",
            Some(me.account_id),
            json!({ "inventory_id": id, "links": n }),
        )
        .await;
    Ok(Json(json!({ "revoked": n })))
}

async fn my_shares(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows: Vec<LinkRow> = sqlx::query_as(
        "select token, inventory_id, created_at, expires_at, revoked_at, views from musubi.item_share_links
         where account_id = $1 and revoked_at is null and (expires_at is null or expires_at > now())
         order by created_at desc limit 200",
    )
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "shares": rows.iter().map(link_doc).collect::<Vec<_>>() }),
    ))
}

/// `GET /v1/inspect/{token}` — public, no session: the item as seen in a profile or market.
async fn inspect(
    State(s): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if token.len() > 64 {
        return Err(Error::NotFound("inspect link".into()).into());
    }
    let row: Option<(Uuid, Uuid, Option<OffsetDateTime>, Option<OffsetDateTime>)> = sqlx::query_as(
        "select inventory_id, account_id, expires_at, revoked_at from musubi.item_share_links where token = $1",
    )
    .bind(&token)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((inv, owner, expires, revoked)) = row else {
        return Err(Error::NotFound("inspect link".into()).into());
    };
    let now = OffsetDateTime::now_utc();
    if revoked.is_some() || expires.is_some_and(|e| e <= now) {
        return Err(Error::NotFound("inspect link expired".into()).into());
    }
    let (item_id, quantity, instance) = owned_entry(&s, owner, inv).await?;
    let item = s.economy.item(&item_id).await?;
    if !inspectable(&item) {
        return Err(Error::NotFound("inspect link".into()).into());
    }
    let owner_row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "select username, display_name, avatar_url from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(owner)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((username, display_name, avatar_url)) = owner_row else {
        return Err(Error::NotFound("inspect link".into()).into());
    };
    let views: i64 = sqlx::query_scalar(
        "update musubi.item_share_links set views = views + 1 where token = $1 returning views",
    )
    .bind(&token)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "token": token,
        "item": {
            "id": item.id, "name": item.name, "category": item.category, "properties": item.properties,
            "version": item.version, "unique": item.unique, "tradeable": item.tradeable,
        },
        "quantity": quantity,
        "instance": instance,
        "owner": { "account_id": owner, "username": username, "display_name": display_name, "avatar_url": avatar_url },
        "views": views,
        "expires_at": ts(expires),
    })))
}
