//! Push beyond "offline notification → push": reusable templates with `{{var}}` placeholders,
//! sends to a list or to everyone with a token, and a schedule — at a UTC instant or at a
//! wall-clock time in each recipient's own timezone (`accounts.timezone`, IANA name).

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/server/push/templates",
            get(templates).put(put_templates),
        )
        .route(
            "/v1/server/push/templates/{id}",
            axum::routing::delete(delete_template),
        )
        .route("/v1/server/push", post(send))
        .route("/v1/server/push/schedule", get(schedule))
        .route(
            "/v1/server/push/schedule/{id}",
            axum::routing::delete(cancel),
        )
}

const MAX_RECIPIENTS: usize = 10_000;

#[derive(Deserialize)]
struct Template {
    id: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    code: i32,
}

#[derive(Deserialize)]
struct PutTemplates {
    templates: Vec<Template>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

async fn put_templates(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<PutTemplates>,
) -> Result<Json<Value>, ApiError> {
    for t in &r.templates {
        if !valid_id(&t.id) {
            return Err(Error::InvalidArgument(format!("template id {:?}", t.id)).into());
        }
        if t.title.trim().is_empty()
            || t.title.chars().count() > 256
            || t.body.chars().count() > 2048
        {
            return Err(
                Error::InvalidArgument("title 1..=256 chars, body ≤ 2048 chars".into()).into(),
            );
        }
        if !(t.data.is_object() || t.data.is_null()) {
            return Err(Error::InvalidArgument("data must be an object".into()).into());
        }
    }
    for t in &r.templates {
        let data = if t.data.is_null() {
            json!({})
        } else {
            t.data.clone()
        };
        sqlx::query(
            "insert into musubi.push_templates (id, title, body, data, code) values ($1, $2, $3, $4, $5)
             on conflict (id) do update set title = excluded.title, body = excluded.body,
                 data = excluded.data, code = excluded.code, updated_at = now()",
        )
        .bind(&t.id)
        .bind(&t.title)
        .bind(&t.body)
        .bind(data)
        .bind(t.code)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    templates(_k, State(s)).await
}

async fn templates(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<(String, String, String, Value, i32)> =
        sqlx::query_as("select id, title, body, data, code from musubi.push_templates order by id")
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
    Ok(Json(json!({
        "templates": rows.into_iter().map(|(id, title, body, data, code)| {
            json!({ "id": id, "title": title, "body": body, "data": data, "code": code })
        }).collect::<Vec<_>>()
    })))
}

async fn delete_template(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.push_templates where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "deleted": n })))
}

#[derive(Deserialize)]
struct SendReq {
    #[serde(default)]
    template_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    code: Option<i32>,
    /// `{{name}}` values for title/body/data; `{{username}}` is always available.
    #[serde(default)]
    vars: Value,
    /// Recipients; absent = everyone with a push token.
    #[serde(default)]
    account_ids: Option<Vec<Uuid>>,
    /// Send later, at this unix time (UTC).
    #[serde(default)]
    send_at: Option<i64>,
    /// Send at this wall-clock time in each recipient's own timezone.
    #[serde(default)]
    local_date: Option<String>,
    #[serde(default)]
    local_time: Option<String>,
}

/// Fill `{{var}}` placeholders in a string.
fn render(s: &str, vars: &serde_json::Map<String, Value>) -> String {
    let mut out = s.to_string();
    for (k, v) in vars {
        let val = match v {
            Value::String(x) => x.clone(),
            other => other.to_string(),
        };
        out = out.replace(&format!("{{{{{k}}}}}"), &val);
    }
    out
}

fn render_value(v: &Value, vars: &serde_json::Map<String, Value>) -> Value {
    match v {
        Value::String(s) => Value::String(render(s, vars)),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), render_value(x, vars)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(|x| render_value(x, vars)).collect()),
        other => other.clone(),
    }
}

/// Title/body/data/code after merging the template (if any) with the request.
async fn resolve(
    s: &AppState,
    template_id: Option<&str>,
    title: Option<&str>,
    body: Option<&str>,
    data: &Value,
    code: Option<i32>,
) -> Result<(String, String, Value, i32)> {
    let (mut t, mut b, mut d, mut c) = (String::new(), String::new(), json!({}), 0);
    if let Some(id) = template_id {
        let row: Option<(String, String, Value, i32)> = sqlx::query_scalar::<_, Value>(
            "select json_build_array(title, body, data, code) from musubi.push_templates where id = $1",
        )
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?
        .map(|v| serde_json::from_value(v).map_err(Error::internal))
        .transpose()?;
        let Some((tt, bb, dd, cc)) = row else {
            return Err(Error::NotFound(format!("push template {id}")));
        };
        (t, b, d, c) = (tt, bb, dd, cc);
    }
    if let Some(x) = title {
        t = x.to_string();
    }
    if let Some(x) = body {
        b = x.to_string();
    }
    if let Some(o) = data.as_object() {
        let dm = d.as_object_mut().expect("object");
        for (k, v) in o {
            dm.insert(k.clone(), v.clone());
        }
    }
    if let Some(x) = code {
        c = x;
    }
    if t.trim().is_empty() || t.chars().count() > 256 {
        return Err(Error::InvalidArgument("title 1..=256 chars".into()));
    }
    Ok((t, b, d, c))
}

/// Send now, or file it in the schedule.
async fn send(
    _k: ServerKey,
    State(s): State<AppState>,
    Json(r): Json<SendReq>,
) -> Result<Json<Value>, ApiError> {
    let (title, body, data, code) = resolve(
        &s,
        r.template_id.as_deref(),
        r.title.as_deref(),
        r.body.as_deref(),
        &r.data,
        r.code,
    )
    .await?;
    if r.account_ids
        .as_ref()
        .is_some_and(|v| v.is_empty() || v.len() > MAX_RECIPIENTS)
    {
        return Err(Error::InvalidArgument(format!("1..={MAX_RECIPIENTS} account_ids")).into());
    }
    let vars = r.vars.as_object().cloned().unwrap_or_default();
    let local = match (&r.local_date, &r.local_time) {
        (Some(d), Some(t)) => {
            let date = time::Date::parse(
                d,
                &time::macros::format_description!("[year]-[month]-[day]"),
            )
            .map_err(|_| Error::InvalidArgument("local_date must be YYYY-MM-DD".into()))?;
            let tm = time::Time::parse(t, &time::macros::format_description!("[hour]:[minute]"))
                .map_err(|_| Error::InvalidArgument("local_time must be HH:MM".into()))?;
            Some((date, tm))
        }
        (None, None) => None,
        _ => {
            return Err(
                Error::InvalidArgument("local_date and local_time go together".into()).into(),
            )
        }
    };
    if r.send_at.is_none() && local.is_none() {
        let n = deliver(
            &s,
            r.account_ids.as_deref(),
            &title,
            &body,
            &data,
            code,
            &vars,
            None,
            None,
        )
        .await?;
        return Ok(Json(json!({ "sent_to": n })));
    }
    let id = Uuid::now_v7();
    // local-time sends become due when the earliest timezone (UTC+14) reaches that time
    let send_at = match (r.send_at, &local) {
        (Some(t), _) => OffsetDateTime::from_unix_timestamp(t)
            .map_err(|_| Error::InvalidArgument("bad send_at".into()))?,
        (None, Some((d, t))) => d.with_time(*t).assume_utc() - time::Duration::hours(14),
        _ => unreachable!(),
    };
    sqlx::query(
        "insert into musubi.push_schedule (id, account_ids, template_id, title, body, data, code, vars, send_at, local_date, local_time)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(id)
    .bind(r.account_ids.as_deref())
    .bind(r.template_id.as_deref())
    .bind(&title)
    .bind(&body)
    .bind(&data)
    .bind(code)
    .bind(Value::Object(vars))
    .bind(send_at)
    .bind(local.map(|l| l.0))
    .bind(local.map(|l| l.1))
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "id": id, "send_at": send_at.unix_timestamp(), "local": local.is_some() }),
    ))
}

/// Push `title`/`body`/`data` to `accounts` (None = everyone with a token). With `local`, only
/// recipients whose own clock has reached that wall time, skipping those already served for
/// `schedule`. Returns how many accounts were pushed to.
#[allow(clippy::too_many_arguments)]
async fn deliver(
    s: &AppState,
    accounts: Option<&[Uuid]>,
    title: &str,
    body: &str,
    data: &Value,
    code: i32,
    vars: &serde_json::Map<String, Value>,
    local: Option<(time::Date, time::Time)>,
    schedule: Option<Uuid>,
) -> Result<usize> {
    let Some(push) = s.notify.push().cloned() else {
        return Err(Error::Unavailable("push is not configured".into()));
    };
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        "select distinct a.id, a.username from musubi.accounts a
         join musubi.push_tokens p on p.account_id = a.id
         where a.deleted_at is null
           and ($1::uuid[] is null or a.id = any($1))
           and ($2::date is null or
                (($2::date + $3::time) at time zone coalesce(nullif(a.timezone, ''), 'UTC')) <= now())
           and ($4::uuid is null or not exists (
                select 1 from musubi.push_schedule_sent x where x.schedule_id = $4 and x.account_id = a.id))
         limit $5",
    )
    .bind(accounts)
    .bind(local.map(|l| l.0))
    .bind(local.map(|l| l.1))
    .bind(schedule)
    .bind(MAX_RECIPIENTS as i64)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut n = 0;
    for (id, username) in rows {
        let mut v = vars.clone();
        v.entry("username".to_string())
            .or_insert(Value::String(username));
        let mut d = render_value(data, &v);
        if !body.is_empty() {
            d["body"] = Value::String(render(body, &v));
        }
        let t = render(title, &v);
        if let Some(sid) = schedule {
            sqlx::query(
                "insert into musubi.push_schedule_sent (schedule_id, account_id) values ($1, $2) on conflict do nothing",
            )
            .bind(sid)
            .bind(id)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
        }
        let push = push.clone();
        tokio::spawn(async move {
            if let Err(e) = push.send(AccountId(id), &t, &d, code).await {
                tracing::warn!(error = %e, "push delivery failed");
            }
        });
        n += 1;
    }
    Ok(n)
}

async fn schedule(_k: ServerKey, State(s): State<AppState>) -> Result<Json<Value>, ApiError> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "select json_build_object('id', id, 'template_id', template_id, 'title', title,
                'recipients', coalesce(array_length(account_ids, 1), -1),
                'send_at', extract(epoch from send_at)::bigint,
                'local', local_date is not null,
                'sent_at', extract(epoch from sent_at)::bigint, 'sent_count', sent_count)
         from musubi.push_schedule
         where sent_at is null or sent_at > now() - interval '7 days'
         order by send_at desc limit 200",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({ "schedule": rows })))
}

async fn cancel(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.push_schedule where id = $1 and sent_at is null")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "cancelled": n })))
}

/// Lease holder, every ~30 s: push what is due. UTC rows go out at once; local-time rows are
/// drained timezone by timezone and closed when the last timezone (UTC-12) has passed the time.
pub async fn dispatch_due(s: &AppState) -> Result<usize> {
    type Row = (
        Uuid,
        Option<Vec<Uuid>>,
        String,
        String,
        Value,
        i32,
        Value,
        Option<time::Date>,
        Option<time::Time>,
    );
    let due: Vec<Row> = sqlx::query_as(
        "select id, account_ids, title, body, data, code, vars, local_date, local_time
         from musubi.push_schedule where sent_at is null and send_at <= now() order by send_at limit 50",
    )
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut total = 0;
    for (id, accounts, title, body, data, code, vars, ld, lt) in due {
        let vars = vars.as_object().cloned().unwrap_or_default();
        let local = ld.zip(lt);
        let n = match deliver(
            s,
            accounts.as_deref(),
            &title,
            &body,
            &data,
            code,
            &vars,
            local,
            Some(id),
        )
        .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(schedule = %id, error = %e, "scheduled push failed");
                continue;
            }
        };
        total += n;
        let done = match local {
            None => true,
            Some((d, t)) => {
                d.with_time(t).assume_utc() + time::Duration::hours(12) <= OffsetDateTime::now_utc()
            }
        };
        sqlx::query(
            "update musubi.push_schedule set sent_count = sent_count + $2, sent_at = case when $3 then now() else null end where id = $1",
        )
        .bind(id)
        .bind(n as i32)
        .bind(done)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    }
    Ok(total)
}
