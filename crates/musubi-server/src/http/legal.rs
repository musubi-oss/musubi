//! Legal documents and consents (P0-49): operators publish versioned ToS / privacy / EULA /
//! marketing texts per locale (and optionally per country); players read what applies to them,
//! accept or decline; mandatory documents gate the account after their grace period.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/legal", get(public_docs))
        .route("/v1/legal/{key}", get(public_doc))
        .route("/v1/account/consents", get(my_consents).post(consent))
        .route("/v1/account/consents/{key}", delete(withdraw))
        .route("/v1/admin/legal", get(admin_list))
        .route("/v1/admin/legal/{key}/{version}", put(upsert))
        .route("/v1/admin/legal/{key}/{version}/retire", post(retire))
        .route("/v1/admin/legal/{key}/consents", get(consents_of_doc))
        .route("/v1/admin/accounts/{id}/consents", get(consents_of_account))
}

#[derive(sqlx::FromRow)]
struct Doc {
    key: String,
    version: i32,
    locale: String,
    title: String,
    url: String,
    body: String,
    mandatory: bool,
    countries: Vec<String>,
    grace_secs: i64,
    published: bool,
    published_at: Option<OffsetDateTime>,
    retired_at: Option<OffsetDateTime>,
    updated_at: OffsetDateTime,
}

const COLS: &str = "key, version, locale, title, url, body, mandatory, countries, grace_secs, published, published_at, retired_at, updated_at";

fn doc_json(d: &Doc, with_body: bool) -> Value {
    let mut v = json!({
        "key": d.key, "version": d.version, "locale": d.locale, "title": d.title, "url": d.url,
        "mandatory": d.mandatory, "countries": d.countries, "grace_secs": d.grace_secs,
        "published": d.published, "published_at": d.published_at.map(|t| t.unix_timestamp()),
        "retired_at": d.retired_at.map(|t| t.unix_timestamp()), "updated_at": d.updated_at.unix_timestamp(),
    });
    if with_body {
        v["body"] = json!(d.body);
    }
    v
}

fn valid_key(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 64
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Latest published version of every key for a locale (falls back to `en`, then any locale),
/// filtered by country when given.
async fn current(
    s: &AppState,
    locale: &str,
    country: Option<&str>,
    only_key: Option<&str>,
) -> Result<Vec<Doc>> {
    let rows: Vec<Doc> = sqlx::query_as(&format!(
        "select distinct on (key) {COLS} from musubi.legal_documents
         where published and retired_at is null and ($1::text is null or key = $1)
           and version = (select max(version) from musubi.legal_documents d2
                          where d2.key = legal_documents.key and d2.published and d2.retired_at is null)
         order by key, (locale = $2) desc, (locale = 'en') desc, locale"
    ))
    .bind(only_key)
    .bind(locale)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .filter(|d| match country {
            Some(c) => {
                d.countries.is_empty() || d.countries.iter().any(|x| x.eq_ignore_ascii_case(c))
            }
            None => true,
        })
        .collect())
}

#[derive(Deserialize, Default)]
struct LocaleQ {
    locale: Option<String>,
    country: Option<String>,
}

/// `GET /v1/legal?locale&country` — no token: what a new player has to read before signing up.
async fn public_docs(
    State(s): State<AppState>,
    Query(q): Query<LocaleQ>,
) -> Result<Json<Value>, ApiError> {
    let docs = current(
        &s,
        q.locale.as_deref().unwrap_or("en"),
        q.country.as_deref(),
        None,
    )
    .await?;
    Ok(Json(json!({
        "documents": docs.iter().map(|d| doc_json(d, false)).collect::<Vec<_>>(),
    })))
}

/// `GET /v1/legal/{key}?locale` — full text of the current version.
async fn public_doc(
    State(s): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<LocaleQ>,
) -> Result<Json<Value>, ApiError> {
    let docs = current(
        &s,
        q.locale.as_deref().unwrap_or("en"),
        q.country.as_deref(),
        Some(&key),
    )
    .await?;
    let d = docs
        .first()
        .ok_or_else(|| Error::NotFound(format!("legal document {key}")))?;
    Ok(Json(doc_json(d, true)))
}

/// `GET /v1/account/consents?locale&country` — pending (with `enforced` / `required_from`),
/// optional (non-mandatory) documents with the player's choice, and the full history.
async fn my_consents(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<LocaleQ>,
) -> Result<Json<Value>, ApiError> {
    let acc = me.account_id;
    let profile: Option<(String, String)> =
        sqlx::query_as("select country, locale from musubi.legal_profiles where account_id = $1")
            .bind(acc.0)
            .fetch_optional(&s.db)
            .await
            .map_err(Error::internal)?;
    let (p_country, p_locale) = profile.unwrap_or_default();
    let country = q.country.clone().unwrap_or(p_country);
    let locale = q.locale.clone().unwrap_or(if p_locale.is_empty() {
        "en".into()
    } else {
        p_locale
    });
    let docs = current(
        &s,
        &locale,
        (!country.is_empty()).then_some(country.as_str()),
        None,
    )
    .await?;
    let history: Vec<(String, i32, bool, OffsetDateTime)> = sqlx::query_as(
        "select key, version, accepted, at from musubi.legal_consents where account_id = $1 order by at desc",
    )
    .bind(acc.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let pending = s
        .legal
        .pending(
            &s.db,
            acc,
            (!country.is_empty()).then_some(country.as_str()),
        )
        .await?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut pending_out = Vec::new();
    let mut optional = Vec::new();
    for d in &docs {
        let decision = history
            .iter()
            .find(|h| h.0 == d.key && h.1 == d.version)
            .map(|h| h.2);
        if d.mandatory {
            if let Some((r, enforced)) = pending.iter().find(|(r, _)| r.key == d.key) {
                let mut v = doc_json(d, false);
                v["enforced"] = json!(*enforced);
                v["required_from"] = json!(r.required_from);
                v["seconds_left"] = json!((r.required_from - now).max(0));
                pending_out.push(v);
            }
        } else {
            let mut v = doc_json(d, false);
            v["accepted"] = decision.map_or(Value::Null, Value::Bool);
            optional.push(v);
        }
    }
    Ok(Json(json!({
        "country": country, "locale": locale,
        "pending": pending_out, "optional": optional,
        "history": history.iter().map(|(k, v, a, at)| json!({
            "key": k, "version": v, "accepted": a, "at": at.unix_timestamp() })).collect::<Vec<_>>(),
    })))
}

#[derive(Deserialize)]
struct Choice {
    key: String,
    version: i32,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ConsentBody {
    accept: Vec<Choice>,
    decline: Vec<Choice>,
    /// ISO country / locale the player declared; remembered for per-country documents.
    country: Option<String>,
    locale: Option<String>,
}

/// `POST /v1/account/consents {accept: [{key, version}], decline: [{key, version}], country?, locale?}`.
async fn consent(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    headers: HeaderMap,
    Json(b): Json<ConsentBody>,
) -> Result<Json<Value>, ApiError> {
    let acc = me.account_id;
    if b.accept.len() + b.decline.len() > 64 {
        return Err(Error::InvalidArgument("too many choices".into()).into());
    }
    if b.country.is_some() || b.locale.is_some() {
        sqlx::query(
            "insert into musubi.legal_profiles (account_id, country, locale) values ($1, $2, $3)
             on conflict (account_id) do update set
                country = case when $2 = '' then legal_profiles.country else $2 end,
                locale = case when $3 = '' then legal_profiles.locale else $3 end,
                updated_at = now()",
        )
        .bind(acc.0)
        .bind(b.country.clone().unwrap_or_default().to_uppercase())
        .bind(b.locale.clone().unwrap_or_default())
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    let ua = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(256)
        .collect::<String>();
    let mut recorded = Vec::new();
    for (choice, accepted) in b
        .accept
        .iter()
        .map(|c| (c, true))
        .chain(b.decline.iter().map(|c| (c, false)))
    {
        let doc: Option<(bool,)> = sqlx::query_as(
            "select bool_or(mandatory) from musubi.legal_documents where key = $1 and version = $2 and published
             group by key",
        )
        .bind(&choice.key)
        .bind(choice.version)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
        let Some((mandatory,)) = doc else {
            return Err(Error::NotFound(format!(
                "legal document {} v{}",
                choice.key, choice.version
            ))
            .into());
        };
        if mandatory && !accepted {
            return Err(Error::InvalidArgument(format!(
                "{} is mandatory and cannot be declined",
                choice.key
            ))
            .into());
        }
        sqlx::query(
            "insert into musubi.legal_consents (account_id, key, version, accepted, locale, country, ip, user_agent)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             on conflict (account_id, key, version) do update set accepted = excluded.accepted,
                locale = excluded.locale, country = excluded.country, ip = excluded.ip,
                user_agent = excluded.user_agent, at = now()",
        )
        .bind(acc.0)
        .bind(&choice.key)
        .bind(choice.version)
        .bind(accepted)
        .bind(b.locale.clone().unwrap_or_default())
        .bind(b.country.clone().unwrap_or_default().to_uppercase())
        .bind(&ip)
        .bind(&ua)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
        recorded
            .push(json!({ "key": choice.key, "version": choice.version, "accepted": accepted }));
        s.events
            .emit(
                "legal.consented",
                Some(acc),
                json!({ "key": choice.key, "version": choice.version, "accepted": accepted }),
            )
            .await;
    }
    s.legal
        .invalidate(Some(acc), s.cluster.as_ref(), &s.db)
        .await;
    let pending = s.legal.pending(&s.db, acc, None).await?;
    Ok(Json(json!({
        "recorded": recorded,
        "pending": pending.iter().map(|(r, e)| json!({ "key": r.key, "version": r.version, "enforced": e })).collect::<Vec<_>>(),
    })))
}

/// `DELETE /v1/account/consents/{key}` — withdraw a non-mandatory consent (marketing …).
async fn withdraw(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(key): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let mandatory: Option<(bool,)> = sqlx::query_as(
        "select bool_or(mandatory) from musubi.legal_documents where key = $1 and published and retired_at is null group by key",
    )
    .bind(&key)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    if mandatory.is_some_and(|m| m.0) {
        return Err(Error::Conflict(format!(
            "{key} is mandatory; delete the account to withdraw"
        ))
        .into());
    }
    let n = sqlx::query(
        "update musubi.legal_consents set accepted = false, at = now() where account_id = $1 and key = $2 and accepted",
    )
    .bind(me.account_id.0)
    .bind(&key)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    s.legal
        .invalidate(Some(me.account_id), s.cluster.as_ref(), &s.db)
        .await;
    s.events
        .emit(
            "legal.withdrawn",
            Some(me.account_id),
            json!({ "key": key, "versions": n }),
        )
        .await;
    Ok(Json(json!({ "key": key, "withdrawn": n })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct UpsertBody {
    locale: Option<String>,
    title: String,
    url: String,
    body: String,
    mandatory: Option<bool>,
    countries: Vec<String>,
    grace_secs: Option<i64>,
    /// Publish now (players see it; mandatory ones start gating after `grace_secs`).
    published: Option<bool>,
}

/// `PUT /v1/admin/legal/{key}/{version} {locale?, title, url?, body?, mandatory, countries, grace_secs, published}`.
async fn upsert(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((key, version)): Path<(String, i32)>,
    Json(b): Json<UpsertBody>,
) -> Result<Json<Value>, ApiError> {
    if !valid_key(&key) {
        return Err(Error::InvalidArgument("key must be [a-z0-9_-]{1,64}".into()).into());
    }
    if version < 1 {
        return Err(Error::InvalidArgument("version starts at 1".into()).into());
    }
    if b.title.trim().is_empty() {
        return Err(Error::InvalidArgument("title is required".into()).into());
    }
    if b.body.len() > 1_000_000 {
        return Err(Error::InvalidArgument("body too large (1 MB)".into()).into());
    }
    let locale = b
        .locale
        .clone()
        .unwrap_or_else(|| "en".into())
        .to_lowercase();
    let countries: Vec<String> = b.countries.iter().map(|c| c.to_uppercase()).collect();
    let d: Doc = sqlx::query_as(&format!(
        "insert into musubi.legal_documents (key, version, locale, title, url, body, mandatory, countries, grace_secs, published, published_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, case when $10 then now() end)
         on conflict (key, version, locale) do update set title = excluded.title, url = excluded.url,
            body = excluded.body, mandatory = excluded.mandatory, countries = excluded.countries,
            grace_secs = excluded.grace_secs, published = excluded.published,
            published_at = case when excluded.published then coalesce(legal_documents.published_at, now()) else legal_documents.published_at end,
            retired_at = null, updated_at = now()
         returning {COLS}"
    ))
    .bind(&key)
    .bind(version)
    .bind(&locale)
    .bind(b.title.trim())
    .bind(&b.url)
    .bind(&b.body)
    .bind(b.mandatory.unwrap_or(true))
    .bind(&countries)
    .bind(b.grace_secs.unwrap_or(0).max(0))
    .bind(b.published.unwrap_or(false))
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    s.legal.invalidate(None, s.cluster.as_ref(), &s.db).await;
    if d.published {
        s.events
            .emit(
                "legal.published",
                None,
                json!({ "key": key, "version": version, "locale": locale, "mandatory": d.mandatory }),
            )
            .await;
    }
    Ok(Json(doc_json(&d, false)))
}

/// `POST /v1/admin/legal/{key}/{version}/retire` — all locales of that version stop applying.
async fn retire(
    _k: ServerKey,
    State(s): State<AppState>,
    Path((key, version)): Path<(String, i32)>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query(
        "update musubi.legal_documents set retired_at = now(), updated_at = now() where key = $1 and version = $2 and retired_at is null",
    )
    .bind(&key)
    .bind(version)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("legal document {key} v{version}")).into());
    }
    s.legal.invalidate(None, s.cluster.as_ref(), &s.db).await;
    Ok(Json(
        json!({ "key": key, "version": version, "retired": n }),
    ))
}

#[derive(Deserialize, Default)]
struct AdminListQ {
    key: Option<String>,
}

/// `GET /v1/admin/legal?key` — every version and locale, with acceptance counts.
async fn admin_list(
    _k: ServerKey,
    State(s): State<AppState>,
    Query(q): Query<AdminListQ>,
) -> Result<Json<Value>, ApiError> {
    let docs: Vec<Doc> = sqlx::query_as(&format!(
        "select {COLS} from musubi.legal_documents where ($1::text is null or key = $1) order by key, version desc, locale"
    ))
    .bind(q.key.as_deref())
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let counts: Vec<(String, i32, i64, i64)> = sqlx::query_as(
        "select key, version, count(*) filter (where accepted), count(*) filter (where not accepted)
         from musubi.legal_consents group by key, version",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let out: Vec<Value> = docs
        .iter()
        .map(|d| {
            let mut v = doc_json(d, false);
            let c = counts.iter().find(|c| c.0 == d.key && c.1 == d.version);
            v["accepted_count"] = json!(c.map_or(0, |c| c.2));
            v["declined_count"] = json!(c.map_or(0, |c| c.3));
            v
        })
        .collect();
    Ok(Json(
        json!({ "documents": out, "required": s.legal.required() }),
    ))
}

#[derive(Deserialize, Default)]
struct ConsentsQ {
    version: Option<i32>,
    accepted: Option<bool>,
    limit: Option<i64>,
    before: Option<i64>,
}

type ConsentRow = (
    Uuid,
    String,
    i32,
    bool,
    String,
    String,
    String,
    String,
    OffsetDateTime,
);

fn consent_json((acc, key, version, accepted, locale, country, ip, ua, at): &ConsentRow) -> Value {
    json!({ "account_id": acc, "key": key, "version": version, "accepted": accepted, "locale": locale,
        "country": country, "ip": ip, "user_agent": ua, "at": at.unix_timestamp() })
}

/// `GET /v1/admin/legal/{key}/consents?version&accepted&limit&before` — audit feed per document.
async fn consents_of_doc(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(key): Path<String>,
    Query(q): Query<ConsentsQ>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<ConsentRow> = sqlx::query_as(
        "select account_id, key, version, accepted, locale, country, ip, user_agent, at from musubi.legal_consents
         where key = $1 and ($2::int is null or version = $2) and ($3::bool is null or accepted = $3)
           and ($4::bigint is null or at < to_timestamp($4))
         order by at desc limit $5",
    )
    .bind(&key)
    .bind(q.version)
    .bind(q.accepted)
    .bind(q.before)
    .bind(q.limit.unwrap_or(100).clamp(1, 1000))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "consents": rows.iter().map(consent_json).collect::<Vec<_>>() }),
    ))
}

/// `GET /v1/admin/accounts/{id}/consents` — one player's consent history (support, GDPR export).
async fn consents_of_account(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let rows: Vec<ConsentRow> = sqlx::query_as(
        "select account_id, key, version, accepted, locale, country, ip, user_agent, at from musubi.legal_consents
         where account_id = $1 order by at desc",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let pending = s.legal.pending(&s.db, AccountId(id), None).await?;
    Ok(Json(json!({
        "consents": rows.iter().map(consent_json).collect::<Vec<_>>(),
        "pending": pending.iter().map(|(r, e)| json!({ "key": r.key, "version": r.version, "enforced": e })).collect::<Vec<_>>(),
    })))
}
