//! Redemption codes and key campaigns (P1-52): a campaign grants a reward table and/or one key from
//! a platform key group (Steam/console keys uploaded in bulk); codes are generated single-use
//! batches or a shared multi-use code; per-account and campaign-wide limits, windows, audiences,
//! revocation and a redemption history. Players call `POST /v1/codes/redeem {code}`.

use std::sync::OnceLock;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use dashmap::DashMap;
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/codes/redeem", post(redeem))
        .route("/v1/codes/redemptions", get(my_redemptions))
        .route("/v1/server/codes/campaigns", get(list_campaigns))
        .route(
            "/v1/server/codes/campaigns/{id}",
            put(upsert_campaign).get(get_campaign),
        )
        .route("/v1/server/codes/campaigns/{id}/generate", post(generate))
        .route(
            "/v1/server/codes/campaigns/{id}/codes",
            post(add_shared).get(list_codes),
        )
        .route(
            "/v1/server/codes/campaigns/{id}/redemptions",
            get(campaign_redemptions),
        )
        .route("/v1/server/codes/{code}/revoke", post(revoke))
        .route("/v1/server/key_groups", get(list_key_groups))
        .route(
            "/v1/server/key_groups/{id}",
            put(upsert_key_group).get(get_key_group),
        )
        .route("/v1/server/key_groups/{id}/keys", post(upload_keys))
}

/// Codes are compared case-insensitively without spaces or dashes.
pub fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

fn random_code(prefix: &str, len: usize) -> String {
    let mut out = String::with_capacity(prefix.len() + len);
    out.push_str(prefix);
    let mut bytes = Vec::new();
    while bytes.len() < len {
        // v7 ids carry 62 random bits in their tail; the time prefix is skipped.
        bytes.extend_from_slice(&Uuid::now_v7().as_bytes()[9..]);
    }
    for b in bytes.into_iter().take(len) {
        out.push(ALPHABET[(b % ALPHABET.len() as u8) as usize] as char);
    }
    out
}

fn valid_id(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

#[derive(sqlx::FromRow)]
struct Campaign {
    id: String,
    name: String,
    reward_table: Option<String>,
    key_group: Option<String>,
    per_account: i32,
    max_redemptions: Option<i64>,
    starts_at: Option<OffsetDateTime>,
    ends_at: Option<OffsetDateTime>,
    audience: Option<String>,
    enabled: bool,
    metadata: Value,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
}

const CAMPAIGN_COLS: &str = "id, name, reward_table, key_group, per_account, max_redemptions, starts_at, ends_at, audience, enabled, metadata, created_at, updated_at";

fn campaign_json(c: &Campaign) -> Value {
    json!({
        "id": c.id, "name": c.name, "reward_table": c.reward_table, "key_group": c.key_group,
        "per_account": c.per_account, "max_redemptions": c.max_redemptions,
        "starts_at": c.starts_at.map(|t| t.unix_timestamp()), "ends_at": c.ends_at.map(|t| t.unix_timestamp()),
        "audience": c.audience, "enabled": c.enabled, "metadata": c.metadata,
        "created_at": c.created_at.unix_timestamp(), "updated_at": c.updated_at.unix_timestamp(),
    })
}

async fn load_campaign(s: &AppState, id: &str) -> Result<Campaign> {
    sqlx::query_as(&format!(
        "select {CAMPAIGN_COLS} from musubi.code_campaigns where id = $1"
    ))
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound(format!("campaign {id}")))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CampaignBody {
    name: String,
    reward_table: Option<String>,
    key_group: Option<String>,
    per_account: Option<i32>,
    max_redemptions: Option<i64>,
    starts_at: Option<i64>,
    ends_at: Option<i64>,
    audience: Option<String>,
    enabled: Option<bool>,
    metadata: Value,
}

/// `PUT /v1/server/codes/campaigns/{id} {name, reward_table?, key_group?, per_account, max_redemptions?,
/// starts_at?, ends_at?, audience?, enabled, metadata}`.
async fn upsert_campaign(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<CampaignBody>,
) -> Result<Json<Value>, ApiError> {
    if !valid_id(&id) {
        return Err(Error::InvalidArgument("id must be [A-Za-z0-9_.-]{1,64}".into()).into());
    }
    if b.name.trim().is_empty() {
        return Err(Error::InvalidArgument("name is required".into()).into());
    }
    if b.reward_table.is_none() && b.key_group.is_none() {
        return Err(Error::InvalidArgument(
            "a campaign needs reward_table and/or key_group".into(),
        )
        .into());
    }
    if let Some(g) = &b.key_group {
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from musubi.key_groups where id = $1)")
                .bind(g)
                .fetch_one(&s.db)
                .await
                .map_err(Error::internal)?;
        if !exists {
            return Err(Error::NotFound(format!("key group {g}")).into());
        }
    }
    if let Some(t) = &b.reward_table {
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from musubi.reward_tables where id = $1)")
                .bind(t)
                .fetch_one(&s.db)
                .await
                .map_err(Error::internal)?;
        if !exists {
            return Err(Error::NotFound(format!("reward table {t}")).into());
        }
    }
    let per_account = b.per_account.unwrap_or(1).max(1);
    let c: Campaign = sqlx::query_as(&format!(
        "insert into musubi.code_campaigns (id, name, reward_table, key_group, per_account, max_redemptions, starts_at, ends_at, audience, enabled, metadata)
         values ($1, $2, $3, $4, $5, $6, to_timestamp($7), to_timestamp($8), $9, $10, $11)
         on conflict (id) do update set name = excluded.name, reward_table = excluded.reward_table,
            key_group = excluded.key_group, per_account = excluded.per_account,
            max_redemptions = excluded.max_redemptions, starts_at = excluded.starts_at, ends_at = excluded.ends_at,
            audience = excluded.audience, enabled = excluded.enabled, metadata = excluded.metadata, updated_at = now()
         returning {CAMPAIGN_COLS}"
    ))
    .bind(&id)
    .bind(b.name.trim())
    .bind(&b.reward_table)
    .bind(&b.key_group)
    .bind(per_account)
    .bind(b.max_redemptions)
    .bind(b.starts_at.map(|t| t as f64))
    .bind(b.ends_at.map(|t| t as f64))
    .bind(&b.audience)
    .bind(b.enabled.unwrap_or(true))
    .bind(if b.metadata.is_object() { b.metadata.clone() } else { json!({}) })
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(campaign_json(&c)))
}

async fn stats(s: &AppState, id: &str) -> Result<Value> {
    let (codes, revoked, uses): (i64, i64, i64) = sqlx::query_as(
        "select count(*), count(*) filter (where revoked), coalesce(sum(uses), 0)::bigint from musubi.codes where campaign_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let (redemptions, accounts): (i64, i64) = sqlx::query_as(
        "select count(*), count(distinct account_id) from musubi.code_redemptions where campaign_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(
        json!({ "codes": codes, "revoked": revoked, "uses": uses, "redemptions": redemptions, "accounts": accounts }),
    )
}

async fn get_campaign(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let c = load_campaign(&s, &id).await?;
    let mut v = campaign_json(&c);
    v["stats"] = stats(&s, &id).await?;
    Ok(Json(v))
}

async fn list_campaigns(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Campaign> = sqlx::query_as(&format!(
        "select {CAMPAIGN_COLS} from musubi.code_campaigns order by created_at desc limit 500"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "campaigns": rows.iter().map(campaign_json).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GenerateBody {
    count: i64,
    length: Option<usize>,
    prefix: String,
    max_uses: Option<i64>,
    batch: String,
}

/// `POST /v1/server/codes/campaigns/{id}/generate {count, length?, prefix?, max_uses?, batch?}` —
/// single-use (default) random codes; returns them once, so hand them to the campaign owner now.
async fn generate(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<GenerateBody>,
) -> Result<Json<Value>, ApiError> {
    load_campaign(&s, &id).await?;
    if !(1..=10_000).contains(&b.count) {
        return Err(Error::InvalidArgument("count must be 1..10000".into()).into());
    }
    let len = b.length.unwrap_or(12);
    if !(6..=32).contains(&len) {
        return Err(Error::InvalidArgument("length must be 6..32".into()).into());
    }
    let prefix = normalize(&b.prefix);
    let max_uses = b.max_uses.unwrap_or(1);
    if max_uses < 1 {
        return Err(Error::InvalidArgument("max_uses must be >= 1".into()).into());
    }
    let batch = if b.batch.is_empty() {
        Uuid::now_v7().simple().to_string()
    } else {
        b.batch.clone()
    };
    let mut codes = Vec::with_capacity(b.count as usize);
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    while (codes.len() as i64) < b.count {
        let code = random_code(&prefix, len);
        let n = sqlx::query(
            "insert into musubi.codes (code, campaign_id, max_uses, batch) values ($1, $2, $3, $4) on conflict do nothing",
        )
        .bind(&code)
        .bind(&id)
        .bind(max_uses)
        .bind(&batch)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 1 {
            codes.push(code);
        }
    }
    tx.commit().await.map_err(Error::internal)?;
    Ok(Json(
        json!({ "campaign_id": id, "batch": batch, "max_uses": max_uses, "codes": codes }),
    ))
}

#[derive(Deserialize)]
struct SharedBody {
    code: String,
    #[serde(default)]
    max_uses: Option<i64>,
}

/// `POST /v1/server/codes/campaigns/{id}/codes {code, max_uses?}` — a shared code (`LAUNCH2026`),
/// unlimited uses unless `max_uses` is set; per-account limits still apply.
async fn add_shared(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<SharedBody>,
) -> Result<Json<Value>, ApiError> {
    load_campaign(&s, &id).await?;
    let code = normalize(&b.code);
    if code.len() < 4 || code.len() > 64 {
        return Err(Error::InvalidArgument("code must be 4..64 alphanumerics".into()).into());
    }
    if b.max_uses.is_some_and(|m| m < 1) {
        return Err(Error::InvalidArgument("max_uses must be >= 1".into()).into());
    }
    let n = sqlx::query(
        "insert into musubi.codes (code, campaign_id, max_uses, batch) values ($1, $2, $3, 'shared') on conflict do nothing",
    )
    .bind(&code)
    .bind(&id)
    .bind(b.max_uses)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::AlreadyExists(format!("code {code}")).into());
    }
    Ok(Json(
        json!({ "campaign_id": id, "code": code, "max_uses": b.max_uses }),
    ))
}

#[derive(Deserialize, Default)]
struct CodesQ {
    unused: Option<bool>,
    batch: Option<String>,
    limit: Option<i64>,
}

async fn list_codes(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<CodesQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(String, Option<i64>, i64, bool, String, OffsetDateTime)> = sqlx::query_as(
        "select code, max_uses, uses, revoked, batch, created_at from musubi.codes
         where campaign_id = $1 and ($2::text is null or batch = $2)
           and (not $3 or (uses = 0 and not revoked))
         order by created_at desc, code limit $4",
    )
    .bind(&id)
    .bind(q.batch.as_deref())
    .bind(q.unused.unwrap_or(false))
    .bind(q.limit.unwrap_or(1000).clamp(1, 10_000))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "codes": rows.iter().map(|(c, m, u, r, b, at)| json!({
        "code": c, "max_uses": m, "uses": u, "revoked": r, "batch": b, "created_at": at.unix_timestamp() })).collect::<Vec<_>>() }),
    ))
}

async fn revoke(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let code = normalize(&code);
    let n = sqlx::query("update musubi.codes set revoked = true where code = $1")
        .bind(&code)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("code {code}")).into());
    }
    Ok(Json(json!({ "code": code, "revoked": true })))
}

type RedemptionRow = (
    Uuid,
    String,
    String,
    Uuid,
    Value,
    Vec<String>,
    OffsetDateTime,
);

fn redemption_json(
    (id, code, campaign, acc, reward, keys, at): &RedemptionRow,
    with_keys: bool,
) -> Value {
    let mut v = json!({ "id": id, "code": code, "campaign_id": campaign, "account_id": acc,
        "reward": reward, "at": at.unix_timestamp() });
    if with_keys {
        v["keys"] = json!(keys);
    } else {
        v["keys"] = json!(keys.len());
    }
    v
}

#[derive(Deserialize, Default)]
struct RedQ {
    limit: Option<i64>,
    before: Option<i64>,
}

async fn campaign_redemptions(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<RedQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<RedemptionRow> = sqlx::query_as(
        "select id, code, campaign_id, account_id, reward, keys, at from musubi.code_redemptions
         where campaign_id = $1 and ($2::bigint is null or at < to_timestamp($2)) order by at desc limit $3",
    )
    .bind(&id)
    .bind(q.before)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "redemptions": rows.iter().map(|r| redemption_json(r, false)).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct KeyGroupBody {
    name: String,
    platform: String,
}

async fn upsert_key_group(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<KeyGroupBody>,
) -> Result<Json<Value>, ApiError> {
    if !valid_id(&id) {
        return Err(Error::InvalidArgument("id must be [A-Za-z0-9_.-]{1,64}".into()).into());
    }
    sqlx::query(
        "insert into musubi.key_groups (id, name, platform) values ($1, $2, $3)
         on conflict (id) do update set name = excluded.name, platform = excluded.platform",
    )
    .bind(&id)
    .bind(if b.name.is_empty() { &id } else { &b.name })
    .bind(&b.platform)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    key_group_json(&s, &id).await.map(Json).map_err(Into::into)
}

async fn key_group_json(s: &AppState, id: &str) -> Result<Value> {
    let row: Option<(String, String, i64, i64)> = sqlx::query_as(
        "select g.name, g.platform,
                (select count(*) from musubi.platform_keys k where k.group_id = g.id),
                (select count(*) from musubi.platform_keys k where k.group_id = g.id and k.claimed_by is not null)
         from musubi.key_groups g where g.id = $1",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let (name, platform, total, claimed) =
        row.ok_or_else(|| Error::NotFound(format!("key group {id}")))?;
    Ok(
        json!({ "id": id, "name": name, "platform": platform, "total": total, "claimed": claimed, "available": total - claimed }),
    )
}

async fn get_key_group(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(key_group_json(&s, &id).await?))
}

async fn list_key_groups(
    _k: ServerKey,
    State(s): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    let ids: Vec<(String,)> =
        sqlx::query_scalar::<_, String>("select id from musubi.key_groups order by id")
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?
            .into_iter()
            .map(|i| (i,))
            .collect();
    let mut out = Vec::new();
    for (id,) in ids {
        out.push(key_group_json(&s, &id).await?);
    }
    Ok(Json(json!({ "key_groups": out })))
}

#[derive(Deserialize)]
struct UploadKeys {
    keys: Vec<String>,
}

/// `POST /v1/server/key_groups/{id}/keys {keys: [...]}` — bulk upload; duplicates are skipped.
async fn upload_keys(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(b): Json<UploadKeys>,
) -> Result<Json<Value>, ApiError> {
    key_group_json(&s, &id).await?;
    if b.keys.is_empty() || b.keys.len() > 50_000 {
        return Err(Error::InvalidArgument("1..50000 keys per upload".into()).into());
    }
    let mut added = 0u64;
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    for k in &b.keys {
        let k = k.trim();
        if k.is_empty() || k.len() > 256 {
            continue;
        }
        added += sqlx::query(
            "insert into musubi.platform_keys (id, group_id, key) values ($1, $2, $3) on conflict do nothing",
        )
        .bind(Uuid::now_v7())
        .bind(&id)
        .bind(k)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    }
    tx.commit().await.map_err(Error::internal)?;
    let mut v = key_group_json(&s, &id).await?;
    v["added"] = json!(added);
    Ok(Json(v))
}

/// Redemption attempts per account per minute (brute-force guard).
fn attempts() -> &'static DashMap<Uuid, (Instant, u32)> {
    static A: OnceLock<DashMap<Uuid, (Instant, u32)>> = OnceLock::new();
    A.get_or_init(DashMap::new)
}

const ATTEMPTS_PER_MINUTE: u32 = 10;

fn note_attempt(acc: AccountId) -> Result<()> {
    let mut e = attempts().entry(acc.0).or_insert((Instant::now(), 0));
    if e.0.elapsed().as_secs() >= 60 {
        *e = (Instant::now(), 0);
    }
    e.1 += 1;
    if e.1 > ATTEMPTS_PER_MINUTE {
        return Err(Error::ResourceExhausted(
            "too many code attempts; try again in a minute".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct RedeemBody {
    code: String,
}

/// `POST /v1/codes/redeem {code}` → `{campaign, name, reward: [...], keys: [...], metadata}`.
async fn redeem(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(b): Json<RedeemBody>,
) -> Result<Json<Value>, ApiError> {
    let acc = me.account_id;
    note_attempt(acc)?;
    let code = normalize(&b.code);
    if code.is_empty() {
        return Err(Error::InvalidArgument("code is required".into()).into());
    }
    let mut tx = s.db.begin().await.map_err(Error::internal)?;
    let row: Option<(String, Option<i64>, i64, bool)> = sqlx::query_as(
        "select campaign_id, max_uses, uses, revoked from musubi.codes where code = $1 for update",
    )
    .bind(&code)
    .fetch_optional(&mut *tx)
    .await
    .map_err(Error::internal)?;
    let Some((campaign_id, max_uses, uses, revoked)) = row else {
        return Err(Error::NotFound("unknown code".into()).into());
    };
    if revoked {
        return Err(Error::NotFound("unknown code".into()).into());
    }
    let c = load_campaign(&s, &campaign_id).await?;
    let now = OffsetDateTime::now_utc();
    if !c.enabled || c.starts_at.is_some_and(|t| t > now) || c.ends_at.is_some_and(|t| t <= now) {
        return Err(Error::Conflict("this code is not active".into()).into());
    }
    if max_uses.is_some_and(|m| uses >= m) {
        return Err(Error::Conflict("this code has already been used".into()).into());
    }
    let (mine, total): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where account_id = $2), count(*) from musubi.code_redemptions where campaign_id = $1",
    )
    .bind(&campaign_id)
    .bind(acc.0)
    .fetch_one(&mut *tx)
    .await
    .map_err(Error::internal)?;
    if mine >= c.per_account as i64 {
        return Err(Error::Conflict("you already redeemed this campaign".into()).into());
    }
    if c.max_redemptions.is_some_and(|m| total >= m) {
        return Err(Error::Conflict("this campaign is exhausted".into()).into());
    }
    if let Some(a) = &c.audience {
        let set = s.audiences.of(&s.db, &s.economy, acc).await?;
        if !set.contains(a) {
            return Err(Error::PermissionDenied("this code is not for your account".into()).into());
        }
    }
    let mut keys: Vec<String> = Vec::new();
    if let Some(g) = &c.key_group {
        let key: Option<(String,)> = sqlx::query_as(
            "update musubi.platform_keys set claimed_by = $2, claimed_at = now()
             where id = (select id from musubi.platform_keys where group_id = $1 and claimed_by is null
                         order by id limit 1 for update skip locked)
             returning key",
        )
        .bind(g)
        .bind(acc.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        match key {
            Some((k,)) => keys.push(k),
            None => return Err(Error::Conflict("no keys left in this campaign".into()).into()),
        }
    }
    sqlx::query("update musubi.codes set uses = uses + 1 where code = $1")
        .bind(&code)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
    let rid = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.code_redemptions (id, code, campaign_id, account_id, keys) values ($1, $2, $3, $4, $5)",
    )
    .bind(rid)
    .bind(&code)
    .bind(&campaign_id)
    .bind(acc.0)
    .bind(&keys)
    .execute(&mut *tx)
    .await
    .map_err(Error::internal)?;
    tx.commit().await.map_err(Error::internal)?;
    let mut reward = json!([]);
    if let Some(t) = &c.reward_table {
        let reference = format!("code:{code}:{}", acc.0);
        match s.economy.grant_table(acc, t, &reference).await {
            Ok(r) => reward = serde_json::to_value(&r).unwrap_or(json!([])),
            Err(e) => tracing::warn!(error = %e, code, "code reward grant failed"),
        }
        let _ = sqlx::query("update musubi.code_redemptions set reward = $2 where id = $1")
            .bind(rid)
            .bind(&reward)
            .execute(&s.db)
            .await;
    }
    s.events
        .emit(
            "code.redeemed",
            Some(acc),
            json!({ "code": code, "campaign_id": campaign_id, "keys": keys.len() }),
        )
        .await;
    Ok(Json(json!({
        "campaign": c.id, "name": c.name, "reward": reward, "keys": keys, "metadata": c.metadata,
    })))
}

async fn my_redemptions(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<RedemptionRow> = sqlx::query_as(
        "select id, code, campaign_id, account_id, reward, keys, at from musubi.code_redemptions
         where account_id = $1 order by at desc limit 200",
    )
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "redemptions": rows.iter().map(|r| redemption_json(r, true)).collect::<Vec<_>>() }),
    ))
}
