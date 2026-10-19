//! Native LiveOps routes (see `crate::liveops`): flags, experiments, live events.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};
use crate::liveops::{self, Experiment, Flag, LiveEvent};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/flags", get(list_flags).post(post_flag))
        .route(
            "/v1/server/flags/{name}",
            get(get_flag).put(put_flag).delete(delete_flag),
        )
        .route(
            "/v1/server/experiments",
            get(list_experiments).post(post_experiment),
        )
        .route(
            "/v1/server/experiments/{id}",
            get(get_experiment)
                .put(put_experiment)
                .delete(delete_experiment),
        )
        .route("/v1/server/experiments/{id}/start", post(start_experiment))
        .route("/v1/server/experiments/{id}/pause", post(pause_experiment))
        .route("/v1/server/experiments/{id}/end", post(end_experiment))
        .route(
            "/v1/server/experiments/{id}/results",
            get(experiment_results),
        )
        .route(
            "/v1/server/experiments/{id}/assignments",
            get(experiment_assignments),
        )
        .route("/v1/server/live_events", get(list_events).post(post_event))
        .route("/v1/server/liveops/timeline", get(timeline))
        .route(
            "/v1/server/live_events/from_template",
            post(event_from_template),
        )
        .route(
            "/v1/server/live_events/{id}/participants",
            get(event_participants),
        )
        .route("/v1/server/live_event_templates", get(list_templates))
        .route(
            "/v1/server/live_event_templates/{id}",
            get(get_template).put(put_template).delete(delete_template),
        )
        .route(
            "/v1/server/live_events/{id}",
            get(get_event).put(put_event).delete(delete_event),
        )
        .route("/v1/server/liveops/eval", get(eval_for))
        .route("/v1/liveops/experiments", get(my_experiments))
        .route("/v1/liveops/experiments/{id}/outcome", post(outcome))
}

fn valid_id(id: &str, what: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return Err(Error::InvalidArgument(format!(
            "{what}: [A-Za-z0-9_.-]{{1,64}}"
        )));
    }
    Ok(())
}

async fn changed(s: &AppState, what: &str, id: &str) {
    crate::runtime_host::liveops_changed(s, json!({ "source": "musubi", what: id }), true).await;
}

// ---------------------------------------------------------------- flags

#[derive(Deserialize)]
struct FlagBody {
    #[serde(default)]
    description: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    default_value: Value,
    #[serde(default)]
    rules: Vec<liveops::FlagRule>,
}

#[derive(Deserialize)]
struct FlagBodyWithName {
    name: String,
    #[serde(flatten)]
    body: FlagBody,
}

fn yes() -> bool {
    true
}

async fn save_flag(s: &AppState, name: &str, b: FlagBody) -> Result<Flag> {
    valid_id(name, "flag name")?;
    if b.rules.len() > 64 {
        return Err(Error::InvalidArgument("at most 64 rules".into()));
    }
    for r in &b.rules {
        if r.rollout_pct.is_some_and(|p| p > 100) {
            return Err(Error::InvalidArgument("rollout_pct: 0..=100".into()));
        }
    }
    sqlx::query(
        "insert into musubi.flags (name, description, enabled, default_value, rules)
         values ($1, $2, $3, $4, $5)
         on conflict (name) do update set description = excluded.description, enabled = excluded.enabled,
            default_value = excluded.default_value, rules = excluded.rules, updated_at = now()",
    )
    .bind(name)
    .bind(&b.description)
    .bind(b.enabled)
    .bind(&b.default_value)
    .bind(serde_json::to_value(&b.rules).map_err(Error::internal)?)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    changed(s, "flag", name).await;
    liveops::load_flag(s, name)
        .await?
        .ok_or_else(|| Error::NotFound("flag".into()))
}

async fn list_flags(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "flags": liveops::load_flags(&s).await? })))
}

async fn get_flag(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> std::result::Result<Json<Flag>, ApiError> {
    Ok(Json(
        liveops::load_flag(&s, &name)
            .await?
            .ok_or_else(|| Error::NotFound("flag".into()))?,
    ))
}

async fn put_flag(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
    Json(b): Json<FlagBody>,
) -> std::result::Result<Json<Flag>, ApiError> {
    Ok(Json(save_flag(&s, &name, b).await?))
}

async fn post_flag(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<FlagBodyWithName>,
) -> std::result::Result<Json<Flag>, ApiError> {
    Ok(Json(save_flag(&s, &b.name, b.body).await?))
}

async fn delete_flag(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.flags where name = $1")
        .bind(&name)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    changed(&s, "flag", &name).await;
    Ok(Json(json!({ "removed": n > 0 })))
}

// ---------------------------------------------------------------- experiments

#[derive(Deserialize)]
struct ExperimentBody {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    audience: Option<String>,
    #[serde(default = "hundred")]
    rollout_pct: i32,
    #[serde(default)]
    variants: Vec<liveops::Variant>,
    #[serde(default)]
    starts_at: Option<i64>,
    #[serde(default)]
    ends_at: Option<i64>,
    #[serde(default)]
    exclusion_group: Option<String>,
    #[serde(default)]
    metadata: Value,
}

#[derive(Deserialize)]
struct ExperimentBodyWithId {
    id: String,
    #[serde(flatten)]
    body: ExperimentBody,
}

fn hundred() -> i32 {
    100
}

async fn save_experiment(s: &AppState, id: &str, b: ExperimentBody) -> Result<Experiment> {
    valid_id(id, "experiment id")?;
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 chars".into()));
    }
    if !(0..=100).contains(&b.rollout_pct) {
        return Err(Error::InvalidArgument("rollout_pct: 0..=100".into()));
    }
    if b.variants.len() < 2 || b.variants.len() > 16 {
        return Err(Error::InvalidArgument("variants: 2..16".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for v in &b.variants {
        valid_id(&v.name, "variant name")?;
        if !seen.insert(v.name.clone()) {
            return Err(Error::InvalidArgument(format!(
                "duplicate variant {}",
                v.name
            )));
        }
    }
    if b.variants.iter().all(|v| v.weight == 0) {
        return Err(Error::InvalidArgument(
            "at least one variant needs weight > 0".into(),
        ));
    }
    let existing = liveops::load_experiment(s, id).await?;
    if let Some(e) = &existing {
        if e.status != "draft"
            && e.variants
                .iter()
                .map(|v| &v.name)
                .ne(b.variants.iter().map(|v| &v.name))
        {
            return Err(Error::Conflict(
                "variants of a started experiment cannot change (end it and create a new one)"
                    .into(),
            ));
        }
    }
    sqlx::query(
        "insert into musubi.experiments (id, name, description, status, audience, rollout_pct, variants, starts_at, ends_at, exclusion_group, metadata)
         values ($1, $2, $3, 'draft', $4, $5, $6, $7, $8, $9, $10)
         on conflict (id) do update set name = excluded.name, description = excluded.description,
            audience = excluded.audience, rollout_pct = excluded.rollout_pct, variants = excluded.variants,
            starts_at = excluded.starts_at, ends_at = excluded.ends_at,
            exclusion_group = excluded.exclusion_group, metadata = excluded.metadata, updated_at = now()",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(&b.audience)
    .bind(b.rollout_pct)
    .bind(serde_json::to_value(&b.variants).map_err(Error::internal)?)
    .bind(b.starts_at)
    .bind(b.ends_at)
    .bind(&b.exclusion_group)
    .bind(if b.metadata.is_null() { json!({}) } else { b.metadata })
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    changed(s, "experiment", id).await;
    liveops::load_experiment(s, id)
        .await?
        .ok_or_else(|| Error::NotFound("experiment".into()))
}

async fn set_status(s: &AppState, id: &str, from: &[&str], to: &str) -> Result<Experiment> {
    let e = liveops::load_experiment(s, id)
        .await?
        .ok_or_else(|| Error::NotFound("experiment".into()))?;
    if !from.contains(&e.status.as_str()) {
        return Err(Error::Conflict(format!("experiment is {}", e.status)));
    }
    sqlx::query("update musubi.experiments set status = $2, updated_at = now() where id = $1")
        .bind(id)
        .bind(to)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    changed(s, "experiment", id).await;
    s.events
        .emit(
            &format!("experiment.{to}"),
            None,
            json!({ "experiment_id": id }),
        )
        .await;
    liveops::load_experiment(s, id)
        .await?
        .ok_or_else(|| Error::NotFound("experiment".into()))
}

async fn list_experiments(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "experiments": liveops::load_experiments(&s, false).await? }),
    ))
}

async fn get_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Experiment>, ApiError> {
    Ok(Json(
        liveops::load_experiment(&s, &id)
            .await?
            .ok_or_else(|| Error::NotFound("experiment".into()))?,
    ))
}

async fn put_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<ExperimentBody>,
) -> std::result::Result<Json<Experiment>, ApiError> {
    Ok(Json(save_experiment(&s, &id, b).await?))
}

async fn post_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<ExperimentBodyWithId>,
) -> std::result::Result<Json<Experiment>, ApiError> {
    Ok(Json(save_experiment(&s, &b.id, b.body).await?))
}

async fn delete_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.experiments where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    changed(&s, "experiment", &id).await;
    Ok(Json(json!({ "removed": n > 0 })))
}

async fn start_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Experiment>, ApiError> {
    Ok(Json(
        set_status(&s, &id, &["draft", "paused"], "running").await?,
    ))
}

async fn pause_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Experiment>, ApiError> {
    Ok(Json(set_status(&s, &id, &["running"], "paused").await?))
}

async fn end_experiment(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Experiment>, ApiError> {
    Ok(Json(
        set_status(&s, &id, &["draft", "running", "paused"], "ended").await?,
    ))
}

async fn experiment_results(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let e = liveops::load_experiment(&s, &id)
        .await?
        .ok_or_else(|| Error::NotFound("experiment".into()))?;
    Ok(Json(liveops::results(&s, &e).await?))
}

#[derive(Deserialize)]
struct PageQ {
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    variant: Option<String>,
}

fn default_limit() -> i64 {
    100
}

async fn experiment_assignments(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<PageQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<(Uuid, String, i64)> = sqlx::query_as(
        "select account_id, variant, extract(epoch from assigned_at)::bigint from musubi.experiment_assignments
         where experiment_id = $1 and ($2::text is null or variant = $2)
         order by assigned_at desc limit $3 offset $4",
    )
    .bind(&id)
    .bind(q.variant.as_deref())
    .bind(q.limit.clamp(1, 1000))
    .bind(q.offset.max(0))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "assignments": rows.iter().map(|(a, v, at)| json!({ "account_id": a, "variant": v, "assigned_at": at })).collect::<Vec<_>>()
    })))
}

/// `GET /v1/liveops/experiments` — the caller's variants (assigning on first sight).
async fn my_experiments(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> std::result::Result<Json<Value>, ApiError> {
    let audiences = s.audiences.of(&s.db, &s.economy, me.account_id).await?;
    let a = liveops::assignments(&s, me.account_id, &audiences).await?;
    Ok(Json(json!({
        "experiments": a.iter().map(|(e, v)| json!({ "experiment_id": e.id, "name": e.name, "variant": v })).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
struct OutcomeBody {
    metric: String,
    #[serde(default = "one")]
    value: f64,
}

fn one() -> f64 {
    1.0
}

/// `POST /v1/liveops/experiments/{id}/outcome {metric, value}` — an outcome for the caller's
/// variant (conversion = value 1, revenue = amount, …).
async fn outcome(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(b): Json<OutcomeBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    valid_id(&b.metric, "metric")?;
    if !b.value.is_finite() {
        return Err(Error::InvalidArgument("value must be finite".into()).into());
    }
    let variant: Option<String> = sqlx::query_scalar(
        "select variant from musubi.experiment_assignments where experiment_id = $1 and account_id = $2",
    )
    .bind(&id)
    .bind(me.account_id.0)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some(variant) = variant else {
        return Err(Error::NotFound("not in this experiment".into()).into());
    };
    record_outcome(&s, &id, me.account_id, &variant, &b.metric, b.value).await?;
    Ok(Json(json!({ "recorded": true, "variant": variant })))
}

pub(crate) async fn record_outcome(
    s: &AppState,
    experiment_id: &str,
    account: AccountId,
    variant: &str,
    metric: &str,
    value: f64,
) -> Result<()> {
    sqlx::query(
        "insert into musubi.experiment_outcomes (id, experiment_id, account_id, variant, metric, value)
         values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(experiment_id)
    .bind(account.0)
    .bind(variant)
    .bind(metric)
    .bind(value)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    s.events
        .emit(
            "experiment.outcome",
            Some(account),
            json!({ "experiment_id": experiment_id, "variant": variant, "metric": metric, "value": value }),
        )
        .await;
    Ok(())
}

// ---------------------------------------------------------------- live events

#[derive(Deserialize)]
struct EventBody {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    phases: Vec<liveops::Phase>,
    starts_at: i64,
    #[serde(default)]
    ends_at: Option<i64>,
    #[serde(default)]
    recurrence_secs: Option<i64>,
    #[serde(default)]
    local_time: bool,
    #[serde(default)]
    audience: Option<String>,
    #[serde(default)]
    metadata: Value,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default = "implicit")]
    participation: String,
    #[serde(default)]
    participation_secs: Option<i64>,
    #[serde(default)]
    max_participants: Option<i64>,
    #[serde(default)]
    template: Option<String>,
}

fn implicit() -> String {
    "implicit".into()
}

fn valid_labels(labels: &[String]) -> Result<()> {
    if labels.len() > 16 {
        return Err(Error::InvalidArgument("labels: at most 16".into()));
    }
    for l in labels {
        if l.is_empty()
            || l.len() > 32
            || !l
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(Error::InvalidArgument(
                "labels: [a-z0-9_-]{1,32} each".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct EventBodyWithId {
    id: String,
    #[serde(flatten)]
    body: EventBody,
}

fn validate_event(b: &EventBody) -> Result<()> {
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 chars".into()));
    }
    if b.phases.is_empty() || b.phases.len() > 32 {
        return Err(Error::InvalidArgument("phases: 1..32".into()));
    }
    let mut end = 0;
    for p in &b.phases {
        if p.offset_secs < end || p.duration_secs <= 0 {
            return Err(Error::InvalidArgument(
                "phases must be ordered, non-overlapping, with positive duration".into(),
            ));
        }
        end = p.offset_secs + p.duration_secs;
    }
    if b.recurrence_secs.is_some_and(|r| r < end) {
        return Err(Error::InvalidArgument(
            "recurrence_secs must cover every phase".into(),
        ));
    }
    if b.ends_at.is_some_and(|e| e <= b.starts_at) {
        return Err(Error::InvalidArgument(
            "ends_at must be after starts_at".into(),
        ));
    }
    valid_labels(&b.labels)?;
    if !matches!(b.participation.as_str(), "implicit" | "explicit") {
        return Err(Error::InvalidArgument(
            "participation: implicit|explicit".into(),
        ));
    }
    if b.participation_secs.is_some_and(|p| p <= 0) || b.max_participants.is_some_and(|m| m <= 0) {
        return Err(Error::InvalidArgument(
            "participation_secs / max_participants must be positive".into(),
        ));
    }
    if b.participation == "implicit"
        && (b.participation_secs.is_some() || b.max_participants.is_some())
    {
        return Err(Error::InvalidArgument(
            "participation_secs / max_participants need participation = explicit".into(),
        ));
    }
    Ok(())
}

async fn save_event(s: &AppState, id: &str, b: EventBody) -> Result<LiveEvent> {
    valid_id(id, "live event id")?;
    validate_event(&b)?;
    sqlx::query(
        "insert into musubi.live_events (id, name, description, enabled, phases, starts_at, ends_at, recurrence_secs, local_time, audience, metadata, labels, participation, participation_secs, max_participants, template)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
         on conflict (id) do update set name = excluded.name, description = excluded.description,
            enabled = excluded.enabled, phases = excluded.phases, starts_at = excluded.starts_at,
            ends_at = excluded.ends_at, recurrence_secs = excluded.recurrence_secs,
            local_time = excluded.local_time, audience = excluded.audience,
            metadata = excluded.metadata, labels = excluded.labels, participation = excluded.participation,
            participation_secs = excluded.participation_secs, max_participants = excluded.max_participants,
            template = excluded.template, updated_at = now()",
    )
    .bind(id)
    .bind(b.name.trim())
    .bind(&b.description)
    .bind(b.enabled)
    .bind(serde_json::to_value(&b.phases).map_err(Error::internal)?)
    .bind(b.starts_at)
    .bind(b.ends_at)
    .bind(b.recurrence_secs)
    .bind(b.local_time)
    .bind(&b.audience)
    .bind(if b.metadata.is_null() { json!({}) } else { b.metadata })
    .bind(json!(b.labels))
    .bind(&b.participation)
    .bind(b.participation_secs)
    .bind(b.max_participants)
    .bind(&b.template)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    changed(s, "live_event", id).await;
    liveops::load_event(s, id)
        .await?
        .ok_or_else(|| Error::NotFound("live event".into()))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct EventListQ {
    label: Option<String>,
    participation: Option<String>,
    template: Option<String>,
}

async fn list_events(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<EventListQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let mut evs = liveops::load_events(&s).await?;
    evs.retain(|e| {
        q.label.as_ref().is_none_or(|l| e.labels.contains(l))
            && q.participation
                .as_ref()
                .is_none_or(|p| &e.participation == p)
            && q.template
                .as_ref()
                .is_none_or(|t| e.template.as_ref() == Some(t))
    });
    Ok(Json(json!({ "live_events": evs })))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ParticipantsQ {
    limit: Option<i64>,
    cycle_started_at: Option<i64>,
}

/// `GET /v1/server/live_events/{id}/participants` — who joined (explicit events), per cycle.
async fn event_participants(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Query(q): Query<ParticipantsQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    liveops::load_event(&s, &id)
        .await?
        .ok_or_else(|| Error::NotFound("live event".into()))?;
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let rows: Vec<(Uuid, i64, i64, i64)> = sqlx::query_as(
        "select account_id, cycle_started_at, joined_at, ends_at from musubi.live_event_participants
         where event_id = $1 and ($2::bigint is null or cycle_started_at = $2)
         order by joined_at desc limit $3",
    )
    .bind(&id)
    .bind(q.cycle_started_at)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let count: i64 = sqlx::query_scalar(
        "select count(*) from musubi.live_event_participants where event_id = $1 and ($2::bigint is null or cycle_started_at = $2)",
    )
    .bind(&id)
    .bind(q.cycle_started_at)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "id": id, "count": count,
        "participants": rows.iter().map(|(a, c, j, e)| json!({
            "account_id": a, "cycle_started_at": c, "joined_at": j, "ends_at": e,
        })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------- live-event templates

/// A template is a stored partial event body (phases, labels, metadata, participation…);
/// `POST /v1/server/live_events/from_template` overlays `starts_at`/name/… and saves an event.
#[derive(Deserialize)]
struct TemplateBody {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(flatten)]
    rest: serde_json::Map<String, Value>,
}

const TEMPLATE_KEYS: &[&str] = &[
    "phases",
    "recurrence_secs",
    "local_time",
    "audience",
    "metadata",
    "labels",
    "participation",
    "participation_secs",
    "max_participants",
    "ends_at",
    "enabled",
];

fn template_json(id: &str, name: &str, body: &Value, updated_at: Option<i64>) -> Value {
    let mut v = body.clone();
    v["id"] = json!(id);
    v["name"] = json!(name);
    if let Some(u) = updated_at {
        v["updated_at"] = json!(u);
    }
    v
}

const TPL_SELECT: &str = "select id, name, body, extract(epoch from updated_at)::bigint from musubi.live_event_templates";
type TplRow = (String, String, Value, i64);

async fn list_templates(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    let rows: Vec<TplRow> = sqlx::query_as(&format!("{TPL_SELECT} order by id"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(Json(json!({
        "templates": rows.iter().map(|(i, n, b, u)| template_json(i, n, b, Some(*u))).collect::<Vec<_>>(),
    })))
}

async fn load_template(s: &AppState, id: &str) -> Result<TplRow> {
    let row: Option<TplRow> = sqlx::query_as(&format!("{TPL_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    row.ok_or_else(|| Error::NotFound("live event template".into()))
}

async fn get_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let (i, n, b, u) = load_template(&s, &id).await?;
    Ok(Json(template_json(&i, &n, &b, Some(u))))
}

async fn put_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<TemplateBody>,
) -> std::result::Result<Json<Value>, ApiError> {
    valid_id(&id, "template id")?;
    if b.name.trim().is_empty() || b.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 chars".into()).into());
    }
    if let Some(k) = b.rest.keys().find(|k| !TEMPLATE_KEYS.contains(&k.as_str())) {
        return Err(Error::InvalidArgument(format!(
            "unknown template field `{k}` (allowed: {})",
            TEMPLATE_KEYS.join(", ")
        ))
        .into());
    }
    // Dry-run the template as an event body to validate phases/labels/participation.
    let mut probe = Value::Object(b.rest.clone());
    probe["name"] = json!(b.name.trim());
    probe["starts_at"] = json!(0);
    probe["ends_at"] = Value::Null;
    let eb: EventBody = serde_json::from_value(probe)
        .map_err(|e| Error::InvalidArgument(format!("template body: {e}")))?;
    validate_event(&eb)?;
    let mut body = Value::Object(b.rest);
    body["description"] = json!(b.description);
    let (i, n, bo, u): TplRow = sqlx::query_as(
        "insert into musubi.live_event_templates (id, name, body) values ($1, $2, $3)
         on conflict (id) do update set name = excluded.name, body = excluded.body, updated_at = now()
         returning id, name, body, extract(epoch from updated_at)::bigint",
    )
    .bind(&id)
    .bind(b.name.trim())
    .bind(&body)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(template_json(&i, &n, &bo, Some(u))))
}

async fn delete_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.live_event_templates where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

#[derive(Deserialize)]
struct FromTemplateBody {
    template: String,
    id: String,
    starts_at: i64,
    #[serde(flatten)]
    overrides: serde_json::Map<String, Value>,
}

/// `POST /v1/server/live_events/from_template {template, id, starts_at, …overrides}` — an event
/// from a template: template fields, then overrides (`metadata` is merged key-wise).
async fn event_from_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<FromTemplateBody>,
) -> std::result::Result<Json<LiveEvent>, ApiError> {
    let (tid, tname, tbody, _) = load_template(&s, &b.template).await?;
    let mut merged = tbody;
    merged["name"] = json!(tname);
    for (k, v) in b.overrides {
        if k == "metadata" {
            if let (Some(dst), Some(src)) = (merged["metadata"].as_object_mut(), v.as_object()) {
                for (mk, mv) in src {
                    dst.insert(mk.clone(), mv.clone());
                }
                continue;
            }
        }
        merged[k] = v;
    }
    merged["starts_at"] = json!(b.starts_at);
    merged["template"] = json!(tid);
    let eb: EventBody = serde_json::from_value(merged)
        .map_err(|e| Error::InvalidArgument(format!("event body: {e}")))?;
    Ok(Json(save_event(&s, &b.id, eb).await?))
}

async fn get_event(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<LiveEvent>, ApiError> {
    Ok(Json(
        liveops::load_event(&s, &id)
            .await?
            .ok_or_else(|| Error::NotFound("live event".into()))?,
    ))
}

async fn put_event(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(b): Json<EventBody>,
) -> std::result::Result<Json<LiveEvent>, ApiError> {
    Ok(Json(save_event(&s, &id, b).await?))
}

async fn post_event(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(b): Json<EventBodyWithId>,
) -> std::result::Result<Json<LiveEvent>, ApiError> {
    Ok(Json(save_event(&s, &b.id, b.body).await?))
}

async fn delete_event(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.live_events where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    changed(&s, "live_event", &id).await;
    Ok(Json(json!({ "removed": n > 0 })))
}

#[derive(Deserialize)]
struct EvalQ {
    account_id: Uuid,
    names: Option<String>,
}

/// `GET /v1/server/liveops/eval?account_id=` — what one account sees (support / debugging).
async fn eval_for(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<EvalQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let acc = AccountId(q.account_id);
    let names: Vec<String> = q
        .names
        .map(|n| {
            n.split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let audiences = s.audiences.of(&s.db, &s.economy, acc).await?;
    let exps = liveops::assignments(&s, acc, &audiences).await?;
    Ok(Json(json!({
        "account_id": acc,
        "audiences": *audiences,
        "flags": liveops::merged_flags(&s, acc, &names).await?,
        "live_events": liveops::merged_live_events(&s, acc).await?["live_events"],
        "experiments": exps.iter().map(|(e, v)| json!({ "experiment_id": e.id, "variant": v })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------- LiveOps timeline

#[derive(Deserialize, Default)]
#[serde(default)]
struct TimelineQ {
    /// Window (Unix seconds); defaults to now-1d .. now+14d.
    from: Option<i64>,
    to: Option<i64>,
    /// Comma-separated subset of `live_event,flag,experiment,news,offer,changeset`.
    kinds: Option<String>,
    /// Keep live events carrying this label / news carrying this tag.
    label: Option<String>,
    limit: Option<usize>,
}

/// (id, name, status, starts_at, ends_at)
type ExpTimelineRow = (String, String, String, Option<i64>, Option<i64>);
/// (id, name, placement_id, enabled, starts_at, ends_at, audience)
type OfferTimelineRow = (
    String,
    String,
    String,
    bool,
    Option<i64>,
    Option<i64>,
    Option<String>,
);
/// (id, title, published_at, expires_at, tags, audience, pinned)
type NewsTimelineRow = (
    uuid::Uuid,
    String,
    i64,
    Option<i64>,
    Value,
    Option<String>,
    bool,
);

fn state_of(start: i64, end: Option<i64>, now: i64) -> &'static str {
    if end.is_some_and(|e| e <= now) {
        "past"
    } else if start <= now {
        "active"
    } else {
        "upcoming"
    }
}

#[allow(clippy::too_many_arguments)]
fn entry(
    kind: &str,
    id: &str,
    name: &str,
    start: i64,
    end: Option<i64>,
    now: i64,
    labels: Value,
    detail: Value,
) -> Value {
    json!({
        "kind": kind, "id": id, "name": name, "starts_at": start, "ends_at": end,
        "state": state_of(start, end, now), "labels": labels, "detail": detail,
    })
}

fn intersects(start: i64, end: Option<i64>, from: i64, to: i64) -> bool {
    start < to && end.is_none_or(|e| e > from)
}

/// `GET /v1/server/liveops/timeline?from&to&kinds&label` — one chronological view of everything
/// scheduled: live-event cycles (recurrence expanded, phases listed), flag rule windows,
/// experiments, news, store offers and scheduled economy changesets, each with
/// `past|active|upcoming`, plus a summary (per kind/state, what is active now, what starts in
/// the next 24 h). Local-time live events are shown at their UTC anchor.
async fn timeline(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<TimelineQ>,
) -> std::result::Result<Json<Value>, ApiError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let from = q.from.unwrap_or(now - 86_400);
    let to = q.to.unwrap_or(now + 14 * 86_400);
    if to <= from || to - from > 366 * 86_400 {
        return Err(Error::InvalidArgument(
            "to must be after from and the window at most 366 days".into(),
        )
        .into());
    }
    let all_kinds = [
        "live_event",
        "flag",
        "experiment",
        "news",
        "offer",
        "changeset",
    ];
    let kinds: Vec<String> = match q.kinds.as_deref() {
        Some(k) if !k.is_empty() => {
            let v: Vec<String> = k
                .split(',')
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect();
            if let Some(bad) = v.iter().find(|x| !all_kinds.contains(&x.as_str())) {
                return Err(Error::InvalidArgument(format!(
                    "unknown kind `{bad}` (live_event|flag|experiment|news|offer|changeset)"
                ))
                .into());
            }
            v
        }
        _ => all_kinds.iter().map(|x| x.to_string()).collect(),
    };
    let want = |k: &str| kinds.iter().any(|x| x == k);
    let mut out: Vec<Value> = Vec::new();

    if want("live_event") {
        for ev in liveops::load_events(&s).await? {
            if !ev.enabled {
                continue;
            }
            if q.label.as_ref().is_some_and(|l| !ev.labels.contains(l)) {
                continue;
            }
            let len = liveops::cycle_len(&ev);
            let detail = |cycle: i64, index: i64| {
                json!({
                    "cycle_index": index, "participation": ev.participation, "audience": ev.audience,
                    "phases": ev.phases.iter().map(|p| json!({
                        "name": p.name, "starts_at": cycle + p.offset_secs, "ends_at": cycle + p.offset_secs + p.duration_secs,
                    })).collect::<Vec<_>>(),
                    "recurrence_secs": ev.recurrence_secs, "local_time": ev.local_time,
                })
            };
            match ev.recurrence_secs.filter(|r| *r > 0) {
                None => {
                    let end = Some(ev.ends_at.unwrap_or(ev.starts_at + len));
                    if intersects(ev.starts_at, end, from, to) {
                        out.push(entry(
                            "live_event",
                            &ev.id,
                            &ev.name,
                            ev.starts_at,
                            end,
                            now,
                            json!(ev.labels),
                            detail(ev.starts_at, 0),
                        ));
                    }
                }
                Some(r) => {
                    let first = if from > ev.starts_at {
                        (from - ev.starts_at - len).max(0) / r
                    } else {
                        0
                    };
                    let mut k = first;
                    let mut n = 0;
                    loop {
                        let cycle = ev.starts_at + k * r;
                        if cycle >= to || n >= 200 || ev.ends_at.is_some_and(|e| cycle >= e) {
                            break;
                        }
                        let end = Some((cycle + len).min(ev.ends_at.unwrap_or(i64::MAX)));
                        if intersects(cycle, end, from, to) {
                            out.push(entry(
                                "live_event",
                                &ev.id,
                                &ev.name,
                                cycle,
                                end,
                                now,
                                json!(ev.labels),
                                detail(cycle, k),
                            ));
                            n += 1;
                        }
                        k += 1;
                    }
                }
            }
        }
    }
    if want("flag") && q.label.is_none() {
        let rows: Vec<(String, bool, Value)> =
            sqlx::query_as("select name, enabled, rules from musubi.flags order by name")
                .fetch_all(&s.db)
                .await
                .map_err(Error::internal)?;
        for (name, enabled, rules) in rows {
            let rules: Vec<liveops::FlagRule> = serde_json::from_value(rules).unwrap_or_default();
            for (i, r) in rules.iter().enumerate() {
                if r.starts_at.is_none() && r.ends_at.is_none() {
                    continue;
                }
                let start = r.starts_at.unwrap_or(i64::MIN / 2);
                if intersects(start, r.ends_at, from, to) {
                    out.push(entry("flag", &name, &name, start.max(from.min(start)), r.ends_at, now, json!([]),
                        json!({ "rule_index": i, "value": r.value, "audience": r.audience, "rollout_pct": r.rollout_pct, "enabled": enabled })));
                }
            }
        }
    }
    if want("experiment") && q.label.is_none() {
        let rows: Vec<ExpTimelineRow> = sqlx::query_as(
            "select id, name, status, starts_at, ends_at from musubi.experiments order by starts_at nulls last, id",
        )
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for (id, name, status, st, en) in rows {
            if st.is_none() && en.is_none() {
                continue;
            }
            let start = st.unwrap_or(from);
            if intersects(start, en, from, to) {
                out.push(entry(
                    "experiment",
                    &id,
                    &name,
                    start,
                    en,
                    now,
                    json!([]),
                    json!({ "status": status }),
                ));
            }
        }
    }
    if want("news") {
        let rows: Vec<NewsTimelineRow> = sqlx::query_as(
            "select id, title, published_at, expires_at, tags, audience, pinned from musubi.news where enabled order by published_at",
        )
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for (id, title, p, e, tags, audience, pinned) in rows {
            if q.label.as_ref().is_some_and(|l| {
                !tags
                    .as_array()
                    .is_some_and(|a| a.iter().any(|t| t.as_str() == Some(l)))
            }) {
                continue;
            }
            if intersects(p, e, from, to) {
                out.push(entry(
                    "news",
                    &id.to_string(),
                    &title,
                    p,
                    e,
                    now,
                    tags,
                    json!({ "audience": audience, "pinned": pinned }),
                ));
            }
        }
    }
    if want("offer") && q.label.is_none() {
        let rows: Vec<OfferTimelineRow> = sqlx::query_as(
            "select id, name, placement_id, enabled, starts_at, ends_at, audience from musubi.offers where enabled order by starts_at nulls last, id",
        )
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for (id, name, placement, _enabled, st, en, audience) in rows {
            if st.is_none() && en.is_none() {
                continue;
            }
            let start = st.unwrap_or(from);
            if intersects(start, en, from, to) {
                out.push(entry(
                    "offer",
                    &id,
                    &name,
                    start,
                    en,
                    now,
                    json!([]),
                    json!({ "placement_id": placement, "audience": audience }),
                ));
            }
        }
    }
    if want("changeset") && q.label.is_none() {
        let rows: Vec<(uuid::Uuid, String, String, Option<i64>)> = sqlx::query_as(
            "select id, name, state, extract(epoch from schedule_at)::bigint from musubi.economy_changesets
             where state = 'scheduled' and schedule_at is not null order by schedule_at",
        )
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for (id, name, state, at) in rows {
            let Some(at) = at else { continue };
            if at >= from && at < to {
                out.push(entry(
                    "changeset",
                    &id.to_string(),
                    &name,
                    at,
                    Some(at),
                    now,
                    json!([]),
                    json!({ "state": state, "publishes_at": at }),
                ));
            }
        }
    }
    out.sort_by(|a, b| {
        a["starts_at"]
            .as_i64()
            .cmp(&b["starts_at"].as_i64())
            .then(a["kind"].as_str().cmp(&b["kind"].as_str()))
            .then(a["id"].as_str().cmp(&b["id"].as_str()))
    });
    let total = out.len();
    out.truncate(q.limit.unwrap_or(500).clamp(1, 2000));
    let mut by_kind = serde_json::Map::new();
    for e in &out {
        let k = e["kind"].as_str().unwrap_or("").to_string();
        let st = e["state"].as_str().unwrap_or("").to_string();
        let slot = by_kind
            .entry(k)
            .or_insert_with(|| json!({ "past": 0, "active": 0, "upcoming": 0 }));
        slot[&st] = json!(slot[&st].as_i64().unwrap_or(0) + 1);
    }
    let active_now: Vec<Value> = out.iter().filter(|e| e["state"] == "active").map(|e| json!({ "kind": e["kind"], "id": e["id"], "name": e["name"], "ends_at": e["ends_at"] })).collect();
    let next_24h: Vec<Value> = out.iter().filter(|e| e["state"] == "upcoming" && e["starts_at"].as_i64().is_some_and(|t| t <= now + 86_400)).map(|e| json!({ "kind": e["kind"], "id": e["id"], "name": e["name"], "starts_at": e["starts_at"] })).collect();
    Ok(Json(json!({
        "from": from, "to": to, "now": now, "kinds": kinds, "total": total, "entries": out,
        "summary": { "by_kind": by_kind, "active_now": active_now, "next_24h": next_24h },
    })))
}
