//! Durable workflows (sagas): long-running server logic written as a module export
//! `workflow_<name>` that is called one step at a time with its persisted state. Every step
//! gets `{id, name, step, attempt, input, state, signal, timed_out}` and answers with
//! directives: `state` (persisted), then one of `sleep_secs` (run again later; default 0 = next
//! tick), `wait_signal` (+ optional `timeout_secs`; parked until `POST …/signal`), `done`
//! (+ `output`) or `fail`. A trap / error retries with exponential backoff up to
//! `max_attempts`; a permanent failure or a cancel runs `workflow_<name>_compensate` when the
//! module exports it (the saga's undo) and the workflow ends `compensated` (or `failed`).
//! One node runs due steps cluster-wide (lease `workflows`); rows are claimed with
//! `for update skip locked`, so a step never runs twice. Survives restarts: the state is the row.

use std::sync::Arc;

use musubi_core::{Error, Result};
use musubi_runtime::CallContext;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;

pub const TERMINAL: &[&str] = &["done", "failed", "compensated", "cancelled"];
const HISTORY_KEEP: usize = 50;
const MAX_BACKOFF_SECS: i64 = 300;

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct StartReq {
    pub name: String,
    pub input: Value,
    /// Idempotency key: a second start with the same `name` + `key` returns the first workflow.
    pub key: Option<String>,
    /// Delay the first step.
    pub delay_secs: Option<i64>,
    pub account_id: Option<Uuid>,
    pub max_attempts: Option<i32>,
    pub max_steps: Option<i32>,
}

fn valid_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Error::InvalidArgument(
            "workflow name must be 1..=64 chars of [A-Za-z0-9_-]".into(),
        ));
    }
    Ok(())
}

fn row_json(r: &sqlx::postgres::PgRow) -> Value {
    let ts = |c: &str| -> Value {
        r.try_get::<Option<OffsetDateTime>, _>(c)
            .ok()
            .flatten()
            .and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .map(Value::String)
            .unwrap_or(Value::Null)
    };
    json!({
        "id": r.get::<Uuid, _>("id"),
        "name": r.get::<String, _>("name"),
        "key": r.get::<Option<String>, _>("key"),
        "status": r.get::<String, _>("status"),
        "step": r.get::<i32, _>("step"),
        "attempt": r.get::<i32, _>("attempt"),
        "max_attempts": r.get::<i32, _>("max_attempts"),
        "input": r.get::<Value, _>("input"),
        "state": r.get::<Value, _>("state"),
        "output": r.get::<Option<Value>, _>("output"),
        "error": r.get::<Option<String>, _>("error"),
        "pending_signals": r.get::<Value, _>("signals").as_array().map(|a| a.len()).unwrap_or(0),
        "history": r.get::<Value, _>("history"),
        "wake_at": ts("wake_at"),
        "account_id": r.get::<Option<Uuid>, _>("account_id"),
        "created_at": ts("created_at"),
        "updated_at": ts("updated_at"),
        "finished_at": ts("finished_at"),
    })
}

const COLS: &str = "id, name, key, status, step, attempt, max_attempts, input, state, output, error, signals, history, wake_at, account_id, created_at, updated_at, finished_at";

pub async fn start(s: &AppState, req: StartReq) -> Result<Value> {
    valid_name(&req.name)?;
    if !req.input.is_object() && !req.input.is_null() {
        return Err(Error::InvalidArgument("input must be a JSON object".into()));
    }
    let export = format!("workflow_{}", req.name);
    if !s.runtime.has_hook(&export) {
        return Err(Error::NotFound(format!("no module exports {export}")));
    }
    if let Some(k) = req.key.as_deref() {
        if k.is_empty() || k.len() > 128 {
            return Err(Error::InvalidArgument("key must be 1..=128 chars".into()));
        }
        let existing = sqlx::query(&format!(
            "select {COLS} from musubi.workflows where name = $1 and key = $2"
        ))
        .bind(&req.name)
        .bind(k)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
        if let Some(r) = existing {
            let mut v = row_json(&r);
            v["created"] = json!(false);
            return Ok(v);
        }
    }
    let id = Uuid::now_v7();
    let wake = OffsetDateTime::now_utc()
        + time::Duration::seconds(req.delay_secs.unwrap_or(0).clamp(0, 366 * 86400));
    let r = sqlx::query(&format!(
        "insert into musubi.workflows (id, name, key, input, wake_at, account_id, max_attempts, max_steps)
         values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (name, key) where key is not null do update set updated_at = musubi.workflows.updated_at
         returning {COLS}"
    ))
    .bind(id)
    .bind(&req.name)
    .bind(&req.key)
    .bind(if req.input.is_null() { json!({}) } else { req.input })
    .bind(wake)
    .bind(req.account_id)
    .bind(req.max_attempts.unwrap_or(5).clamp(1, 100))
    .bind(req.max_steps.unwrap_or(10_000).clamp(1, 1_000_000))
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut v = row_json(&r);
    v["created"] = json!(v["id"] == json!(id));
    Ok(v)
}

pub async fn get(s: &AppState, id: Uuid) -> Result<Value> {
    let r = sqlx::query(&format!(
        "select {COLS} from musubi.workflows where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("workflow {id}")))?;
    Ok(row_json(&r))
}

pub async fn list(
    s: &AppState,
    name: Option<&str>,
    status: Option<&str>,
    account_id: Option<Uuid>,
    limit: i64,
) -> Result<Value> {
    let rows = sqlx::query(&format!(
        "select {COLS} from musubi.workflows
          where ($1::text is null or name = $1)
            and ($2::text is null or status = $2)
            and ($3::uuid is null or account_id = $3)
          order by created_at desc limit $4"
    ))
    .bind(name)
    .bind(status)
    .bind(account_id)
    .bind(limit.clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({ "workflows": rows.iter().map(row_json).collect::<Vec<_>>() }))
}

/// Queue a signal; a workflow parked on `wait_signal` wakes at once.
pub async fn signal(s: &AppState, id: Uuid, payload: Value) -> Result<Value> {
    let r = sqlx::query(&format!(
        "update musubi.workflows
            set signals = signals || jsonb_build_array($2::jsonb),
                status = case when status = 'waiting' then 'running' else status end,
                wake_at = case when status = 'waiting' then now() else wake_at end,
                updated_at = now()
          where id = $1 and status not in ('done', 'failed', 'compensated', 'cancelled')
          returning {COLS}"
    ))
    .bind(id)
    .bind(payload)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("workflow {id} is not running")))?;
    Ok(row_json(&r))
}

/// Cancel a live workflow; runs the compensate export if the module has one.
pub async fn cancel(s: &AppState, id: Uuid, reason: &str) -> Result<Value> {
    let r = sqlx::query(&format!(
        "update musubi.workflows
            set status = 'cancelled', error = $2, finished_at = now(), updated_at = now(), wake_at = null
          where id = $1 and status not in ('done', 'failed', 'compensated', 'cancelled')
          returning {COLS}"
    ))
    .bind(id)
    .bind(if reason.is_empty() { "cancelled" } else { reason })
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("workflow {id} is not running")))?;
    let v = row_json(&r);
    let _ = compensate(s, &v, "cancelled").await;
    finished(s, &v["name"], id, "cancelled").await;
    get(s, id).await
}

async fn finished(s: &AppState, name: &Value, id: Uuid, status: &str) {
    s.events
        .emit(
            "workflow.finished",
            None,
            json!({ "id": id, "name": name, "status": status }),
        )
        .await;
}

async fn call_export(s: &AppState, export: &str, payload: &Value) -> Option<Result<Value>> {
    let bytes = match serde_json::to_vec(payload) {
        Ok(b) => b,
        Err(e) => return Some(Err(Error::internal(e))),
    };
    let call = CallContext {
        server: true,
        ..Default::default()
    };
    match s.runtime.hook(export, call, bytes).await {
        None => None,
        Some(Err(e)) => Some(Err(e)),
        Some(Ok(out)) => Some(Ok(if out.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&out).unwrap_or_else(
                |_| json!({ "fail": format!("non-JSON reply: {}", String::from_utf8_lossy(&out)) }),
            )
        })),
    }
}

/// Run the saga's undo step; returns whether the module had one and it succeeded.
async fn compensate(s: &AppState, wf: &Value, why: &str) -> Result<bool> {
    let name = wf["name"].as_str().unwrap_or("");
    let export = format!("workflow_{name}_compensate");
    let payload = json!({
        "id": wf["id"], "name": name, "input": wf["input"], "state": wf["state"],
        "error": wf["error"], "reason": why,
    });
    match call_export(s, &export, &payload).await {
        None => Ok(false),
        Some(Ok(out)) => {
            if let Some(st) = out.get("state") {
                sqlx::query(
                    "update musubi.workflows set state = $2, updated_at = now() where id = $1",
                )
                .bind(wf["id"].as_str().and_then(|x| x.parse::<Uuid>().ok()))
                .bind(st)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
            }
            Ok(true)
        }
        Some(Err(e)) => Err(e),
    }
}

/// Permanent failure: run the saga's undo and decide the terminal status.
async fn fail_permanently(s: &AppState, wf: &Value, err: String) -> (String, Value) {
    let mut wf = wf.clone();
    wf["error"] = json!(err);
    let status = match compensate(s, &wf, "failed").await {
        Ok(true) => "compensated",
        Ok(false) => "failed",
        Err(e) => {
            wf["error"] = json!(format!("{err}; compensate failed: {e}"));
            "failed"
        }
    };
    (status.to_string(), wf["error"].clone())
}

fn backoff_secs(attempt: i32) -> i64 {
    (1i64 << (attempt.clamp(1, 20) - 1)).min(MAX_BACKOFF_SECS)
}

/// Run every due step (claimed with `skip locked`); returns how many ran.
pub async fn run_due(s: &AppState, limit: i64) -> Result<usize> {
    let mut ran = 0usize;
    loop {
        if ran as i64 >= limit {
            break;
        }
        let mut tx = s.db.begin().await.map_err(Error::internal)?;
        let row = sqlx::query(&format!(
            "select {COLS}, max_steps from musubi.workflows
              where status = 'running' and wake_at <= now()
              order by wake_at limit 1 for update skip locked"
        ))
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(r) = row else { break };
        let id: Uuid = r.get("id");
        let mut wf = row_json(&r);
        let max_steps: i32 = r.get("max_steps");
        // claim: push the wake a minute out so a crashed step is retried, not lost
        sqlx::query(
            "update musubi.workflows set wake_at = now() + interval '60 seconds' where id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let mut signals: Vec<Value> = r
            .get::<Value, _>("signals")
            .as_array()
            .cloned()
            .unwrap_or_default();
        let signal = if signals.is_empty() {
            Value::Null
        } else {
            signals.remove(0)
        };
        let timed_out = signal.is_null()
            && wf["history"]
                .as_array()
                .and_then(|h| h.last())
                .map(|h| h["waited"] == json!(true))
                .unwrap_or(false);
        step(s, &mut wf, max_steps, signal, signals, timed_out).await?;
        ran += 1;
    }
    Ok(ran)
}

async fn step(
    s: &AppState,
    wf: &mut Value,
    max_steps: i32,
    signal: Value,
    remaining_signals: Vec<Value>,
    timed_out: bool,
) -> Result<()> {
    let id: Uuid = wf["id"].as_str().unwrap().parse().unwrap();
    let name = wf["name"].as_str().unwrap_or("").to_string();
    let step_no = wf["step"].as_i64().unwrap_or(0) as i32;
    let attempt = wf["attempt"].as_i64().unwrap_or(0) as i32;
    let max_attempts = wf["max_attempts"].as_i64().unwrap_or(5) as i32;
    let now = OffsetDateTime::now_utc();
    let payload = json!({
        "id": id, "name": name, "step": step_no, "attempt": attempt,
        "input": wf["input"], "state": wf["state"], "signal": signal, "timed_out": timed_out,
        "now": now.unix_timestamp(),
    });
    let export = format!("workflow_{name}");
    let outcome = call_export(s, &export, &payload).await;
    let mut hist: Vec<Value> = wf["history"].as_array().cloned().unwrap_or_default();
    let mut entry = json!({ "step": step_no, "attempt": attempt, "at": now.unix_timestamp(), "signal": !signal.is_null() });
    let signals_json = Value::Array(remaining_signals.clone());

    let (status, set_state, output, error, wake, next_step, next_attempt) = match outcome {
        None => {
            let err = format!("no module exports {export}");
            entry["ok"] = json!(false);
            entry["error"] = json!(err);
            hist.push(entry);
            let (status, error) = fail_permanently(s, wf, err).await;
            (status, None, None, Some(error), None, step_no, attempt)
        }
        Some(Err(e)) => {
            let err = e.to_string();
            entry["ok"] = json!(false);
            entry["error"] = json!(err);
            hist.push(entry);
            if attempt + 1 >= max_attempts {
                let (status, error) = fail_permanently(s, wf, err).await;
                (status, None, None, Some(error), None, step_no, attempt + 1)
            } else {
                let secs = backoff_secs(attempt + 1);
                (
                    "running".to_string(),
                    None,
                    None,
                    Some(json!(err)),
                    Some(now + time::Duration::seconds(secs)),
                    step_no,
                    attempt + 1,
                )
            }
        }
        Some(Ok(out)) => {
            let set_state = out.get("state").cloned();
            if let Some(st) = &set_state {
                wf["state"] = st.clone();
            }
            if let Some(f) = out.get("fail").filter(|f| !f.is_null()) {
                let err = f
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| f.to_string());
                entry["ok"] = json!(false);
                entry["error"] = json!(err);
                hist.push(entry);
                let (status, error) = fail_permanently(s, wf, err).await;
                (status, set_state, None, Some(error), None, step_no + 1, 0)
            } else if out["done"] == json!(true) {
                entry["ok"] = json!(true);
                entry["done"] = json!(true);
                hist.push(entry);
                (
                    "done".to_string(),
                    set_state,
                    Some(out.get("output").cloned().unwrap_or(Value::Null)),
                    None,
                    None,
                    step_no + 1,
                    0,
                )
            } else if step_no + 1 >= max_steps {
                let err = format!("max_steps ({max_steps}) reached");
                entry["ok"] = json!(false);
                entry["error"] = json!(err);
                hist.push(entry);
                let (status, error) = fail_permanently(s, wf, err).await;
                (status, set_state, None, Some(error), None, step_no + 1, 0)
            } else if out["wait_signal"] == json!(true) && remaining_signals.is_empty() {
                entry["ok"] = json!(true);
                entry["waited"] = json!(true);
                hist.push(entry);
                let timeout = out["timeout_secs"].as_i64().filter(|t| *t > 0);
                (
                    if timeout.is_some() {
                        "running"
                    } else {
                        "waiting"
                    }
                    .to_string(),
                    set_state,
                    None,
                    None,
                    timeout.map(|t| now + time::Duration::seconds(t)),
                    step_no + 1,
                    0,
                )
            } else {
                let secs = out["sleep_secs"]
                    .as_i64()
                    .unwrap_or(0)
                    .clamp(0, 366 * 86400);
                entry["ok"] = json!(true);
                if secs > 0 {
                    entry["sleep_secs"] = json!(secs);
                }
                hist.push(entry);
                (
                    "running".to_string(),
                    set_state,
                    None,
                    None,
                    Some(now + time::Duration::seconds(secs)),
                    step_no + 1,
                    0,
                )
            }
        }
    };
    if hist.len() > HISTORY_KEEP {
        let drop = hist.len() - HISTORY_KEEP;
        hist.drain(..drop);
    }
    let terminal = TERMINAL.contains(&status.as_str());
    sqlx::query(
        "update musubi.workflows
            set status = $2, step = $3, attempt = $4,
                state = coalesce($5, state), output = coalesce($6, output), error = $7,
                signals = $8, history = $9, wake_at = $10, updated_at = now(),
                finished_at = case when $11 then now() else finished_at end
          where id = $1 and status not in ('cancelled')",
    )
    .bind(id)
    .bind(&status)
    .bind(next_step)
    .bind(next_attempt)
    .bind(set_state)
    .bind(output)
    .bind(error.as_ref().and_then(|e| e.as_str().map(str::to_string)))
    .bind(signals_json)
    .bind(Value::Array(hist))
    .bind(wake)
    .bind(terminal)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if terminal {
        finished(s, &json!(name), id, &status).await;
    }
    Ok(())
}

/// Leader loop: one node runs due steps every second.
pub fn start_runner(state: &AppState) {
    let st = state.clone();
    let lease = st
        .cluster
        .clone()
        .map(|c| c.lease("workflows", musubi_cluster::DEAD_AFTER));
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick.tick().await;
            if let Some(l) = &lease {
                if !l.held() {
                    continue;
                }
            }
            match run_due(&st, 256).await {
                Ok(0) => {}
                Ok(n) => tracing::debug!(steps = n, "workflow steps ran"),
                Err(e) => tracing::warn!(error = %e, "workflow runner failed"),
            }
        }
    });
}

#[allow(dead_code)]
fn _assert_send(_: Arc<AppState>) {}
