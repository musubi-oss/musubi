//! Bans depth (competitive P0-8): the paged ban list, device / IP / CIDR ban rules, the ban
//! reason catalog, and the per-account device + address registry that rules are built from.
//! Account bans themselves stay on `/v1/admin/accounts/{id}/ban` (see `admin.rs`).

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::bans::{BanRules, Rule, FEATURES};
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/admin/bans", get(list_bans))
        .route("/v1/admin/bans/check", get(check))
        .route("/v1/admin/bans/rules", get(list_rules).post(create_rule))
        .route("/v1/admin/bans/rules/{id}", delete(delete_rule))
        .route("/v1/admin/bans/reasons", get(list_reasons))
        .route(
            "/v1/admin/bans/reasons/{code}",
            get(get_reason).put(put_reason).delete(delete_reason),
        )
        .route("/v1/admin/accounts/{id}/devices", get(devices))
        .route("/v1/admin/accounts/{id}/ips", get(ips))
        .route("/v1/admin/accounts/{id}/ban/devices", post(ban_devices))
        .route(
            "/v1/admin/accounts/{id}/security_events",
            get(security_events),
        )
}

#[derive(Deserialize)]
pub struct ListQ {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
    /// `full` | `shadow` | `feature`
    pub kind: Option<String>,
}

fn default_limit() -> i64 {
    50
}

fn ts(t: Option<OffsetDateTime>) -> Value {
    json!(t.map(|t| t.unix_timestamp()))
}

/// `GET /v1/admin/bans` — every active account ban, newest first (the Steam-style "banned feed").
async fn list_bans(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    type Row = (
        Uuid,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<OffsetDateTime>,
        bool,
        Vec<String>,
        Option<String>,
        OffsetDateTime,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select b.account_id, a.username, b.reason, b.reason_code, b.message, b.until, b.shadow,
                b.features, b.actor, b.created_at
         from musubi.account_bans b join musubi.accounts a on a.id = b.account_id
         where (b.until is null or b.until > now())
           and ($3::text is null
                or ($3 = 'shadow' and b.shadow)
                or ($3 = 'feature' and cardinality(b.features) > 0 and not b.shadow)
                or ($3 = 'full' and not b.shadow and cardinality(b.features) = 0))
         order by b.created_at desc limit $1 offset $2",
    )
    .bind(q.limit.clamp(1, 500))
    .bind(q.offset.max(0))
    .bind(q.kind.as_deref())
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let total: i64 = sqlx::query_scalar(
        "select count(*) from musubi.account_bans where until is null or until > now()",
    )
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let bans: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, username, reason, code, message, until, shadow, features, actor, created)| {
                json!({
                    "account_id": id, "username": username, "reason": reason, "reason_code": code,
                    "message": message, "until": ts(until), "shadow": shadow, "features": features,
                    "actor": actor, "created_at": created.unix_timestamp(),
                })
            },
        )
        .collect();
    Ok(Json(
        json!({ "bans": bans, "total": total, "offset": q.offset.max(0) }),
    ))
}

#[derive(Deserialize)]
pub struct CheckQ {
    pub ip: Option<String>,
    pub device: Option<String>,
}

/// `GET /v1/admin/bans/check?ip=&device=` — tokenless probe for game servers / launchers.
async fn check(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<CheckQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let ip = q.ip.as_deref().map(|ip| s.ban_rules.check_ip(ip).err());
    let device = q
        .device
        .as_deref()
        .map(|d| s.ban_rules.check_device(d).err());
    Ok(Json(json!({
        "ip_banned": ip.as_ref().map(|e| e.is_some()),
        "ip_message": ip.flatten().map(|e| e.to_string()),
        "device_banned": device.as_ref().map(|e| e.is_some()),
        "device_message": device.flatten().map(|e| e.to_string()),
    })))
}

fn rule_json(r: &Rule, actor: Option<&str>, created: Option<OffsetDateTime>) -> Value {
    json!({
        "id": r.id, "kind": r.kind, "value": r.value, "reason": r.reason, "message": r.message,
        "until": r.until, "actor": actor, "created_at": created.map(|c| c.unix_timestamp()),
    })
}

async fn list_rules(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    type Row = (
        Uuid,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<OffsetDateTime>,
        Option<String>,
        OffsetDateTime,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select id, kind, value, reason, message, until, actor, created_at from musubi.ban_rules
         where (until is null or until > now()) and ($3::text is null or kind = $3)
         order by created_at desc limit $1 offset $2",
    )
    .bind(q.limit.clamp(1, 500))
    .bind(q.offset.max(0))
    .bind(q.kind.as_deref())
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let rules: Vec<Value> = rows
        .into_iter()
        .map(
            |(id, kind, value, reason, message, until, actor, created)| {
                rule_json(
                    &Rule {
                        id,
                        kind,
                        value,
                        reason,
                        message,
                        until: until.map(|u| u.unix_timestamp()),
                    },
                    actor.as_deref(),
                    Some(created),
                )
            },
        )
        .collect();
    Ok(Json(json!({ "rules": rules })))
}

#[derive(Deserialize, Default)]
pub struct RuleBody {
    /// `device` | `ip` | `cidr`
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub until_secs: Option<i64>,
}

/// Insert (or replace) one rule, update the cluster cache, emit the event.
pub(crate) async fn add_rule(
    s: &AppState,
    kind: &str,
    value: &str,
    reason: Option<&str>,
    message: Option<&str>,
    until: Option<i64>,
    actor: &str,
) -> Result<Rule> {
    BanRules::validate(kind, value)?;
    let value = value.trim().to_string();
    let id: Uuid = sqlx::query_scalar(
        "insert into musubi.ban_rules (id, kind, value, reason, message, until, actor)
         values ($1, $2, $3, $4, $5, to_timestamp($6), $7)
         on conflict (kind, value) do update set reason = excluded.reason,
            message = excluded.message, until = excluded.until, actor = excluded.actor,
            created_at = now()
         returning id",
    )
    .bind(Uuid::now_v7())
    .bind(kind)
    .bind(&value)
    .bind(reason)
    .bind(message)
    .bind(until.map(|u| u as f64))
    .bind(actor)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let rule = Rule {
        id,
        kind: kind.to_string(),
        value,
        reason: reason.map(str::to_string),
        message: message.map(str::to_string),
        until,
    };
    s.ban_rules
        .set(id, Some(rule.clone()), s.cluster.as_ref())
        .await;
    s.events
        .emit(
            "ban_rule.created",
            None,
            json!({ "id": id, "kind": rule.kind, "value": rule.value, "until": until }),
        )
        .await;
    Ok(rule)
}

async fn create_rule(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<RuleBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    if b.until_secs.is_some_and(|u| u <= 0) {
        return Err(Error::InvalidArgument("until_secs must be positive".into()).into());
    }
    let until = b
        .until_secs
        .map(|u| OffsetDateTime::now_utc().unix_timestamp() + u);
    let r = add_rule(
        &s,
        &b.kind,
        &b.value,
        b.reason.as_deref(),
        b.message.as_deref(),
        until,
        "server",
    )
    .await?;
    Ok(Json(rule_json(&r, Some("server"), None)))
}

async fn delete_rule(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.ban_rules where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("ban rule".into()).into());
    }
    s.ban_rules.set(id, None, s.cluster.as_ref()).await;
    s.events
        .emit("ban_rule.removed", None, json!({ "id": id }))
        .await;
    Ok(Json(json!({ "removed": true })))
}

#[derive(Deserialize)]
pub struct ReasonBody {
    pub title: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub default_duration_secs: Option<i64>,
    #[serde(default = "default_severity")]
    pub severity: String,
    #[serde(default)]
    pub features: Vec<String>,
    /// Escalation ladder for sanctions (`POST /v1/admin/accounts/{id}/sanctions {escalate: true}`):
    /// `[{action, duration_secs?, message?}, …]`, the n-th prior sanction with this reason picks step n.
    #[serde(default)]
    pub ladder: serde_json::Value,
}

fn default_severity() -> String {
    "medium".into()
}

pub(crate) fn validate_features(f: &[String]) -> Result<()> {
    if let Some(bad) = f.iter().find(|f| !FEATURES.contains(&f.as_str())) {
        return Err(Error::InvalidArgument(format!(
            "unknown feature `{bad}`; one of {}",
            FEATURES.join(", ")
        )));
    }
    Ok(())
}

type ReasonRow = (
    String,
    String,
    String,
    Option<i64>,
    String,
    Vec<String>,
    OffsetDateTime,
);

fn reason_json((code, title, message, dur, severity, features, updated): ReasonRow) -> Value {
    json!({
        "code": code, "title": title, "message": message, "default_duration_secs": dur,
        "severity": severity, "features": features, "updated_at": updated.unix_timestamp(),
    })
}

const REASON_SELECT: &str = "select code, title, message, default_duration_secs, severity, features, updated_at from musubi.ban_reasons";

pub(crate) async fn load_reason(s: &AppState, code: &str) -> Result<Option<ReasonRow>> {
    sqlx::query_as(&format!("{REASON_SELECT} where code = $1"))
        .bind(code)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)
}

async fn list_reasons(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<ReasonRow> = sqlx::query_as(&format!("{REASON_SELECT} order by code"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(
        json!({ "reasons": rows.into_iter().map(reason_json).collect::<Vec<_>>() }),
    ))
}

async fn get_reason(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(code): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let r = load_reason(&s, &code)
        .await?
        .ok_or_else(|| Error::NotFound("ban reason".into()))?;
    Ok(Json(reason_json(r)))
}

async fn put_reason(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(code): Path<String>,
    Json(b): Json<ReasonBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    if code.is_empty()
        || code.len() > 64
        || !code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(Error::InvalidArgument("code: [A-Za-z0-9_.-]{1,64}".into()).into());
    }
    if b.title.trim().is_empty() || b.title.len() > 128 {
        return Err(Error::InvalidArgument("title: 1..128 chars".into()).into());
    }
    if b.message.chars().count() > 512 {
        return Err(Error::InvalidArgument("message over 512 chars".into()).into());
    }
    if b.default_duration_secs.is_some_and(|d| d <= 0) {
        return Err(Error::InvalidArgument("default_duration_secs must be positive".into()).into());
    }
    if !["low", "medium", "high", "critical"].contains(&b.severity.as_str()) {
        return Err(
            Error::InvalidArgument("severity: low | medium | high | critical".into()).into(),
        );
    }
    validate_features(&b.features)?;
    sqlx::query(
        "insert into musubi.ban_reasons (code, title, message, default_duration_secs, severity, features)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (code) do update set title = excluded.title, message = excluded.message,
            default_duration_secs = excluded.default_duration_secs, severity = excluded.severity,
            features = excluded.features, updated_at = now()",
    )
    .bind(&code)
    .bind(b.title.trim())
    .bind(&b.message)
    .bind(b.default_duration_secs)
    .bind(&b.severity)
    .bind(&b.features)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    sqlx::query("update musubi.ban_reasons set ladder = $2 where code = $1")
        .bind(&code)
        .bind(if b.ladder.is_array() {
            b.ladder.clone()
        } else {
            serde_json::json!([])
        })
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.events
        .emit("ban_reason.updated", None, json!({ "code": code }))
        .await;
    let r = load_reason(&s, &code)
        .await?
        .ok_or_else(|| Error::NotFound("ban reason".into()))?;
    Ok(Json(reason_json(r)))
}

async fn delete_reason(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(code): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.ban_reasons where code = $1")
        .bind(&code)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

/// `GET /v1/admin/accounts/{id}/devices` — every device the account logged in from.
async fn devices(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    type Row = (
        String,
        Option<String>,
        Option<String>,
        OffsetDateTime,
        OffsetDateTime,
        i32,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select device_id, user_agent, host(last_ip), first_seen_at, last_seen_at, logins
         from musubi.account_devices where account_id = $1 order by last_seen_at desc",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let devices: Vec<Value> = rows
        .into_iter()
        .map(|(d, ua, ip, first, last, logins)| {
            json!({
                "device_id": d, "user_agent": ua, "last_ip": ip, "logins": logins,
                "first_seen_at": first.unix_timestamp(), "last_seen_at": last.unix_timestamp(),
                "banned": s.ban_rules.check_device(&d).is_err(),
            })
        })
        .collect();
    Ok(Json(json!({ "devices": devices })))
}

/// `GET /v1/admin/accounts/{id}/ips` — every address the account logged in from.
async fn ips(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    type Row = (String, OffsetDateTime, OffsetDateTime, i32);
    let rows: Vec<Row> = sqlx::query_as(
        "select host(ip), first_seen_at, last_seen_at, logins
         from musubi.account_ips where account_id = $1 order by last_seen_at desc",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let ips: Vec<Value> = rows
        .into_iter()
        .map(|(ip, first, last, logins)| {
            json!({
                "ip": ip, "logins": logins, "first_seen_at": first.unix_timestamp(),
                "last_seen_at": last.unix_timestamp(), "banned": s.ban_rules.check_ip(&ip).is_err(),
            })
        })
        .collect();
    Ok(Json(json!({ "ips": ips })))
}

#[derive(Deserialize, Default)]
pub struct BanDevicesBody {
    #[serde(default = "default_true")]
    pub devices: bool,
    #[serde(default)]
    pub ips: bool,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub until_secs: Option<i64>,
}

fn default_true() -> bool {
    true
}

/// Create device (and optionally IP) rules for everything in the account's registry.
pub(crate) async fn ban_registry(
    s: &AppState,
    account: Uuid,
    devices: bool,
    ips: bool,
    reason: Option<&str>,
    message: Option<&str>,
    until: Option<i64>,
) -> Result<(usize, usize)> {
    let mut n_dev = 0;
    let mut n_ip = 0;
    if devices {
        let devs: Vec<String> = sqlx::query_scalar(
            "select device_id from musubi.account_devices where account_id = $1",
        )
        .bind(account)
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for d in devs {
            add_rule(s, "device", &d, reason, message, until, "server").await?;
            n_dev += 1;
        }
    }
    if ips {
        let addrs: Vec<String> =
            sqlx::query_scalar("select host(ip) from musubi.account_ips where account_id = $1")
                .bind(account)
                .fetch_all(&s.db)
                .await
                .map_err(Error::internal)?;
        for a in addrs {
            add_rule(s, "ip", &a, reason, message, until, "server").await?;
            n_ip += 1;
        }
    }
    Ok((n_dev, n_ip))
}

/// `POST /v1/admin/accounts/{id}/ban/devices` — ban the hardware/addresses behind an account
/// without (or in addition to) banning the account itself.
async fn ban_devices(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    body: Option<Json<BanDevicesBody>>,
) -> std::result::Result<Json<Value>, ApiError> {
    let b = body.map(|Json(b)| b).unwrap_or_default();
    if b.until_secs.is_some_and(|u| u <= 0) {
        return Err(Error::InvalidArgument("until_secs must be positive".into()).into());
    }
    let until = b
        .until_secs
        .map(|u| OffsetDateTime::now_utc().unix_timestamp() + u);
    let (d, i) = ban_registry(
        &s,
        id,
        b.devices,
        b.ips,
        b.reason.as_deref(),
        b.message.as_deref(),
        until,
    )
    .await?;
    Ok(Json(
        json!({ "devices_banned": d, "ips_banned": i, "until": until }),
    ))
}

/// `GET /v1/admin/accounts/{id}/security_events` — logins, suspicious logins, bans, MFA changes…
async fn security_events(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Query(q): Query<crate::security::TimelineQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(crate::security::timeline(&s.db, id, &q).await?))
}
