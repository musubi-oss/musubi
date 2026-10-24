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

/// 24 chars from a 62-letter alphabet (~143 bits), sampled uniformly (no modulo bias).
fn token() -> String {
    use rand::distributions::{Distribution, Uniform};
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let pick = Uniform::from(0..A.len());
    let mut rng = rand::rngs::OsRng;
    (0..24).map(|_| A[pick.sample(&mut rng)] as char).collect()
}

/// Invites are stored by SHA-256 of the token so a database read does not hand out pending
/// admin invites; rows created before this change still carry the plaintext and keep working.
fn token_hash(token: &str) -> String {
    crate::keys::hash(token)
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
    // the accepted invite becomes an unscoped key of `role`: only an unscoped key may offer that
    super::admin::may_delegate(&caller, &[])?;
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
    let plain = token();
    let mut row: InviteRow = sqlx::query_as(&format!(
        "insert into musubi.operator_invites (id, email, role, token, note, invited_by, expires_at)
         values ($1, $2, $3, $4, $5, $6, now() + make_interval(hours => $7)) returning {INVITE_COLS}"
    ))
    .bind(id)
    .bind(&email)
    .bind(role.as_str())
    .bind(token_hash(&plain))
    .bind(&b.note)
    .bind(&caller.name)
    .bind(ttl as i32)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    row.3 = plain;
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
        "select {INVITE_COLS} from musubi.operator_invites where token = $1 or token = $2"
    ))
    .bind(token_hash(token))
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
    // The invite is claimed with a conditional update inside the transaction: a second accept
    // racing on the same token (or one after a rescind) gets 409 and never keeps a key.
    if let Err(e) = record_acceptance(s.db.clone(), &r, member_id, &name, key.id).await {
        let _ = s.keys.revoke(&s.db, key.id).await;
        return Err(e.into());
    }
    if let Some(c) = &s.cluster {
        let _ = c.publish(crate::keys::KEYS_TOPIC, &key.id).await;
    }
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

async fn record_acceptance(
    db: sqlx::PgPool,
    r: &InviteRow,
    member_id: Uuid,
    name: &str,
    key_id: Uuid,
) -> Result<()> {
    let mut tx = db.begin().await.map_err(Error::internal)?;
    let claimed = sqlx::query(
        "update musubi.operator_invites set status = 'accepted', decided_at = now(), key_id = $2 \
         where id = $1 and status = 'invited' and expires_at > now()",
    )
    .bind(r.0)
    .bind(key_id)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if claimed == 0 {
        return Err(Error::Conflict("invite already decided or expired".into()));
    }
    sqlx::query(
        "insert into musubi.operators (id, email, name, role, key_id, invite_id) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(member_id)
    .bind(&r.1)
    .bind(name)
    .bind(&r.2)
    .bind(key_id)
    .bind(r.0)
    .execute(&mut *tx)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.is_unique_violation() => {
            Error::Conflict(format!("{} is already a member", r.1))
        }
        e => Error::internal(e),
    })?;
    tx.commit().await.map_err(Error::internal)
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
        if let Some(c) = &s.cluster {
            let _ = c.publish(crate::keys::KEYS_TOPIC, &kid).await;
        }
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

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn token_is_24_alphanumerics_and_unique() {
        let a = token();
        let b = token();
        assert_eq!(a.chars().count(), 24);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        assert_ne!(a, b);
    }

    #[test]
    fn stored_form_is_the_server_key_hash() {
        let t = token();
        assert_eq!(token_hash(&t), crate::keys::hash(&t));
        assert_ne!(token_hash(&t), t);
        assert_eq!(token_hash(&t).len(), 64);
    }

    /// Two accepts of the same invite: the first claims it, the second gets 409 and no member row.
    #[tokio::test]
    async fn acceptance_is_claimed_once() {
        let Ok(url) = std::env::var("MUSUBI_TEST_DATABASE_URL") else {
            return;
        };
        let db = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .unwrap();
        crate::db::migrate(&db).await.unwrap();
        let id = Uuid::now_v7();
        let email = format!("audit-{}@example.test", id.simple());
        let row: InviteRow = sqlx::query_as(&format!(
            "insert into musubi.operator_invites (id, email, role, token, note, invited_by, expires_at)
             values ($1, $2, 'support', $3, '', 'test', now() + interval '1 hour') returning {INVITE_COLS}"
        ))
        .bind(id)
        .bind(&email)
        .bind(token_hash(&token()))
        .fetch_one(&db)
        .await
        .unwrap();
        let first = record_acceptance(db.clone(), &row, Uuid::now_v7(), "a", Uuid::now_v7()).await;
        assert!(first.is_ok(), "{first:?}");
        let second = record_acceptance(db.clone(), &row, Uuid::now_v7(), "b", Uuid::now_v7()).await;
        assert!(matches!(second, Err(Error::Conflict(_))), "{second:?}");
        let (members,): (i64,) =
            sqlx::query_as("select count(*) from musubi.operators where invite_id = $1")
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(members, 1);
        sqlx::query("delete from musubi.operators where invite_id = $1")
            .bind(id)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("delete from musubi.operator_invites where id = $1")
            .bind(id)
            .execute(&db)
            .await
            .unwrap();
    }
}
