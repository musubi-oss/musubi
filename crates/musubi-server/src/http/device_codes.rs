//! Device-code / handoff login (RFC 8628 shape). A client that cannot type a password — TV,
//! console, headless box, second screen — calls `POST /v1/auth/device_code` and shows the
//! `user_code` (`ABCD-EFGH`); the player, signed in on a phone or browser, looks it up with
//! `GET /v1/auth/device_code/{user_code}` (consent screen: which client, since when) and calls
//! `…/approve` (or `…/deny`); meanwhile the device polls `POST …/token` every `interval` seconds
//! until it gets a full [`musubi_auth::Session`] for the approving account. Codes live
//! [`TTL_SECS`], are single-use, `slow_down` throttles eager pollers, the secret `device_code`
//! is stored hashed; operators list and revoke outstanding codes under `/v1/server/device_codes`.

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

pub const TTL_SECS: i64 = 600;
pub const INTERVAL_SECS: i64 = 5;
pub const PROVIDER: &str = "device_code";
const USER_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/auth/device_code", post(start))
        .route("/v1/auth/device_code/token", post(token))
        .route("/v1/auth/device_code/approve", post(approve))
        .route("/v1/auth/device_code/deny", post(deny))
        .route("/v1/auth/device_code/{user_code}", get(info))
        .route("/v1/server/device_codes", get(list))
        .route("/v1/server/device_codes/{id}", delete(remove))
}

fn hash(code: &str) -> String {
    crate::keys::hash(code)
}

fn gen_device_code() -> String {
    crate::keys::random_secret()
}

fn gen_user_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut s = String::with_capacity(9);
    for i in 0..8 {
        if i == 4 {
            s.push('-');
        }
        s.push(USER_ALPHABET[rng.gen_range(0..USER_ALPHABET.len())] as char);
    }
    s
}

/// `abcd efgh`, `ABCDEFGH`, `abcd-efgh` → `ABCD-EFGH`.
fn normalize(code: &str) -> Result<String, Error> {
    let s: String = code
        .trim()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .map(|c| match c {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    if s.len() != 8 {
        return Err(Error::InvalidArgument("user_code: 8 characters".into()));
    }
    Ok(format!("{}-{}", &s[..4], &s[4..]))
}

fn ts(t: OffsetDateTime) -> i64 {
    t.unix_timestamp()
}

#[derive(sqlx::FromRow)]
struct Row {
    id: Uuid,
    user_code: String,
    client: String,
    device_id: Option<String>,
    vars: Value,
    status: String,
    account_id: Option<Uuid>,
    ip: Option<String>,
    poll_count: i32,
    created_at: OffsetDateTime,
    expires_at: OffsetDateTime,
    last_poll_at: Option<OffsetDateTime>,
    approved_at: Option<OffsetDateTime>,
    consumed_at: Option<OffsetDateTime>,
}

const COLS: &str =
    "id, user_code, client, device_id, vars, status, account_id, ip, poll_count, created_at, \
    expires_at, last_poll_at, approved_at, consumed_at";

impl Row {
    fn expired(&self, now: OffsetDateTime) -> bool {
        matches!(self.status.as_str(), "pending" | "approved") && self.expires_at <= now
    }
    fn effective_status(&self, now: OffsetDateTime) -> &str {
        if self.expired(now) {
            "expired"
        } else {
            &self.status
        }
    }
    fn to_json(&self, now: OffsetDateTime, full: bool) -> Value {
        let mut v = json!({
            "id": self.id,
            "user_code": self.user_code,
            "client": self.client,
            "status": self.effective_status(now),
            "created_at": ts(self.created_at),
            "expires_at": ts(self.expires_at),
            "ip_hint": self.ip.as_deref().map(mask_ip),
        });
        if full {
            v["device_id"] = json!(self.device_id);
            v["vars"] = self.vars.clone();
            v["account_id"] = json!(self.account_id.map(AccountId));
            v["ip"] = json!(self.ip);
            v["poll_count"] = json!(self.poll_count);
            v["last_poll_at"] = json!(self.last_poll_at.map(ts));
            v["approved_at"] = json!(self.approved_at.map(ts));
            v["consumed_at"] = json!(self.consumed_at.map(ts));
        }
        v
    }
}

/// `203.0.113.42` → `203.0.x.x`; IPv6 keeps the first two groups.
fn mask_ip(ip: &str) -> String {
    if let Some((a, rest)) = ip.split_once('.') {
        let b = rest.split('.').next().unwrap_or("");
        return format!("{a}.{b}.x.x");
    }
    let mut it = ip.split(':');
    let (a, b) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
    format!("{a}:{b}::x")
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StartReq {
    client: Option<String>,
    device_id: Option<String>,
    vars: serde_json::Map<String, Value>,
}

async fn start(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<StartReq>,
) -> Result<Json<Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let client = r.client.unwrap_or_else(|| "device".into());
    if client.trim().is_empty() || client.len() > 64 {
        return Err(Error::InvalidArgument("client: 1..=64 chars".into()).into());
    }
    if r.device_id
        .as_ref()
        .is_some_and(|d| d.is_empty() || d.len() > 128)
    {
        return Err(Error::InvalidArgument("device_id: 1..=128 chars".into()).into());
    }
    if serde_json::to_vec(&r.vars).map(|v| v.len()).unwrap_or(0) > 4096 {
        return Err(Error::InvalidArgument("vars exceed 4 KB".into()).into());
    }
    let device_code = gen_device_code();
    let id = Uuid::now_v7();
    let mut user_code = gen_user_code();
    for attempt in 0..5 {
        let res = sqlx::query(
            "insert into musubi.device_codes (id, code_hash, user_code, client, device_id, vars, ip, expires_at) \
             values ($1, $2, $3, $4, $5, $6, $7, now() + make_interval(secs => $8))",
        )
        .bind(id)
        .bind(hash(&device_code))
        .bind(&user_code)
        .bind(client.trim())
        .bind(&r.device_id)
        .bind(Value::Object(r.vars.clone()))
        .bind(&ip.0)
        .bind(TTL_SECS as f64)
        .execute(&s.db)
        .await;
        match res {
            Ok(_) => break,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() && attempt < 4 => {
                user_code = gen_user_code();
            }
            Err(e) => return Err(Error::internal(e).into()),
        }
    }
    s.events
        .emit(
            "auth.device_code_started",
            None,
            json!({ "id": id, "client": client, "user_code": user_code }),
        )
        .await;
    Ok(Json(json!({
        "device_code": device_code,
        "user_code": user_code,
        "expires_in": TTL_SECS,
        "interval": INTERVAL_SECS,
        "expires_at": OffsetDateTime::now_utc().unix_timestamp() + TTL_SECS,
    })))
}

#[derive(Deserialize)]
struct TokenReq {
    device_code: String,
}

fn oauth_err(status: StatusCode, error: &str, extra: Value) -> Response {
    let mut body = json!({ "error": error });
    if let Some(o) = extra.as_object() {
        for (k, v) in o {
            body[k] = v.clone();
        }
    }
    (status, Json(body)).into_response()
}

/// Device poll. `202 {status: pending}` until approved, `429 slow_down` when polled faster than
/// `interval`, `403 access_denied`, `410 expired_token`, `409 already_used`, `404 invalid_grant`;
/// `200` with the session exactly once.
async fn token(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<TokenReq>,
) -> Result<Response, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.device_codes where code_hash = $1 for update"
    ))
    .bind(hash(&r.device_code))
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return Ok(oauth_err(
            StatusCode::NOT_FOUND,
            "invalid_grant",
            Value::Null,
        ));
    };
    let now = OffsetDateTime::now_utc();
    if row.expired(now) {
        sqlx::query("update musubi.device_codes set status = 'expired' where id = $1")
            .bind(row.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        return Ok(oauth_err(StatusCode::GONE, "expired_token", Value::Null));
    }
    match row.status.as_str() {
        "pending" => {
            let too_fast = row
                .last_poll_at
                .is_some_and(|t| (now - t).whole_seconds() < INTERVAL_SECS);
            sqlx::query(
                "update musubi.device_codes set last_poll_at = now(), poll_count = poll_count + 1 where id = $1",
            )
            .bind(row.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            tx.commit().await.map_err(Error::internal)?;
            if too_fast {
                return Ok(oauth_err(
                    StatusCode::TOO_MANY_REQUESTS,
                    "slow_down",
                    json!({ "interval": INTERVAL_SECS }),
                ));
            }
            Ok(oauth_err(
                StatusCode::ACCEPTED,
                "authorization_pending",
                json!({ "status": "pending", "interval": INTERVAL_SECS, "expires_at": ts(row.expires_at) }),
            ))
        }
        "denied" => Ok(oauth_err(
            StatusCode::FORBIDDEN,
            "access_denied",
            Value::Null,
        )),
        "consumed" => Ok(oauth_err(StatusCode::CONFLICT, "already_used", Value::Null)),
        "expired" => Ok(oauth_err(StatusCode::GONE, "expired_token", Value::Null)),
        "approved" => {
            let Some(account) = row.account_id.map(AccountId) else {
                return Err(Error::Internal("approved code without account".into()).into());
            };
            let vars = row.vars.as_object().cloned().unwrap_or_default();
            let sess = s
                .auth
                .session_for(account, vars, row.device_id.as_deref())
                .await?;
            sqlx::query(
                "update musubi.device_codes set status = 'consumed', consumed_at = now(), \
                 last_poll_at = now(), poll_count = poll_count + 1 where id = $1",
            )
            .bind(row.id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            tx.commit().await.map_err(Error::internal)?;
            super::auth::authenticated(&s, &sess, PROVIDER, &ip.0).await?;
            s.events
                .emit(
                    "auth.device_code_consumed",
                    Some(account),
                    json!({ "id": row.id, "client": row.client, "session_id": sess.session_id }),
                )
                .await;
            Ok(Json(sess).into_response())
        }
        other => Err(Error::Internal(format!("device code in state {other}")).into()),
    }
}

/// Consent-screen lookup by the signed-in approver.
async fn info(
    Bearer(_me): Bearer,
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    // RFC 8628 §5.1: user codes are short, so lookups and decisions are rate-limited like logins
    s.limits.auth_ip(&ip.0)?;
    let code = normalize(&code)?;
    let row: Option<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.device_codes where user_code = $1 and status in ('pending', 'approved')"
    ))
    .bind(&code)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = OffsetDateTime::now_utc();
    match row {
        Some(r) if !r.expired(now) => Ok(Json(r.to_json(now, false))),
        _ => Err(Error::NotFound("no active request with that code".into()).into()),
    }
}

#[derive(Deserialize)]
struct CodeReq {
    user_code: String,
}

async fn decide(s: &AppState, me: AccountId, code: &str, approve: bool) -> Result<Value, ApiError> {
    let code = normalize(code)?;
    let status = if approve { "approved" } else { "denied" };
    let row: Option<Row> = sqlx::query_as(&format!(
        "update musubi.device_codes set status = $2, account_id = $3, approved_at = now() \
         where user_code = $1 and status = 'pending' and expires_at > now() returning {COLS}"
    ))
    .bind(&code)
    .bind(status)
    .bind(me.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return Err(Error::NotFound("no pending request with that code".into()).into());
    };
    s.events
        .emit(
            if approve {
                "auth.device_code_approved"
            } else {
                "auth.device_code_denied"
            },
            Some(me),
            json!({ "id": row.id, "client": row.client }),
        )
        .await;
    Ok(json!({ "status": status, "client": row.client, "id": row.id }))
}

async fn approve(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<CodeReq>,
) -> Result<Json<Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    Ok(Json(decide(&s, me.account_id, &r.user_code, true).await?))
}

async fn deny(
    Bearer(me): Bearer,
    State(s): State<AppState>,
    ip: super::ClientIp,
    Json(r): Json<CodeReq>,
) -> Result<Json<Value>, ApiError> {
    s.limits.auth_ip(&ip.0)?;
    Ok(Json(decide(&s, me.account_id, &r.user_code, false).await?))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    status: Option<String>,
    account_id: Option<Uuid>,
    limit: Option<i64>,
}

async fn list(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if let Some(st) = &q.status {
        if !matches!(
            st.as_str(),
            "pending" | "approved" | "denied" | "consumed" | "expired"
        ) {
            return Err(Error::InvalidArgument(
                "status must be pending|approved|denied|consumed|expired".into(),
            )
            .into());
        }
    }
    let rows: Vec<Row> = sqlx::query_as(&format!(
        "select {COLS} from musubi.device_codes \
         where ($1::text is null or \
            case when $1 = 'expired' then (status = 'expired' or (status in ('pending','approved') and expires_at <= now())) \
                 when $1 in ('pending','approved') then status = $1 and expires_at > now() \
                 else status = $1 end) \
         and ($2::uuid is null or account_id = $2) \
         order by created_at desc limit $3"
    ))
    .bind(&q.status)
    .bind(q.account_id)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = OffsetDateTime::now_utc();
    Ok(Json(json!({
        "codes": rows.iter().map(|r| r.to_json(now, true)).collect::<Vec<_>>()
    })))
}

/// Revoke (or purge) a code — an approved-but-unconsumed code can no longer be exchanged.
async fn remove(
    ServerKey: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.device_codes where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("device code {id}")).into());
    }
    Ok(Json(json!({ "deleted": true })))
}
