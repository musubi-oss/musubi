//! Named audiences (plan 5.12): `[rules]` are the mailbox `Audience` (lang, location, platform,
//! age bracket, signup window, progression track level range, activity, metadata) stored under a
//! name. Everyone can ask which audiences an account is in: the matchmaker adds
//! `audience_<name> = "true"` string properties to every ticket (queue rules and `query` can use
//! them), the store hides items whose `audiences` list the account is not in, modules call the
//! `audiences.of` host op. Answers are cached per account for `TTL` on the node.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use musubi_economy::{Audience, EconomyService};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

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
        .route("/v1/server/audiences/refresh", post(refresh_all))
        .route("/v1/server/audiences/{name}/members", get(members))
        .route("/v1/server/audiences/{name}/refresh", post(refresh_one))
        .route("/v1/audiences/me", get(me))
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
    /// Keep a member table and run `actions` on every change (leader sweep, every 5 min, or
    /// `POST …/{name}/refresh`).
    #[serde(default)]
    tracked: bool,
    #[serde(default)]
    actions: Actions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Actions {
    pub on_enter: Vec<Action>,
    pub on_exit: Vec<Action>,
}

/// What happens to an account entering / leaving a tracked audience.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Grant a reward table (idempotent per audience + account + direction + day).
    RewardTable { table: String },
    /// In-app notification.
    Notification {
        subject: String,
        #[serde(default)]
        content: Value,
        #[serde(default = "default_code")]
        code: i32,
    },
    /// Emit `<name>` on the event bus (webhooks, modules, Zanshin).
    Event { name: String },
    /// Merge `set` into `accounts.metadata`.
    Metadata { set: Value },
}

fn default_code() -> i32 {
    0
}

fn validate_actions(a: &Actions) -> Result<()> {
    if a.on_enter.len() + a.on_exit.len() > 16 {
        return Err(Error::InvalidArgument("at most 16 actions".into()));
    }
    for x in a.on_enter.iter().chain(a.on_exit.iter()) {
        match x {
            Action::RewardTable { table } if table.is_empty() => {
                return Err(Error::InvalidArgument("reward_table.table".into()))
            }
            Action::Notification { subject, .. } if subject.is_empty() || subject.len() > 256 => {
                return Err(Error::InvalidArgument(
                    "notification.subject: 1..256".into(),
                ))
            }
            Action::Event { name }
                if name.is_empty()
                    || name.len() > 128
                    || !name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-') =>
            {
                return Err(Error::InvalidArgument(
                    "event.name: [A-Za-z0-9._-]{1,128}".into(),
                ))
            }
            Action::Metadata { set } if !set.is_object() => {
                return Err(Error::InvalidArgument(
                    "metadata.set must be an object".into(),
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

async fn put(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    check_name(&name)?;
    validate_actions(&b.actions)?;
    b.rules.validate_rules()?;
    if b.description.len() > 500 {
        return Err(Error::InvalidArgument("description: at most 500 chars".into()).into());
    }
    let count = s
        .economy
        .count_audience(&b.rules, time::OffsetDateTime::now_utc())
        .await?;
    let row: Row = sqlx::query_as(
        "insert into musubi.audiences (name, rules, description, tracked, actions) values ($1, $2, $3, $4, $5)
         on conflict (name) do update set rules = excluded.rules,
             description = excluded.description, tracked = excluded.tracked,
             actions = excluded.actions, updated_at = now()
         returning name, rules, description, extract(epoch from updated_at)::bigint",
    )
    .bind(&name)
    .bind(serde_json::to_value(&b.rules).map_err(Error::internal)?)
    .bind(&b.description)
    .bind(b.tracked)
    .bind(serde_json::to_value(&b.actions).map_err(Error::internal)?)
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

/// `GET /v1/audiences/me` — the caller's audiences (same answer modules / the matchmaker see).
async fn me(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let names = s.audiences.of(&s.db, &s.economy, me.account_id).await?;
    Ok(Json(json!({ "audiences": *names })))
}

#[derive(Deserialize)]
struct PageQ {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

fn default_limit() -> i64 {
    100
}

/// `GET /v1/server/audiences/{name}/members` — tracked membership (since the last refresh).
async fn members(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Query(q): Query<PageQ>,
) -> Result<Json<Value>, ApiError> {
    let tracked: Option<bool> =
        sqlx::query_scalar("select tracked from musubi.audiences where name = $1")
            .bind(&name)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some(tracked) = tracked else {
        return Err(Error::NotFound("audience".into()).into());
    };
    let rows: Vec<(uuid::Uuid, i64)> = sqlx::query_as(
        "select account_id, extract(epoch from entered_at)::bigint from musubi.audience_members
         where audience = $1 order by entered_at desc limit $2 offset $3",
    )
    .bind(&name)
    .bind(q.limit.clamp(1, 1000))
    .bind(q.offset.max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let total: i64 =
        sqlx::query_scalar("select count(*) from musubi.audience_members where audience = $1")
            .bind(&name)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    Ok(Json(json!({
        "audience": name, "tracked": tracked, "total": total,
        "members": rows.iter().map(|(id, at)| json!({ "account_id": id, "entered_at": at })).collect::<Vec<_>>(),
    })))
}

async fn refresh_one(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<(Value, Value, bool)> =
        sqlx::query_as("select rules, actions, tracked from musubi.audiences where name = $1")
            .bind(&name)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((rules, actions, _)) = row else {
        return Err(Error::NotFound("audience".into()).into());
    };
    let r = track(&s, &name, &rules, &actions).await?;
    Ok(Json(r))
}

async fn refresh_all(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "audiences": track_all(&s).await? })))
}

/// Leader sweep: re-evaluate every tracked audience.
pub async fn track_all(s: &AppState) -> Result<Vec<Value>> {
    let rows: Vec<(String, Value, Value)> = sqlx::query_as(
        "select name, rules, actions from musubi.audiences where tracked order by name",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out = Vec::new();
    for (name, rules, actions) in rows {
        match track(s, &name, &rules, &actions).await {
            Ok(v) => out.push(v),
            Err(e) => tracing::warn!(audience = %name, error = %e, "audience refresh failed"),
        }
    }
    Ok(out)
}

/// Members cap per refresh; bigger audiences are tracked by their oldest accounts first.
const TRACK_LIMIT: i64 = 100_000;
/// Actions run per refresh per direction (the rest catch up on the next pass).
const ACTION_LIMIT: usize = 5_000;

/// Diff the live rule result against the member table, run enter/exit actions, emit events.
async fn track(s: &AppState, name: &str, rules: &Value, actions: &Value) -> Result<Value> {
    let aud: Audience = serde_json::from_value(rules.clone()).map_err(Error::internal)?;
    let actions: Actions = serde_json::from_value(actions.clone()).unwrap_or_default();
    let now = time::OffsetDateTime::now_utc();
    let current = s.economy.audience_accounts(&aud, now, TRACK_LIMIT).await?;
    let current: std::collections::HashSet<uuid::Uuid> = current.into_iter().map(|a| a.0).collect();
    let prev: Vec<(uuid::Uuid,)> =
        sqlx::query_as("select account_id from musubi.audience_members where audience = $1")
            .bind(name)
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    let prev: std::collections::HashSet<uuid::Uuid> = prev.into_iter().map(|(a,)| a).collect();
    let entered: Vec<uuid::Uuid> = current.difference(&prev).copied().collect();
    let exited: Vec<uuid::Uuid> = prev.difference(&current).copied().collect();
    if !entered.is_empty() {
        sqlx::query(
            "insert into musubi.audience_members (audience, account_id)
             select $1, unnest($2::uuid[]) on conflict do nothing",
        )
        .bind(name)
        .bind(&entered)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    if !exited.is_empty() {
        sqlx::query(
            "delete from musubi.audience_members where audience = $1 and account_id = any($2)",
        )
        .bind(name)
        .bind(&exited)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    for id in entered.iter().take(ACTION_LIMIT) {
        run_actions(s, name, "entered", *id, &actions.on_enter).await;
    }
    for id in exited.iter().take(ACTION_LIMIT) {
        run_actions(s, name, "exited", *id, &actions.on_exit).await;
    }
    if !entered.is_empty() || !exited.is_empty() {
        s.audiences.invalidate();
    }
    Ok(json!({
        "audience": name, "members": current.len(), "entered": entered.len(), "exited": exited.len(),
    }))
}

async fn run_actions(s: &AppState, name: &str, dir: &str, id: uuid::Uuid, actions: &[Action]) {
    let acc = AccountId(id);
    s.events
        .emit(
            &format!("audience.{dir}"),
            Some(acc),
            json!({ "audience": name }),
        )
        .await;
    for a in actions {
        let res: Result<()> = match a {
            Action::RewardTable { table } => {
                let day = time::OffsetDateTime::now_utc().date();
                let reference = format!("audience:{name}:{dir}:{day}");
                s.economy
                    .grant_table(acc, table, &reference)
                    .await
                    .map(|_| ())
            }
            Action::Notification {
                subject,
                content,
                code,
            } => s
                .notify
                .send(vec![musubi_social::NewNotification {
                    user_id: acc,
                    sender_id: None,
                    subject: subject.clone(),
                    content: content.clone(),
                    code: *code,
                    persistent: true,
                    ttl_secs: None,
                }])
                .await
                .map(|_| ()),
            Action::Event { name: ev } => {
                s.events
                    .emit(ev, Some(acc), json!({ "audience": name, "direction": dir }))
                    .await;
                Ok(())
            }
            Action::Metadata { set } => {
                sqlx::query("update musubi.accounts set metadata = metadata || $2 where id = $1")
                    .bind(id)
                    .bind(set)
                    .execute(&s.db)
                    .await
                    .map(|_| ())
                    .map_err(Error::internal)
            }
        };
        if let Err(e) = res {
            tracing::warn!(audience = %name, account = %id, error = %e, "audience action failed");
        }
    }
}
