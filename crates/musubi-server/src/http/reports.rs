//! Player reports (plan 5.12): any player can report another with a category, free text and
//! evidence pointers (match, world tick, chat message, URL); moderators work the queue and can
//! ban straight from the decision, which also closes the target's other open reports.
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::extract::Bearer;
use crate::http::{ApiError, ServerKey};

pub const CATEGORIES: &[&str] = &[
    "cheating",
    "harassment",
    "griefing",
    "offensive_name",
    "spam",
    "exploit",
    "other",
];
const STATUSES: &[&str] = &["open", "reviewing", "actioned", "dismissed"];
/// Reports one account may file per day.
const PER_DAY: i64 = 20;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/reports", post(file).get(mine))
        .route("/v1/admin/reports", get(list))
        .route("/v1/admin/reports/{id}", get(one).post(decide))
        .route("/v1/admin/accounts/{id}/reports", get(of_account))
}

#[derive(Deserialize)]
struct FileBody {
    target_id: Uuid,
    category: String,
    #[serde(default)]
    details: String,
    /// Free-form pointers: `match_id`, `world`, `tick`, `channel_id`, `message_id`, `url`, …
    #[serde(default)]
    evidence: Value,
}

async fn file(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<FileBody>,
) -> Result<Json<Value>, ApiError> {
    let target = AccountId(b.target_id);
    if target == me.account_id {
        return Err(Error::InvalidArgument("cannot report yourself".into()).into());
    }
    if !CATEGORIES.contains(&b.category.as_str()) {
        return Err(
            Error::InvalidArgument(format!("category must be one of {CATEGORIES:?}")).into(),
        );
    }
    if b.details.chars().count() > 2000 {
        return Err(Error::InvalidArgument("details over 2000 chars".into()).into());
    }
    if !b.evidence.is_object() && !b.evidence.is_null() {
        return Err(Error::InvalidArgument("evidence must be an object".into()).into());
    }
    if serde_json::to_vec(&b.evidence)
        .map(|v| v.len())
        .unwrap_or(0)
        > 4096
    {
        return Err(Error::InvalidArgument("evidence over 4 KB".into()).into());
    }
    let (today, dup): (i64, bool) = sqlx::query_as(
        "select (select count(*) from musubi.player_reports where reporter_id = $1 and created_at > now() - interval '1 day'),
                exists(select 1 from musubi.player_reports where reporter_id = $1 and target_id = $2 and status in ('open', 'reviewing'))",
    )
    .bind(me.account_id.0)
    .bind(target.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if dup {
        return Err(
            Error::AlreadyExists("you already have an open report on this player".into()).into(),
        );
    }
    if today >= PER_DAY {
        return Err(Error::ResourceExhausted(format!("{PER_DAY} reports per day")).into());
    }
    let id = Uuid::now_v7();
    let evidence = if b.evidence.is_null() {
        json!({})
    } else {
        b.evidence
    };
    sqlx::query(
        "insert into musubi.player_reports (id, reporter_id, target_id, category, details, evidence) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(me.account_id.0)
    .bind(target.0)
    .bind(&b.category)
    .bind(&b.details)
    .bind(&evidence)
    .execute(&s.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(d) if d.is_foreign_key_violation() => Error::NotFound("account".into()),
        e => Error::internal(e),
    })?;
    let open: i64 = sqlx::query_scalar(
        "select count(*) from musubi.player_reports where target_id = $1 and status in ('open', 'reviewing')",
    )
    .bind(target.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "player.reported",
            Some(target),
            json!({ "report_id": id, "reporter_id": me.account_id, "category": b.category, "open_reports": open }),
        )
        .await;
    Ok(Json(json!({ "id": id, "status": "open" })))
}

type Row = (
    Uuid,
    Uuid,
    Uuid,
    String,
    String,
    Value,
    String,
    Option<String>,
    Option<String>,
    Option<OffsetDateTime>,
    OffsetDateTime,
);
const COLS: &str = "id, reporter_id, target_id, category, details, evidence, status, decision, decided_by, decided_at, created_at";

fn ts(t: OffsetDateTime) -> Option<String> {
    t.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

fn report_json(r: Row) -> Value {
    let (
        id,
        reporter,
        target,
        category,
        details,
        evidence,
        status,
        decision,
        by,
        decided_at,
        created_at,
    ) = r;
    json!({
        "id": id, "reporter_id": reporter, "target_id": target, "category": category,
        "details": details, "evidence": evidence, "status": status, "decision": decision,
        "decided_by": by, "decided_at": decided_at.and_then(ts), "created_at": ts(created_at),
    })
}

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.player_reports where reporter_id = $1 order by created_at desc limit 100"
    ))
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    // reporters see outcome, not moderator notes
    let reports: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let mut v = report_json(r);
            v["decided_by"] = Value::Null;
            v
        })
        .collect();
    Ok(Json(json!({ "reports": reports })))
}

#[derive(Deserialize)]
struct ListQ {
    status: Option<String>,
    target_id: Option<Uuid>,
    category: Option<String>,
    #[serde(default = "default_limit")]
    limit: i64,
    cursor: Option<Uuid>,
}

fn default_limit() -> i64 {
    50
}

async fn list(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    if let Some(st) = &q.status {
        if !STATUSES.contains(&st.as_str()) {
            return Err(Error::InvalidArgument("status".into()).into());
        }
    }
    let limit = q.limit.clamp(1, 200);
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.player_reports
         where ($1::text is null or status = $1) and ($2::uuid is null or target_id = $2)
           and ($3::text is null or category = $3) and ($4::uuid is null or id < $4)
         order by id desc limit $5"
    ))
    .bind(&q.status)
    .bind(q.target_id)
    .bind(&q.category)
    .bind(q.cursor)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let next = (rows.len() as i64 == limit)
        .then(|| rows.last().map(|r| r.0))
        .flatten();
    Ok(Json(
        json!({ "reports": rows.into_iter().map(report_json).collect::<Vec<_>>(), "cursor": next }),
    ))
}

async fn one(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.player_reports where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    row.map(|r| Json(report_json(r)))
        .ok_or_else(|| Error::NotFound("report".into()).into())
}

/// Per-account summary for the moderator view: counts by category and status, recent reports.
async fn of_account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let by_cat: Vec<(String, i64)> = sqlx::query_as(
        "select category, count(*) from musubi.player_reports where target_id = $1 group by category order by 2 desc",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let by_status: Vec<(String, i64)> = sqlx::query_as(
        "select status, count(*) from musubi.player_reports where target_id = $1 group by status",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let distinct: i64 = sqlx::query_scalar(
        "select count(distinct reporter_id) from musubi.player_reports where target_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let recent: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.player_reports where target_id = $1 order by id desc limit 20"
    ))
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "account_id": id,
        "distinct_reporters": distinct,
        "by_category": by_cat.into_iter().map(|(c, n)| json!({"category": c, "count": n})).collect::<Vec<_>>(),
        "by_status": by_status.into_iter().map(|(c, n)| json!({"status": c, "count": n})).collect::<Vec<_>>(),
        "recent": recent.into_iter().map(report_json).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct DecideBody {
    /// `reviewing`, `actioned` or `dismissed`.
    status: String,
    #[serde(default)]
    decision: String,
    #[serde(default)]
    by: Option<String>,
    /// With `actioned`: ban the target right here (same semantics as `/v1/admin/accounts/{id}/ban`);
    /// every other open report on them is closed as actioned too.
    #[serde(default)]
    ban: Option<super::admin::BanBody>,
}

async fn decide(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(b): Json<DecideBody>,
) -> Result<Json<Value>, ApiError> {
    if !["reviewing", "actioned", "dismissed"].contains(&b.status.as_str()) {
        return Err(Error::InvalidArgument(
            "status must be reviewing, actioned or dismissed".into(),
        )
        .into());
    }
    if b.ban.is_some() && b.status != "actioned" {
        return Err(Error::InvalidArgument("a ban requires status actioned".into()).into());
    }
    if b.decision.chars().count() > 2000 {
        return Err(Error::InvalidArgument("decision over 2000 chars".into()).into());
    }
    let by = b.by.clone().unwrap_or_else(|| "admin".into());
    let closing = b.status != "reviewing";
    let row: Option<(Uuid,)> = sqlx::query_as(
        "update musubi.player_reports
         set status = $2, decision = nullif($3, ''), decided_by = $4,
             decided_at = case when $5 then now() else null end
         where id = $1 returning target_id",
    )
    .bind(id)
    .bind(&b.status)
    .bind(&b.decision)
    .bind(&by)
    .bind(closing)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((target,)) = row else {
        return Err(Error::NotFound("report".into()).into());
    };
    let target = AccountId(target);
    let mut banned = Value::Null;
    let mut also_closed = 0u64;
    if let Some(ban) = b.ban {
        banned = super::admin::apply_ban(&s, target, ban).await?;
        also_closed = sqlx::query(
            "update musubi.player_reports set status = 'actioned', decision = $2, decided_by = $3, decided_at = now()
             where target_id = $1 and status in ('open', 'reviewing')",
        )
        .bind(target.0)
        .bind(format!("banned with report {id}"))
        .bind(&by)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    }
    s.events
        .emit(
            "report.decided",
            Some(target),
            json!({ "report_id": id, "status": b.status, "by": by, "banned": !banned.is_null(), "also_closed": also_closed }),
        )
        .await;
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.player_reports where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut v = row.map(report_json).unwrap_or(Value::Null);
    v["ban"] = banned;
    v["also_closed"] = json!(also_closed);
    Ok(Json(v))
}
