//! Referral programs (LootLocker parity). The server defines a program (`PUT
//! /v1/server/referrals/programs/{id}`): what the invited player gets on redeem
//! (`referee_reward_table`), what the inviter gets per accepted referral
//! (`referrer_reward_table`) and at milestones (`milestones: [{count, reward_table}]`), caps
//! (`max_referrals_per_player`, `referee_max_account_age_days` — only fresh accounts count) and
//! an active window. Every player has one invite code per program (`GET
//! /v1/referrals/{program}/invite`, created lazily); a new player redeems it once per program
//! (`POST /v1/referrals/{program}/redeem {code}`). Operators see stats, list and void referrals
//! (voiding does not claw rewards back — that is a ledger decision — but stops milestone credit).

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

const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/server/referrals/programs", get(server_list))
        .route(
            "/v1/server/referrals/programs/{id}",
            put(server_put).get(server_get).delete(server_delete),
        )
        .route(
            "/v1/server/referrals/programs/{id}/referrals",
            get(server_referrals),
        )
        .route("/v1/server/referrals/{id}/void", post(server_void))
        .route("/v1/server/accounts/{id}/referrals", get(server_account))
        .route("/v1/referrals", get(my_programs))
        .route("/v1/referrals/{program}/invite", get(my_invite))
        .route("/v1/referrals/{program}/redeem", post(redeem))
        .route("/v1/referrals/{program}/referrals", get(my_referrals))
}

type ProgRow = (
    String,
    String,
    bool,
    Option<String>,
    Option<String>,
    Value,
    Option<i32>,
    Option<i32>,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    Value,
    OffsetDateTime,
    OffsetDateTime,
);
const PROG_COLS: &str = "id, name, enabled, referee_reward_table, referrer_reward_table, milestones, \
    max_referrals_per_player, referee_max_account_age_days, starts_at, ends_at, metadata, created_at, updated_at";

type RefRow = (
    Uuid,
    String,
    Uuid,
    Uuid,
    String,
    String,
    Option<String>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);
const REF_COLS: &str =
    "id, program_id, referrer_id, referee_id, code, status, void_reason, created_at, voided_at";

fn ts(t: OffsetDateTime) -> Value {
    json!(t
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default())
}

fn ots(t: Option<OffsetDateTime>) -> Value {
    t.map(ts).unwrap_or(Value::Null)
}

fn prog_doc(r: &ProgRow) -> Value {
    json!({
        "id": r.0, "name": r.1, "enabled": r.2,
        "referee_reward_table": r.3, "referrer_reward_table": r.4, "milestones": r.5,
        "max_referrals_per_player": r.6, "referee_max_account_age_days": r.7,
        "starts_at": ots(r.8), "ends_at": ots(r.9), "metadata": r.10,
        "created_at": ts(r.11), "updated_at": ts(r.12),
    })
}

fn ref_doc(r: &RefRow) -> Value {
    json!({
        "id": r.0, "program_id": r.1, "referrer_id": r.2, "referee_id": r.3, "code": r.4,
        "status": r.5, "void_reason": r.6, "created_at": ts(r.7), "voided_at": ots(r.8),
    })
}

fn valid_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-' || b == b'.'
        })
    {
        return Err(Error::InvalidArgument(
            "program id must be 1..=64 of [a-z0-9_.-]".into(),
        ));
    }
    Ok(())
}

fn fresh_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(ALPHABET[rng.gen_range(0..ALPHABET.len())] as char);
    }
    s
}

/// `abcd efgh`, `ABCDEFGH`, `abcd-efgh` → `ABCD-EFGH` (O→0, I/L→1).
fn normalize(code: &str) -> Result<String> {
    let s: String = code
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    if s.len() != 8 || !s.bytes().all(|b| ALPHABET.contains(&b)) {
        return Err(Error::InvalidArgument("malformed referral code".into()));
    }
    Ok(format!("{}-{}", &s[..4], &s[4..]))
}

async fn load_prog(s: &AppState, id: &str) -> Result<Option<ProgRow>> {
    sqlx::query_as(&format!(
        "select {PROG_COLS} from musubi.referral_programs where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)
}

async fn need_prog(s: &AppState, id: &str, client: bool) -> Result<ProgRow> {
    match load_prog(s, id).await? {
        Some(p) if p.2 || !client => Ok(p),
        _ => Err(Error::NotFound(format!("referral program {id}"))),
    }
}

fn active(p: &ProgRow, now: OffsetDateTime) -> bool {
    p.2 && p.8.is_none_or(|t| t <= now) && p.9.is_none_or(|t| t > now)
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

async fn table_exists(s: &AppState, t: &str) -> Result<()> {
    let n: i64 = sqlx::query_scalar("select count(*) from musubi.reward_tables where id = $1")
        .bind(t)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
    if n == 0 {
        return Err(Error::NotFound(format!("reward table {t}")));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(default)]
struct PutBody {
    name: String,
    enabled: bool,
    referee_reward_table: Option<String>,
    referrer_reward_table: Option<String>,
    milestones: Vec<Milestone>,
    max_referrals_per_player: Option<i32>,
    referee_max_account_age_days: Option<i32>,
    starts_at: Option<i64>,
    ends_at: Option<i64>,
    metadata: Value,
}

impl Default for PutBody {
    fn default() -> Self {
        Self {
            name: String::new(),
            enabled: true,
            referee_reward_table: None,
            referrer_reward_table: None,
            milestones: Vec::new(),
            max_referrals_per_player: None,
            referee_max_account_age_days: None,
            starts_at: None,
            ends_at: None,
            metadata: json!({}),
        }
    }
}

#[derive(Deserialize, serde::Serialize, Clone)]
struct Milestone {
    count: i64,
    reward_table: String,
}

async fn server_put(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<PutBody>,
) -> Result<Json<Value>, ApiError> {
    valid_id(&id)?;
    if b.name.len() > 256 {
        return Err(Error::InvalidArgument("name must be <= 256 chars".into()).into());
    }
    if b.max_referrals_per_player.is_some_and(|n| n < 1) {
        return Err(Error::InvalidArgument("max_referrals_per_player must be >= 1".into()).into());
    }
    if b.referee_max_account_age_days
        .is_some_and(|n| !(1..=3650).contains(&n))
    {
        return Err(
            Error::InvalidArgument("referee_max_account_age_days must be 1..=3650".into()).into(),
        );
    }
    if b.milestones.len() > 32 {
        return Err(Error::InvalidArgument("at most 32 milestones".into()).into());
    }
    let mut last = 0;
    for m in &b.milestones {
        if m.count <= last {
            return Err(Error::InvalidArgument(
                "milestones must have strictly increasing count >= 1".into(),
            )
            .into());
        }
        last = m.count;
    }
    if !b.metadata.is_object() {
        return Err(Error::InvalidArgument("metadata must be an object".into()).into());
    }
    let starts_at = b
        .starts_at
        .map(|t| {
            OffsetDateTime::from_unix_timestamp(t)
                .map_err(|_| Error::InvalidArgument("starts_at".into()))
        })
        .transpose()?;
    let ends_at = b
        .ends_at
        .map(|t| {
            OffsetDateTime::from_unix_timestamp(t)
                .map_err(|_| Error::InvalidArgument("ends_at".into()))
        })
        .transpose()?;
    if let (Some(a), Some(z)) = (starts_at, ends_at) {
        if z <= a {
            return Err(Error::InvalidArgument("ends_at must be after starts_at".into()).into());
        }
    }
    for t in b
        .referee_reward_table
        .iter()
        .chain(b.referrer_reward_table.iter())
        .chain(b.milestones.iter().map(|m| &m.reward_table))
    {
        table_exists(&s, t).await?;
    }
    let row: ProgRow = sqlx::query_as(&format!(
        "insert into musubi.referral_programs
            (id, name, enabled, referee_reward_table, referrer_reward_table, milestones,
             max_referrals_per_player, referee_max_account_age_days, starts_at, ends_at, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         on conflict (id) do update set name = excluded.name, enabled = excluded.enabled,
            referee_reward_table = excluded.referee_reward_table,
            referrer_reward_table = excluded.referrer_reward_table, milestones = excluded.milestones,
            max_referrals_per_player = excluded.max_referrals_per_player,
            referee_max_account_age_days = excluded.referee_max_account_age_days,
            starts_at = excluded.starts_at, ends_at = excluded.ends_at, metadata = excluded.metadata,
            updated_at = now()
         returning {PROG_COLS}"
    ))
    .bind(&id)
    .bind(&b.name)
    .bind(b.enabled)
    .bind(&b.referee_reward_table)
    .bind(&b.referrer_reward_table)
    .bind(json!(b.milestones))
    .bind(b.max_referrals_per_player)
    .bind(b.referee_max_account_age_days)
    .bind(starts_at)
    .bind(ends_at)
    .bind(&b.metadata)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "referral.program_updated",
            None,
            json!({ "program_id": id }),
        )
        .await;
    Ok(Json(prog_doc(&row)))
}

async fn stats(s: &AppState, id: &str) -> Result<Value> {
    let (accepted, voided, referrers, codes): (i64, i64, i64, i64) = sqlx::query_as(
        "select
            (select count(*) from musubi.referrals where program_id = $1 and status = 'accepted'),
            (select count(*) from musubi.referrals where program_id = $1 and status = 'voided'),
            (select count(distinct referrer_id) from musubi.referrals where program_id = $1 and status = 'accepted'),
            (select count(*) from musubi.referral_codes where program_id = $1)",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let top: Vec<(Uuid, i64)> = sqlx::query_as(
        "select referrer_id, count(*) from musubi.referrals
         where program_id = $1 and status = 'accepted'
         group by referrer_id order by count(*) desc, referrer_id limit 10",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({
        "accepted": accepted, "voided": voided, "referrers": referrers, "invite_codes": codes,
        "top_referrers": top.iter().map(|(a, n)| json!({ "account_id": a, "referrals": n })).collect::<Vec<_>>(),
    }))
}

async fn server_get(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = need_prog(&s, &id, false).await?;
    let mut v = prog_doc(&p);
    v["active"] = json!(active(&p, OffsetDateTime::now_utc()));
    v["stats"] = stats(&s, &id).await?;
    Ok(Json(v))
}

async fn server_list(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<ProgRow> = sqlx::query_as(&format!(
        "select {PROG_COLS} from musubi.referral_programs order by id"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "programs": rows.iter().map(prog_doc).collect::<Vec<_>>() }),
    ))
}

async fn server_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.referral_programs where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("referral program {id}")).into());
    }
    s.events
        .emit(
            "referral.program_deleted",
            None,
            json!({ "program_id": id }),
        )
        .await;
    Ok(Json(json!({ "deleted": true })))
}

#[derive(Deserialize, Default)]
struct ListQ {
    status: Option<String>,
    referrer: Option<Uuid>,
    limit: Option<i64>,
}

async fn server_referrals(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    need_prog(&s, &id, false).await?;
    if let Some(st) = q.status.as_deref() {
        if !["accepted", "voided"].contains(&st) {
            return Err(Error::InvalidArgument("status must be accepted|voided".into()).into());
        }
    }
    let limit = q.limit.unwrap_or(100);
    if !(1..=1000).contains(&limit) {
        return Err(Error::InvalidArgument("limit must be 1..=1000".into()).into());
    }
    let rows: Vec<RefRow> = sqlx::query_as(&format!(
        "select {REF_COLS} from musubi.referrals
         where program_id = $1 and ($2::text is null or status = $2) and ($3::uuid is null or referrer_id = $3)
         order by created_at desc limit $4"
    ))
    .bind(&id)
    .bind(&q.status)
    .bind(q.referrer)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "program_id": id, "referrals": rows.iter().map(ref_doc).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct VoidBody {
    reason: Option<String>,
}

async fn server_void(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Json(b): Json<VoidBody>,
) -> Result<Json<Value>, ApiError> {
    if b.reason.as_deref().is_some_and(|r| r.len() > 512) {
        return Err(Error::InvalidArgument("reason must be <= 512 chars".into()).into());
    }
    let row: Option<RefRow> = sqlx::query_as(&format!(
        "update musubi.referrals set status = 'voided', void_reason = $2, voided_at = now()
         where id = $1 and status = 'accepted' returning {REF_COLS}"
    ))
    .bind(id)
    .bind(&b.reason)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        let exists: i64 = sqlx::query_scalar("select count(*) from musubi.referrals where id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
        return Err(if exists > 0 {
            Error::Conflict(format!("referral {id} is already voided"))
        } else {
            Error::NotFound(format!("referral {id}"))
        }
        .into());
    };
    s.events
        .emit(
            "referral.voided",
            Some(AccountId(row.2)),
            json!({ "referral_id": id, "program_id": row.1, "referee_id": row.3, "reason": b.reason }),
        )
        .await;
    Ok(Json(ref_doc(&row)))
}

async fn account_view(s: &AppState, acc: Uuid, client: bool) -> Result<Vec<Value>> {
    let progs: Vec<ProgRow> = sqlx::query_as(&format!(
        "select {PROG_COLS} from musubi.referral_programs where enabled or not $1 order by id"
    ))
    .bind(client)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let codes: Vec<(String, String)> =
        sqlx::query_as("select program_id, code from musubi.referral_codes where account_id = $1")
            .bind(acc)
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    let counts: Vec<(String, i64, i64)> = sqlx::query_as(
        "select program_id,
                count(*) filter (where status = 'accepted'),
                count(*) filter (where status = 'voided')
         from musubi.referrals where referrer_id = $1 group by program_id",
    )
    .bind(acc)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let redeemed: Vec<(String, Uuid, String)> = sqlx::query_as(
        "select program_id, referrer_id, status from musubi.referrals where referee_id = $1",
    )
    .bind(acc)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = OffsetDateTime::now_utc();
    Ok(progs
        .iter()
        .map(|p| {
            let mut v = prog_doc(p);
            let (acc_n, void_n) = counts
                .iter()
                .find(|c| c.0 == p.0)
                .map(|c| (c.1, c.2))
                .unwrap_or((0, 0));
            v["active"] = json!(active(p, now));
            v["invite_code"] = json!(codes.iter().find(|c| c.0 == p.0).map(|c| &c.1));
            v["referrals"] = json!(acc_n);
            v["voided"] = json!(void_n);
            v["remaining"] = json!(p.6.map(|m| (m as i64 - acc_n).max(0)));
            v["next_milestone"] =
                p.5.as_array()
                    .and_then(|ms| ms.iter().find(|m| m["count"].as_i64().unwrap_or(0) > acc_n))
                    .cloned()
                    .unwrap_or(Value::Null);
            v["redeemed"] = redeemed
                .iter()
                .find(|r| r.0 == p.0)
                .map(|r| json!({ "referrer_id": r.1, "status": r.2 }))
                .unwrap_or(Value::Null);
            v
        })
        .collect())
}

async fn server_account(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(acc): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    account_exists(&s, acc).await?;
    Ok(Json(
        json!({ "account_id": acc, "programs": account_view(&s, acc, false).await? }),
    ))
}

async fn my_programs(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "programs": account_view(&s, me.account_id.0, true).await? }),
    ))
}

/// `GET /v1/referrals/{program}/invite` — the caller's shareable code for this program, minted
/// on first call (`get-referral-invite`).
async fn my_invite(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(program): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = need_prog(&s, &program, true).await?;
    let existing: Option<String> = sqlx::query_scalar(
        "select code from musubi.referral_codes where program_id = $1 and account_id = $2",
    )
    .bind(&program)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let code = match existing {
        Some(c) => c,
        None => {
            let mut code = None;
            for _ in 0..8 {
                let c = fresh_code();
                let r = sqlx::query(
                    "insert into musubi.referral_codes (program_id, account_id, code) values ($1, $2, $3)
                     on conflict (program_id, code) do nothing",
                )
                .bind(&program)
                .bind(me.account_id.0)
                .bind(&c)
                .execute(&s.db)
                .await
                .map_err(Error::internal)?;
                if r.rows_affected() == 1 {
                    code = Some(c);
                    break;
                }
            }
            code.ok_or_else(|| Error::Internal("could not mint a unique referral code".into()))?
        }
    };
    let n: i64 = sqlx::query_scalar(
        "select count(*) from musubi.referrals where program_id = $1 and referrer_id = $2 and status = 'accepted'",
    )
    .bind(&program)
    .bind(me.account_id.0)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "program_id": program, "code": code, "referrals": n,
        "remaining": p.6.map(|m| (m as i64 - n).max(0)),
        "active": active(&p, OffsetDateTime::now_utc()),
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RedeemBody {
    code: String,
}

/// `POST /v1/referrals/{program}/redeem {code}` — accept an invite: once per program, not your
/// own code, only while the program is active, only fresh accounts when the program says so,
/// and only while the inviter has referrals left. Pays the referee, the referrer, and any
/// milestone the referrer just crossed.
async fn redeem(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(program): Path<String>,
    Json(b): Json<RedeemBody>,
) -> Result<Json<Value>, ApiError> {
    let p = need_prog(&s, &program, true).await?;
    let now = OffsetDateTime::now_utc();
    if !active(&p, now) {
        return Err(Error::Conflict(format!("referral program {program} is not active")).into());
    }
    let code = normalize(&b.code)?;
    let referrer: Option<Uuid> = sqlx::query_scalar(
        "select account_id from musubi.referral_codes where program_id = $1 and code = $2",
    )
    .bind(&program)
    .bind(&code)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(referrer) = referrer else {
        return Err(Error::NotFound("referral code".into()).into());
    };
    if referrer == me.account_id.0 {
        return Err(Error::InvalidArgument("you cannot redeem your own code".into()).into());
    }
    if let Some(days) = p.7 {
        let created: OffsetDateTime =
            sqlx::query_scalar("select created_at from musubi.accounts where id = $1")
                .bind(me.account_id.0)
                .fetch_one(&s.db)
                .await
                .map_err(Error::internal)?;
        if created < now - time::Duration::days(days as i64) {
            return Err(Error::PermissionDenied(format!(
                "only accounts younger than {days} days can be referred"
            ))
            .into());
        }
    }
    let used: i64 = sqlx::query_scalar(
        "select count(*) from musubi.referrals where program_id = $1 and referrer_id = $2 and status = 'accepted'",
    )
    .bind(&program)
    .bind(referrer)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if p.6.is_some_and(|m| used >= m as i64) {
        return Err(Error::ResourceExhausted("this invite has no referrals left".into()).into());
    }
    let id = Uuid::now_v7();
    let r = sqlx::query(
        "insert into musubi.referrals (id, program_id, referrer_id, referee_id, code)
         values ($1, $2, $3, $4, $5) on conflict (program_id, referee_id) do nothing",
    )
    .bind(id)
    .bind(&program)
    .bind(referrer)
    .bind(me.account_id.0)
    .bind(&code)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if r.rows_affected() == 0 {
        return Err(Error::Conflict(format!("already referred in program {program}")).into());
    }
    let total = used + 1;
    let mut referee_rewards = Value::Null;
    let mut referrer_rewards = Value::Null;
    let mut milestone_rewards = Vec::new();
    if let Some(t) = p.3.as_deref() {
        referee_rewards = grant(
            &s,
            me.account_id,
            t,
            &format!("referral:{program}:{id}:referee"),
        )
        .await;
    }
    if let Some(t) = p.4.as_deref() {
        referrer_rewards = grant(
            &s,
            AccountId(referrer),
            t,
            &format!("referral:{program}:{id}:referrer"),
        )
        .await;
    }
    for m in p.5.as_array().into_iter().flatten() {
        if m["count"].as_i64() == Some(total) {
            if let Some(t) = m["reward_table"].as_str() {
                let r = grant(
                    &s,
                    AccountId(referrer),
                    t,
                    &format!("referral:{program}:milestone:{total}:{referrer}"),
                )
                .await;
                milestone_rewards.push(json!({ "count": total, "reward_table": t, "rewards": r }));
            }
        }
    }
    s.events
        .emit(
            "referral.accepted",
            Some(AccountId(referrer)),
            json!({
                "referral_id": id, "program_id": program, "referee_id": me.account_id.0,
                "referrals": total, "milestones": milestone_rewards,
            }),
        )
        .await;
    s.events
        .emit(
            "referral.redeemed",
            Some(me.account_id),
            json!({ "referral_id": id, "program_id": program, "referrer_id": referrer }),
        )
        .await;
    Ok(Json(json!({
        "referral_id": id, "program_id": program, "referrer_id": referrer,
        "rewards": referee_rewards, "referrer_rewards": referrer_rewards,
        "milestones": milestone_rewards,
    })))
}

async fn grant(s: &AppState, who: AccountId, table: &str, reference: &str) -> Value {
    match s.economy.grant_table(who, table, reference).await {
        Ok(r) => json!(r),
        Err(e) => {
            tracing::warn!(table, error = %e, "referral reward grant failed");
            json!({ "error": e.to_string() })
        }
    }
}

async fn my_referrals(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(program): Path<String>,
) -> Result<Json<Value>, ApiError> {
    need_prog(&s, &program, true).await?;
    let rows: Vec<RefRow> = sqlx::query_as(&format!(
        "select {REF_COLS} from musubi.referrals where program_id = $1 and referrer_id = $2
         order by created_at desc limit 500"
    ))
    .bind(&program)
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| json!({ "id": r.0, "referee_id": r.3, "status": r.5, "created_at": ts(r.7) }))
        .collect();
    Ok(Json(json!({ "program_id": program, "referrals": items })))
}
