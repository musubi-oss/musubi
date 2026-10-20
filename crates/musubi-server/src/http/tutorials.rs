//! Tutorial sequences with step tracking (Hiro parity). The server defines tutorials
//! (`PUT /v1/server/tutorials/{id}` — `start_step`, `max_step`, optional `reward_table`, labels);
//! each player has one progress row per tutorial: `none` (no row) → `accept` → `step`s up to
//! `max_step` → `completed` (grants the reward table, reference `tutorial:{id}`), or `decline` /
//! `abandon` on the way; `reset` returns to `none` and counts resets. Every transition emits a
//! `tutorial.*` bus event (account-bound) so modules can react with their own rewards.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub const MAX_STEP: i32 = 10_000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/tutorials", get(server_list))
        .route(
            "/v1/server/tutorials/{id}",
            put(server_put).get(server_get).delete(server_delete),
        )
        .route("/v1/server/tutorials/{id}/progress", get(server_progress))
        .route("/v1/server/accounts/{id}/tutorials", get(server_account))
        .route(
            "/v1/server/accounts/{id}/tutorials/{tid}/reset",
            post(server_reset),
        )
        .route("/v1/tutorials", get(my_list))
        .route("/v1/tutorials/{id}", get(my_one))
        .route("/v1/tutorials/{id}/accept", post(accept))
        .route("/v1/tutorials/{id}/decline", post(decline))
        .route("/v1/tutorials/{id}/abandon", post(abandon))
        .route("/v1/tutorials/{id}/reset", post(reset))
        .route("/v1/tutorials/{id}/step", put(step))
}

type DefRow = (
    String,
    String,
    i32,
    i32,
    Option<String>,
    Vec<String>,
    bool,
    Value,
    OffsetDateTime,
    OffsetDateTime,
);
const DEF_COLS: &str =
    "id, title, start_step, max_step, reward_table, labels, enabled, metadata, created_at, updated_at";

type ProgRow = (
    String,
    String,
    i32,
    i32,
    OffsetDateTime,
    Option<OffsetDateTime>,
    OffsetDateTime,
);
type AccountProgRow = (
    Uuid,
    String,
    i32,
    i32,
    OffsetDateTime,
    Option<OffsetDateTime>,
    OffsetDateTime,
);
const PROG_COLS: &str = "tutorial_id, state, step, resets, started_at, completed_at, updated_at";

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.'
        })
    {
        return Err(Error::InvalidArgument(
            "tutorial id must be 1..=64 of [a-z0-9_.-]".into(),
        ));
    }
    Ok(())
}

fn ts(t: OffsetDateTime) -> Value {
    json!(t
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default())
}

fn def_doc(r: &DefRow) -> Value {
    json!({
        "id": r.0, "title": r.1, "start_step": r.2, "max_step": r.3,
        "reward_table": r.4, "labels": r.5, "enabled": r.6, "metadata": r.7,
        "created_at": ts(r.8), "updated_at": ts(r.9),
    })
}

fn prog_doc(d: &DefRow, p: Option<&ProgRow>) -> Value {
    let mut v = def_doc(d);
    match p {
        None => {
            v["state"] = json!("none");
            v["step"] = json!(d.2);
            v["resets"] = json!(0);
        }
        Some(p) => {
            v["state"] = json!(if p.1 == "reset" { "none" } else { p.1.as_str() });
            v["step"] = json!(p.2);
            v["resets"] = json!(p.3);
            v["started_at"] = ts(p.4);
            v["completed_at"] = p.5.map(ts).unwrap_or(Value::Null);
            v["progress_updated_at"] = ts(p.6);
        }
    }
    v
}

async fn load_def(s: &AppState, id: &str) -> Result<Option<DefRow>> {
    sqlx::query_as(&format!(
        "select {DEF_COLS} from musubi.tutorials where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

async fn need_def(s: &AppState, id: &str, client: bool) -> Result<DefRow> {
    match load_def(s, id).await? {
        Some(d) if d.6 || !client => Ok(d),
        _ => Err(Error::NotFound(format!("tutorial {id}"))),
    }
}

async fn load_prog(s: &AppState, acc: Uuid, id: &str) -> Result<Option<ProgRow>> {
    sqlx::query_as(&format!(
        "select {PROG_COLS} from musubi.tutorial_progress where account_id = $1 and tutorial_id = $2"
    ))
    .bind(acc)
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

async fn account_exists(s: &AppState, acc: Uuid) -> Result<()> {
    let n: i64 = sqlx::query_scalar(
        "select count(*) from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(acc)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if n == 0 {
        return Err(Error::NotFound(format!("account {acc}")));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(default)]
struct PutBody {
    title: String,
    start_step: i32,
    max_step: i32,
    reward_table: Option<String>,
    labels: Vec<String>,
    enabled: bool,
    metadata: Value,
}

impl Default for PutBody {
    fn default() -> Self {
        Self {
            title: String::new(),
            start_step: 0,
            max_step: 1,
            reward_table: None,
            labels: Vec::new(),
            enabled: true,
            metadata: json!({}),
        }
    }
}

async fn server_put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    valid_id(&id)?;
    if b.title.len() > 256 {
        return Err(Error::InvalidArgument("title must be <= 256 chars".into()).into());
    }
    if b.start_step < 0 || b.max_step < 1 || b.max_step > MAX_STEP || b.max_step < b.start_step {
        return Err(Error::InvalidArgument(format!(
            "need 0 <= start_step <= max_step <= {MAX_STEP} and max_step >= 1"
        ))
        .into());
    }
    if b.labels.len() > 16 || b.labels.iter().any(|l| l.is_empty() || l.len() > 64) {
        return Err(Error::InvalidArgument("labels: <= 16 of 1..=64 chars".into()).into());
    }
    if !b.metadata.is_object() {
        return Err(Error::InvalidArgument("metadata must be an object".into()).into());
    }
    if let Some(t) = b.reward_table.as_deref() {
        let n: i64 = sqlx::query_scalar("select count(*) from musubi.reward_tables where id = $1")
            .bind(t)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
        if n == 0 {
            return Err(Error::NotFound(format!("reward table {t}")).into());
        }
    }
    let row: DefRow = sqlx::query_as(&format!(
        "insert into musubi.tutorials (id, title, start_step, max_step, reward_table, labels, enabled, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (id) do update set title = excluded.title, start_step = excluded.start_step,
            max_step = excluded.max_step, reward_table = excluded.reward_table, labels = excluded.labels,
            enabled = excluded.enabled, metadata = excluded.metadata, updated_at = now()
         returning {DEF_COLS}"
    ))
    .bind(&id)
    .bind(&b.title)
    .bind(b.start_step)
    .bind(b.max_step)
    .bind(&b.reward_table)
    .bind(&b.labels)
    .bind(b.enabled)
    .bind(&b.metadata)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "tutorial.updated",
            None,
            json!({ "tutorial_id": id, "max_step": b.max_step }),
        )
        .await;
    Ok(Json(def_doc(&row)))
}

async fn server_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let d = need_def(&s, &id, false).await?;
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "select state, count(*) from musubi.tutorial_progress where tutorial_id = $1 group by state",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut v = def_doc(&d);
    v["counts"] = counts.into_iter().map(|(k, n)| (k, json!(n))).collect();
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
struct ListQ {
    label: Option<String>,
}

async fn server_list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<DefRow> = sqlx::query_as(&format!(
        "select {DEF_COLS} from musubi.tutorials where $1::text is null or $1 = any(labels) order by id"
    ))
    .bind(&q.label)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "tutorials": rows.iter().map(def_doc).collect::<Vec<_>>() }),
    ))
}

async fn server_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.tutorials where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("tutorial {id}")).into());
    }
    s.events
        .emit("tutorial.deleted", None, json!({ "tutorial_id": id }))
        .await;
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize, Default)]
struct ProgQ {
    state: Option<String>,
    limit: Option<i64>,
}

async fn server_progress(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<ProgQ>,
) -> Result<Json<Value>, ApiError> {
    need_def(&s, &id, false).await?;
    if let Some(st) = q.state.as_deref() {
        if !["accepted", "declined", "abandoned", "completed", "reset"].contains(&st) {
            return Err(Error::InvalidArgument(
                "state must be accepted|declined|abandoned|completed|reset".into(),
            )
            .into());
        }
    }
    let limit = q.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(Error::InvalidArgument("limit must be 1..=1000".into()).into());
    }
    let rows: Vec<AccountProgRow> = sqlx::query_as(
        "select account_id, state, step, resets, started_at, completed_at, updated_at
             from musubi.tutorial_progress
             where tutorial_id = $1 and ($2::text is null or state = $2)
             order by updated_at desc limit $3",
    )
    .bind(&id)
    .bind(&q.state)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "account_id": r.0, "state": r.1, "step": r.2, "resets": r.3,
                "started_at": ts(r.4), "completed_at": r.5.map(ts).unwrap_or(Value::Null),
                "updated_at": ts(r.6),
            })
        })
        .collect();
    Ok(Json(json!({ "tutorial_id": id, "progress": items })))
}

async fn list_for(s: &AppState, acc: Uuid, client: bool) -> Result<Vec<Value>> {
    let defs: Vec<DefRow> = sqlx::query_as(&format!(
        "select {DEF_COLS} from musubi.tutorials where enabled or not $1 order by id"
    ))
    .bind(client)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let progs: Vec<ProgRow> = sqlx::query_as(&format!(
        "select {PROG_COLS} from musubi.tutorial_progress where account_id = $1"
    ))
    .bind(acc)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(defs
        .iter()
        .map(|d| prog_doc(d, progs.iter().find(|p| p.0 == d.0)))
        .collect())
}

async fn server_account(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(acc): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, acc).await?;
    Ok(Json(
        json!({ "account_id": acc, "tutorials": list_for(&s, acc, false).await? }),
    ))
}

async fn server_reset(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((acc, tid)): Path<(Uuid, String)>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, acc).await?;
    let d = need_def(&s, &tid, false).await?;
    do_reset(&s, AccountId(acc), &d, "server").await
}

async fn do_reset(
    s: &AppState,
    me: AccountId,
    d: &DefRow,
    by: &str,
) -> Result<Json<Value>, ApiError> {
    let Some(p) = load_prog(s, me.0, &d.0).await? else {
        return Err(Error::NotFound(format!("no progress for tutorial {}", d.0)).into());
    };
    if p.1 == "reset" {
        return Err(Error::Conflict(format!("tutorial {} is already reset", d.0)).into());
    }
    // the row stays (keeps the reset counter) but reads as `none` to the client
    sqlx::query(
        "update musubi.tutorial_progress set state = 'reset', step = $3, resets = resets + 1,
            completed_at = null, updated_at = now()
         where account_id = $1 and tutorial_id = $2",
    )
    .bind(me.0)
    .bind(&d.0)
    .bind(d.2)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "tutorial.reset",
            Some(me),
            json!({ "tutorial_id": d.0, "was": p.1, "resets": p.3 + 1, "by": by }),
        )
        .await;
    let p = load_prog(s, me.0, &d.0).await?;
    Ok(Json(prog_doc(d, p.as_ref())))
}

async fn my_list(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "tutorials": list_for(&s, me.account_id.0, true).await? }),
    ))
}

async fn my_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let d = need_def(&s, &id, true).await?;
    let p = load_prog(&s, me.account_id.0, &id).await?;
    Ok(Json(prog_doc(&d, p.as_ref())))
}

async fn transition(
    s: &AppState,
    me: AccountId,
    id: &str,
    from: &[&str],
    to: &str,
) -> Result<Json<Value>, ApiError> {
    let d = need_def(s, id, true).await?;
    let p = load_prog(s, me.0, id).await?;
    let cur = match p.as_ref().map(|p| p.1.as_str()) {
        None | Some("reset") => "none",
        Some(st) => st,
    };
    if !from.contains(&cur) {
        return Err(Error::Conflict(format!("tutorial {id} is {cur}, cannot {to}")).into());
    }
    let step = if to == "accepted" {
        d.2
    } else {
        p.as_ref().map(|p| p.2).unwrap_or(d.2)
    };
    sqlx::query(
        "insert into musubi.tutorial_progress (account_id, tutorial_id, state, step)
         values ($1, $2, $3, $4)
         on conflict (account_id, tutorial_id) do update set state = excluded.state,
            step = excluded.step, updated_at = now(),
            started_at = case when excluded.state = 'accepted' then now() else musubi.tutorial_progress.started_at end",
    )
    .bind(me.0)
    .bind(id)
    .bind(to)
    .bind(step)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            &format!("tutorial.{to}"),
            Some(me),
            json!({ "tutorial_id": id, "step": step, "was": cur }),
        )
        .await;
    let p = load_prog(s, me.0, id).await?;
    Ok(Json(prog_doc(&d, p.as_ref())))
}

async fn accept(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    transition(
        &s,
        me.account_id,
        &id,
        &["none", "declined", "abandoned"],
        "accepted",
    )
    .await
}

async fn decline(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    transition(&s, me.account_id, &id, &["none", "accepted"], "declined").await
}

async fn abandon(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    transition(&s, me.account_id, &id, &["accepted"], "abandoned").await
}

async fn reset(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let d = need_def(&s, &id, true).await?;
    do_reset(&s, me.account_id, &d, "player").await
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StepBody {
    step: Option<i32>,
    complete: bool,
}

async fn step(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<StepBody>,
) -> Result<Json<Value>, ApiError> {
    let d = need_def(&s, &id, true).await?;
    let p = load_prog(&s, me.account_id.0, &id).await?;
    let Some(p) = p.filter(|p| p.1 == "accepted") else {
        return Err(Error::Conflict(format!("tutorial {id} is not accepted")).into());
    };
    let target = if b.complete {
        d.3
    } else {
        b.step
            .ok_or_else(|| Error::InvalidArgument("step or complete=true required".into()))?
    };
    if target <= p.2 || target > d.3 {
        return Err(Error::InvalidArgument(format!(
            "step must be > current {} and <= max_step {}",
            p.2, d.3
        ))
        .into());
    }
    let done = target == d.3;
    sqlx::query(
        "update musubi.tutorial_progress set step = $3, updated_at = now(),
            state = case when $4 then 'completed' else state end,
            completed_at = case when $4 then now() else completed_at end
         where account_id = $1 and tutorial_id = $2",
    )
    .bind(me.account_id.0)
    .bind(&id)
    .bind(target)
    .bind(done)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut rewards = Value::Null;
    if done {
        if let Some(t) = d.4.as_deref() {
            let reference = format!("tutorial:{id}:{}", me.account_id.0);
            rewards = match s.economy.grant_table(me.account_id, t, &reference).await {
                Ok(r) => json!(r),
                Err(e) => {
                    tracing::warn!(tutorial = %id, error = %e, "tutorial reward grant failed");
                    json!({ "error": e.to_string() })
                }
            };
        }
        s.events
            .emit(
                "tutorial.completed",
                Some(me.account_id),
                json!({ "tutorial_id": id, "step": target, "reward_table": d.4, "rewards": rewards }),
            )
            .await;
    } else {
        s.events
            .emit(
                "tutorial.step",
                Some(me.account_id),
                json!({ "tutorial_id": id, "step": target, "max_step": d.3 }),
            )
            .await;
    }
    let p = load_prog(&s, me.account_id.0, &id).await?;
    let mut v = prog_doc(&d, p.as_ref());
    if done {
        v["rewards"] = rewards;
    }
    Ok(Json(v))
}
