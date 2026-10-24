//! Sanctions with appeals: typed moderation actions (`warn`, `mute`, `restrict_matchmaking`,
//! `restrict_social`, `restrict_trading`, `shadow`, `ban`, `custom`) that are enforced through the
//! ban cache (feature bans / full ban / shadow), can wait for a second moderator (`pending`), follow
//! an escalation ladder from the reason catalog, and can be appealed once by the player and decided
//! by a moderator. Game servers read active sanctions per account or tail the sync feed.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::admin::{apply_ban, lift_ban, BanBody};
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/admin/accounts/{id}/sanctions",
            post(create).get(of_account),
        )
        .route("/v1/admin/sanctions", get(list))
        .route("/v1/admin/sanctions/{id}", get(one))
        .route("/v1/admin/sanctions/{id}/approve", post(approve))
        .route("/v1/admin/sanctions/{id}/reject", post(reject))
        .route("/v1/admin/sanctions/{id}/revoke", post(revoke))
        .route("/v1/admin/appeals", get(appeals))
        .route("/v1/admin/appeals/{id}/decide", post(decide))
        .route("/v1/account/sanctions", get(mine))
        .route("/v1/account/sanctions/{id}/appeal", post(appeal))
        .route("/v1/server/accounts/{id}/sanctions", get(active_of))
        .route("/v1/server/sanctions/sync", get(sync))
}

/// Longest sanction / delay: 100 years (keeps timestamp arithmetic in range).
const MAX_SANCTION_SECS: i64 = 100 * 365 * 24 * 3600;
const MAX_REASON: usize = 256;
const MAX_MESSAGE: usize = 1024;
const MAX_NOTE: usize = 4000;
const MAX_SOURCE_BYTES: usize = 4 * 1024;
const MAX_FEATURES: usize = 32;
/// `to_timestamp()` cursor ceiling (9999-12-31T23:59:59Z): larger values make Postgres error out.
const MAX_CURSOR: i64 = 253_402_300_799;

/// Window of a sanction: `starts_in_secs` (negative = now) and `duration_secs`, both bounded by
/// [`MAX_SANCTION_SECS`] so `now + delay + duration` can never overflow.
fn schedule(
    now: OffsetDateTime,
    starts_in_secs: i64,
    duration_secs: Option<i64>,
) -> Result<(OffsetDateTime, Option<OffsetDateTime>)> {
    let delay = starts_in_secs.max(0);
    if delay > MAX_SANCTION_SECS {
        return Err(Error::InvalidArgument(format!(
            "starts_in_secs must be at most {MAX_SANCTION_SECS}"
        )));
    }
    if duration_secs.is_some_and(|d| !(1..=MAX_SANCTION_SECS).contains(&d)) {
        return Err(Error::InvalidArgument(format!(
            "duration_secs must be 1..={MAX_SANCTION_SECS}"
        )));
    }
    let starts = now
        .checked_add(time::Duration::seconds(delay))
        .ok_or_else(|| Error::InvalidArgument("starts_in_secs out of range".into()))?;
    let ends = duration_secs
        .map(|d| {
            starts
                .checked_add(time::Duration::seconds(d))
                .ok_or_else(|| Error::InvalidArgument("duration_secs out of range".into()))
        })
        .transpose()?;
    Ok((starts, ends))
}

/// Clamp a unix-seconds cursor into what `to_timestamp()` accepts.
fn cursor(secs: Option<i64>) -> Option<i64> {
    secs.map(|s| s.clamp(0, MAX_CURSOR))
}

pub const ACTIONS: &[&str] = &[
    "warn",
    "mute",
    "restrict_matchmaking",
    "restrict_social",
    "restrict_trading",
    "shadow",
    "ban",
    "custom",
];

/// Which ban features an action maps to (`ban` = full ban, `shadow` = shadow flag, `warn` = none).
fn features_of(action: &str, custom: &[String]) -> Vec<String> {
    let f: &[&str] = match action {
        "mute" => &["chat", "voice"],
        "restrict_matchmaking" => &["matchmaking"],
        "restrict_social" => &["friends", "groups", "chat"],
        "restrict_trading" => &["trading", "ugc"],
        "custom" => return custom.to_vec(),
        _ => &[],
    };
    f.iter().map(|s| s.to_string()).collect()
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    account_id: Uuid,
    action: String,
    features: Vec<String>,
    reason_code: Option<String>,
    reason: String,
    message: String,
    status: String,
    starts_at: OffsetDateTime,
    ends_at: Option<OffsetDateTime>,
    created_by: String,
    source: Value,
    note: String,
    revoke_note: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    revoked_at: Option<OffsetDateTime>,
}

const SELECT: &str = "select id, account_id, action, features, reason_code, reason, message, status, starts_at, ends_at,
    created_by, source, note, revoke_note, created_at, updated_at, revoked_at from musubi.sanctions";

fn to_json(r: Row) -> Value {
    let Row {
        id,
        account_id,
        action,
        features,
        reason_code,
        reason,
        message,
        status,
        starts_at,
        ends_at,
        created_by,
        source,
        note,
        revoke_note,
        created_at,
        updated_at,
        revoked_at,
    } = r;
    json!({
        "id": id, "account_id": account_id, "action": action, "features": features,
        "reason_code": reason_code, "reason": reason, "message": message, "status": status,
        "starts_at": starts_at.unix_timestamp(), "ends_at": ends_at.map(|t| t.unix_timestamp()),
        "created_by": created_by, "source": source, "note": note, "revoke_note": revoke_note,
        "created_at": created_at.unix_timestamp(), "updated_at": updated_at.unix_timestamp(),
        "revoked_at": revoked_at.map(|t| t.unix_timestamp()),
    })
}

/// The player-facing view: no moderator note / source.
fn player_view(mut v: Value) -> Value {
    if let Some(o) = v.as_object_mut() {
        o.remove("note");
        o.remove("source");
        o.remove("created_by");
    }
    v
}

async fn load(s: &AppState, id: Uuid) -> Result<Row> {
    sqlx::query_as(&format!("{SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound(format!("sanction {id}")))
}

/// Recompute the account's effective ban from its active sanctions: full ban if any `ban`, else
/// the union of feature restrictions; `shadow` if any; lifted when nothing enforcing remains.
pub(crate) async fn recompute(s: &AppState, account: AccountId) -> Result<()> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} where account_id = $1 and status = 'active' and starts_at <= now()
         and (ends_at is null or ends_at > now()) order by created_at"
    ))
    .bind(account.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let enforcing: Vec<&Row> = rows
        .iter()
        .filter(|r| r.action == "ban" || r.action == "shadow" || !r.features.is_empty())
        .collect();
    if enforcing.is_empty() {
        let from_sanctions: Option<(String,)> = sqlx::query_as(
            "select reason from musubi.account_bans where account_id = $1 and reason like 'sanction:%'",
        )
        .bind(account.0)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
        if from_sanctions.is_some() {
            lift_ban(s, account, false).await?;
        }
        return Ok(());
    }
    let full = enforcing.iter().any(|r| r.action == "ban");
    let shadow = enforcing.iter().any(|r| r.action == "shadow");
    let permanent = enforcing.iter().any(|r| r.ends_at.is_none());
    let until = if permanent {
        None
    } else {
        enforcing.iter().filter_map(|r| r.ends_at).max()
    };
    let mut features: Vec<String> = Vec::new();
    if !full {
        for r in &enforcing {
            for f in &r.features {
                if !features.contains(f) {
                    features.push(f.clone());
                }
            }
        }
    }
    let message = enforcing
        .iter()
        .map(|r| r.message.as_str())
        .filter(|m| !m.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let body = BanBody {
        reason: Some(format!(
            "sanction:{}",
            enforcing
                .iter()
                .map(|r| r.id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )),
        message: (!message.is_empty()).then_some(message),
        until_secs: until
            .map(|u| (u.unix_timestamp() - OffsetDateTime::now_utc().unix_timestamp()).max(1)),
        shadow,
        features,
        ..Default::default()
    };
    apply_ban(s, account, body).await.map_err(|e| e.0)?;
    Ok(())
}

async fn notify_player(s: &AppState, account: AccountId, subject: &str, content: Value) {
    let _ = s
        .notify
        .send(vec![musubi_social::NewNotification {
            user_id: account,
            sender_id: None,
            subject: subject.into(),
            content,
            code: 90,
            persistent: true,
            ttl_secs: None,
        }])
        .await;
}

#[derive(Deserialize, Default)]
struct CreateBody {
    /// One of [`ACTIONS`]; may be omitted with `escalate: true`.
    action: Option<String>,
    /// From the reason catalog: fills reason/message/duration, and holds the escalation ladder.
    reason_code: Option<String>,
    reason: Option<String>,
    message: Option<String>,
    duration_secs: Option<i64>,
    /// For `custom`: the feature list to restrict.
    #[serde(default)]
    features: Vec<String>,
    /// Pick action and duration from the reason's `ladder` by how many prior sanctions with this
    /// reason the account already has (the last step repeats).
    #[serde(default)]
    escalate: bool,
    /// Create as `pending`: not enforced until `/approve`.
    #[serde(default)]
    pending: bool,
    /// Where it came from: `{report_id, match_id, automod, …}`.
    #[serde(default)]
    source: Value,
    #[serde(default)]
    note: String,
    /// Delay before it starts (scheduled sanctions).
    #[serde(default)]
    starts_in_secs: i64,
}

async fn create(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(mut b): Json<CreateBody>,
) -> Result<Json<Value>, ApiError> {
    let account = AccountId(id);
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from musubi.accounts where id = $1)")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if !exists {
        return Err(Error::NotFound(format!("account {id}")).into());
    }
    if let Some(code) = b.reason_code.clone() {
        let row: Option<(String, String, Option<i64>, Value)> = sqlx::query_as(
            "select title, message, default_duration_secs, ladder from musubi.ban_reasons where code = $1",
        )
        .bind(&code)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
        let (title, message, dur, ladder) =
            row.ok_or_else(|| Error::NotFound(format!("ban reason {code}")))?;
        if b.reason.is_none() {
            b.reason = Some(title);
        }
        if b.message.is_none() && !message.is_empty() {
            b.message = Some(message);
        }
        if b.escalate {
            let steps = ladder.as_array().cloned().unwrap_or_default();
            if steps.is_empty() {
                return Err(Error::InvalidArgument(format!("reason {code} has no ladder")).into());
            }
            let prior: i64 = sqlx::query_scalar(
                "select count(*) from musubi.sanctions where account_id = $1 and reason_code = $2
                 and status <> 'revoked'",
            )
            .bind(id)
            .bind(&code)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
            let step = &steps[(prior as usize).min(steps.len() - 1)];
            b.action = step["action"].as_str().map(str::to_string);
            b.duration_secs = step["duration_secs"].as_i64();
            if let Some(m) = step["message"].as_str() {
                b.message = Some(m.into());
            }
        } else if b.duration_secs.is_none() {
            b.duration_secs = dur;
        }
    } else if b.escalate {
        return Err(Error::InvalidArgument("escalate needs a reason_code".into()).into());
    }
    let action = b
        .action
        .clone()
        .ok_or_else(|| Error::InvalidArgument("action is required".into()))?;
    if !ACTIONS.contains(&action.as_str()) {
        return Err(Error::InvalidArgument(format!("action must be one of {ACTIONS:?}")).into());
    }
    let features = features_of(&action, &b.features);
    for f in &features {
        if !crate::bans::FEATURES.contains(&f.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "unknown feature {f}; known: {:?}",
                crate::bans::FEATURES
            ))
            .into());
        }
    }
    if action == "custom" && features.is_empty() {
        return Err(Error::InvalidArgument("custom needs features".into()).into());
    }
    if features.len() > MAX_FEATURES {
        return Err(Error::InvalidArgument(format!("at most {MAX_FEATURES} features")).into());
    }
    if b.reason
        .as_deref()
        .is_some_and(|r| r.chars().count() > MAX_REASON)
    {
        return Err(Error::InvalidArgument(format!("reason over {MAX_REASON} chars")).into());
    }
    if b.message
        .as_deref()
        .is_some_and(|m| m.chars().count() > MAX_MESSAGE)
    {
        return Err(Error::InvalidArgument(format!("message over {MAX_MESSAGE} chars")).into());
    }
    if b.note.chars().count() > MAX_NOTE {
        return Err(Error::InvalidArgument(format!("note over {MAX_NOTE} chars")).into());
    }
    if b.source.is_object()
        && serde_json::to_vec(&b.source)
            .map(|v| v.len())
            .unwrap_or(usize::MAX)
            > MAX_SOURCE_BYTES
    {
        return Err(Error::InvalidArgument("source exceeds 4 KiB".into()).into());
    }
    let sid = Uuid::now_v7();
    let status = if b.pending { "pending" } else { "active" };
    let (starts, ends) = schedule(OffsetDateTime::now_utc(), b.starts_in_secs, b.duration_secs)?;
    sqlx::query(
        "insert into musubi.sanctions (id, account_id, action, features, reason_code, reason, message, status,
            starts_at, ends_at, created_by, source, note)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'admin', $11, $12)",
    )
    .bind(sid)
    .bind(id)
    .bind(&action)
    .bind(&features)
    .bind(&b.reason_code)
    .bind(b.reason.clone().unwrap_or_default())
    .bind(b.message.clone().unwrap_or_default())
    .bind(status)
    .bind(starts)
    .bind(ends)
    .bind(if b.source.is_object() { b.source.clone() } else { json!({}) })
    .bind(&b.note)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if !b.pending {
        recompute(&s, account).await?;
        notify_player(
            &s,
            account,
            "sanction",
            json!({ "id": sid, "action": action, "message": b.message, "ends_at": ends.map(|t| t.unix_timestamp()) }),
        )
        .await;
    }
    s.events
        .emit(
            "sanction.created",
            Some(account),
            json!({ "id": sid, "action": action, "status": status, "reason_code": b.reason_code }),
        )
        .await;
    Ok(Json(to_json(load(&s, sid).await?)))
}

#[derive(Deserialize, Default)]
struct ListQ {
    status: Option<String>,
    action: Option<String>,
    limit: Option<i64>,
    before: Option<i64>,
}

async fn of_account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} where account_id = $1 and ($2::text is null or status = $2) order by created_at desc limit $3"
    ))
    .bind(id)
    .bind(q.status.as_deref())
    .bind(q.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "sanctions": rows.into_iter().map(to_json).collect::<Vec<_>>() }),
    ))
}

async fn list(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} where ($1::text is null or status = $1) and ($2::text is null or action = $2)
         and ($3::bigint is null or created_at < to_timestamp($3)) order by created_at desc limit $4"
    ))
    .bind(q.status.as_deref())
    .bind(q.action.as_deref())
    .bind(cursor(q.before))
    .bind(q.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "sanctions": rows.into_iter().map(to_json).collect::<Vec<_>>() }),
    ))
}

async fn one(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut v = to_json(load(&s, id).await?);
    let appeal: Option<AppealRow> = sqlx::query_as(
        "select id, text, status, decision_note, decided_by, created_at, decided_at
             from musubi.sanction_appeals where sanction_id = $1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    v["appeal"] = appeal.map_or(Value::Null, appeal_json);
    Ok(Json(v))
}

type AppealRow = (
    Uuid,
    String,
    String,
    String,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

fn appeal_json((id, text, status, note, by, at, decided): AppealRow) -> Value {
    json!({ "id": id, "text": text, "status": status, "decision_note": note, "decided_by": by,
        "created_at": at.unix_timestamp(), "decided_at": decided.map(|t| t.unix_timestamp()) })
}

#[derive(Deserialize, Default)]
struct NoteBody {
    #[serde(default)]
    note: String,
}

async fn set_status(
    s: &AppState,
    id: Uuid,
    from: &[&str],
    to: &str,
    note: &str,
) -> Result<(Row, bool)> {
    if note.chars().count() > MAX_NOTE {
        return Err(Error::InvalidArgument(format!(
            "note over {MAX_NOTE} chars"
        )));
    }
    let r = load(s, id).await?;
    if !from.contains(&r.status.as_str()) {
        return Err(Error::Conflict(format!(
            "sanction {id} is {}, expected one of {from:?}",
            r.status
        )));
    }
    // the transition itself is conditional: two moderators racing on the same sanction cannot
    // both win (the loser sees a conflict instead of re-applying the update)
    let from_list: Vec<String> = from.iter().map(|f| f.to_string()).collect();
    let n = sqlx::query(
        "update musubi.sanctions set status = $2, updated_at = now(),
            revoke_note = case when $2 = 'revoked' then $3 else revoke_note end,
            revoked_at = case when $2 = 'revoked' then now() else revoked_at end
         where id = $1 and status = any($4)",
    )
    .bind(id)
    .bind(to)
    .bind(note)
    .bind(&from_list)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::Conflict(format!(
            "sanction {id} changed concurrently, expected one of {from:?}"
        )));
    }
    let was_pending = r.status == "pending";
    Ok((r, was_pending))
}

async fn approve(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    body: Option<Json<NoteBody>>,
) -> Result<Json<Value>, ApiError> {
    let note = body.map(|Json(b)| b.note).unwrap_or_default();
    let (r, _) = set_status(&s, id, &["pending"], "active", &note).await?;
    let account = AccountId(r.account_id);
    recompute(&s, account).await?;
    notify_player(
        &s,
        account,
        "sanction",
        json!({ "id": id, "action": r.action, "message": r.message, "ends_at": r.ends_at.map(|t| t.unix_timestamp()) }),
    )
    .await;
    s.events
        .emit(
            "sanction.approved",
            Some(account),
            json!({ "id": id, "action": r.action }),
        )
        .await;
    Ok(Json(to_json(load(&s, id).await?)))
}

async fn reject(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    body: Option<Json<NoteBody>>,
) -> Result<Json<Value>, ApiError> {
    let note = body.map(|Json(b)| b.note).unwrap_or_default();
    let (r, _) = set_status(&s, id, &["pending"], "revoked", &note).await?;
    s.events
        .emit(
            "sanction.rejected",
            Some(AccountId(r.account_id)),
            json!({ "id": id }),
        )
        .await;
    Ok(Json(to_json(load(&s, id).await?)))
}

async fn revoke(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    body: Option<Json<NoteBody>>,
) -> Result<Json<Value>, ApiError> {
    let note = body.map(|Json(b)| b.note).unwrap_or_default();
    let (r, _) = set_status(&s, id, &["pending", "active"], "revoked", &note).await?;
    let account = AccountId(r.account_id);
    recompute(&s, account).await?;
    notify_player(
        &s,
        account,
        "sanction_lifted",
        json!({ "id": id, "action": r.action }),
    )
    .await;
    s.events
        .emit(
            "sanction.revoked",
            Some(account),
            json!({ "id": id, "action": r.action, "note": note }),
        )
        .await;
    Ok(Json(to_json(load(&s, id).await?)))
}

/// Leader sweep: flip `active` sanctions past `ends_at` to `expired` and recompute those accounts.
pub(crate) async fn expire_sanctions(s: &AppState) -> Result<usize> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "update musubi.sanctions set status = 'expired', updated_at = now()
         where status = 'active' and ends_at is not null and ends_at <= now() returning id, account_id",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut accounts: Vec<Uuid> = rows.iter().map(|r| r.1).collect();
    accounts.sort_unstable();
    accounts.dedup();
    for a in accounts {
        recompute(s, AccountId(a)).await?;
    }
    for (id, a) in &rows {
        s.events
            .emit("sanction.expired", Some(AccountId(*a)), json!({ "id": id }))
            .await;
    }
    Ok(rows.len())
}

#[derive(Deserialize, Default)]
struct AppealsQ {
    status: Option<String>,
    limit: Option<i64>,
}

async fn appeals(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<AppealsQ>,
) -> Result<Json<Value>, ApiError> {
    type R = (
        Uuid,
        String,
        String,
        String,
        Option<String>,
        OffsetDateTime,
        Option<OffsetDateTime>,
        Uuid,
        Uuid,
        String,
    );
    let rows: Vec<R> = sqlx::query_as(
        "select a.id, a.text, a.status, a.decision_note, a.decided_by, a.created_at, a.decided_at,
                a.sanction_id, a.account_id, s.action
         from musubi.sanction_appeals a join musubi.sanctions s on s.id = a.sanction_id
         where ($1::text is null or a.status = $1) order by a.created_at limit $2",
    )
    .bind(q.status.as_deref().or(Some("open")).filter(|s| *s != "all"))
    .bind(q.limit.unwrap_or(100).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let out: Vec<Value> = rows
        .into_iter()
        .map(|(id, text, st, note, by, at, dec, sid, acc, action)| {
            let mut v = appeal_json((id, text, st, note, by, at, dec));
            v["sanction_id"] = json!(sid);
            v["account_id"] = json!(acc);
            v["action"] = json!(action);
            v
        })
        .collect();
    Ok(Json(json!({ "appeals": out })))
}

#[derive(Deserialize)]
struct DecideBody {
    /// `upheld` keeps the sanction; `overturned` revokes it.
    outcome: String,
    #[serde(default)]
    note: String,
}

async fn decide(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(b): Json<DecideBody>,
) -> Result<Json<Value>, ApiError> {
    if !["upheld", "overturned"].contains(&b.outcome.as_str()) {
        return Err(Error::InvalidArgument("outcome must be upheld or overturned".into()).into());
    }
    if b.note.chars().count() > MAX_NOTE {
        return Err(Error::InvalidArgument(format!("note over {MAX_NOTE} chars")).into());
    }
    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "update musubi.sanction_appeals set status = $2, decision_note = $3, decided_by = 'admin', decided_at = now()
         where id = $1 and status = 'open' returning sanction_id, account_id, status",
    )
    .bind(id)
    .bind(&b.outcome)
    .bind(&b.note)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((sid, acc, _)) = row else {
        return Err(Error::NotFound(format!("open appeal {id}")).into());
    };
    let account = AccountId(acc);
    if b.outcome == "overturned" {
        let r = load(&s, sid).await?;
        if ["pending", "active"].contains(&r.status.as_str()) {
            set_status(
                &s,
                sid,
                &["pending", "active"],
                "revoked",
                &format!("appeal overturned: {}", b.note),
            )
            .await?;
            recompute(&s, account).await?;
        }
    }
    notify_player(
        &s,
        account,
        "appeal_decided",
        json!({ "appeal_id": id, "sanction_id": sid, "outcome": b.outcome, "note": b.note }),
    )
    .await;
    s.events
        .emit(
            "appeal.decided",
            Some(account),
            json!({ "appeal_id": id, "sanction_id": sid, "outcome": b.outcome }),
        )
        .await;
    Ok(Json(
        json!({ "appeal_id": id, "sanction_id": sid, "outcome": b.outcome }),
    ))
}

async fn mine(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} where account_id = $1 and status <> 'pending' order by created_at desc limit 200"
    ))
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let appeals: Vec<(Uuid, AppealRow)> = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            String,
            String,
            Option<String>,
            OffsetDateTime,
            Option<OffsetDateTime>,
        ),
    >(
        "select sanction_id, id, text, status, decision_note, decided_by, created_at, decided_at
         from musubi.sanction_appeals where account_id = $1",
    )
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?
    .into_iter()
    .map(|(sid, id, t, st, n, by, at, dec)| (sid, (id, t, st, n, by, at, dec)))
    .collect();
    let out: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            let sid = r.id;
            let mut v = player_view(to_json(r));
            v["appeal"] = appeals
                .iter()
                .find(|(s, _)| *s == sid)
                .map_or(Value::Null, |(_, a)| appeal_json(a.clone()));
            v
        })
        .collect();
    let active = out.iter().filter(|v| v["status"] == "active").count();
    Ok(Json(json!({ "sanctions": out, "active": active })))
}

#[derive(Deserialize)]
struct AppealBody {
    text: String,
}

async fn appeal(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(b): Json<AppealBody>,
) -> Result<Json<Value>, ApiError> {
    let text = b.text.trim();
    if text.is_empty() || text.len() > 4000 {
        return Err(Error::InvalidArgument("text must be 1..4000 chars".into()).into());
    }
    let r = load(&s, id).await?;
    if r.account_id != me.account_id.0 || r.status == "pending" {
        return Err(Error::NotFound(format!("sanction {id}")).into());
    }
    if r.status != "active" {
        return Err(Error::Conflict(format!("sanction is {}", r.status)).into());
    }
    let aid = Uuid::now_v7();
    let res = sqlx::query(
        "insert into musubi.sanction_appeals (id, sanction_id, account_id, text) values ($1, $2, $3, $4)",
    )
    .bind(aid)
    .bind(id)
    .bind(me.account_id.0)
    .bind(text)
    .execute(&s.db)
    .await;
    if let Err(sqlx::Error::Database(e)) = &res {
        if e.is_unique_violation() {
            return Err(Error::Conflict("this sanction was already appealed".into()).into());
        }
    }
    res.map_err(Error::internal)?;
    s.events
        .emit(
            "appeal.filed",
            Some(me.account_id),
            json!({ "appeal_id": aid, "sanction_id": id, "action": r.action }),
        )
        .await;
    Ok(Json(
        json!({ "appeal_id": aid, "sanction_id": id, "status": "open" }),
    ))
}

/// `GET /v1/server/accounts/{id}/sanctions` — what is enforced right now (for game servers).
async fn active_of(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} where account_id = $1 and status = 'active' and starts_at <= now()
         and (ends_at is null or ends_at > now()) order by created_at"
    ))
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut features: Vec<String> = Vec::new();
    let mut full = false;
    for r in &rows {
        full |= r.action == "ban";
        for f in &r.features {
            if !features.contains(f) {
                features.push(f.clone());
            }
        }
    }
    Ok(Json(json!({
        "sanctions": rows.into_iter().map(to_json).collect::<Vec<_>>(),
        "banned": full, "restricted": features,
    })))
}

#[derive(Deserialize, Default)]
struct SyncQ {
    /// Unix seconds; rows changed after this (exclusive).
    since: Option<i64>,
    limit: Option<i64>,
}

/// `GET /v1/server/sanctions/sync?since` — every sanction changed since the cursor, oldest first,
/// with the cursor to continue from (mirror into your own service).
async fn sync(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<SyncQ>,
) -> Result<Json<Value>, ApiError> {
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "{SELECT} where updated_at > to_timestamp($1) order by updated_at, id limit $2"
    ))
    .bind(cursor(q.since).unwrap_or(0))
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let next = rows.last().map(|r| r.updated_at.unix_timestamp());
    let more = rows.len() as i64 == limit;
    Ok(Json(json!({
        "sanctions": rows.into_iter().map(to_json).collect::<Vec<_>>(),
        "next": next, "more": more,
    })))
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn schedule_rejects_values_that_used_to_panic() {
        let now = OffsetDateTime::now_utc();
        assert!(schedule(now, i64::MAX, None).is_err());
        assert!(schedule(now, 0, Some(i64::MAX)).is_err());
        assert!(schedule(now, MAX_SANCTION_SECS, Some(MAX_SANCTION_SECS)).is_ok());
        assert!(schedule(now, MAX_SANCTION_SECS + 1, None).is_err());
        assert!(schedule(now, 0, Some(0)).is_err());
        assert!(schedule(now, 0, Some(-1)).is_err());
    }

    #[test]
    fn schedule_keeps_the_old_semantics_in_range() {
        let now = OffsetDateTime::now_utc();
        let (s, e) = schedule(now, -30, None).unwrap();
        assert_eq!(s, now, "negative delay = now");
        assert_eq!(e, None);
        let (s, e) = schedule(now, 60, Some(3600)).unwrap();
        assert_eq!(s, now + time::Duration::seconds(60));
        assert_eq!(e, Some(s + time::Duration::seconds(3600)));
    }

    #[test]
    fn cursors_stay_inside_to_timestamp_range() {
        assert_eq!(cursor(None), None);
        assert_eq!(cursor(Some(-5)), Some(0));
        assert_eq!(cursor(Some(1_700_000_000)), Some(1_700_000_000));
        assert_eq!(cursor(Some(i64::MAX)), Some(MAX_CURSOR));
    }

    #[test]
    fn features_of_maps_actions() {
        assert_eq!(features_of("mute", &[]), vec!["chat", "voice"]);
        assert!(features_of("ban", &[]).is_empty());
        assert_eq!(features_of("custom", &["ugc".to_string()]), vec!["ugc"]);
    }
}
