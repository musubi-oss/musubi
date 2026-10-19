//! Environments & promotion (competitive P0-40). A cluster knows its siblings (stage, prod,
//! PR previews) by name and URL; `promote` exports the chosen content sections here and imports
//! them there — dry-run first for a diff, `protected` environments need an explicit `confirm`.
//! Every attempt lands in `musubi.promotions`.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/environments", get(list))
        .route(
            "/v1/admin/environments/{name}",
            get(get_one).put(put_one).delete(delete_one),
        )
        .route("/v1/admin/environments/{name}/status", get(status))
        .route("/v1/admin/environments/{name}/promote", post(promote))
        .route("/v1/admin/promotions", get(promotions))
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Environment {
    pub name: String,
    pub url: String,
    #[serde(skip_serializing)]
    pub admin_key: String,
    pub protected: bool,
    pub description: String,
}

const SELECT: &str = "select name, url, admin_key, protected, description from musubi.environments";

async fn load(s: &AppState, name: &str) -> Result<Environment> {
    let row: Option<Environment> = sqlx::query_as(&format!("{SELECT} where name = $1"))
        .bind(name)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    row.ok_or_else(|| Error::NotFound(format!("environment {name}")))
}

fn view(e: &Environment) -> Value {
    json!({
        "name": e.name, "url": e.url, "protected": e.protected, "description": e.description,
        "admin_key_set": !e.admin_key.is_empty(),
    })
}

async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<Environment> = sqlx::query_as(&format!("{SELECT} order by name"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(json!({
        "this": { "name": s.cfg.node.environment, "project": s.cfg.node.project },
        "environments": rows.iter().map(view).collect::<Vec<_>>(),
    })))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(view(&load(&s, &name).await?)))
}

#[derive(Deserialize)]
struct EnvBody {
    url: String,
    /// Admin server key of the target cluster; omit on update to keep the stored one.
    #[serde(default)]
    admin_key: Option<String>,
    #[serde(default)]
    protected: bool,
    #[serde(default)]
    description: String,
}

async fn put_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<EnvBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            Error::InvalidArgument("environment name: 1..=32 of [A-Za-z0-9_-]".into()).into(),
        );
    }
    if name == s.cfg.node.environment {
        return Err(Error::InvalidArgument("that is this environment".into()).into());
    }
    let url = b.url.trim().trim_end_matches('/').to_string();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(Error::InvalidArgument("url must be http(s)".into()).into());
    }
    let existing = load(&s, &name).await.ok();
    let key = match (b.admin_key, existing) {
        (Some(k), _) if !k.is_empty() => k,
        (_, Some(e)) => e.admin_key,
        _ => return Err(Error::InvalidArgument("admin_key required".into()).into()),
    };
    sqlx::query(
        "insert into musubi.environments (name, url, admin_key, protected, description) values ($1, $2, $3, $4, $5)
         on conflict (name) do update set url = excluded.url, admin_key = excluded.admin_key,
            protected = excluded.protected, description = excluded.description, updated_at = now()",
    )
    .bind(&name)
    .bind(&url)
    .bind(&key)
    .bind(b.protected)
    .bind(&b.description)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(view(&load(&s, &name).await?)))
}

async fn delete_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.environments where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .unwrap_or_default()
}

/// `GET /v1/admin/environments/{name}/status` — health and latest content version over there.
async fn status(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let e = load(&s, &name).await?;
    let c = client();
    let health = match c.get(format!("{}/healthz", e.url)).send().await {
        Ok(r) => json!({ "ok": r.status().is_success(), "status": r.status().as_u16() }),
        Err(err) => json!({ "ok": false, "error": err.to_string() }),
    };
    let versions = c
        .get(format!("{}/v1/admin/content/versions", e.url))
        .query(&[("limit", "1")])
        .header("x-musubi-server-key", &e.admin_key)
        .send()
        .await;
    let latest = match versions {
        Ok(r) if r.status().is_success() => r
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["versions"].as_array().and_then(|a| a.first().cloned()))
            .unwrap_or(Value::Null),
        Ok(r) => json!({ "error": format!("versions {}", r.status()) }),
        Err(err) => json!({ "error": err.to_string() }),
    };
    Ok(Json(
        json!({ "environment": view(&e), "health": health, "latest_content_version": latest }),
    ))
}

#[derive(Deserialize)]
struct PromoteBody {
    /// Content / config sections (see `/v1/admin/content?sections=`); empty = everything.
    #[serde(default)]
    sections: Vec<String>,
    #[serde(default)]
    prune: bool,
    #[serde(default)]
    note: String,
    #[serde(default)]
    dry_run: bool,
    /// Required for a real import into a `protected` environment.
    #[serde(default)]
    confirm: bool,
}

/// `POST /v1/admin/environments/{name}/promote {sections, prune, note, dry_run, confirm}`
async fn promote(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PromoteBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    let e = load(&s, &name).await?;
    if b.note.len() > 500 {
        return Err(Error::InvalidArgument("note: at most 500 chars".into()).into());
    }
    if e.protected && !b.dry_run && !b.confirm {
        return Err(Error::Conflict(format!(
            "environment {name} is protected: dry-run first, then promote with confirm = true"
        ))
        .into());
    }
    let sections = if b.sections.is_empty() {
        crate::http::content::all_sections()
    } else {
        b.sections.clone()
    };
    let bundle = {
        let mut tx = s.db.begin().await.map_err(Error::internal)?;
        let bundle =
            crate::http::content::export_bundle(&mut tx, &sections, &s.cfg.node.environment)
                .await?;
        tx.commit().await.map_err(Error::internal)?;
        bundle
    };
    let note = if b.note.is_empty() {
        format!("promoted from {}", s.cfg.node.environment)
    } else {
        b.note.clone()
    };
    let res = client()
        .post(format!("{}/v1/admin/content/import", e.url))
        .header("x-musubi-server-key", &e.admin_key)
        .json(&json!({
            "bundle": bundle, "dry_run": b.dry_run, "prune": b.prune, "note": note, "environment": name,
        }))
        .send()
        .await;
    let (ok, result) = match res {
        Ok(r) => {
            let status = r.status();
            let body = r.json::<Value>().await.unwrap_or(Value::Null);
            (
                status.is_success(),
                json!({ "status": status.as_u16(), "body": body }),
            )
        }
        Err(err) => (false, json!({ "error": err.to_string() })),
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.promotions (id, to_env, from_env, sections, dry_run, prune, note, ok, result)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(&name)
    .bind(&s.cfg.node.environment)
    .bind(&sections)
    .bind(b.dry_run)
    .bind(b.prune)
    .bind(&note)
    .bind(ok)
    .bind(&result)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if !b.dry_run {
        s.events
            .emit(
                "content.promoted",
                None,
                json!({ "promotion_id": id, "to": name, "ok": ok, "sections": sections }),
            )
            .await;
    }
    if !ok {
        return Err(Error::Internal(format!("promotion to {name} failed: {result}")).into());
    }
    Ok(Json(json!({
        "promotion_id": id, "to": name, "from": s.cfg.node.environment, "dry_run": b.dry_run,
        "sections": sections, "result": result["body"],
    })))
}

#[derive(Deserialize)]
struct PromotionsQ {
    to: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
}

fn default_limit() -> i64 {
    50
}

async fn promotions(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<PromotionsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    type R = (
        Uuid,
        String,
        String,
        Vec<String>,
        bool,
        bool,
        String,
        bool,
        Value,
        i64,
    );
    let rows: Vec<R> = sqlx::query_as(
        "select id, to_env, from_env, sections, dry_run, prune, note, ok, result, extract(epoch from created_at)::bigint
         from musubi.promotions where ($1::text is null or to_env = $1) order by created_at desc limit $2",
    )
    .bind(q.to.as_deref())
    .bind(q.limit.clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "promotions": rows.iter().map(|(id, to, from, sections, dry_run, prune, note, ok, result, at)| json!({
            "id": id, "to": to, "from": from, "sections": sections, "dry_run": dry_run, "prune": prune,
            "note": note, "ok": ok, "result": result, "created_at": at,
        })).collect::<Vec<_>>(),
    })))
}
