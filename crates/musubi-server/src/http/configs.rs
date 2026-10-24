//! Typed game configs (plan 3.8): JSON documents the server writes and clients read, validated
//! against `[configs.<name>]` of the deployed IDL when declared (free-form otherwise).

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::remote_config::{ctx_from, resolve, Ctx};
use crate::http::{ApiError, Bearer, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/configs", get(list))
        .route("/v1/configs/{name}", get(get_one))
        .route("/v1/server/configs", get(list_server))
        .route(
            "/v1/server/configs/{name}",
            get(get_server).put(put).delete(delete),
        )
}

type Row = (String, Value, i32, i64);

fn doc((name, value, version, updated_at): Row) -> Value {
    json!({ "name": name, "value": value, "version": version, "updated_at": updated_at })
}

pub fn check_name(name: &str) -> Result<(), ApiError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument("config name: 1..64 chars of [A-Za-z0-9_.-]".into()).into())
    }
}

async fn list(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let configs = resolve(&s, None, &ctx, Some(me.account_id)).await?;
    Ok(Json(json!({ "configs": configs })))
}

async fn list_server(
    State(s): State<AppState>,
    _k: ServerKey,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let configs = resolve(&s, None, &ctx, None).await?;
    Ok(Json(json!({ "configs": configs })))
}

async fn get_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let mut configs = resolve(&s, Some(&name), &ctx, None).await?;
    match configs.pop() {
        Some(c) => Ok(Json(c)),
        None => Err(Error::NotFound(format!("config `{name}`")).into()),
    }
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let mut configs = resolve(&s, Some(&name), &ctx, Some(me.account_id)).await?;
    match configs.pop() {
        Some(c) => Ok(Json(c)),
        None => Err(Error::NotFound(format!("config `{name}`")).into()),
    }
}

/// Validates against the IDL, upserts, records a history entry. Shared with CSV import and
/// rollback.
pub async fn store(
    s: &AppState,
    name: &str,
    value: &Value,
    note: Option<&str>,
) -> Result<Value, ApiError> {
    check_name(name)?;
    if serde_json::to_vec(value).map(|v| v.len()).unwrap_or(0) > 1 << 20 {
        return Err(Error::InvalidArgument("config value: at most 1 MiB".into()).into());
    }
    let version = s.idl.check_config(name, value)?;
    let row: Row = sqlx::query_as(
        "insert into musubi.configs (name, value, version) values ($1, $2, $3)
         on conflict (name) do update set value = excluded.value, version = excluded.version,
             updated_at = now()
         returning name, value, version, extract(epoch from updated_at)::bigint",
    )
    .bind(name)
    .bind(value)
    .bind(version)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    crate::http::remote_config::record(s, name, version, Some(value), note).await;
    s.events
        .emit(
            "config.updated",
            None,
            json!({ "name": name, "version": version }),
        )
        .await;
    Ok(doc(row))
}

#[derive(Deserialize)]
struct PutBody {
    value: Value,
    #[serde(default)]
    note: Option<String>,
}

async fn put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(store(&s, &name, &b.value, b.note.as_deref()).await?))
}

async fn delete(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.configs where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n > 0 {
        crate::http::remote_config::record(&s, &name, 0, None, Some("deleted")).await;
    }
    Ok(Json(json!({ "deleted": n > 0 })))
}
