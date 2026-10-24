//! Asynchronous data-rights requests (GDPR/CCPA export and erasure) — Pragma parity. A player
//! files `POST /v1/account/data_rights {kind: export|erase}`; the leader polls the queue every
//! `[compliance].data_rights_poll_minutes` (or `POST /v1/server/data_rights/run`), builds the
//! export bundle (`AccountService::export`, game data tables included unless
//! `data_rights_include_game_data = false` / `include_game_data: false`) or erases the account
//! (sessions kicked, soft-delete, deletions ledger), and keeps the result for
//! `result_ttl_days` so the player downloads it from `GET /v1/account/data_rights/{id}/download`.
//! Operators list/inspect/cancel requests and file them on a player's behalf.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

const RESULT_TTL_DAYS: i64 = 7;
const BATCH: i64 = 20;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/account/data_rights", get(my_list).post(my_request))
        .route("/v1/account/data_rights/{id}", get(my_one))
        .route("/v1/account/data_rights/{id}/download", get(my_download))
        .route("/v1/account/data_rights/{id}/cancel", post(my_cancel))
        .route(
            "/v1/server/data_rights",
            get(server_list).post(server_request),
        )
        .route("/v1/server/data_rights/run", post(server_run))
        .route("/v1/server/data_rights/{id}", get(server_one))
        .route("/v1/server/data_rights/{id}/download", get(server_download))
        .route("/v1/server/data_rights/{id}/cancel", post(server_cancel))
}

type Row = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    bool,
    OffsetDateTime,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    Option<String>,
    bool,
);
const COLS: &str = "id, account_id, kind, status, requested_by, include_game_data, requested_at, \
    started_at, completed_at, expires_at, error, result is not null";

fn ots(t: Option<OffsetDateTime>) -> Value {
    t.map(|t| {
        json!(t
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default())
    })
    .unwrap_or(Value::Null)
}

fn doc(r: &Row) -> Value {
    json!({
        "id": r.0, "account_id": r.1, "kind": r.2, "status": r.3, "requested_by": r.4,
        "include_game_data": r.5, "requested_at": ots(Some(r.6)), "started_at": ots(r.7),
        "completed_at": ots(r.8), "expires_at": ots(r.9), "error": r.10,
        "downloadable": r.11 && r.3 == "done" && r.2 == "export",
    })
}

async fn load(s: &AppState, id: Uuid) -> Result<Option<Row>> {
    sqlx::query_as(&format!(
        "select {COLS} from musubi.data_rights_requests where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

fn valid_kind(k: &str) -> Result<()> {
    if !["export", "erase"].contains(&k) {
        return Err(Error::InvalidArgument("kind must be export|erase".into()));
    }
    Ok(())
}

/// File a request; one open (pending/running) request per account and kind.
async fn file(
    s: &AppState,
    account: Uuid,
    kind: &str,
    by: &str,
    include_game_data: Option<bool>,
) -> Result<Value> {
    valid_kind(kind)?;
    let include = include_game_data.unwrap_or(s.cfg.compliance.data_rights_include_game_data);
    let open: i64 = sqlx::query_scalar(
        "select count(*) from musubi.data_rights_requests
         where account_id = $1 and kind = $2 and status in ('pending', 'running')",
    )
    .bind(account)
    .bind(kind)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if open > 0 {
        return Err(Error::Conflict(format!(
            "an open {kind} request already exists"
        )));
    }
    let id = Uuid::now_v7();
    let row: Row = sqlx::query_as(&format!(
        "insert into musubi.data_rights_requests (id, account_id, kind, requested_by, include_game_data)
         values ($1, $2, $3, $4, $5) returning {COLS}"
    ))
    .bind(id)
    .bind(account)
    .bind(kind)
    .bind(by)
    .bind(include)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "data_rights.requested",
            Some(AccountId(account)),
            json!({ "request_id": id, "kind": kind, "by": by }),
        )
        .await;
    Ok(doc(&row))
}

async fn cancel(s: &AppState, id: Uuid, account: Option<Uuid>) -> Result<Value> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "update musubi.data_rights_requests set status = 'cancelled', completed_at = now()
         where id = $1 and status = 'pending' and ($2::uuid is null or account_id = $2)
         returning {COLS}"
    ))
    .bind(id)
    .bind(account)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return match load(s, id).await? {
            Some(r) if account.is_none_or(|a| a == r.1) => Err(Error::Conflict(format!(
                "request is {} and cannot be cancelled",
                r.3
            ))),
            _ => Err(Error::NotFound(format!("request {id}"))),
        };
    };
    s.events
        .emit(
            "data_rights.cancelled",
            Some(AccountId(row.1)),
            json!({ "request_id": id, "kind": row.2 }),
        )
        .await;
    Ok(doc(&row))
}

/// Process up to `BATCH` pending requests oldest first and expire old results. Returns
/// `(done, failed, expired)`. Leader-only in the sweep; `POST /v1/server/data_rights/run` for tests
/// and operators.
pub async fn poll(s: &AppState) -> Result<(usize, usize, u64)> {
    let expired = sqlx::query(
        "update musubi.data_rights_requests set result = null
         where result is not null and expires_at is not null and expires_at < now()",
    )
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    let claimed: Vec<Row> = sqlx::query_as(&format!(
        "update musubi.data_rights_requests set status = 'running', started_at = now()
         where id in (select id from musubi.data_rights_requests where status = 'pending'
                      order by requested_at limit $1 for update skip locked)
         returning {COLS}"
    ))
    .bind(BATCH)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let (mut done, mut failed) = (0, 0);
    for r in &claimed {
        let acc = AccountId(r.1);
        let outcome: Result<Option<Value>> = match r.2.as_str() {
            "export" => s.accounts.export(acc).await.map(|mut v| {
                if !r.5 {
                    v["tables"] = json!({});
                    v["game_data_omitted"] = json!(true);
                }
                Some(v)
            }),
            _ => {
                super::admin::kick_everywhere(s, acc).await;
                s.accounts.delete(acc).await.map(|_| None)
            }
        };
        match outcome {
            Ok(result) => {
                sqlx::query(
                    "update musubi.data_rights_requests
                     set status = 'done', completed_at = now(), result = $2,
                         expires_at = case when $2::jsonb is null then null else now() + make_interval(days => $3) end
                     where id = $1",
                )
                .bind(r.0)
                .bind(&result)
                .bind(RESULT_TTL_DAYS as i32)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
                if r.2 == "erase" {
                    s.events
                        .emit(
                            "account.deleted",
                            Some(acc),
                            json!({ "by": "data_rights", "request_id": r.0 }),
                        )
                        .await;
                }
                s.events
                    .emit(
                        "data_rights.completed",
                        Some(acc),
                        json!({ "request_id": r.0, "kind": r.2, "by": r.4 }),
                    )
                    .await;
                done += 1;
            }
            Err(e) => {
                // `error` is shown to the player in `GET /v1/account/data_requests`: internal
                // (DB / IO) texts stay in the log
                let shown = match &e {
                    Error::Internal(_) => {
                        tracing::warn!(request_id = %r.0, kind = %r.2, error = %e, "data rights request failed");
                        "internal error".to_string()
                    }
                    other => other.to_string(),
                };
                sqlx::query(
                    "update musubi.data_rights_requests set status = 'failed', completed_at = now(), error = $2 where id = $1",
                )
                .bind(r.0)
                .bind(&shown)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
                s.events
                    .emit(
                        "data_rights.failed",
                        Some(acc),
                        json!({ "request_id": r.0, "kind": r.2, "error": shown }),
                    )
                    .await;
                failed += 1;
            }
        }
    }
    Ok((done, failed, expired))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RequestBody {
    kind: String,
    include_game_data: Option<bool>,
}

async fn my_request(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<RequestBody>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        file(&s, me.account_id.0, &b.kind, "player", b.include_game_data).await?,
    ))
}

async fn my_list(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.data_rights_requests where account_id = $1 order by requested_at desc limit 50"
    ))
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "requests": rows.iter().map(doc).collect::<Vec<_>>(),
        "poll_minutes": s.cfg.compliance.data_rights_poll_minutes,
    })))
}

async fn my_one(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    match load(&s, id).await? {
        Some(r) if r.1 == me.account_id.0 => Ok(Json(doc(&r))),
        _ => Err(Error::NotFound(format!("request {id}")).into()),
    }
}

async fn my_download(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<(Uuid, String, Option<Value>)> = sqlx::query_as(
        "select account_id, status, result from musubi.data_rights_requests where id = $1 and kind = 'export'",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match row {
        Some((acc, _, _)) if acc != me.account_id.0 => {
            Err(Error::NotFound(format!("request {id}")).into())
        }
        Some((_, st, Some(v))) if st == "done" => Ok(Json(v)),
        Some((_, st, None)) if st == "done" => Err(Error::NotFound("export expired".into()).into()),
        Some((_, st, _)) => Err(Error::Conflict(format!("export is {st}")).into()),
        None => Err(Error::NotFound(format!("request {id}")).into()),
    }
}

/// `GET /v1/server/data_rights/{id}/download` — the finished export of any account, for operators.
async fn server_download(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<(Uuid, String, Option<Value>)> = sqlx::query_as(
        "select account_id, status, result from musubi.data_rights_requests where id = $1 and kind = 'export'",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    match row {
        Some((_, st, Some(v))) if st == "done" => Ok(Json(v)),
        Some((_, st, None)) if st == "done" => Err(Error::NotFound("export expired".into()).into()),
        Some((_, st, _)) => Err(Error::Conflict(format!("export is {st}")).into()),
        None => Err(Error::NotFound(format!("request {id}")).into()),
    }
}

async fn my_cancel(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(cancel(&s, id, Some(me.account_id.0)).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ServerRequestBody {
    account_id: Option<Uuid>,
    kind: String,
    include_game_data: Option<bool>,
}

async fn server_request(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<ServerRequestBody>,
) -> Result<Json<Value>, ApiError> {
    let Some(acc) = b.account_id else {
        return Err(Error::InvalidArgument("account_id required".into()).into());
    };
    let n: i64 = sqlx::query_scalar(
        "select count(*) from musubi.accounts where id = $1 and deleted_at is null",
    )
    .bind(acc)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if n == 0 {
        return Err(Error::NotFound(format!("account {acc}")).into());
    }
    Ok(Json(
        file(&s, acc, &b.kind, "server", b.include_game_data).await?,
    ))
}

#[derive(Deserialize, Default)]
struct ListQ {
    status: Option<String>,
    kind: Option<String>,
    account_id: Option<Uuid>,
    limit: Option<i64>,
}

async fn server_list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    if let Some(st) = q.status.as_deref() {
        if !["pending", "running", "done", "failed", "cancelled"].contains(&st) {
            return Err(Error::InvalidArgument(
                "status must be pending|running|done|failed|cancelled".into(),
            )
            .into());
        }
    }
    if let Some(k) = q.kind.as_deref() {
        valid_kind(k)?;
    }
    let limit = q.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(Error::InvalidArgument("limit must be 1..=1000".into()).into());
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.data_rights_requests
         where ($1::text is null or status = $1) and ($2::text is null or kind = $2)
           and ($3::uuid is null or account_id = $3)
         order by requested_at desc limit $4"
    ))
    .bind(&q.status)
    .bind(&q.kind)
    .bind(q.account_id)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let pending: i64 = sqlx::query_scalar(
        "select count(*) from musubi.data_rights_requests where status = 'pending'",
    )
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "requests": rows.iter().map(doc).collect::<Vec<_>>(),
        "pending": pending,
        "poll_minutes": s.cfg.compliance.data_rights_poll_minutes,
    })))
}

async fn server_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    match load(&s, id).await? {
        Some(r) => Ok(Json(doc(&r))),
        None => Err(Error::NotFound(format!("request {id}")).into()),
    }
}

async fn server_cancel(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(cancel(&s, id, None).await?))
}

async fn server_run(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let (done, failed, expired) = poll(&s).await?;
    Ok(Json(
        json!({ "done": done, "failed": failed, "expired": expired }),
    ))
}
