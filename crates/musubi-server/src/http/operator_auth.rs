//! Console sign-in for operators (`musubi.operators`): e-mail + password. A successful login
//! issues a short-lived server key of the operator's role (`session:<email>`, `SESSION_TTL`) —
//! the console then talks to the API exactly like a key holder; logout revokes that key. Five
//! failures lock the account for 15 minutes. Admins create operators directly (first admin:
//! with the bootstrap server key) or reset passwords; everyone can change their own.
use axum::extract::{Path, State};
use axum::routing::{delete, get, post, put};
use axum::{Extension, Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};
use crate::keys::{Caller, Role};

const SESSION_TTL: time::Duration = time::Duration::hours(12);
const MAX_FAILURES: i32 = 5;
const LOCK_FOR: time::Duration = time::Duration::minutes(15);
const BCRYPT_COST: u32 = 10;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/operator/login", post(login))
        .route("/v1/operator/logout", delete(logout))
        .route("/v1/operator/me", get(me))
        .route("/v1/operator/me/password", put(change_password))
        .route("/v1/admin/operators", post(create))
        .route("/v1/admin/operators/{id}/password", put(reset_password))
}

fn check_password(p: &str) -> Result<()> {
    if p.chars().count() < 10 || p.len() > 256 {
        return Err(Error::InvalidArgument(
            "password: 10..=256 characters".into(),
        ));
    }
    Ok(())
}

fn hash_password(p: &str) -> Result<String> {
    bcrypt::hash(p, BCRYPT_COST).map_err(|e| Error::internal(std::io::Error::other(e.to_string())))
}

fn rfc(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

type OpRow = (
    Uuid,
    String,
    String,
    String,
    Option<String>,
    i32,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
);
const OP_COLS: &str =
    "id, email, name, role, password_hash, failed_logins, locked_until, last_login_at";

fn op_json(r: &OpRow) -> Value {
    json!({
        "id": r.0, "email": r.1, "name": r.2, "role": r.3, "has_password": r.4.is_some(),
        "locked_until": r.6.map(rfc), "last_login_at": r.7.map(rfc),
    })
}

#[derive(Deserialize)]
struct LoginBody {
    email: String,
    password: String,
}

async fn login(
    State(s): State<AppState>,
    Json(b): Json<LoginBody>,
) -> Result<Json<Value>, ApiError> {
    let email = b.email.trim().to_ascii_lowercase();
    let bad = || Error::Unauthenticated("wrong e-mail or password".into());
    let row: Option<OpRow> = sqlx::query_as(&format!(
        "select {OP_COLS} from musubi.operators where email = $1"
    ))
    .bind(&email)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(r) = row else {
        // same cost as a real check so timing does not reveal membership
        let _ = bcrypt::verify(
            &b.password,
            "$2b$10$CwTycUXWue0Thq9StjUM0uJ8ZbQ6WIu1gQ1UyG5nQhJXzOeDqeEqS",
        );
        return Err(bad().into());
    };
    if let Some(until) = r.6 {
        if until > OffsetDateTime::now_utc() {
            return Err(
                Error::PermissionDenied(format!("account locked until {}", rfc(until))).into(),
            );
        }
    }
    let Some(hash) = r.4.as_deref() else {
        return Err(Error::PermissionDenied(
            "this operator has no password yet — sign in with a key or ask an admin to set one"
                .into(),
        )
        .into());
    };
    let ok = bcrypt::verify(&b.password, hash).unwrap_or(false);
    if !ok {
        let failures = r.5 + 1;
        let lock = if failures >= MAX_FAILURES {
            Some(OffsetDateTime::now_utc() + LOCK_FOR)
        } else {
            None
        };
        sqlx::query(
            "update musubi.operators set failed_logins = case when $2::timestamptz is null then $3 else 0 end, locked_until = $2 where id = $1",
        )
        .bind(r.0)
        .bind(lock)
        .bind(failures)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        return Err(bad().into());
    }
    let role =
        Role::parse(&r.3).ok_or_else(|| Error::internal(std::io::Error::other("bad role")))?;
    let expires = OffsetDateTime::now_utc() + SESSION_TTL;
    let (key, secret) = s
        .keys
        .issue_with(
            &s.db,
            &format!("session:{email}"),
            role,
            Some(expires),
            Some(&email),
            &[],
        )
        .await?;
    sqlx::query(
        "update musubi.operators set failed_logins = 0, locked_until = null, last_login_at = now() where id = $1",
    )
    .bind(r.0)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "key": secret, "key_id": key.id, "role": r.3, "expires_at": rfc(expires),
        "operator": op_json(&r),
    })))
}

/// Revokes the session key the request came with (any key named `session:*`; other keys are left alone).
async fn logout(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Value>, ApiError> {
    let revoked = if caller.name.starts_with("session:") {
        s.keys.revoke(&s.db, caller.id).await?
    } else {
        false
    };
    Ok(Json(json!({ "revoked": revoked })))
}

/// The operator behind the caller's session key (`session:<email>`), or just the key's role for plain keys.
async fn me(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
) -> Result<Json<Value>, ApiError> {
    let email = caller.name.strip_prefix("session:");
    let op: Option<OpRow> = match email {
        Some(e) => sqlx::query_as(&format!(
            "select {OP_COLS} from musubi.operators where email = $1"
        ))
        .bind(e)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?,
        None => None,
    };
    Ok(Json(json!({
        "key_name": caller.name, "role": caller.role.as_str(), "scopes": caller.scopes,
        "operator": op.as_ref().map(op_json),
    })))
}

#[derive(Deserialize)]
struct ChangeBody {
    current: String,
    password: String,
}

async fn change_password(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Json(b): Json<ChangeBody>,
) -> Result<Json<Value>, ApiError> {
    let Some(email) = caller.name.strip_prefix("session:") else {
        return Err(Error::PermissionDenied(
            "only a signed-in operator can change a password".into(),
        )
        .into());
    };
    check_password(&b.password)?;
    let row: Option<OpRow> = sqlx::query_as(&format!(
        "select {OP_COLS} from musubi.operators where email = $1"
    ))
    .bind(email)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let r = row.ok_or_else(|| Error::NotFound("operator".into()))?;
    let ok =
        r.4.as_deref()
            .map(|h| bcrypt::verify(&b.current, h).unwrap_or(false))
            .unwrap_or(false);
    if !ok {
        return Err(Error::Unauthenticated("current password is wrong".into()).into());
    }
    let h = hash_password(&b.password)?;
    sqlx::query(
        "update musubi.operators set password_hash = $2, password_set_at = now() where id = $1",
    )
    .bind(r.0)
    .bind(h)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CreateBody {
    email: String,
    name: Option<String>,
    role: String,
    password: String,
}

/// Admin: add an operator with a password straight away (no invite round-trip; also how the very
/// first admin account is made, with the bootstrap server key).
async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Json(b): Json<CreateBody>,
) -> Result<Json<Value>, ApiError> {
    if caller.role != Role::Admin {
        return Err(Error::PermissionDenied("admin key required".into()).into());
    }
    let email = b.email.trim().to_ascii_lowercase();
    if !(email.contains('@') && email.len() <= 254 && !email.contains(char::is_whitespace)) {
        return Err(Error::InvalidArgument("email: a valid address".into()).into());
    }
    let role = Role::parse(&b.role.trim().to_ascii_lowercase())
        .ok_or_else(|| Error::InvalidArgument("role must be server, admin or support".into()))?;
    check_password(&b.password)?;
    let name = b
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| email.split('@').next().unwrap_or("operator").to_string());
    if name.len() > 64 {
        return Err(Error::InvalidArgument("name: at most 64 chars".into()).into());
    }
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from musubi.operators where email = $1)")
            .bind(&email)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if exists {
        return Err(Error::Conflict(format!("{email} is already a member")).into());
    }
    let h = hash_password(&b.password)?;
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.operators (id, email, name, role, password_hash, password_set_at) values ($1, $2, $3, $4, $5, now())",
    )
    .bind(id)
    .bind(&email)
    .bind(&name)
    .bind(role.as_str())
    .bind(h)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "id": id, "email": email, "name": name, "role": role.as_str(), "has_password": true }),
    ))
}

#[derive(Deserialize)]
struct ResetBody {
    password: String,
}

/// Admin: set or reset an operator's password (also clears a lockout).
async fn reset_password(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<Uuid>,
    Json(b): Json<ResetBody>,
) -> Result<Json<Value>, ApiError> {
    if caller.role != Role::Admin {
        return Err(Error::PermissionDenied("admin key required".into()).into());
    }
    check_password(&b.password)?;
    let h = hash_password(&b.password)?;
    let n = sqlx::query(
        "update musubi.operators set password_hash = $2, password_set_at = now(), failed_logins = 0, locked_until = null where id = $1",
    )
    .bind(id)
    .bind(h)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("operator".into()).into());
    }
    Ok(Json(json!({ "ok": true })))
}
