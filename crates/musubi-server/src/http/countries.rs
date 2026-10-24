//! Country API: the ISO 3166-1 list with per-country overrides (age restriction, blocked,
//! metadata) and named country groups (`eu`, `latam`, `gdpr`…) that pricing, remote-config
//! selectors (`countries: ["group:eu"]`) and operators can reference instead of listing codes.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;

use crate::app::AppState;
use crate::http::countries_iso::{Country, COUNTRIES};
use crate::http::{ApiError, ServerKey};

const CACHE_SECS: u64 = 15;
const MAX_METADATA_BYTES: usize = 4 * 1024;
/// A group lists at most every country a few times over (duplicates / alpha-3 / numeric mixes).
const MAX_GROUP_CODES: usize = 1000;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/countries", get(list))
        .route("/v1/countries/{code}", get(get_one))
        .route(
            "/v1/server/countries/{code}",
            axum::routing::put(put_override).delete(delete_override),
        )
        .route("/v1/server/country_groups", get(list_groups))
        .route(
            "/v1/server/country_groups/{name}",
            get(get_group).put(put_group).delete(delete_group),
        )
}

pub fn find(code: &str) -> Option<&'static Country> {
    let c = code.trim().to_ascii_uppercase();
    COUNTRIES
        .iter()
        .find(|x| x.alpha2 == c || x.alpha3 == c || x.numeric == c)
}

fn lookup(code: &str) -> Result<&'static Country> {
    find(code).ok_or_else(|| Error::NotFound(format!("country {code}")))
}

#[derive(Clone, Default)]
struct Override {
    age_restriction: Option<i32>,
    blocked: bool,
    metadata: Value,
    updated_at: Option<OffsetDateTime>,
}

#[derive(Clone)]
struct Group {
    name: String,
    description: Option<String>,
    countries: Vec<String>,
    updated_at: OffsetDateTime,
}

#[derive(Clone, Default)]
struct Snapshot {
    overrides: HashMap<String, Override>,
    groups: Vec<Group>,
}

type Cache = RwLock<Option<(Instant, Snapshot)>>;

fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(|| RwLock::new(None))
}

fn invalidate() {
    *cache().write().unwrap_or_else(|e| e.into_inner()) = None;
}

async fn load(s: &AppState) -> Result<Snapshot> {
    if let Some((at, snap)) = cache().read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if at.elapsed() < Duration::from_secs(CACHE_SECS) {
            return Ok(snap.clone());
        }
    }
    let ov: Vec<(String, Option<i32>, bool, Value, OffsetDateTime)> = sqlx::query_as(
        "select code, age_restriction, blocked, metadata, updated_at from musubi.country_overrides",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let gr: Vec<(String, Option<String>, Vec<String>, OffsetDateTime)> = sqlx::query_as(
        "select name, description, countries, updated_at from musubi.country_groups order by name",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let snap = Snapshot {
        overrides: ov
            .into_iter()
            .map(|(c, a, b, m, u)| {
                (
                    c,
                    Override {
                        age_restriction: a,
                        blocked: b,
                        metadata: m,
                        updated_at: Some(u),
                    },
                )
            })
            .collect(),
        groups: gr
            .into_iter()
            .map(|(name, description, countries, updated_at)| Group {
                name,
                description,
                countries,
                updated_at,
            })
            .collect(),
    };
    *cache().write().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), snap.clone()));
    Ok(snap)
}

/// Lower-case names of the groups `code` belongs to (empty for unknown/missing codes); used by
/// remote-config selectors (`countries: ["group:eu"]`). Storage errors degrade to "no groups".
pub async fn groups_for(s: &AppState, code: Option<&str>) -> Vec<String> {
    let Some(c) = code.and_then(find) else {
        return Vec::new();
    };
    match load(s).await {
        Ok(snap) => snap
            .groups
            .iter()
            .filter(|g| g.countries.iter().any(|x| x == c.alpha2))
            .map(|g| g.name.clone())
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "country groups unavailable");
            Vec::new()
        }
    }
}

fn doc(c: &Country, snap: &Snapshot) -> Value {
    let o = snap.overrides.get(c.alpha2).cloned().unwrap_or_default();
    let groups: Vec<&str> = snap
        .groups
        .iter()
        .filter(|g| g.countries.iter().any(|x| x == c.alpha2))
        .map(|g| g.name.as_str())
        .collect();
    json!({
        "code": c.alpha2, "alpha3": c.alpha3, "numeric": c.numeric, "name": c.name,
        "age_restriction": o.age_restriction, "blocked": o.blocked, "metadata": o.metadata,
        "groups": groups,
        "updated_at": o.updated_at.map(|t| t.unix_timestamp()),
    })
}

fn group_doc(g: &Group) -> Value {
    json!({
        "name": g.name, "description": g.description, "countries": g.countries,
        "count": g.countries.len(), "updated_at": g.updated_at.unix_timestamp(),
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListQuery {
    group: Option<String>,
    blocked: Option<bool>,
    q: Option<String>,
}

/// `GET /v1/countries?group&blocked&q` — the full ISO 3166-1 list with overrides and group
/// membership; no token (sign-up screens need it before login).
async fn list(
    State(s): State<AppState>,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let snap = load(&s).await?;
    let group = q.group.as_deref().map(|g| g.trim().to_ascii_lowercase());
    if let Some(g) = &group {
        if !snap.groups.iter().any(|x| &x.name == g) {
            return Err(Error::NotFound(format!("country group {g}")).into());
        }
    }
    let needle = q.q.as_deref().map(|x| x.trim().to_ascii_lowercase());
    let countries: Vec<Value> = COUNTRIES
        .iter()
        .filter(|c| {
            group.as_ref().is_none_or(|g| {
                snap.groups
                    .iter()
                    .any(|x| &x.name == g && x.countries.iter().any(|y| y == c.alpha2))
            })
        })
        .filter(|c| {
            q.blocked.is_none_or(|b| {
                snap.overrides
                    .get(c.alpha2)
                    .map(|o| o.blocked)
                    .unwrap_or(false)
                    == b
            })
        })
        .filter(|c| {
            needle.as_ref().is_none_or(|n| {
                c.name.to_ascii_lowercase().contains(n.as_str())
                    || c.alpha2.eq_ignore_ascii_case(n)
                    || c.alpha3.eq_ignore_ascii_case(n)
            })
        })
        .map(|c| doc(c, &snap))
        .collect();
    Ok(Json(
        json!({ "countries": countries, "total": countries.len() }),
    ))
}

/// `GET /v1/countries/{code}` — alpha-2, alpha-3 or numeric code.
async fn get_one(
    State(s): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let c = lookup(&code)?;
    let snap = load(&s).await?;
    Ok(Json(doc(c, &snap)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct OverrideBody {
    age_restriction: Option<i32>,
    blocked: Option<bool>,
    metadata: Option<Value>,
}

/// `PUT /v1/server/countries/{code} {age_restriction?, blocked?, metadata?}` — per-country
/// policy: minimum age (0..=21, null = none), `blocked` (no sign-ups / purchases for the
/// country), free-form metadata (tax rate, store currency…) ≤ 4 KiB. Absent fields are kept.
async fn put_override(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(code): Path<String>,
    Json(b): Json<OverrideBody>,
) -> Result<Json<Value>, ApiError> {
    let c = lookup(&code)?;
    if b.age_restriction.is_some_and(|a| !(0..=21).contains(&a)) {
        return Err(Error::InvalidArgument("age_restriction must be 0..=21".into()).into());
    }
    if let Some(m) = &b.metadata {
        if !m.is_object() {
            return Err(Error::InvalidArgument("metadata must be an object".into()).into());
        }
        if serde_json::to_vec(m).map(|v| v.len()).unwrap_or(usize::MAX) > MAX_METADATA_BYTES {
            return Err(Error::InvalidArgument("metadata exceeds 4 KiB".into()).into());
        }
    }
    if b.age_restriction.is_none() && b.blocked.is_none() && b.metadata.is_none() {
        return Err(Error::InvalidArgument(
            "set at least one of age_restriction, blocked, metadata".into(),
        )
        .into());
    }
    let has_age = b.age_restriction.is_some();
    sqlx::query(
        "insert into musubi.country_overrides (code, age_restriction, blocked, metadata)
         values ($1, $2, coalesce($3, false), coalesce($4, '{}'::jsonb))
         on conflict (code) do update set
           age_restriction = case when $5 then $2 else country_overrides.age_restriction end,
           blocked = coalesce($3, country_overrides.blocked),
           metadata = coalesce($4, country_overrides.metadata),
           updated_at = now()",
    )
    .bind(c.alpha2)
    .bind(b.age_restriction)
    .bind(b.blocked)
    .bind(b.metadata)
    .bind(has_age)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    invalidate();
    let snap = load(&s).await?;
    Ok(Json(doc(c, &snap)))
}

/// `DELETE /v1/server/countries/{code}` — drop the override (back to ISO defaults).
async fn delete_override(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(code): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let c = lookup(&code)?;
    let n = sqlx::query("delete from musubi.country_overrides where code = $1")
        .bind(c.alpha2)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    invalidate();
    Ok(Json(json!({ "code": c.alpha2, "removed": n > 0 })))
}

/// `GET /v1/server/country_groups` — all groups with their members.
async fn list_groups(State(s): State<AppState>, _k: ServerKey) -> Result<Json<Value>, ApiError> {
    let snap = load(&s).await?;
    let groups: Vec<Value> = snap.groups.iter().map(group_doc).collect();
    Ok(Json(json!({ "groups": groups })))
}

fn group_name(name: &str) -> Result<String> {
    let n = name.trim().to_ascii_lowercase();
    if n.is_empty()
        || n.len() > 32
        || !n
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(Error::InvalidArgument(
            "group name must be [a-z0-9_-]{1,32}".into(),
        ));
    }
    Ok(n)
}

async fn get_group(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let name = group_name(&name)?;
    let snap = load(&s).await?;
    let g = snap
        .groups
        .iter()
        .find(|g| g.name == name)
        .ok_or_else(|| Error::NotFound(format!("country group {name}")))?;
    Ok(Json(group_doc(g)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GroupBody {
    description: Option<String>,
    countries: Vec<String>,
}

/// `PUT /v1/server/country_groups/{name} {countries: ["DE", "FR", …], description?}` — create or
/// replace a group; codes may be alpha-2/alpha-3/numeric and are stored as alpha-2, deduplicated.
async fn put_group(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<GroupBody>,
) -> Result<Json<Value>, ApiError> {
    let name = group_name(&name)?;
    if b.countries.is_empty() || b.countries.len() > MAX_GROUP_CODES {
        return Err(Error::InvalidArgument(format!(
            "countries must have 1..={MAX_GROUP_CODES} codes"
        ))
        .into());
    }
    if b.description.as_ref().is_some_and(|d| d.len() > 200) {
        return Err(Error::InvalidArgument("description exceeds 200 chars".into()).into());
    }
    let mut codes: Vec<String> = Vec::new();
    for c in &b.countries {
        let k = find(c)
            .ok_or_else(|| Error::InvalidArgument(format!("unknown country {c}")))?
            .alpha2
            .to_string();
        if !codes.contains(&k) {
            codes.push(k);
        }
    }
    codes.sort();
    sqlx::query(
        "insert into musubi.country_groups (name, description, countries) values ($1, $2, $3)
         on conflict (name) do update set description = $2, countries = $3, updated_at = now()",
    )
    .bind(&name)
    .bind(&b.description)
    .bind(&codes)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    invalidate();
    let snap = load(&s).await?;
    let g = snap
        .groups
        .iter()
        .find(|g| g.name == name)
        .ok_or_else(|| Error::NotFound(format!("country group {name}")))?;
    Ok(Json(group_doc(g)))
}

async fn delete_group(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let name = group_name(&name)?;
    let n = sqlx::query("delete from musubi.country_groups where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    invalidate();
    if n == 0 {
        return Err(Error::NotFound(format!("country group {name}")).into());
    }
    Ok(Json(json!({ "name": name, "removed": true })))
}
