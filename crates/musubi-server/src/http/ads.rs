//! Rewarded video / ad placements (competitive P1-53): the client opens a view for a placement,
//! shows the network's ad, and the network's server-to-server callback (ironSource scheme or the
//! generic HMAC scheme) grants the placement's reward table once per event, within per-player
//! daily caps and cooldowns. `trust_client` placements may be completed by the client itself.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use md5::{Digest as _, Md5};
use musubi_core::config::AdProviderConfig;
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/ad_placements", get(list_placements))
        .route(
            "/v1/server/ad_placements/{id}",
            get(get_placement)
                .put(put_placement)
                .delete(delete_placement),
        )
        .route("/v1/server/ad_placements/{id}/stats", get(stats))
        .route("/v1/server/ads/rewards", get(server_rewards))
        .route("/v1/ads/placements", get(my_placements))
        .route("/v1/ads/placements/{id}/start", post(start_view))
        .route("/v1/ads/placements/{id}/complete", post(complete_view))
        .route("/v1/ads/rewards", get(my_rewards))
        .route("/v1/ads/callback/{provider}", get(callback).post(callback))
}

fn iso(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

#[derive(sqlx::FromRow)]
struct Placement {
    id: String,
    name: String,
    description: String,
    reward_table: String,
    daily_cap: i32,
    cooldown_secs: i32,
    platforms: Vec<String>,
    trust_client: bool,
    enabled: bool,
    metadata: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

const PLACEMENT_COLS: &str = "id, name, description, reward_table, daily_cap, cooldown_secs, platforms, trust_client, enabled, metadata, created_at, updated_at";

impl Placement {
    fn json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "description": self.description,
            "reward_table": self.reward_table, "daily_cap": self.daily_cap,
            "cooldown_secs": self.cooldown_secs, "platforms": self.platforms,
            "trust_client": self.trust_client, "enabled": self.enabled, "metadata": self.metadata,
            "created_at": iso(self.created_at), "updated_at": iso(self.updated_at),
        })
    }
}

async fn load_placement(db: &PgPool, id: &str) -> Result<Placement> {
    sqlx::query_as::<_, Placement>(&format!(
        "select {PLACEMENT_COLS} from musubi.ad_placements where id = $1"
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound("ad placement".into()))
}

fn check_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(Error::InvalidArgument(
            "placement id: 1..=64 chars of [A-Za-z0-9_.-]".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct PlacementIn {
    pub name: String,
    pub description: String,
    pub reward_table: String,
    pub daily_cap: i32,
    pub cooldown_secs: i32,
    pub platforms: Vec<String>,
    pub trust_client: bool,
    pub enabled: Option<bool>,
    pub metadata: Value,
}

/// `PUT /v1/server/ad_placements/{id} {name, description, reward_table, daily_cap, cooldown_secs,
/// platforms, trust_client, enabled, metadata}`
async fn put_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<PlacementIn>,
) -> std::result::Result<Json<Value>, ApiError> {
    check_id(&id)?;
    if b.reward_table.trim().is_empty() || b.reward_table.len() > 128 {
        return Err(Error::InvalidArgument("reward_table required".into()).into());
    }
    if b.daily_cap < 0 || b.cooldown_secs < 0 {
        return Err(
            Error::InvalidArgument("daily_cap and cooldown_secs must be >= 0".into()).into(),
        );
    }
    if b.name.len() > 128 || b.description.len() > 1024 {
        return Err(Error::InvalidArgument("name <= 128, description <= 1024 chars".into()).into());
    }
    if b.platforms.len() > 16 || b.platforms.iter().any(|p| p.is_empty() || p.len() > 32) {
        return Err(
            Error::InvalidArgument("platforms: up to 16 names of <= 32 chars".into()).into(),
        );
    }
    let metadata = if b.metadata.is_null() {
        json!({})
    } else {
        b.metadata
    };
    sqlx::query(
        "insert into musubi.ad_placements (id, name, description, reward_table, daily_cap, cooldown_secs, platforms, trust_client, enabled, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (id) do update set name = $2, description = $3, reward_table = $4, daily_cap = $5,
            cooldown_secs = $6, platforms = $7, trust_client = $8, enabled = $9, metadata = $10, updated_at = now()",
    )
    .bind(&id)
    .bind(b.name.trim())
    .bind(b.description.trim())
    .bind(b.reward_table.trim())
    .bind(b.daily_cap)
    .bind(b.cooldown_secs)
    .bind(&b.platforms)
    .bind(b.trust_client)
    .bind(b.enabled.unwrap_or(true))
    .bind(&metadata)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(load_placement(&s.db, &id).await?.json()))
}

async fn list_placements(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows = sqlx::query_as::<_, Placement>(&format!(
        "select {PLACEMENT_COLS} from musubi.ad_placements order by id"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({"placements": rows.iter().map(Placement::json).collect::<Vec<_>>()}),
    ))
}

async fn get_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(load_placement(&s.db, &id).await?.json()))
}

async fn delete_placement(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.ad_placements where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound("ad placement".into()).into());
    }
    Ok(Json(json!({"deleted": true})))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct DaysQ {
    pub days: Option<i64>,
}

/// `GET /v1/server/ad_placements/{id}/stats?days=7` — per-day views, grants, rejections and
/// unique rewarded players.
async fn stats(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<DaysQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    load_placement(&s.db, &id).await?;
    let days = q.days.unwrap_or(7).clamp(1, 90);
    type Row = (OffsetDateTime, i64, i64, i64, i64);
    let rows: Vec<Row> = sqlx::query_as(
        "select d, coalesce(g, 0), coalesce(rj, 0), coalesce(du, 0), coalesce(u, 0) from (
           select date_trunc('day', created_at) as d,
                  count(*) filter (where status = 'granted') as g,
                  count(*) filter (where status = 'rejected') as rj,
                  count(*) filter (where status = 'duplicate') as du,
                  count(distinct account_id) filter (where status = 'granted') as u
           from musubi.ad_rewards
           where placement_id = $1 and created_at > now() - make_interval(days => $2::int)
           group by 1) x order by d",
    )
    .bind(&id)
    .bind(days)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let (views, completed): (i64, i64) = sqlx::query_as(
        "select count(*)::bigint, count(*) filter (where status = 'rewarded')::bigint from musubi.ad_views
         where placement_id = $1 and created_at > now() - make_interval(days => $2::int)",
    )
    .bind(&id)
    .bind(days)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let totals = rows
        .iter()
        .fold((0, 0, 0), |a, r| (a.0 + r.1, a.1 + r.2, a.2 + r.3));
    Ok(Json(json!({
        "placement_id": id, "days": days,
        "views_started": views, "views_rewarded": completed,
        "granted": totals.0, "rejected": totals.1, "duplicates": totals.2,
        "fill_rate_pct": if views > 0 { Some((completed as f64 / views as f64 * 10_000.0).round() / 100.0) } else { None },
        "daily": rows.iter().map(|(d, g, rj, du, u)| json!({
            "day": iso(*d), "granted": g, "rejected": rj, "duplicates": du, "users": u
        })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------------------------
// player side

struct Window {
    today: i64,
    last_granted: Option<OffsetDateTime>,
}

async fn window(db: &PgPool, placement: &str, account: Uuid) -> Result<Window> {
    let (today, last): (i64, Option<OffsetDateTime>) = sqlx::query_as(
        "select count(*) filter (where created_at >= date_trunc('day', now() at time zone 'utc') at time zone 'utc')::bigint,
                max(created_at)
         from musubi.ad_rewards where placement_id = $1 and account_id = $2 and status = 'granted'",
    )
    .bind(placement)
    .bind(account)
    .fetch_one(db)
    .await
    .map_err(Error::internal)?;
    Ok(Window {
        today,
        last_granted: last,
    })
}

fn availability(
    p: &Placement,
    w: &Window,
    now: OffsetDateTime,
) -> (bool, Option<String>, Option<String>) {
    if !p.enabled {
        return (false, Some("disabled".into()), None);
    }
    if p.daily_cap > 0 && w.today >= p.daily_cap as i64 {
        return (false, Some("daily_cap".into()), None);
    }
    if p.cooldown_secs > 0 {
        if let Some(last) = w.last_granted {
            let until = last + time::Duration::seconds(p.cooldown_secs as i64);
            if until > now {
                return (false, Some("cooldown".into()), Some(iso(until)));
            }
        }
    }
    (true, None, None)
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct PlatformQ {
    pub platform: Option<String>,
}

/// `GET /v1/ads/placements?platform=` — enabled placements with the caller's remaining daily
/// grants, cooldown and availability.
async fn my_placements(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<PlatformQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows = sqlx::query_as::<_, Placement>(&format!(
        "select {PLACEMENT_COLS} from musubi.ad_placements where enabled order by id"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let now = OffsetDateTime::now_utc();
    let mut out = Vec::new();
    for p in rows.iter().filter(|p| {
        p.platforms.is_empty()
            || q.platform
                .as_deref()
                .map(|pl| p.platforms.iter().any(|x| x == pl))
                .unwrap_or(true)
    }) {
        let w = window(&s.db, &p.id, me.account_id.0).await?;
        let (ok, why, until) = availability(p, &w, now);
        out.push(json!({
            "id": p.id, "name": p.name, "description": p.description,
            "reward_table": p.reward_table, "platforms": p.platforms, "trust_client": p.trust_client,
            "metadata": p.metadata, "daily_cap": p.daily_cap, "today": w.today,
            "remaining_today": if p.daily_cap > 0 { Some((p.daily_cap as i64 - w.today).max(0)) } else { None },
            "cooldown_secs": p.cooldown_secs, "cooldown_until": until,
            "available": ok, "unavailable_reason": why,
        }));
    }
    Ok(Json(json!({"placements": out})))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct StartIn {
    pub platform: String,
}

/// `POST /v1/ads/placements/{id}/start {platform}` → `{view_id}`: pass `view_id` to the ad
/// network as the user id / custom data so the callback lands on this view.
async fn start_view(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<StartIn>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = load_placement(&s.db, &id).await?;
    if !p.platforms.is_empty() && !p.platforms.contains(&b.platform) {
        return Err(
            Error::PermissionDenied("placement is not available on this platform".into()).into(),
        );
    }
    let w = window(&s.db, &id, me.account_id.0).await?;
    let (ok, why, until) = availability(&p, &w, OffsetDateTime::now_utc());
    if !ok {
        return Err(Error::ResourceExhausted(format!(
            "placement unavailable: {}{}",
            why.unwrap_or_default(),
            until.map(|u| format!(" until {u}")).unwrap_or_default()
        ))
        .into());
    }
    let vid = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.ad_views (id, placement_id, account_id, platform) values ($1, $2, $3, $4)",
    )
    .bind(vid)
    .bind(&id)
    .bind(me.account_id.0)
    .bind(b.platform.chars().take(32).collect::<String>())
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({"view_id": vid, "placement_id": id, "reward_table": p.reward_table}),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct CompleteIn {
    pub view_id: Uuid,
}

/// `POST /v1/ads/placements/{id}/complete {view_id}` — client-reported completion; only for
/// placements with `trust_client` (prototyping, networks without S2S callbacks).
async fn complete_view(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<CompleteIn>,
) -> std::result::Result<Json<Value>, ApiError> {
    let p = load_placement(&s.db, &id).await?;
    if !p.trust_client {
        return Err(Error::PermissionDenied(
            "this placement is rewarded by the ad network's server callback only".into(),
        )
        .into());
    }
    let view = load_view(&s.db, b.view_id).await?;
    if view.account_id != me.account_id.0 || view.placement_id != id {
        return Err(Error::NotFound("ad view".into()).into());
    }
    let r = reward(
        &s,
        "client",
        &b.view_id.to_string(),
        Some(&view),
        view.account_id,
        &id,
    )
    .await?;
    Ok(Json(r))
}

#[derive(sqlx::FromRow)]
struct View {
    id: Uuid,
    placement_id: String,
    account_id: Uuid,
    status: String,
}

async fn load_view(db: &PgPool, id: Uuid) -> Result<View> {
    sqlx::query_as::<_, View>(
        "select id, placement_id, account_id, status from musubi.ad_views where id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound("ad view".into()))
}

/// Grants the placement's reward table once per `(provider, external_id)`; caps/cooldowns record
/// a `rejected` row instead of granting. Never fails for business reasons so networks get their
/// OK and stop retrying.
async fn reward(
    s: &AppState,
    provider: &str,
    external_id: &str,
    view: Option<&View>,
    account: Uuid,
    placement_id: &str,
) -> Result<Value> {
    let existing: Option<(Uuid, String, String)> = sqlx::query_as(
        "select id, status, reason from musubi.ad_rewards where provider = $1 and external_id = $2",
    )
    .bind(provider)
    .bind(external_id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if let Some((id, status, reason)) = existing {
        return Ok(json!({"status": "duplicate", "of": status, "reason": reason, "reward_id": id}));
    }
    let p = load_placement(&s.db, placement_id).await?;
    let w = window(&s.db, placement_id, account).await?;
    let (ok, why, _) = availability(&p, &w, OffsetDateTime::now_utc());
    let view_used = view.map(|v| v.status != "started").unwrap_or(false);
    let (status, reason, roll) = if view_used {
        ("rejected", "view already rewarded".to_string(), None)
    } else if !ok {
        ("rejected", why.unwrap_or_default(), None)
    } else {
        match s
            .economy
            .roll(
                AccountId(account),
                &p.reward_table,
                &format!("ad:{provider}:{external_id}"),
                Some(&format!("ad:{provider}:{external_id}")),
            )
            .await
        {
            Ok(r) => (
                "granted",
                String::new(),
                serde_json::to_value(&r.rewards).ok(),
            ),
            Err(e) => ("rejected", format!("reward table: {e}"), None),
        }
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.ad_rewards (id, placement_id, account_id, provider, external_id, view_id, status, reason, roll)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         on conflict (provider, external_id) do nothing",
    )
    .bind(id)
    .bind(placement_id)
    .bind(account)
    .bind(provider)
    .bind(external_id)
    .bind(view.map(|v| v.id))
    .bind(status)
    .bind(&reason)
    .bind(&roll)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    if let Some(v) = view {
        sqlx::query(
            "update musubi.ad_views set status = $2, reward_id = $3, completed_at = now() where id = $1 and status = 'started'",
        )
        .bind(v.id)
        .bind(if status == "granted" { "rewarded" } else { "rejected" })
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    s.events
        .emit(
            if status == "granted" {
                "ad.rewarded"
            } else {
                "ad.rejected"
            },
            Some(AccountId(account)),
            json!({"placement_id": placement_id, "provider": provider, "external_id": external_id,
                   "reward_id": id, "reason": reason, "rewards": roll}),
        )
        .await;
    Ok(
        json!({"status": status, "reason": reason, "reward_id": id, "rewards": roll, "placement_id": placement_id}),
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct RewardsQ {
    pub account_id: Option<Uuid>,
    pub placement: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

async fn rewards_rows(db: &PgPool, q: &RewardsQ, account: Option<Uuid>) -> Result<Value> {
    type Row = (
        Uuid,
        String,
        Uuid,
        String,
        String,
        Option<Uuid>,
        String,
        String,
        Option<Value>,
        OffsetDateTime,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "select id, placement_id, account_id, provider, external_id, view_id, status, reason, roll, created_at
         from musubi.ad_rewards
         where ($1::uuid is null or account_id = $1)
           and ($2::text is null or placement_id = $2)
           and ($3::text is null or status = $3)
         order by created_at desc limit $4",
    )
    .bind(account.or(q.account_id))
    .bind(&q.placement)
    .bind(&q.status)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    Ok(
        json!({"rewards": rows.into_iter().map(|(id, pl, acc, prov, ext, view, st, reason, roll, at)| json!({
        "id": id, "placement_id": pl, "account_id": acc, "provider": prov, "external_id": ext,
        "view_id": view, "status": st, "reason": reason, "rewards": roll, "at": iso(at)
    })).collect::<Vec<_>>()}),
    )
}

/// `GET /v1/ads/rewards?placement&status&limit`
async fn my_rewards(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<RewardsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(rewards_rows(&s.db, &q, Some(me.account_id.0)).await?))
}

/// `GET /v1/server/ads/rewards?account_id&placement&status&limit`
async fn server_rewards(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<RewardsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(rewards_rows(&s.db, &q, None).await?))
}

// ---------------------------------------------------------------------------------------------
// network callbacks

fn ip_allowed(p: &AdProviderConfig, ip: &str) -> bool {
    if p.allowed_ips.is_empty() {
        return true;
    }
    let Ok(addr) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    p.allowed_ips.iter().any(|a| {
        if let Some(net) = crate::bans::IpNet::parse(a) {
            net.contains(addr)
        } else {
            a == ip
        }
    })
}

/// `sig = hex(hmac_sha256(secret, "k=v&k=v…"))` over every parameter but `sig`, keys sorted.
pub fn custom_signature(secret: &str, params: &BTreeMap<String, String>) -> String {
    let canonical = params
        .iter()
        .filter(|(k, _)| k.as_str() != "sig")
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("hmac key");
    mac.update(canonical.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// ironSource: `signature = md5(timestamp + eventId + userId + privateKey)`.
pub fn ironsource_signature(
    secret: &str,
    timestamp: &str,
    event_id: &str,
    user_id: &str,
) -> String {
    let mut h = Md5::new();
    h.update(timestamp.as_bytes());
    h.update(event_id.as_bytes());
    h.update(user_id.as_bytes());
    h.update(secret.as_bytes());
    hex::encode(h.finalize())
}

fn constant_eq(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| {
            acc | (x.to_ascii_lowercase() ^ y.to_ascii_lowercase())
        }) == 0
}

/// Account + optional view from the network's user field: either our `view_id` (preferred — the
/// client passed it as the user id / custom data) or the player's account id.
async fn resolve(
    db: &PgPool,
    user: &str,
    placement_param: Option<&str>,
) -> Result<(Uuid, Option<View>, String)> {
    let id = Uuid::parse_str(user.trim())
        .map_err(|_| Error::InvalidArgument("user id must be a view id or account id".into()))?;
    if let Ok(v) = load_view(db, id).await {
        let pl = v.placement_id.clone();
        return Ok((v.account_id, Some(v), pl));
    }
    let Some(pl) = placement_param.filter(|p| !p.is_empty()) else {
        return Err(Error::InvalidArgument(
            "placement required when the user field is an account id".into(),
        ));
    };
    let (exists,): (bool,) =
        sqlx::query_as("select exists(select 1 from musubi.accounts where id = $1)")
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(Error::internal)?;
    if !exists {
        return Err(Error::NotFound("account".into()));
    }
    Ok((id, None, pl.to_string()))
}

/// `GET|POST /v1/ads/callback/{provider}` — the ad network's server-to-server reward callback.
/// Parameters come from the query string (and a JSON body on POST). ironSource gets the
/// `eventId:OK` text it expects; other providers get JSON.
async fn callback(
    State(s): State<AppState>,
    ip: super::ClientIp,
    Path(provider): Path<String>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> std::result::Result<Response, ApiError> {
    let Some(cfg) = s.cfg.ads.providers.iter().find(|p| p.name == provider) else {
        return Err(Error::NotFound("ad provider".into()).into());
    };
    if !ip_allowed(cfg, &ip.0) {
        return Err(Error::PermissionDenied("callback source not allowed".into()).into());
    }
    let mut params = query;
    if !body.is_empty()
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|c| c.starts_with("application/json"))
            .unwrap_or(false)
    {
        if let Ok(Value::Object(m)) = serde_json::from_slice::<Value>(&body) {
            for (k, v) in m {
                let v = match v {
                    Value::String(x) => x,
                    other => other.to_string(),
                };
                params.entry(k).or_insert(v);
            }
        }
    }
    let get = |k: &str| params.get(k).cloned().unwrap_or_default();
    match cfg.kind.as_str() {
        "ironsource" => {
            let (user, event, ts, sig) = (
                get("appUserId"),
                get("eventId"),
                get("timestamp"),
                get("signature"),
            );
            if user.is_empty() || event.is_empty() || sig.is_empty() {
                return Err(Error::InvalidArgument(
                    "appUserId, eventId and signature required".into(),
                )
                .into());
            }
            if !constant_eq(&ironsource_signature(&cfg.secret, &ts, &event, &user), &sig) {
                return Err(Error::Unauthenticated("bad signature".into()).into());
            }
            let item = params.get("itemName").cloned();
            let (account, view, placement) = resolve(&s.db, &user, item.as_deref()).await?;
            reward(&s, &provider, &event, view.as_ref(), account, &placement).await?;
            Ok((StatusCode::OK, format!("{event}:OK")).into_response())
        }
        "custom" => {
            let sig = get("sig");
            if sig.is_empty() || !constant_eq(&custom_signature(&cfg.secret, &params), &sig) {
                return Err(Error::Unauthenticated("bad signature".into()).into());
            }
            let ts: i64 = get("ts")
                .parse()
                .map_err(|_| Error::InvalidArgument("ts".into()))?;
            let now = OffsetDateTime::now_utc().unix_timestamp();
            if (now - ts).unsigned_abs() > s.cfg.ads.callback_ttl_secs {
                return Err(Error::Unauthenticated("callback expired".into()).into());
            }
            let user = params
                .get("view_id")
                .or_else(|| params.get("user_id"))
                .cloned()
                .unwrap_or_default();
            let event = get("event_id");
            if user.is_empty() || event.is_empty() {
                return Err(
                    Error::InvalidArgument("view_id|user_id and event_id required".into()).into(),
                );
            }
            let placement = params.get("placement").cloned();
            let (account, view, placement) = resolve(&s.db, &user, placement.as_deref()).await?;
            let r = reward(&s, &provider, &event, view.as_ref(), account, &placement).await?;
            Ok(Json(r).into_response())
        }
        other => {
            Err(Error::Internal(format!("ad provider kind `{other}` is not supported")).into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ironsource_signature_matches_spec_shape() {
        // md5("1700000000" + "evt1" + "user1" + "key")
        let sig = ironsource_signature("key", "1700000000", "evt1", "user1");
        assert_eq!(sig.len(), 32);
        assert_eq!(
            sig,
            ironsource_signature("key", "1700000000", "evt1", "user1")
        );
        assert_ne!(
            sig,
            ironsource_signature("other", "1700000000", "evt1", "user1")
        );
        assert!(constant_eq(&sig, &sig.to_uppercase()));
    }

    #[test]
    fn custom_signature_ignores_sig_and_sorts_keys() {
        let mut p = BTreeMap::new();
        p.insert("user_id".to_string(), "u".to_string());
        p.insert("event_id".to_string(), "e".to_string());
        p.insert("ts".to_string(), "1".to_string());
        let a = custom_signature("s", &p);
        p.insert("sig".to_string(), a.clone());
        assert_eq!(custom_signature("s", &p), a);
        p.insert("event_id".to_string(), "e2".to_string());
        assert_ne!(custom_signature("s", &p), a);
    }
}
