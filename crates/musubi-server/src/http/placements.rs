//! Queued game-session placements (GameLift `StartGameSessionPlacement` parity): an allocation
//! request that waits for capacity instead of failing with 429. The caller gets a placement id,
//! polls it (or listens to `placement.*` events), and the leader's worker keeps retrying the routed
//! allocation until it is fulfilled, cancelled or the deadline passes.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/session_placements", post(create).get(list))
        .route(
            "/v1/server/session_placements/{id}",
            get(get_one).delete(cancel),
        )
}

/// Same shape as `POST /v1/server/allocate` plus the queue knobs.
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct PlacementReq {
    pool: String,
    region: Option<String>,
    regions: Vec<String>,
    pings: std::collections::HashMap<String, f64>,
    max_ping_ms: Option<f64>,
    client: Option<musubi_fleet::GeoPoint>,
    payload: Value,
    backfill: bool,
    selector: musubi_fleet::AllocSelector,
    counter_actions: std::collections::HashMap<String, musubi_fleet::CounterAction>,
    list_actions: std::collections::HashMap<String, musubi_fleet::ListAction>,
    selectors: Vec<musubi_fleet::AllocSelector>,
    priorities: Vec<musubi_fleet::Priority>,
    scheduling: musubi_fleet::Scheduling,
    /// How long the placement may wait for capacity (1..=600, default 60).
    timeout_secs: Option<u64>,
    /// Opaque caller data (ticket id, party, mode) echoed back on the placement and its events.
    metadata: Value,
    /// Try once synchronously before queueing (default true) — when capacity exists the response
    /// is already `fulfilled`.
    try_now: Option<bool>,
}

impl PlacementReq {
    fn hint(&self) -> musubi_fleet::RouteHint {
        musubi_fleet::RouteHint {
            region: self.region.clone(),
            regions: self.regions.clone(),
            pings: self.pings.clone(),
            max_ping_ms: self.max_ping_ms,
            client: self.client,
        }
    }
    fn alloc(&self) -> musubi_fleet::AllocateRequest {
        musubi_fleet::AllocateRequest {
            payload: self.payload.clone(),
            backfill: self.backfill,
            selector: self.selector.clone(),
            counter_actions: self.counter_actions.clone(),
            list_actions: self.list_actions.clone(),
            selectors: self.selectors.clone(),
            priorities: self.priorities.clone(),
            scheduling: self.scheduling,
        }
    }
}

fn parse_req(v: &Value) -> Result<PlacementReq> {
    let r: PlacementReq = serde_json::from_value(v.clone())
        .map_err(|e| Error::InvalidArgument(format!("placement: {e}")))?;
    if r.pool.is_empty() {
        return Err(Error::InvalidArgument("pool is required".into()));
    }
    let t = r.timeout_secs.unwrap_or(60);
    if !(1..=600).contains(&t) {
        return Err(Error::InvalidArgument(
            "timeout_secs must be 1..=600".into(),
        ));
    }
    if !r.metadata.is_null() && !r.metadata.is_object() {
        return Err(Error::InvalidArgument("metadata must be an object".into()));
    }
    r.hint().validate()?;
    Ok(r)
}

type Row = (
    Uuid,
    String,
    String,
    Value,
    Value,
    i32,
    Option<String>,
    Option<Value>,
    i64,
    i64,
    i64,
    Option<i64>,
);

const COLS: &str = "id, pool, status, request, metadata, attempts, last_error, result, \
    extract(epoch from created_at)::bigint, extract(epoch from updated_at)::bigint, \
    extract(epoch from timeout_at)::bigint, extract(epoch from fulfilled_at)::bigint";

async fn row_json(s: &AppState, r: Row) -> Result<Value> {
    let (
        id,
        pool,
        status,
        request,
        metadata,
        attempts,
        last_error,
        result,
        created,
        updated,
        timeout,
        fulfilled,
    ) = r;
    let position: Option<i64> = if status == "pending" {
        let (n,): (i64,) = sqlx::query_as(
            "select count(*) from musubi.session_placements p
             where p.status = 'pending' and p.pool = $1
               and p.created_at < (select created_at from musubi.session_placements where id = $2)",
        )
        .bind(&pool)
        .bind(id)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
        Some(n)
    } else {
        None
    };
    Ok(json!({
        "id": id, "pool": pool, "status": status, "request": request, "metadata": metadata,
        "attempts": attempts, "last_error": last_error, "allocation": result,
        "created_at": created, "updated_at": updated, "timeout_at": timeout,
        "fulfilled_at": fulfilled, "queue_position": position,
    }))
}

async fn fetch(s: &AppState, id: Uuid) -> Result<Row> {
    sqlx::query_as(&format!(
        "select {COLS} from musubi.session_placements where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("placement {id}")))
}

async fn emit(s: &AppState, code: &str, id: Uuid, pool: &str, metadata: &Value, extra: Value) {
    let mut data = json!({ "id": id, "pool": pool, "metadata": metadata });
    if let (Some(d), Some(e)) = (data.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            d.insert(k.clone(), v.clone());
        }
    }
    s.events.emit(code, None, data).await;
}

/// `POST /v1/server/session_placements` — queue a routed allocation; `try_now` (default) attempts it once
/// first, so a pool with free capacity answers `fulfilled` straight away.
async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(body): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let r = parse_req(&body)?;
    let id = Uuid::now_v7();
    let timeout = r.timeout_secs.unwrap_or(60) as i64;
    let metadata = if r.metadata.is_null() {
        json!({})
    } else {
        r.metadata.clone()
    };
    let mut status = "pending";
    let mut result: Option<Value> = None;
    let mut last_error: Option<String> = None;
    let mut attempts = 0;
    if r.try_now.unwrap_or(true) {
        attempts = 1;
        match s
            .fleets
            .allocate_routed_with(&r.pool, &r.hint(), r.alloc(), std::time::Duration::ZERO)
            .await
        {
            Ok(a) => {
                status = "fulfilled";
                result = Some(serde_json::to_value(a).map_err(Error::internal)?);
            }
            Err(e @ Error::InvalidArgument(_)) => return Err(e.into()),
            Err(Error::NotFound(m)) => return Err(Error::NotFound(m).into()),
            Err(e) => last_error = Some(e.to_string()),
        }
    }
    sqlx::query(
        "insert into musubi.session_placements (id, pool, status, request, metadata, attempts, last_error, result, timeout_at, fulfilled_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, now() + make_interval(secs => $9::double precision),
                 case when $3 = 'fulfilled' then now() end)",
    )
    .bind(id)
    .bind(&r.pool)
    .bind(status)
    .bind(&body)
    .bind(&metadata)
    .bind(attempts)
    .bind(&last_error)
    .bind(&result)
    .bind(timeout as f64)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if status == "fulfilled" {
        emit(
            &s,
            "placement.fulfilled",
            id,
            &r.pool,
            &metadata,
            json!({ "allocation": result, "attempts": 1 }),
        )
        .await;
    } else {
        emit(
            &s,
            "placement.queued",
            id,
            &r.pool,
            &metadata,
            json!({ "timeout_secs": timeout }),
        )
        .await;
    }
    Ok(Json(row_json(&s, fetch(&s, id).await?).await?))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id: Uuid = id
        .parse()
        .map_err(|_| Error::InvalidArgument("bad placement id".into()))?;
    Ok(Json(row_json(&s, fetch(&s, id).await?).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQ {
    status: Option<String>,
    pool: Option<String>,
    limit: Option<i64>,
}

async fn list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.session_placements
         where ($1::text is null or status = $1) and ($2::text is null or pool = $2)
         order by created_at desc limit $3"
    ))
    .bind(&q.status)
    .bind(&q.pool)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(row_json(&s, r).await?);
    }
    let (pending,): (i64,) =
        sqlx::query_as("select count(*) from musubi.session_placements where status = 'pending'")
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    Ok(Json(json!({ "placements": out, "pending": pending })))
}

/// `DELETE /v1/server/session_placements/{id}` — stop a pending placement (GameLift
/// `StopGameSessionPlacement`); 409 once it has already been fulfilled/timed out.
async fn cancel(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let id: Uuid = id
        .parse()
        .map_err(|_| Error::InvalidArgument("bad placement id".into()))?;
    let row = fetch(&s, id).await?;
    let n = sqlx::query(
        "update musubi.session_placements set status = 'cancelled', updated_at = now()
         where id = $1 and status = 'pending'",
    )
    .bind(id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::Conflict(format!("placement is {}", row.2)).into());
    }
    emit(&s, "placement.cancelled", id, &row.1, &row.4, json!({})).await;
    Ok(Json(row_json(&s, fetch(&s, id).await?).await?))
}

/// One worker pass: expire overdue placements, retry the rest oldest-first. Returns
/// `(fulfilled, timed_out)`. Runs on the lease holder every second (`spawn_worker`) and from tests.
pub async fn tick(s: &AppState) -> Result<(usize, usize)> {
    let expired: Vec<(Uuid, String, Value, i32)> = sqlx::query_as(
        "update musubi.session_placements set status = 'timed_out', updated_at = now()
         where status = 'pending' and timeout_at <= now()
         returning id, pool, metadata, attempts",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let timed_out = expired.len();
    for (id, pool, metadata, attempts) in &expired {
        emit(
            s,
            "placement.timed_out",
            *id,
            pool,
            metadata,
            json!({ "attempts": attempts }),
        )
        .await;
    }

    let pending: Vec<(Uuid, String, Value, Value, i32, i64)> = sqlx::query_as(
        "select id, pool, request, metadata, attempts,
                extract(epoch from timeout_at - now())::bigint
         from musubi.session_placements where status = 'pending'
         order by created_at limit 200",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut fulfilled = 0;
    // A pool that just answered "exhausted" will answer the same to the next ticket in line.
    let mut exhausted: std::collections::HashSet<String> = Default::default();
    for (id, pool, request, metadata, attempts, remaining) in pending {
        if exhausted.contains(&pool) {
            continue;
        }
        let req = match parse_req(&request) {
            Ok(r) => r,
            Err(e) => {
                fail(s, id, &pool, &metadata, &e.to_string()).await?;
                continue;
            }
        };
        // The routed allocation cold-starts a server itself and waits for it to report ready, so
        // each attempt gives the pool up to 5 s (bounded by the deadline) instead of polling with 0.
        let wait = std::time::Duration::from_secs(remaining.clamp(0, 5) as u64);
        match s
            .fleets
            .allocate_routed_with(&pool, &req.hint(), req.alloc(), wait)
            .await
        {
            Ok(a) => {
                let result = serde_json::to_value(&a).map_err(Error::internal)?;
                let n = sqlx::query(
                    "update musubi.session_placements set status = 'fulfilled', result = $2, attempts = attempts + 1,
                            last_error = null, updated_at = now(), fulfilled_at = now()
                     where id = $1 and status = 'pending'",
                )
                .bind(id)
                .bind(&result)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?
                .rows_affected();
                if n == 0 {
                    // Cancelled while we were allocating: hand the server back.
                    if let Err(e) = s.fleets.sdk_shutdown(a.server_id).await {
                        tracing::debug!(error = %e, server = %a.server_id, "placement: release after cancel");
                    }
                    continue;
                }
                fulfilled += 1;
                emit(
                    s,
                    "placement.fulfilled",
                    id,
                    &pool,
                    &metadata,
                    json!({ "allocation": result, "attempts": attempts + 1 }),
                )
                .await;
            }
            Err(e @ (Error::NotFound(_) | Error::InvalidArgument(_))) => {
                fail(s, id, &pool, &metadata, &e.to_string()).await?;
            }
            Err(e) => {
                if matches!(e, Error::ResourceExhausted(_)) {
                    exhausted.insert(pool.clone());
                }
                sqlx::query(
                    "update musubi.session_placements set attempts = attempts + 1, last_error = $2, updated_at = now()
                     where id = $1 and status = 'pending'",
                )
                .bind(id)
                .bind(e.to_string())
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
            }
        }
    }
    Ok((fulfilled, timed_out))
}

async fn fail(s: &AppState, id: Uuid, pool: &str, metadata: &Value, why: &str) -> Result<()> {
    sqlx::query(
        "update musubi.session_placements set status = 'failed', last_error = $2, attempts = attempts + 1, updated_at = now()
         where id = $1 and status = 'pending'",
    )
    .bind(id)
    .bind(why)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    emit(
        s,
        "placement.failed",
        id,
        pool,
        metadata,
        json!({ "error": why }),
    )
    .await;
    Ok(())
}

/// Leader-only background worker: `tick` every second while `active()`.
pub fn spawn_worker(state: AppState, active: impl Fn() -> bool + Send + 'static) {
    tokio::spawn(async move {
        let mut tick_ = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tick_.tick().await;
            if !active() {
                continue;
            }
            match tick(&state).await {
                Ok((f, t)) if f + t > 0 => {
                    tracing::info!(fulfilled = f, timed_out = t, "placements processed")
                }
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "placement worker failed"),
            }
        }
    });
}
