//! Remote config depth (P1-64): variants layered on a base config by selector (environment,
//! platform, client version range, country, deterministic rollout percentage) as RFC 7386 merge
//! patches, an audited history with rollback and side-by-side diff, CSV/spreadsheet ingestion,
//! a checksum manifest for cheap "anything changed?" polls, and client version gates with
//! redirect-by-version (`/v1/client/version`).

use std::cmp::Ordering;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/configs/manifest", get(manifest))
        .route("/v1/server/configs/{name}/variants", get(list_variants))
        .route(
            "/v1/server/configs/{name}/variants/{variant}",
            put(put_variant).delete(delete_variant),
        )
        .route("/v1/server/configs/{name}/history", get(history))
        .route("/v1/server/configs/{name}/rollback", post(rollback))
        .route("/v1/server/configs/{name}/diff", get(diff_route))
        .route("/v1/server/configs/{name}/csv", put(put_csv))
        .route("/v1/server/configs/{name}/preview", get(preview))
        .route("/v1/server/client_versions", get(list_client_versions))
        .route(
            "/v1/server/client_versions/{platform}",
            put(put_client_version).delete(delete_client_version),
        )
        .route("/v1/client/version", get(client_version))
        .route(
            "/v1/server/client_versions/{platform}/logic/{logic_version}",
            put(put_logic_rule).delete(delete_logic_rule),
        )
}

/// Who is asking: from the query string, falling back to `x-musubi-platform`,
/// `x-musubi-client-version` and `x-musubi-country` headers.
#[derive(Deserialize, Default, Clone, Debug)]
pub struct Ctx {
    pub platform: Option<String>,
    pub version: Option<String>,
    pub country: Option<String>,
    /// Request address (first `X-Forwarded-For` hop / `X-Real-IP`) for the GeoIP country fallback.
    #[serde(skip)]
    pub client_ip: Option<String>,
    /// Server-side preview only: resolve rollouts as this account would see them.
    pub account_id: Option<AccountId>,
    pub prefix: Option<String>,
    /// Country groups of `country` (filled by `enrich`); selectors may list `group:<name>`.
    #[serde(skip)]
    pub country_groups: Vec<String>,
}

pub fn ctx_from(mut q: Ctx, headers: &HeaderMap) -> Ctx {
    let h = |k: &str| {
        headers
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    q.platform = q.platform.or_else(|| h("x-musubi-platform"));
    q.version = q.version.or_else(|| h("x-musubi-client-version"));
    q.country = q.country.or_else(|| h("x-musubi-country"));
    q.client_ip = h("x-forwarded-for")
        .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
        .or_else(|| h("x-real-ip"));
    q
}

/// Resolve the country's groups so `countries: ["group:eu"]` selectors can match.
pub async fn enrich(s: &AppState, mut ctx: Ctx) -> Ctx {
    if ctx.country.is_none() {
        // no `x-musubi-country`: the request address's country, when GeoIP knows it
        ctx.country = ctx
            .client_ip
            .as_deref()
            .and_then(|ip| s.geoip.country_of(ip));
    }
    if ctx.country.is_some() {
        ctx.country_groups = crate::http::countries::groups_for(s, ctx.country.as_deref()).await;
    }
    ctx
}

/// Compare dotted versions segment by segment: numeric where both are numbers, lexical
/// otherwise; missing trailing segments count as zero (`1.2` == `1.2.0`).
pub fn cmp_version(a: &str, b: &str) -> Ordering {
    let split = |s: &str| -> Vec<String> {
        s.split(['.', '-', '+'])
            .map(|p| p.trim().to_string())
            .collect()
    };
    let (a, b) = (split(a), split(b));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).map(String::as_str).unwrap_or("0");
        let y = b.get(i).map(String::as_str).unwrap_or("0");
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            _ => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    Ordering::Equal
}

fn str_list(v: &Value) -> Vec<String> {
    match v {
        Value::String(s) => vec![s.to_ascii_lowercase()],
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_ascii_lowercase()))
            .collect(),
        _ => vec![],
    }
}

/// Deterministic 0..99 bucket of an account for a (config, variant) pair.
pub fn bucket(account: AccountId, name: &str, variant: &str) -> u32 {
    let mut h = sha2::Sha256::new();
    h.update(account.to_string().as_bytes());
    h.update(b"|");
    h.update(name.as_bytes());
    h.update(b"|");
    h.update(variant.as_bytes());
    let d = h.finalize();
    u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 100
}

/// `{environment, platforms, min_version, max_version, countries, rollout_pct}` — every key
/// present must hold; a key that needs context the caller did not send fails the match.
pub fn selector_matches(sel: &Value, env: &str, ctx: &Ctx, bucket: Option<u32>) -> bool {
    let Some(sel) = sel.as_object() else {
        return true;
    };
    if let Some(e) = sel.get("environment") {
        if !str_list(e).iter().any(|x| x == &env.to_ascii_lowercase()) {
            return false;
        }
    }
    if let Some(p) = sel.get("platforms") {
        let want = str_list(p);
        if !want.is_empty() {
            match &ctx.platform {
                Some(pl) if want.contains(&pl.to_ascii_lowercase()) => {}
                _ => return false,
            }
        }
    }
    if let Some(min) = sel.get("min_version").and_then(Value::as_str) {
        match &ctx.version {
            Some(v) if cmp_version(v, min) != Ordering::Less => {}
            _ => return false,
        }
    }
    if let Some(max) = sel.get("max_version").and_then(Value::as_str) {
        match &ctx.version {
            Some(v) if cmp_version(v, max) != Ordering::Greater => {}
            _ => return false,
        }
    }
    if let Some(c) = sel.get("countries") {
        let want = str_list(c);
        if !want.is_empty() {
            match &ctx.country {
                Some(cc)
                    if want.contains(&cc.to_ascii_lowercase())
                        || ctx
                            .country_groups
                            .iter()
                            .any(|g| want.contains(&format!("group:{g}"))) => {}
                _ => return false,
            }
        }
    }
    if let Some(pct) = sel.get("rollout_pct").and_then(Value::as_f64) {
        if pct < 100.0 {
            match bucket {
                Some(b) if (b as f64) < pct => {}
                _ => return false,
            }
        }
    }
    true
}

/// RFC 7386 JSON merge patch.
pub fn merge_patch(target: &mut Value, patch: &Value) {
    match patch {
        Value::Object(p) => {
            if !target.is_object() {
                *target = Value::Object(Map::new());
            }
            let t = target.as_object_mut().unwrap();
            for (k, v) in p {
                if v.is_null() {
                    t.remove(k);
                } else {
                    merge_patch(t.entry(k.clone()).or_insert(Value::Null), v);
                }
            }
        }
        other => *target = other.clone(),
    }
}

/// Canonical JSON (sorted keys, no whitespace) for stable checksums.
pub fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                canonical(&m[*k], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

pub fn checksum(v: &Value) -> String {
    let mut s = String::new();
    canonical(v, &mut s);
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

/// Structural diff as a flat list of `{path, op, from, to}` (JSON pointer paths).
pub fn diff(a: &Value, b: &Value, path: &str, out: &mut Vec<Value>) {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let p = format!("{path}/{}", k.replace('~', "~0").replace('/', "~1"));
                match (x.get(k), y.get(k)) {
                    (Some(u), Some(v)) => diff(u, v, &p, out),
                    (Some(u), None) => out.push(json!({"path": p, "op": "remove", "from": u})),
                    (None, Some(v)) => out.push(json!({"path": p, "op": "add", "to": v})),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (u, v)) in x.iter().zip(y).enumerate() {
                diff(u, v, &format!("{path}/{i}"), out);
            }
        }
        _ if a != b => {
            out.push(json!({"path": if path.is_empty() { "/" } else { path }, "op": "replace", "from": a, "to": b}))
        }
        _ => {}
    }
}

/// RFC 4180-ish CSV: quoted fields, doubled quotes, CRLF or LF, `//` comment lines skipped.
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = vec![];
    let mut row = vec![];
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut at_line_start = true;
    while let Some(c) = chars.next() {
        if at_line_start && !quoted && c == '/' && chars.peek() == Some(&'/') {
            for n in chars.by_ref() {
                if n == '\n' {
                    break;
                }
            }
            continue;
        }
        at_line_start = false;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if row.iter().any(|f| !f.trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
                at_line_start = true;
            }
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if row.iter().any(|f| !f.trim().is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// Spreadsheet cells become typed JSON: ints, floats, bools, `null`/empty, inline JSON
/// arrays/objects, else strings.
pub fn infer_cell(s: &str) -> Value {
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if t.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if t.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if let Ok(i) = t.parse::<i64>() {
        return json!(i);
    }
    if let Ok(f) = t.parse::<f64>() {
        if f.is_finite() {
            return json!(f);
        }
    }
    if (t.starts_with('[') && t.ends_with(']')) || (t.starts_with('{') && t.ends_with('}')) {
        if let Ok(v) = serde_json::from_str::<Value>(t) {
            return v;
        }
    }
    Value::String(s.to_string())
}

fn set_path(obj: &mut Value, parts: &[&str], cell: Value) {
    if !obj.is_object() {
        *obj = Value::Object(Map::new());
    }
    let m = obj.as_object_mut().unwrap();
    match parts {
        [] => {}
        [last] => {
            m.insert(last.to_string(), cell);
        }
        [head, rest @ ..] => {
            let next = m
                .entry(head.to_string())
                .or_insert_with(|| Value::Object(Map::new()));
            set_path(next, rest, cell);
        }
    }
}

/// Sheet → JSON: rows as objects keyed by the header, or an object keyed by `key` column;
/// dotted headers (`reward.gold`) nest.
pub fn csv_to_value(text: &str, key: Option<&str>) -> Result<Value, Error> {
    let rows = parse_csv(text);
    let Some((head, body)) = rows.split_first() else {
        return Err(Error::InvalidArgument("csv: header row required".into()));
    };
    let head: Vec<String> = head.iter().map(|h| h.trim().to_string()).collect();
    if head.iter().any(String::is_empty) {
        return Err(Error::InvalidArgument("csv: empty header column".into()));
    }
    let key_idx =
        match key {
            Some(k) => Some(head.iter().position(|h| h == k).ok_or_else(|| {
                Error::InvalidArgument(format!("csv: key column `{k}` not found"))
            })?),
            None => None,
        };
    let mut list = vec![];
    let mut map = Map::new();
    for (ln, r) in body.iter().enumerate() {
        if r.len() > head.len() {
            return Err(Error::InvalidArgument(format!(
                "csv: row {} has {} cells, header has {}",
                ln + 2,
                r.len(),
                head.len()
            )));
        }
        let mut obj = Value::Object(Map::new());
        for (i, h) in head.iter().enumerate() {
            let cell = infer_cell(r.get(i).map(String::as_str).unwrap_or(""));
            if cell.is_null() {
                continue;
            }
            let parts: Vec<&str> = h.split('.').collect();
            set_path(&mut obj, &parts, cell);
        }
        match key_idx {
            Some(ki) => {
                let k = r.get(ki).map(|s| s.trim().to_string()).unwrap_or_default();
                if k.is_empty() {
                    return Err(Error::InvalidArgument(format!(
                        "csv: row {} has an empty key",
                        ln + 2
                    )));
                }
                if map.insert(k.clone(), obj).is_some() {
                    return Err(Error::InvalidArgument(format!("csv: duplicate key `{k}`")));
                }
            }
            None => list.push(obj),
        }
    }
    Ok(if key_idx.is_some() {
        Value::Object(map)
    } else {
        Value::Array(list)
    })
}

type ConfigRow = (String, Value, i32, i64);
type VariantRow = (String, String, Value, Value, i32, bool, i64);

fn variant_doc(
    (name, variant, selector, patch, priority, enabled, updated_at): &VariantRow,
) -> Value {
    json!({ "name": name, "variant": variant, "selector": selector, "patch": patch,
        "priority": priority, "enabled": enabled, "updated_at": updated_at })
}

/// Base configs with every matching variant patched in (priority ascending, then name).
pub async fn resolve(
    s: &AppState,
    name: Option<&str>,
    ctx: &Ctx,
    account: Option<AccountId>,
) -> Result<Vec<Value>, ApiError> {
    let configs: Vec<ConfigRow> = match name {
        Some(n) => sqlx::query_as(
            "select name, value, version, extract(epoch from updated_at)::bigint
             from musubi.configs where name = $1",
        )
        .bind(n)
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?,
        None => sqlx::query_as(
            "select name, value, version, extract(epoch from updated_at)::bigint
             from musubi.configs order by name",
        )
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?,
    };
    if configs.is_empty() {
        return Ok(vec![]);
    }
    let variants: Vec<VariantRow> = match name {
        Some(n) => sqlx::query_as(
            "select name, variant, selector, patch, priority, enabled,
                    extract(epoch from updated_at)::bigint
             from musubi.config_variants where name = $1 and enabled
             order by priority, variant",
        )
        .bind(n)
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?,
        None => sqlx::query_as(
            "select name, variant, selector, patch, priority, enabled,
                    extract(epoch from updated_at)::bigint
             from musubi.config_variants where enabled order by name, priority, variant",
        )
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?,
    };
    let env = s.cfg.node.environment.as_str();
    let account = account.or(ctx.account_id);
    let mut out = Vec::with_capacity(configs.len());
    for (cname, mut value, version, updated_at) in configs {
        let mut applied = vec![];
        for v in variants.iter().filter(|v| v.0 == cname) {
            let b = account.map(|a| bucket(a, &cname, &v.1));
            if selector_matches(&v.2, env, ctx, b) {
                merge_patch(&mut value, &v.3);
                applied.push(v.1.clone());
            }
        }
        let sum = checksum(&value);
        out.push(
            json!({ "name": cname, "value": value, "version": version, "updated_at": updated_at,
            "variants": applied, "checksum": sum }),
        );
    }
    Ok(out)
}

fn etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(',')
                .any(|t| t.trim().trim_matches('"') == etag || t.trim() == "*")
        })
        .unwrap_or(false)
}

pub fn etag_json(headers: &HeaderMap, etag: &str, body: Value) -> Response {
    let quoted = format!("\"{etag}\"");
    if etag_matches(headers, etag) {
        return (StatusCode::NOT_MODIFIED, [(header::ETAG, quoted)]).into_response();
    }
    (
        [
            (header::ETAG, quoted),
            (
                header::CACHE_CONTROL,
                "private, max-age=0, must-revalidate".to_string(),
            ),
        ],
        Json(body),
    )
        .into_response()
}

pub fn manifest_checksum(items: &[(String, String)]) -> String {
    let mut h = sha2::Sha256::new();
    for (k, v) in items {
        h.update(k.as_bytes());
        h.update(b":");
        h.update(v.as_bytes());
        h.update(b"\n");
    }
    hex::encode(h.finalize())
}

async fn manifest(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Response, ApiError> {
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let all = resolve(&s, None, &ctx, Some(me.account_id)).await?;
    let items: Vec<(String, String)> = all
        .iter()
        .map(|c| {
            (
                c["name"].as_str().unwrap_or("").to_string(),
                c["checksum"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect();
    let sum = manifest_checksum(&items);
    let configs: Vec<Value> = all
        .iter()
        .map(|c| json!({ "name": c["name"], "version": c["version"], "checksum": c["checksum"], "variants": c["variants"] }))
        .collect();
    Ok(etag_json(
        &headers,
        &sum,
        json!({ "configs": configs, "checksum": sum }),
    ))
}

async fn preview(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    headers: HeaderMap,
    Query(q): Query<Ctx>,
) -> Result<Json<Value>, ApiError> {
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let mut v = resolve(&s, Some(&name), &ctx, None).await?;
    match v.pop() {
        Some(c) => Ok(Json(c)),
        None => Err(Error::NotFound(format!("config `{name}`")).into()),
    }
}

async fn config_exists(s: &AppState, name: &str) -> Result<(), ApiError> {
    let n: Option<(String,)> = sqlx::query_as("select name from musubi.configs where name = $1")
        .bind(name)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    if n.is_none() {
        return Err(Error::NotFound(format!("config `{name}`")).into());
    }
    Ok(())
}

async fn list_variants(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    config_exists(&s, &name).await?;
    let rows: Vec<VariantRow> = sqlx::query_as(
        "select name, variant, selector, patch, priority, enabled,
                extract(epoch from updated_at)::bigint
         from musubi.config_variants where name = $1 order by priority, variant",
    )
    .bind(&name)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "variants": rows.iter().map(variant_doc).collect::<Vec<_>>() }),
    ))
}

#[derive(Deserialize)]
struct VariantBody {
    #[serde(default)]
    selector: Value,
    patch: Value,
    #[serde(default)]
    priority: i32,
    #[serde(default = "yes")]
    enabled: bool,
}

fn yes() -> bool {
    true
}

fn check_selector(sel: &Value) -> Result<(), ApiError> {
    let Some(m) = sel.as_object() else {
        if sel.is_null() {
            return Ok(());
        }
        return Err(Error::InvalidArgument("selector must be an object".into()).into());
    };
    const KEYS: &[&str] = &[
        "environment",
        "platforms",
        "min_version",
        "max_version",
        "countries",
        "rollout_pct",
    ];
    for k in m.keys() {
        if !KEYS.contains(&k.as_str()) {
            return Err(Error::InvalidArgument(format!(
                "selector: unknown key `{k}` (one of {KEYS:?})"
            ))
            .into());
        }
    }
    if let Some(p) = m.get("rollout_pct") {
        match p.as_f64() {
            Some(x) if (0.0..=100.0).contains(&x) => {}
            _ => return Err(Error::InvalidArgument("selector.rollout_pct: 0..100".into()).into()),
        }
    }
    for k in ["min_version", "max_version"] {
        if let Some(v) = m.get(k) {
            if !v.is_string() {
                return Err(Error::InvalidArgument(format!("selector.{k}: string")).into());
            }
        }
    }
    Ok(())
}

async fn put_variant(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, variant)): Path<(String, String)>,
    Json(b): Json<VariantBody>,
) -> Result<Json<Value>, ApiError> {
    config_exists(&s, &name).await?;
    if variant.is_empty() || variant.len() > 64 {
        return Err(Error::InvalidArgument("variant name: 1..64 chars".into()).into());
    }
    check_selector(&b.selector)?;
    if !b.patch.is_object() {
        return Err(
            Error::InvalidArgument("patch must be a JSON merge patch object".into()).into(),
        );
    }
    if serde_json::to_vec(&b.patch).map(|v| v.len()).unwrap_or(0) > 1 << 20 {
        return Err(Error::InvalidArgument("patch: at most 1 MiB".into()).into());
    }
    let n: (i64,) = sqlx::query_as(
        "select count(*) from musubi.config_variants where name = $1 and variant <> $2",
    )
    .bind(&name)
    .bind(&variant)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    if n.0 >= 32 {
        return Err(Error::ResourceExhausted("at most 32 variants per config".into()).into());
    }
    let sel = if b.selector.is_null() {
        json!({})
    } else {
        b.selector
    };
    let row: VariantRow = sqlx::query_as(
        "insert into musubi.config_variants (name, variant, selector, patch, priority, enabled)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (name, variant) do update set selector = excluded.selector,
             patch = excluded.patch, priority = excluded.priority, enabled = excluded.enabled,
             updated_at = now()
         returning name, variant, selector, patch, priority, enabled,
                   extract(epoch from updated_at)::bigint",
    )
    .bind(&name)
    .bind(&variant)
    .bind(&sel)
    .bind(&b.patch)
    .bind(b.priority)
    .bind(b.enabled)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "config.variant_updated",
            None,
            json!({ "name": name, "variant": variant }),
        )
        .await;
    Ok(Json(variant_doc(&row)))
}

async fn delete_variant(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((name, variant)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.config_variants where name = $1 and variant = $2")
        .bind(&name)
        .bind(&variant)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "deleted": n > 0 })))
}

/// Appends a history entry; called by `configs::put`/`delete`/CSV/rollback.
pub async fn record(
    s: &AppState,
    name: &str,
    version: i32,
    value: Option<&Value>,
    note: Option<&str>,
) {
    let _ = sqlx::query(
        "insert into musubi.config_history (name, version, value, note) values ($1, $2, $3, $4)",
    )
    .bind(name)
    .bind(version)
    .bind(value)
    .bind(note)
    .execute(&s.db)
    .await;
    let _ = sqlx::query(
        "delete from musubi.config_history where name = $1 and id not in
            (select id from musubi.config_history where name = $1 order by id desc limit 200)",
    )
    .bind(name)
    .execute(&s.db)
    .await;
}

type HistRow = (i64, i32, Option<Value>, Option<String>, i64);

fn hist_doc((id, version, value, note, at): HistRow, with_value: bool) -> Value {
    let mut v = json!({ "id": id, "version": version, "note": note, "created_at": at,
        "deleted": value.is_none(), "checksum": value.as_ref().map(checksum) });
    if with_value {
        v["value"] = value.unwrap_or(Value::Null);
    }
    v
}

#[derive(Deserialize)]
struct HistQ {
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    values: bool,
}

async fn history(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Query(q): Query<HistQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<HistRow> = sqlx::query_as(
        "select id, version, value, note, extract(epoch from created_at)::bigint
         from musubi.config_history where name = $1 order by id desc limit $2",
    )
    .bind(&name)
    .bind(q.limit.unwrap_or(50).clamp(1, 200))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "history": rows.into_iter().map(|r| hist_doc(r, q.values)).collect::<Vec<_>>() }),
    ))
}

async fn hist_value(s: &AppState, name: &str, id: i64) -> Result<Option<Value>, ApiError> {
    let row: Option<(Option<Value>,)> =
        sqlx::query_as("select value from musubi.config_history where name = $1 and id = $2")
            .bind(name)
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    match row {
        Some((v,)) => Ok(v),
        None => Err(Error::NotFound(format!("history entry {id} of `{name}`")).into()),
    }
}

async fn current_value(s: &AppState, name: &str) -> Result<Option<Value>, ApiError> {
    let row: Option<(Value,)> = sqlx::query_as("select value from musubi.configs where name = $1")
        .bind(name)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(row.map(|r| r.0))
}

#[derive(Deserialize)]
struct RollbackBody {
    id: i64,
    #[serde(default)]
    note: Option<String>,
}

/// Re-applies the value of a history entry as a new revision (schema-checked like a PUT).
async fn rollback(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<RollbackBody>,
) -> Result<Json<Value>, ApiError> {
    let Some(value) = hist_value(&s, &name, b.id).await? else {
        return Err(Error::InvalidArgument(format!("history entry {} is a deletion", b.id)).into());
    };
    let note = b.note.unwrap_or_else(|| format!("rollback to #{}", b.id));
    let row = crate::http::configs::store(&s, &name, &value, Some(&note)).await?;
    Ok(Json(row))
}

#[derive(Deserialize)]
struct DiffQ {
    from: Option<i64>,
    to: Option<i64>,
}

/// `?from=<history id>&to=<history id>`; `to` defaults to the live value, `from` to the
/// entry just before `to` (or the previous revision when `to` is live).
async fn diff_route(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Query(q): Query<DiffQ>,
) -> Result<Json<Value>, ApiError> {
    let to = match q.to {
        Some(id) => hist_value(&s, &name, id).await?,
        None => current_value(&s, &name).await?,
    };
    let from = match q.from {
        Some(id) => hist_value(&s, &name, id).await?,
        None => {
            let anchor = q.to;
            let row: Option<(Option<Value>,)> = match anchor {
                Some(id) => sqlx::query_as(
                    "select value from musubi.config_history where name = $1 and id < $2 order by id desc limit 1",
                )
                .bind(&name)
                .bind(id)
                .fetch_optional(&s.db)
                .await
                .map_err(Error::internal)?,
                None => sqlx::query_as(
                    "select value from musubi.config_history where name = $1 order by id desc offset 1 limit 1",
                )
                .bind(&name)
                .fetch_optional(&s.db)
                .await
                .map_err(Error::internal)?,
            };
            row.and_then(|r| r.0)
        }
    };
    let (a, b) = (from.unwrap_or(Value::Null), to.unwrap_or(Value::Null));
    let mut changes = vec![];
    diff(&a, &b, "", &mut changes);
    Ok(Json(
        json!({ "name": name, "from": q.from, "to": q.to, "changes": changes,
        "from_checksum": checksum(&a), "to_checksum": checksum(&b) }),
    ))
}

#[derive(Deserialize)]
struct CsvQ {
    key: Option<String>,
    note: Option<String>,
}

/// `PUT text/csv` → the config becomes an array of row objects, or an object keyed by `?key=col`.
async fn put_csv(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(name): Path<String>,
    Query(q): Query<CsvQ>,
    body: String,
) -> Result<Json<Value>, ApiError> {
    if body.len() > 4 << 20 {
        return Err(Error::InvalidArgument("csv: at most 4 MiB".into()).into());
    }
    let value = csv_to_value(&body, q.key.as_deref())?;
    let note = q.note.unwrap_or_else(|| "csv import".into());
    let row = crate::http::configs::store(&s, &name, &value, Some(&note)).await?;
    Ok(Json(row))
}

// ---- client version gates -------------------------------------------------------------------

type CvRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Value,
    Option<String>,
    i64,
    Value,
);

fn cv_doc((platform, min, latest, store, redirects, message, at, logic): &CvRow) -> Value {
    json!({ "platform": platform, "min_version": min, "latest_version": latest, "store_url": store,
        "redirects": redirects, "message": message, "updated_at": at, "logic_rules": logic })
}

/// Per logic (protocol) version: the client patch range a platform still serves. A client whose
/// logic version has no rule is retired when any rule exists; below `min_patch` it must hotfix.
#[derive(Deserialize, serde::Serialize, Clone)]
pub struct LogicRule {
    pub logic_version: i64,
    pub min_patch: String,
    #[serde(default)]
    pub latest_patch: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

fn validate_logic_rules(rules: &[LogicRule]) -> Result<(), Error> {
    if rules.len() > 32 {
        return Err(Error::InvalidArgument("at most 32 logic rules".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for l in rules {
        if l.logic_version < 0 || !seen.insert(l.logic_version) {
            return Err(Error::InvalidArgument(
                "logic_rules: logic_version must be unique and >= 0".into(),
            ));
        }
        if l.min_patch.trim().is_empty() || l.min_patch.len() > 64 {
            return Err(Error::InvalidArgument(
                "logic_rules: min_patch required".into(),
            ));
        }
        if let Some(lp) = &l.latest_patch {
            if cmp_version(&l.min_patch, lp) == Ordering::Greater {
                return Err(Error::InvalidArgument(format!(
                    "logic {}: min_patch above latest_patch",
                    l.logic_version
                )));
            }
        }
    }
    Ok(())
}

/// What a client at (`logic_version`, `version`) should do under `rules`.
fn logic_gate(rules: &[LogicRule], logic_version: i64, version: &str) -> Value {
    if rules.is_empty() {
        return json!({ "logic_version": logic_version, "logic_supported": true, "patch_required": false,
            "patch_available": false, "logic_configured": false });
    }
    let Some(rule) = rules.iter().find(|l| l.logic_version == logic_version) else {
        let newest = rules.iter().map(|l| l.logic_version).max();
        return json!({ "logic_version": logic_version, "logic_supported": false, "patch_required": false,
            "patch_available": false, "logic_configured": true, "newest_logic_version": newest,
            "logic_message": Value::Null });
    };
    let patch_required = cmp_version(version, &rule.min_patch) == Ordering::Less;
    let patch_available = rule
        .latest_patch
        .as_deref()
        .map(|lp| cmp_version(version, lp) == Ordering::Less)
        .unwrap_or(false);
    json!({ "logic_version": logic_version, "logic_supported": true, "patch_required": patch_required,
        "patch_available": patch_available, "logic_configured": true, "min_patch": rule.min_patch,
        "latest_patch": rule.latest_patch, "logic_message": rule.message })
}

#[derive(Deserialize)]
struct CvBody {
    min_version: Option<String>,
    latest_version: Option<String>,
    store_url: Option<String>,
    #[serde(default)]
    redirects: Vec<Redirect>,
    message: Option<String>,
    /// Per logic version patch requirements (hotfix workflow: bump `min_patch`).
    #[serde(default)]
    logic_rules: Vec<LogicRule>,
}

#[derive(Deserialize, serde::Serialize, Clone)]
struct Redirect {
    min_version: String,
    url: String,
    #[serde(default)]
    note: Option<String>,
}

async fn put_client_version(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(platform): Path<String>,
    Json(b): Json<CvBody>,
) -> Result<Json<Value>, ApiError> {
    let platform = platform.to_ascii_lowercase();
    if platform.is_empty() || platform.len() > 32 {
        return Err(Error::InvalidArgument("platform: 1..32 chars".into()).into());
    }
    if b.redirects.len() > 16 {
        return Err(Error::InvalidArgument("at most 16 redirects".into()).into());
    }
    for r in &b.redirects {
        if r.min_version.is_empty() || r.url.is_empty() {
            return Err(
                Error::InvalidArgument("redirect: min_version and url required".into()).into(),
            );
        }
    }
    if let (Some(min), Some(latest)) = (&b.min_version, &b.latest_version) {
        if cmp_version(min, latest) == Ordering::Greater {
            return Err(Error::InvalidArgument("min_version above latest_version".into()).into());
        }
    }
    validate_logic_rules(&b.logic_rules)?;
    let row: CvRow = sqlx::query_as(
        "insert into musubi.client_versions (platform, min_version, latest_version, store_url, redirects, message, logic_rules)
         values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (platform) do update set min_version = excluded.min_version,
             latest_version = excluded.latest_version, store_url = excluded.store_url,
             redirects = excluded.redirects, message = excluded.message,
             logic_rules = excluded.logic_rules, updated_at = now()
         returning platform, min_version, latest_version, store_url, redirects, message,
                   extract(epoch from updated_at)::bigint, logic_rules",
    )
    .bind(&platform)
    .bind(&b.min_version)
    .bind(&b.latest_version)
    .bind(&b.store_url)
    .bind(serde_json::to_value(&b.redirects).unwrap_or(json!([])))
    .bind(&b.message)
    .bind(serde_json::to_value(&b.logic_rules).unwrap_or(json!([])))
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "client_version.updated",
            None,
            json!({ "platform": platform }),
        )
        .await;
    Ok(Json(cv_doc(&row)))
}

async fn delete_client_version(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(platform): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.client_versions where platform = $1")
        .bind(platform.to_ascii_lowercase())
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "deleted": n > 0 })))
}

async fn list_client_versions(
    State(s): State<AppState>,
    _k: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<CvRow> = sqlx::query_as(
        "select platform, min_version, latest_version, store_url, redirects, message,
                extract(epoch from updated_at)::bigint, logic_rules
         from musubi.client_versions order by platform",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "platforms": rows.iter().map(cv_doc).collect::<Vec<_>>() }),
    ))
}

/// Pure decision: supported / update required / redirect (highest `min_version` ≤ client wins).
fn gate(min: Option<&str>, latest: Option<&str>, redirects: &[Redirect], version: &str) -> Value {
    let update_required = min
        .map(|m| cmp_version(version, m) == Ordering::Less)
        .unwrap_or(false);
    let update_available = latest
        .map(|l| cmp_version(version, l) == Ordering::Less)
        .unwrap_or(false);
    let redirect = redirects
        .iter()
        .filter(|r| cmp_version(version, &r.min_version) != Ordering::Less)
        .max_by(|a, b| cmp_version(&a.min_version, &b.min_version))
        .map(|r| json!({ "url": r.url, "min_version": r.min_version, "note": r.note }));
    json!({
        "version": version,
        "supported": !update_required,
        "update_required": update_required,
        "update_available": update_available,
        "redirect": redirect,
    })
}

/// No token: a client's very first call. `platform` + `version` from the query or headers.
async fn client_version(
    State(s): State<AppState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    Query(q): Query<Ctx>,
) -> Result<Json<Value>, ApiError> {
    // logic (protocol) version: `?logic_version=` or `x-musubi-logic-version`
    let logic_version: Option<i64> = uri
        .query()
        .and_then(|qs| {
            qs.split('&').find_map(|kv| {
                kv.strip_prefix("logic_version=")
                    .and_then(|v| v.parse().ok())
            })
        })
        .or_else(|| {
            headers
                .get("x-musubi-logic-version")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok())
        });
    let ctx = crate::http::remote_config::enrich(&s, ctx_from(q, &headers)).await;
    let (Some(platform), Some(version)) = (ctx.platform, ctx.version) else {
        return Err(Error::InvalidArgument("platform and version required".into()).into());
    };
    let row: Option<CvRow> = sqlx::query_as(
        "select platform, min_version, latest_version, store_url, redirects, message,
                extract(epoch from updated_at)::bigint, logic_rules
         from musubi.client_versions where platform = $1",
    )
    .bind(platform.to_ascii_lowercase())
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(row) = row else {
        return Ok(Json(
            json!({ "platform": platform, "version": version, "supported": true,
            "update_required": false, "update_available": false, "redirect": null, "configured": false }),
        ));
    };
    let redirects: Vec<Redirect> = serde_json::from_value(row.4.clone()).unwrap_or_default();
    let mut v = gate(row.1.as_deref(), row.2.as_deref(), &redirects, &version);
    v["platform"] = json!(row.0);
    v["latest_version"] = json!(row.2);
    v["min_version"] = json!(row.1);
    v["store_url"] = json!(row.3);
    v["message"] = json!(row.5);
    v["configured"] = json!(true);
    if let Some(lv) = logic_version {
        let rules: Vec<LogicRule> = serde_json::from_value(row.7.clone()).unwrap_or_default();
        let lg = logic_gate(&rules, lv, &version);
        let blocked = lg["logic_supported"] == false || lg["patch_required"] == true;
        if blocked {
            v["supported"] = json!(false);
            v["update_required"] = json!(true);
        }
        if lg["patch_available"] == true {
            v["update_available"] = json!(true);
        }
        v["logic"] = lg;
    }
    Ok(Json(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(cmp_version("1.2.10", "1.2.9"), Ordering::Greater);
        assert_eq!(cmp_version("1.2", "1.2.0"), Ordering::Equal);
        assert_eq!(cmp_version("2.0.0-beta", "2.0.0"), Ordering::Greater);
        assert_eq!(cmp_version("0.9", "1.0"), Ordering::Less);
    }

    #[test]
    fn selectors_and_patches() {
        let ctx = Ctx {
            platform: Some("iOS".into()),
            version: Some("1.4.2".into()),
            ..Default::default()
        };
        let sel = json!({"platforms": ["ios"], "min_version": "1.4", "max_version": "1.9", "environment": "prod"});
        assert!(selector_matches(&sel, "prod", &ctx, None));
        assert!(!selector_matches(&sel, "stage", &ctx, None));
        assert!(!selector_matches(
            &json!({"min_version": "1.5"}),
            "prod",
            &ctx,
            None
        ));
        assert!(!selector_matches(
            &json!({"rollout_pct": 50}),
            "prod",
            &ctx,
            None
        ));
        assert!(selector_matches(
            &json!({"rollout_pct": 50}),
            "prod",
            &ctx,
            Some(10)
        ));
        assert!(!selector_matches(
            &json!({"rollout_pct": 50}),
            "prod",
            &ctx,
            Some(70)
        ));
        let mut v = json!({"a": {"x": 1, "y": 2}, "b": [1, 2]});
        merge_patch(&mut v, &json!({"a": {"y": null, "z": 3}, "b": [9]}));
        assert_eq!(v, json!({"a": {"x": 1, "z": 3}, "b": [9]}));
        let mut d = vec![];
        diff(
            &json!({"a": 1, "b": {"c": 2}}),
            &json!({"a": 1, "b": {"c": 3}, "d": true}),
            "",
            &mut d,
        );
        assert_eq!(d.len(), 2);
        assert_eq!(d[0]["path"], "/b/c");
        assert_eq!(d[1]["op"], "add");
        assert_eq!(
            checksum(&json!({"b": 1, "a": 2})),
            checksum(&json!({"a": 2, "b": 1}))
        );
    }

    #[test]
    fn csv_rows_and_keys() {
        let text = "id,name,reward.gold,tags,enabled\n// comment\nsword,\"Iron, Sword\",10,\"[\"\"melee\"\"]\",true\nbow,Bow,,[],false\n";
        let v = csv_to_value(text, Some("id")).unwrap();
        assert_eq!(v["sword"]["name"], "Iron, Sword");
        assert_eq!(v["sword"]["reward"]["gold"], 10);
        assert_eq!(v["sword"]["tags"][0], "melee");
        assert_eq!(v["bow"]["enabled"], false);
        assert!(v["bow"].get("reward").is_none());
        let l = csv_to_value(text, None).unwrap();
        assert_eq!(l.as_array().unwrap().len(), 2);
        assert!(csv_to_value("id,name\na,b\na,c\n", Some("id")).is_err());
    }

    #[test]
    fn version_gate_redirects_newest_match() {
        let r = vec![
            Redirect {
                min_version: "2.0".into(),
                url: "https://v2".into(),
                note: None,
            },
            Redirect {
                min_version: "3.0".into(),
                url: "https://v3".into(),
                note: None,
            },
        ];
        let g = gate(Some("1.5"), Some("2.4"), &r, "1.2");
        assert_eq!(g["update_required"], true);
        assert!(g["redirect"].is_null());
        let g = gate(Some("1.5"), Some("2.4"), &r, "2.1");
        assert_eq!(g["supported"], true);
        assert_eq!(g["update_available"], true);
        assert_eq!(g["redirect"]["url"], "https://v2");
        let g = gate(Some("1.5"), Some("2.4"), &r, "3.1");
        assert_eq!(g["redirect"]["url"], "https://v3");
        assert_eq!(g["update_available"], false);
    }
}

#[derive(Deserialize)]
struct LogicRuleBody {
    min_patch: String,
    #[serde(default)]
    latest_patch: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

async fn load_logic_rules(s: &AppState, platform: &str) -> Result<Vec<LogicRule>, ApiError> {
    let row: Option<Value> =
        sqlx::query_scalar("select logic_rules from musubi.client_versions where platform = $1")
            .bind(platform)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    match row {
        Some(v) => Ok(serde_json::from_value(v).unwrap_or_default()),
        None => Err(Error::NotFound(format!("client_versions/{platform}")).into()),
    }
}

async fn save_logic_rules(
    s: &AppState,
    platform: &str,
    rules: &[LogicRule],
    logic_version: i64,
    action: &str,
) -> Result<Json<Value>, ApiError> {
    validate_logic_rules(rules)?;
    sqlx::query(
        "update musubi.client_versions set logic_rules = $2, updated_at = now() where platform = $1",
    )
    .bind(platform)
    .bind(serde_json::to_value(rules).unwrap_or(json!([])))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "client_version.updated",
            None,
            json!({ "platform": platform, "logic_version": logic_version, "action": action }),
        )
        .await;
    Ok(Json(json!({ "platform": platform, "logic_rules": rules })))
}

/// Hotfix workflow: `PUT /v1/server/client_versions/{platform}/logic/{lv} {min_patch, latest_patch,
/// message}` adds or replaces one logic version's patch requirement without resending the rest.
async fn put_logic_rule(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((platform, logic_version)): Path<(String, i64)>,
    Json(b): Json<LogicRuleBody>,
) -> Result<Json<Value>, ApiError> {
    let platform = platform.to_ascii_lowercase();
    let mut rules = load_logic_rules(&s, &platform).await?;
    rules.retain(|l| l.logic_version != logic_version);
    rules.push(LogicRule {
        logic_version,
        min_patch: b.min_patch,
        latest_patch: b.latest_patch,
        message: b.message,
    });
    rules.sort_by_key(|l| l.logic_version);
    save_logic_rules(&s, &platform, &rules, logic_version, "upsert").await
}

/// Retire a logic version: clients on it get `logic_supported: false`.
async fn delete_logic_rule(
    State(s): State<AppState>,
    _k: ServerKey,
    Path((platform, logic_version)): Path<(String, i64)>,
) -> Result<Json<Value>, ApiError> {
    let platform = platform.to_ascii_lowercase();
    let mut rules = load_logic_rules(&s, &platform).await?;
    let before = rules.len();
    rules.retain(|l| l.logic_version != logic_version);
    if rules.len() == before {
        return Err(Error::NotFound(format!("logic version {logic_version}")).into());
    }
    save_logic_rules(&s, &platform, &rules, logic_version, "delete").await
}
