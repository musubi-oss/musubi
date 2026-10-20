//! Developer players (Metaplay parity): team accounts flagged by an operator. They get into
//! `maintenance` / `limited` access mode, carry the virtual audience `developers` (so offers,
//! news, live events, flags and experiments can be developer-only through their normal
//! `audience` field) and may run development-only actions in production.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use super::{ApiError, Bearer, ServerKey};
use crate::app::AppState;

/// Virtual audience every developer player is a member of.
pub const AUDIENCE: &str = "developers";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/account/developer", get(me))
        .route("/v1/admin/developers", get(list))
        .route(
            "/v1/admin/accounts/{id}/developer",
            get(get_one).put(set).delete(unset),
        )
}

pub async fn is_developer(db: &PgPool, account: AccountId) -> Result<bool> {
    sqlx::query_scalar(
        "select exists(select 1 from musubi.developer_players where account_id = $1)",
    )
    .bind(account.0)
    .fetch_one(db)
    .await
    .map_err(Error::internal)
}

/// Guard for `[DevelopmentOnlyAction]`-style endpoints: open outside `production`, otherwise
/// only developer players pass.
pub async fn development_only(s: &AppState, account: AccountId) -> Result<()> {
    if s.cfg.dev.environment != "production" || is_developer(&s.db, account).await? {
        return Ok(());
    }
    Err(Error::PermissionDenied(
        "development-only action: this account is not a developer player".into(),
    ))
}

type Row = (Uuid, String, String, time::OffsetDateTime);
const COLS: &str = "account_id, note, set_by, created_at";

fn card(r: &Row) -> Value {
    json!({
        "account_id": r.0, "developer": true, "note": r.1, "set_by": r.2,
        "created_at": r.3.format(&Rfc3339).ok(),
    })
}

async fn me(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let dev = is_developer(&s.db, me.account_id).await?;
    Ok(Json(json!({
        "developer": dev,
        "environment": s.cfg.dev.environment,
        "development_actions": development_only(&s, me.account_id).await.is_ok(),
    })))
}

async fn list(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.developer_players order by created_at"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "developers": rows.iter().map(card).collect::<Vec<_>>() }),
    ))
}

async fn get_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.developer_players where account_id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(match row {
        Some(r) => card(&r),
        None => json!({ "account_id": id, "developer": false }),
    }))
}

#[derive(Deserialize, Default)]
struct SetBody {
    #[serde(default)]
    note: String,
    #[serde(default)]
    set_by: String,
}

async fn set(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
    body: Option<Json<SetBody>>,
) -> Result<Json<Value>, ApiError> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    if b.note.len() > 512 || b.set_by.len() > 128 {
        return Err(Error::InvalidArgument("note <= 512 chars, set_by <= 128".into()).into());
    }
    s.accounts.get(AccountId(id)).await?;
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.developer_players (account_id, note, set_by) values ($1, $2, $3)
         on conflict (account_id) do update set note = excluded.note, set_by = excluded.set_by
         returning {COLS}"
    ))
    .bind(id)
    .bind(&b.note)
    .bind(&b.set_by)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.audiences.invalidate();
    s.events
        .emit(
            "account.developer_set",
            Some(AccountId(id)),
            json!({ "note": b.note, "set_by": b.set_by }),
        )
        .await;
    Ok(Json(card(&row)))
}

async fn unset(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.developer_players where account_id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("developer player".into()).into());
    }
    s.audiences.invalidate();
    s.events
        .emit("account.developer_unset", Some(AccountId(id)), json!({}))
        .await;
    Ok(Json(json!({ "account_id": id, "developer": false })))
}
