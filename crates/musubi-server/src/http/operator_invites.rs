//! Operator team (Hathora org invites parity): admins invite people by e-mail with a role
//! (`server`, `admin`, `support`); the invitee opens the link, accepts (a key of that role is
//! issued and shown once) or rejects; admins list members and pending invites and rescind them.
//! Mail delivery is left to the operator's own channel — the invite link is returned on creation.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};
use crate::keys::Caller;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/operators", get(members))
        .route(
            "/v1/admin/operators/{id}",
            axum::routing::delete(remove_member),
        )
        .route("/v1/admin/operators/invites", get(invites).post(invite))
        .route(
            "/v1/admin/operators/invites/{id}",
            axum::routing::delete(rescind),
        )
        .route("/v1/operator_invites/{token}", get(peek))
        .route("/v1/operator_invites/{token}/accept", post(accept))
        .route("/v1/operator_invites/{token}/reject", post(reject))
}

fn rfc(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

type InviteRow = (
    Uuid,
    String,
    String,
    String,
    String,
    String,
    String,
    OffsetDateTime,
    OffsetDateTime,
    Option<OffsetDateTime>,
    Option<Uuid>,
);
const INVITE_COLS: &str =
    "id, email, role, token, note, invited_by, status, created_at, expires_at, decided_at, key_id";

fn invite_json(r: &InviteRow, with_token: bool) -> Value {
    let mut v = json!({
        "id": r.0, "email": r.1, "role": r.2, "note": r.4, "invited_by": r.5, "status": r.6,
        "created_at": rfc(r.7), "expires_at": rfc(r.8), "decided_at": r.9.map(rfc), "key_id": r.10,
        "expired": r.6 == "invited" && r.8 < OffsetDateTime::now_utc(),
    });
    if with_token {
        v["token"] = json!(r.3);
        v["path"] = json!(format!("/v1/operator_invites/{}", r.3));
    }
    v
}

fn email_ok(e: &str) -> bool {
    let e = e.trim();
    e.len() >= 3
        && e.len() <= 254
        && e.contains('@')
        && !e.starts_with('@')
        && !e.ends_with('@')
        && !e.chars().any(char::is_whitespace)
}

fn token() -> String {
    use rand::RngCore;
    let mut b = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut b);
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    b.iter()
        .map(|x| A[(*x as usize) % A.len()] as char)
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct InviteBody {
    email: String,
    role: String,
    note: String,
    ttl_hours: Option<i64>,
}

async fn invite(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Json(b): Json<InviteBody>,
) -> Result<Json<Value>, ApiError> {
    if !email_ok(&b.email) {
        return Err(
            Error::InvalidArgument("email: a valid address, at most 254 chars".into()).into(),
        );
    }
    let email = b.email.trim().to_ascii_lowercase();
    let role = crate::keys::Role::parse(&b.role.trim().to_ascii_lowercase())
        .ok_or_else(|| Error::InvalidArgument("role must be server, admin or support".into()))?;
    let ttl = b.ttl_hours.unwrap_or(72);
    if !(1..=24 * 30).contains(&ttl) {
        return Err(Error::InvalidArgument("ttl_hours: 1..=720".into()).into());
    }
    if b.note.len() > 500 {
        return Err(Error::InvalidArgument("note: at most 500 chars".into()).into());
    }
    let (member,): (bool,) =
        sqlx::query_as("select exists(select 1 from musubi.operators where email = $1)")
            .bind(&email)
            .fetch_one(&s.db)
            .await
            .map_err(Error::internal)?;
    if member {
        return Err(Error::Conflict(format!("{email} is already a member")).into());
    }
    let (pending,): (bool,) = sqlx::query_as(
        "select exists(select 1 from musubi.operator_invites where lower(email) = $1 and status = 'invited' and expires_at > now())",
    )
    .bind(&email)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if pending {
        return Err(Error::Conflict(format!("{email} already has a pending invite")).into());
    }
    let id = Uuid::now_v7();
    let row: InviteRow = sqlx::query_as(&format!(
        "insert into musubi.operator_invites (id, email, role, token, note, invited_by, expires_at)
         values ($1, $2, $3, $4, $5, $6, now() + ($7 || ' hours')::interval) returning {INVITE_COLS}"
    ))
    .bind(id)
    .bind(&email)
    .bind(role.as_str())
    .bind(token())
    .bind(&b.note)
    .bind(&caller.name)
    .bind(ttl.to_string())
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "operator.invited",
            None,
            json!({"invite_id": id, "email": email, "role": role.as_str(), "by": caller.name}),
        )
        .await;
    Ok(Json(invite_json(&row, true)))
}

#[derive(Deserialize, Default)]
struct ListQ {
    status: Option<String>,
}

async fn invites(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> Result<Json<Value>, ApiError> {
    if let Some(st) = &q.status {
        if !["invited", "accepted", "rejected", "rescinded"].contains(&st.as_str()) {
            return Err(Error::InvalidArgument(
                "status: invited|accepted|rejected|rescinded".into(),
            )
            .into());
        }
    }
    let rows: Vec<InviteRow> = sqlx::query_as(&format!(
        "select {INVITE_COLS} from musubi.operator_invites where ($1::text is null or status = $1)
         order by created_at desc limit 500"
    ))
    .bind(&q.status)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "invites": rows.iter().map(|r| invite_json(r, false)).collect::<Vec<_>>() }),
    ))
}

async fn rescind(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<InviteRow> = sqlx::query_as(&format!(
        "select {INVITE_COLS} from musubi.operator_invites where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return Err(Error::NotFound("invite".into()).into());
    };
    if row.6 != "invited" {
        return Err(Error::Conflict(format!("invite already {}", row.6)).into());
    }
    let row: InviteRow = sqlx::query_as(&format!(
        "update musubi.operator_invites set status = 'rescinded', decided_at = now() where id = $1 returning {INVITE_COLS}"
    ))
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "operator.invite_rescinded",
            None,
            json!({"invite_id": id, "email": row.1, "by": caller.name}),
        )
        .await;
    Ok(Json(invite_json(&row, false)))
}

async fn live_invite(s: &AppState, token: &str) -> Result<InviteRow> {
    let row: Option<InviteRow> = sqlx::query_as(&format!(
        "select {INVITE_COLS} from musubi.operator_invites where token = $1"
    ))
    .bind(token)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return Err(Error::NotFound("invite".into()));
    };
    if row.6 != "invited" {
        return Err(Error::Conflict(format!("invite already {}", row.6)));
    }
    if row.8 < OffsetDateTime::now_utc() {
        return Err(Error::Conflict("invite expired".into()));
    }
    Ok(row)
}

/// What the invitee sees before deciding (no token echo, no secrets).
async fn peek(
    State(s): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = live_invite(&s, &token).await?;
    Ok(Json(json!({
        "email": r.1, "role": r.2, "note": r.4, "invited_by": r.5, "expires_at": rfc(r.8),
    })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AcceptBody {
    name: String,
}

/// Accept: the invitee becomes a member and gets a key of the invited role (secret shown once).
async fn accept(
    State(s): State<AppState>,
    Path(token): Path<String>,
    Json(b): Json<AcceptBody>,
) -> Result<Json<Value>, ApiError> {
    let r = live_invite(&s, &token).await?;
    let name = if b.name.trim().is_empty() {
        r.1.clone()
    } else {
        b.name.trim().to_string()
    };
    if name.len() > 64 {
        return Err(Error::InvalidArgument("name: at most 64 chars".into()).into());
    }
    let role = crate::keys::Role::parse(&r.2)
        .ok_or_else(|| Error::internal(std::io::Error::other("bad role on invite")))?;
    let (key, secret) = s
        .keys
        .issue_with(&s.db, &name, role, None, Some(&r.1), &[])
        .await?;
    let member_id = Uuid::now_v7();
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    sqlx::query(
        "insert into musubi.operators (id, email, name, role, key_id, invite_id) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(member_id)
    .bind(&r.1)
    .bind(&name)
    .bind(&r.2)
    .bind(key.id)
    .bind(r.0)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    sqlx::query(
        "update musubi.operator_invites set status = 'accepted', decided_at = now(), key_id = $2 where id = $1",
    )
    .bind(r.0)
    .bind(key.id)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    s.events
        .emit(
            "operator.joined",
            None,
            json!({"invite_id": r.0, "member_id": member_id, "email": r.1, "role": r.2, "key_id": key.id}),
        )
        .await;
    Ok(Json(json!({
        "member_id": member_id, "email": r.1, "name": name, "role": r.2,
        "key_id": key.id, "secret": secret,
    })))
}

async fn reject(
    State(s): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let r = live_invite(&s, &token).await?;
    sqlx::query(
        "update musubi.operator_invites set status = 'rejected', decided_at = now() where id = $1",
    )
    .bind(r.0)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "operator.invite_rejected",
            None,
            json!({"invite_id": r.0, "email": r.1}),
        )
        .await;
    Ok(Json(json!({"rejected": true})))
}

type MemberRow = (
    Uuid,
    String,
    String,
    String,
    Option<Uuid>,
    Option<Uuid>,
    OffsetDateTime,
);

async fn members(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    #[allow(clippy::type_complexity)]
    let rows: Vec<(MemberRow, bool, Option<OffsetDateTime>)> = sqlx::query_as::<_, (
        Uuid, String, String, String, Option<Uuid>, Option<Uuid>, OffsetDateTime, bool, Option<OffsetDateTime>,
    )>(
        "select id, email, name, role, key_id, invite_id, joined_at, password_hash is not null, last_login_at \
         from musubi.operators order by joined_at",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?
    .into_iter()
    .map(|r| ((r.0, r.1, r.2, r.3, r.4, r.5, r.6), r.7, r.8))
    .collect();
    Ok(Json(json!({
        "members": rows.iter().map(|(r, has_password, last_login)| json!({
            "id": r.0, "email": r.1, "name": r.2, "role": r.3, "key_id": r.4, "invite_id": r.5,
            "joined_at": rfc(r.6), "has_password": has_password, "last_login_at": last_login.map(rfc),
        })).collect::<Vec<_>>()
    })))
}

/// Removes the member; their key is revoked so access ends with the membership.
async fn remove_member(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Extension(caller): Extension<Caller>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<MemberRow> = sqlx::query_as(
        "delete from musubi.operators where id = $1 returning id, email, name, role, key_id, invite_id, joined_at",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return Err(Error::NotFound("member".into()).into());
    };
    if let Some(kid) = row.4 {
        s.keys.revoke(&s.db, kid).await?;
    }
    s.events
        .emit(
            "operator.removed",
            None,
            json!({"member_id": id, "email": row.1, "key_id": row.4, "by": caller.name}),
        )
        .await;
    Ok(Json(
        json!({"removed": true, "key_revoked": row.4.is_some()}),
    ))
}
