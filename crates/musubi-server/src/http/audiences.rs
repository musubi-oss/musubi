//! Named audiences (plan 5.12): `[rules]` are the mailbox `Audience` (lang, location, platform,
//! age bracket, signup window, progression track level range, activity, metadata) stored under a
//! name. Everyone can ask which audiences an account is in: the matchmaker adds
//! `audience_<name> = "true"` string properties to every ticket (queue rules and `query` can use
//! them), the store hides items whose `audiences` list the account is not in, modules call the
//! `audiences.of` host op. Answers are cached per account for `TTL` on the node.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use musubi_economy::{Audience, EconomyService};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub const TTL: Duration = Duration::from_secs(60);

type Defs = Arc<Vec<(String, Audience)>>;

/// Definitions (reloaded from the table when older than `TTL`) plus per-account answers.
#[derive(Clone, Default)]
pub struct Audiences {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    defs: Option<(Instant, Defs)>,
    members: HashMap<AccountId, (Instant, Arc<Vec<String>>)>,
}

impl Audiences {
    pub fn invalidate(&self) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.defs = None;
        g.members.clear();
    }

    async fn defs(&self, db: &PgPool) -> Result<Defs> {
        if let Some((at, d)) = &self.inner.lock().unwrap_or_else(|e| e.into_inner()).defs {
            if at.elapsed() < TTL {
                return Ok(d.clone());
            }
        }
        let rows: Vec<(String, Value)> =
            sqlx::query_as("select name, rules from musubi.audiences order by name")
                .fetch_all(db)
                .await
                .map_err(Error::internal)?;
        let defs: Vec<(String, Audience)> = rows
            .into_iter()
            .filter_map(|(n, r)| serde_json::from_value(r).ok().map(|a| (n, a)))
            .collect();
        let defs = Arc::new(defs);
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).defs =
            Some((Instant::now(), defs.clone()));
        Ok(defs)
    }

    /// Audiences `account` is in right now (cached `TTL`).
    pub async fn of(
        &self,
        db: &PgPool,
        economy: &EconomyService,
        account: AccountId,
    ) -> Result<Arc<Vec<String>>> {
        if let Some((at, m)) = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .members
            .get(&account)
        {
            if at.elapsed() < TTL {
                return Ok(m.clone());
            }
        }
        let defs = self.defs(db).await?;
        let names = if defs.is_empty() {
            Vec::new()
        } else {
            economy
                .audiences_of(account, &defs, time::OffsetDateTime::now_utc())
                .await?
        };
        let names = Arc::new(names);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.members.retain(|_, (at, _)| at.elapsed() < TTL);
        g.members.insert(account, (Instant::now(), names.clone()));
        Ok(names)
    }
}

fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument(
            "audience name: 1..64 chars of [A-Za-z0-9_-]".into(),
        ))
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/audiences", get(list))
        .route(
            "/v1/server/audiences/{name}",
            axum::routing::put(put).delete(delete_one),
        )
        .route("/v1/server/audiences/of/{account_id}", get(of_account))
}

type Row = (String, Value, String, i64);

fn doc((name, rules, description, updated_at): Row) -> Value {
    json!({ "name": name, "rules": rules, "description": description, "updated_at": updated_at })
}

async fn list(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(
        "select name, rules, description, extract(epoch from updated_at)::bigint
         from musubi.audiences order by name",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "audiences": rows.into_iter().map(doc).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
struct PutBody {
    rules: Audience,
    #[serde(default)]
    description: String,
}

async fn put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    check_name(&name)?;
    b.rules.validate_rules()?;
    if b.description.len() > 500 {
        return Err(Error::InvalidArgument("description: at most 500 chars".into()).into());
    }
    let count = s
        .economy
        .count_audience(&b.rules, time::OffsetDateTime::now_utc())
        .await?;
    let row: Row = sqlx::query_as(
        "insert into musubi.audiences (name, rules, description) values ($1, $2, $3)
         on conflict (name) do update set rules = excluded.rules,
             description = excluded.description, updated_at = now()
         returning name, rules, description, extract(epoch from updated_at)::bigint",
    )
    .bind(&name)
    .bind(serde_json::to_value(&b.rules).map_err(Error::internal)?)
    .bind(&b.description)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.audiences.invalidate();
    let mut v = doc(row);
    v["count"] = json!(count);
    Ok(Json(v))
}

async fn delete_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.audiences where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    s.audiences.invalidate();
    Ok(Json(json!({ "deleted": n > 0 })))
}

async fn of_account(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(account_id): Path<uuid::Uuid>,
) -> Result<Json<Value>, ApiError> {
    let names = s
        .audiences
        .of(&s.db, &s.economy, AccountId(account_id))
        .await?;
    Ok(Json(
        json!({ "account_id": account_id, "audiences": *names }),
    ))
}
