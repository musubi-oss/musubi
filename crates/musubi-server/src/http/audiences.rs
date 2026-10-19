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
        let rows: Vec<(String, Value)> = sqlx::query_as(
            "select name, rules from musubi.audiences where kind = 'rule' order by name",
        )
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
        let names = Arc::new(apply_overrides(db, account, names).await?);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.members.retain(|_, (at, _)| at.elapsed() < TTL);
        g.members.insert(account, (Instant::now(), names.clone()));
        Ok(names)
    }
}

/// Managed (imported) memberships, then identity overrides (P0-19): `include` forces the
/// account in regardless of rules, `exclude` forces it out; expired overrides are ignored.
async fn apply_overrides(
    db: &PgPool,
    account: AccountId,
    mut names: Vec<String>,
) -> Result<Vec<String>> {
    let managed: Vec<(String,)> = sqlx::query_as(
        "select m.audience from musubi.audience_managed_members m
         join musubi.audiences a on a.name = m.audience
         where m.account_id = $1 and a.kind = 'managed' order by m.audience",
    )
    .bind(account.0)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    for (n,) in managed {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    let overrides: Vec<(String, String)> = sqlx::query_as(
        "select audience, mode from musubi.audience_overrides
         where account_id = $1 and (expires_at is null or expires_at > now()) order by audience",
    )
    .bind(account.0)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    for (n, mode) in overrides {
        if mode == "include" {
            if !names.contains(&n) {
                names.push(n);
            }
        } else {
            names.retain(|x| x != &n);
        }
    }
    Ok(names)
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
        .route(
            "/v1/server/audiences/{name}/members",
            get(members).put(put_members),
        )
        .route(
            "/v1/server/audiences/{name}/members/csv",
            post(put_members_csv),
        )
        .route(
            "/v1/server/audiences/{name}/overrides",
            get(list_overrides)
                .put(put_overrides)
                .delete(clear_overrides),
        )
        .route(
            "/v1/server/audiences/{name}/overrides/{account_id}",
            axum::routing::delete(delete_override),
        )
        .route("/v1/server/audiences/{name}/refresh", post(refresh_one))
        .route("/v1/audiences/me", get(me))
}

type Row = (String, Value, String, i64, String, Option<String>);

fn doc((name, rules, description, updated_at, kind, source): Row) -> Value {
    json!({
        "name": name, "rules": rules, "description": description, "updated_at": updated_at,
        "kind": kind, "source": source,
    })
}

type ListRow = (
    String,
    Value,
    String,
    i64,
    String,
    Option<String>,
    i64,
    i64,
    i64,
);

async fn list(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let rows: Vec<ListRow> = sqlx::query_as(
        "select a.name, a.rules, a.description, extract(epoch from a.updated_at)::bigint, a.kind, a.source,
            (select count(*) from musubi.audience_overrides o where o.audience = a.name and o.mode = 'include' and (o.expires_at is null or o.expires_at > now())),
            (select count(*) from musubi.audience_overrides o where o.audience = a.name and o.mode = 'exclude' and (o.expires_at is null or o.expires_at > now())),
            (select count(*) from musubi.audience_managed_members m where m.audience = a.name)
         from musubi.audiences a order by a.name",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "audiences": rows.into_iter().map(|(n, r, d, u, k, src, inc, exc, mem)| {
            let mut v = doc((n, r, d, u, k, src));
            v["overrides"] = json!({ "include": inc, "exclude": exc });
            v["managed_members"] = json!(mem);
            v
        }).collect::<Vec<_>>(),
    })))
}

fn rule_kind() -> String {
    "rule".into()
}

#[derive(Deserialize)]
struct PutBody {
    #[serde(default)]
    rules: Audience,
    /// `rule` (membership from `rules`) | `managed` (membership imported via
    /// `PUT …/members` / `POST …/members/csv`, e.g. a segment exported from analytics).
    #[serde(default = "rule_kind")]
    kind: String,
    #[serde(default)]
    source: Option<String>,
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
    if !matches!(b.kind.as_str(), "rule" | "managed") {
        return Err(Error::InvalidArgument("kind: rule|managed".into()).into());
    }
    if b.source
        .as_ref()
        .is_some_and(|x| x.is_empty() || x.len() > 64)
    {
        return Err(Error::InvalidArgument("source: 1..64 chars".into()).into());
    }
    let rules = serde_json::to_value(&b.rules).map_err(Error::internal)?;
    if b.kind == "managed"
        && rules != serde_json::to_value(Audience::default()).map_err(Error::internal)?
    {
        return Err(Error::InvalidArgument(
            "managed audiences have no rules — import members via PUT …/members".into(),
        )
        .into());
    }
    let count = if b.kind == "managed" {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from musubi.audience_managed_members where audience = $1",
        )
        .bind(&name)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?
    } else {
        s.economy
            .count_audience(&b.rules, time::OffsetDateTime::now_utc())
            .await?
    };
    let row: Row = sqlx::query_as(
        "insert into musubi.audiences (name, rules, description, tracked, actions, kind, source) values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (name) do update set rules = excluded.rules,
             description = excluded.description, tracked = excluded.tracked,
             actions = excluded.actions, kind = excluded.kind, source = excluded.source, updated_at = now()
         returning name, rules, description, extract(epoch from updated_at)::bigint, kind, source",
    )
    .bind(&name)
    .bind(rules)
    .bind(&b.description)
    .bind(b.tracked)
    .bind(serde_json::to_value(&b.actions).map_err(Error::internal)?)
    .bind(&b.kind)
    .bind(&b.source)
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
    let row: Option<(bool, String)> =
        sqlx::query_as("select tracked, kind from musubi.audiences where name = $1")
            .bind(&name)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let Some((tracked, kind)) = row else {
        return Err(Error::NotFound("audience".into()).into());
    };
    let (table, at) = if kind == "managed" {
        ("audience_managed_members", "imported_at")
    } else {
        ("audience_members", "entered_at")
    };
    let rows: Vec<(uuid::Uuid, i64)> = sqlx::query_as(&format!(
        "select account_id, extract(epoch from {at})::bigint from musubi.{table}
         where audience = $1 order by {at} desc limit $2 offset $3"
    ))
    .bind(&name)
    .bind(q.limit.clamp(1, 1000))
    .bind(q.offset.max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let total: i64 = sqlx::query_scalar(&format!(
        "select count(*) from musubi.{table} where audience = $1"
    ))
    .bind(&name)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "audience": name, "tracked": tracked, "kind": kind, "total": total,
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
    let actions: Actions = serde_json::from_value(actions.clone()).unwrap_or_default();
    let now = time::OffsetDateTime::now_utc();
    let kind: Option<String> =
        sqlx::query_scalar("select kind from musubi.audiences where name = $1")
            .bind(name)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let current: Vec<uuid::Uuid> = if kind.as_deref() == Some("managed") {
        sqlx::query_scalar(
            "select account_id from musubi.audience_managed_members where audience = $1",
        )
        .bind(name)
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?
    } else {
        let aud: Audience = serde_json::from_value(rules.clone()).map_err(Error::internal)?;
        s.economy
            .audience_accounts(&aud, now, TRACK_LIMIT)
            .await?
            .into_iter()
            .map(|a| a.0)
            .collect()
    };
    let current: std::collections::HashSet<uuid::Uuid> = current.into_iter().collect();
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

// ---------------------------------------------------------------- overrides & managed members (P0-19)

async fn audience_kind(s: &AppState, name: &str) -> Result<(String, bool, Value, Value)> {
    let row: Option<(String, bool, Value, Value)> = sqlx::query_as(
        "select kind, tracked, rules, actions from musubi.audiences where name = $1",
    )
    .bind(name)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    row.ok_or_else(|| Error::NotFound("audience".into()))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OverridesBody {
    include: Vec<uuid::Uuid>,
    exclude: Vec<uuid::Uuid>,
    note: String,
    expires_in_secs: Option<i64>,
}

async fn overrides_doc(s: &AppState, name: &str) -> Result<Value> {
    sqlx::query(
        "delete from musubi.audience_overrides where audience = $1 and expires_at <= now()",
    )
    .bind(name)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let rows: Vec<(uuid::Uuid, String, String, Option<i64>, i64)> = sqlx::query_as(
        "select account_id, mode, note, extract(epoch from expires_at)::bigint, extract(epoch from created_at)::bigint
         from musubi.audience_overrides where audience = $1 order by created_at desc, account_id limit 10000",
    )
    .bind(name)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let pick = |m: &str| -> Vec<Value> {
        rows.iter()
            .filter(|r| r.1 == m)
            .map(|(a, _, n, e, c)| json!({ "account_id": a, "note": n, "expires_at": e, "created_at": c }))
            .collect()
    };
    Ok(json!({ "audience": name, "include": pick("include"), "exclude": pick("exclude") }))
}

/// `GET /v1/server/audiences/{name}/overrides` — identity include/exclude lists.
async fn list_overrides(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    audience_kind(&s, &name).await?;
    Ok(Json(overrides_doc(&s, &name).await?))
}

/// `PUT /v1/server/audiences/{name}/overrides {include: [ids], exclude: [ids], note, expires_in_secs}`
/// — force accounts in/out regardless of rules or imports (QA, VIPs, support cases).
async fn put_overrides(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<OverridesBody>,
) -> Result<Json<Value>, ApiError> {
    audience_kind(&s, &name).await?;
    if b.include.is_empty() && b.exclude.is_empty() {
        return Err(Error::InvalidArgument("include or exclude required".into()).into());
    }
    if b.include.len() + b.exclude.len() > 1000 {
        return Err(Error::InvalidArgument("at most 1000 ids per call".into()).into());
    }
    if b.include.iter().any(|i| b.exclude.contains(i)) {
        return Err(
            Error::InvalidArgument("an id cannot be both included and excluded".into()).into(),
        );
    }
    if b.expires_in_secs.is_some_and(|e| e <= 0) || b.note.len() > 500 {
        return Err(Error::InvalidArgument("expires_in_secs > 0; note <= 500 chars".into()).into());
    }
    for (mode, ids) in [("include", &b.include), ("exclude", &b.exclude)] {
        if ids.is_empty() {
            continue;
        }
        sqlx::query(
            "insert into musubi.audience_overrides (audience, account_id, mode, note, expires_at)
             select $1, unnest($2::uuid[]), $3, $4,
                    case when $5::bigint is null then null else now() + ($5::bigint * interval '1 second') end
             on conflict (audience, account_id) do update set mode = excluded.mode, note = excluded.note,
                expires_at = excluded.expires_at, created_at = now()",
        )
        .bind(&name)
        .bind(ids)
        .bind(mode)
        .bind(&b.note)
        .bind(b.expires_in_secs)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    s.audiences.invalidate();
    s.events
        .emit(
            "audience.overrides_updated",
            None,
            json!({ "audience": name, "include": b.include.len(), "exclude": b.exclude.len() }),
        )
        .await;
    Ok(Json(overrides_doc(&s, &name).await?))
}

async fn delete_override(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, account_id)): Path<(String, uuid::Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query(
        "delete from musubi.audience_overrides where audience = $1 and account_id = $2",
    )
    .bind(&name)
    .bind(account_id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    s.audiences.invalidate();
    Ok(Json(json!({ "removed": n > 0 })))
}

async fn clear_overrides(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.audience_overrides where audience = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    s.audiences.invalidate();
    Ok(Json(json!({ "removed": n })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MembersBody {
    account_ids: Vec<uuid::Uuid>,
    mode: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ModeQ {
    mode: Option<String>,
}

const IMPORT_LIMIT: usize = 10_000;

/// Apply an import to a managed audience: `replace` (default) | `add` | `remove`.
async fn import_members(
    s: &AppState,
    name: &str,
    ids: &[uuid::Uuid],
    mode: &str,
    invalid: usize,
) -> Result<Value> {
    let (kind, tracked, rules, actions) = audience_kind(s, name).await?;
    if kind != "managed" {
        return Err(Error::Conflict(
            "audience is rule-based — change kind to managed to import members".into(),
        ));
    }
    if !matches!(mode, "replace" | "add" | "remove") {
        return Err(Error::InvalidArgument("mode: replace|add|remove".into()));
    }
    if ids.len() > IMPORT_LIMIT {
        return Err(Error::InvalidArgument(format!(
            "at most {IMPORT_LIMIT} ids per call"
        )));
    }
    let unknown: i64 = sqlx::query_scalar(
        "select count(*) from unnest($1::uuid[]) x where not exists (select 1 from musubi.accounts a where a.id = x)",
    )
    .bind(ids)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let mut removed = 0u64;
    if mode == "replace" {
        removed = sqlx::query(
            "delete from musubi.audience_managed_members where audience = $1 and not (account_id = any($2))",
        )
        .bind(name)
        .bind(ids)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    }
    let added = if mode == "remove" {
        removed = sqlx::query(
            "delete from musubi.audience_managed_members where audience = $1 and account_id = any($2)",
        )
        .bind(name)
        .bind(ids)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        0
    } else {
        sqlx::query(
            "insert into musubi.audience_managed_members (audience, account_id)
             select $1, x from unnest($2::uuid[]) x where exists (select 1 from musubi.accounts a where a.id = x)
             on conflict do nothing",
        )
        .bind(name)
        .bind(ids)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected()
    };
    tx.commit().await.map_err(Error::internal)?;
    let total: i64 = sqlx::query_scalar(
        "select count(*) from musubi.audience_managed_members where audience = $1",
    )
    .bind(name)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.audiences.invalidate();
    let mut tracking = Value::Null;
    if tracked {
        tracking = track(s, name, &rules, &actions).await?;
    }
    s.events
        .emit(
            "audience.imported",
            None,
            json!({ "audience": name, "mode": mode, "added": added, "removed": removed, "total": total }),
        )
        .await;
    Ok(json!({
        "audience": name, "mode": mode, "total": total, "added": added, "removed": removed,
        "unknown": unknown, "invalid": invalid, "tracking": tracking,
    }))
}

/// `PUT /v1/server/audiences/{name}/members {account_ids, mode}` — import a managed audience.
async fn put_members(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<MembersBody>,
) -> Result<Json<Value>, ApiError> {
    let mode = b.mode.unwrap_or_else(|| "replace".into());
    Ok(Json(
        import_members(&s, &name, &b.account_ids, &mode, 0).await?,
    ))
}

/// `POST /v1/server/audiences/{name}/members/csv?mode=` — one account id per line (header
/// `account_id`/`id` and extra columns ignored); malformed lines are counted as `invalid`.
async fn put_members_csv(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Query(q): Query<ModeQ>,
    body: String,
) -> Result<Json<Value>, ApiError> {
    let mut ids = Vec::new();
    let mut invalid = 0usize;
    for line in body.lines() {
        let cell = line
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"');
        if cell.is_empty()
            || cell.eq_ignore_ascii_case("account_id")
            || cell.eq_ignore_ascii_case("id")
        {
            continue;
        }
        match cell.parse::<uuid::Uuid>() {
            Ok(u) => ids.push(u),
            Err(_) => invalid += 1,
        }
    }
    if ids.is_empty() && invalid > 0 {
        return Err(Error::InvalidArgument("no valid account ids in CSV".into()).into());
    }
    let mode = q.mode.unwrap_or_else(|| "replace".into());
    Ok(Json(import_members(&s, &name, &ids, &mode, invalid).await?))
}
