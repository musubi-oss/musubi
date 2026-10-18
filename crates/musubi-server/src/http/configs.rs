//! Typed game configs (plan 3.8): JSON documents the server writes and clients read, validated
//! against `[configs.<name>]` of the deployed IDL when declared (free-form otherwise).

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/configs", get(list))
        .route("/v1/configs/{name}", get(get_one))
        .route("/v1/server/configs", get(list_server))
        .route(
            "/v1/server/configs/{name}",
            axum::routing::put(put).delete(delete),
        )
}

type Row = (String, Value, i32, i64);

fn doc((name, value, version, updated_at): Row) -> Value {
    json!({ "name": name, "value": value, "version": version, "updated_at": updated_at })
}

fn check_name(name: &str) -> Result<(), ApiError> {
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

async fn all(s: &AppState) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(
        "select name, value, version, extract(epoch from updated_at)::bigint
         from musubi.configs order by name",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "configs": rows.into_iter().map(doc).collect::<Vec<_>>() }),
    ))
}

async fn list(State(s): State<AppState>, Bearer(_): Bearer) -> Result<Json<Value>, ApiError> {
    all(&s).await
}

async fn list_server(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    all(&s).await
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<Row> = sqlx::query_as(
        "select name, value, version, extract(epoch from updated_at)::bigint
         from musubi.configs where name = $1",
    )
    .bind(&name)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match row {
        Some(r) => Ok(Json(doc(r))),
        None => Err(Error::NotFound(format!("config `{name}`")).into()),
    }
}

#[derive(Deserialize)]
struct PutBody {
    value: Value,
}

async fn put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    check_name(&name)?;
    if serde_json::to_vec(&b.value).map(|v| v.len()).unwrap_or(0) > 1 << 20 {
        return Err(Error::InvalidArgument("config value: at most 1 MiB".into()).into());
    }
    let version = s.idl.check_config(&name, &b.value)?;
    let row: Row = sqlx::query_as(
        "insert into musubi.configs (name, value, version) values ($1, $2, $3)
         on conflict (name) do update set value = excluded.value, version = excluded.version,
             updated_at = now()
         returning name, value, version, extract(epoch from updated_at)::bigint",
    )
    .bind(&name)
    .bind(&b.value)
    .bind(version)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(doc(row)))
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
    Ok(Json(json!({ "deleted": n > 0 })))
}
