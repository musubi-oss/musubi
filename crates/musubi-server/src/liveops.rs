//! Native LiveOps (competitive P0-17 / P0-52 / P0-53): feature flags with targeting rules,
//! A/B experiments with sticky assignment, exposure/outcome tracking and scorecards, and
//! scheduled live events with phases. Zanshin (`[zanshin]`) stays an optional overlay: its
//! flags / live events are merged in for names musubi does not define itself.

use std::collections::{HashMap, HashSet};

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::app::AppState;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FlagRule {
    /// Named audience (`/v1/server/audiences`) the account must be in.
    pub audience: Option<String>,
    /// Percentage 0..=100 of accounts (deterministic per flag + account).
    pub rollout_pct: Option<u8>,
    pub value: Value,
    pub starts_at: Option<i64>,
    pub ends_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Flag {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub default_value: Value,
    #[serde(default)]
    pub rules: Vec<FlagRule>,
    /// Category labels (filter in lists, group in tooling).
    #[serde(default)]
    pub labels: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Variant {
    pub name: String,
    pub weight: u32,
    /// Flag overrides for accounts in this variant.
    pub flags: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Experiment {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `draft` | `running` | `paused` | `ended`
    #[serde(default = "draft")]
    pub status: String,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default = "hundred")]
    pub rollout_pct: i32,
    #[serde(default)]
    pub variants: Vec<Variant>,
    #[serde(default)]
    pub starts_at: Option<i64>,
    #[serde(default)]
    pub ends_at: Option<i64>,
    /// At most one running experiment per group per account.
    #[serde(default)]
    pub exclusion_group: Option<String>,
    #[serde(default)]
    pub metadata: Value,
    #[serde(default)]
    pub labels: Vec<String>,
}

fn draft() -> String {
    "draft".into()
}

fn hundred() -> i32 {
    100
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Phase {
    pub name: String,
    pub offset_secs: i64,
    pub duration_secs: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveEvent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub phases: Vec<Phase>,
    /// Unix seconds; with `local_time` the wall-clock of this instant (UTC) is replayed in the
    /// account's zone (`accounts.metadata.tz_offset_min`).
    pub starts_at: i64,
    #[serde(default)]
    pub ends_at: Option<i64>,
    /// Repeat every N seconds (daily = 86400, weekly = 604800).
    #[serde(default)]
    pub recurrence_secs: Option<i64>,
    #[serde(default)]
    pub local_time: bool,
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub metadata: Value,
    /// Category labels (filter in lists, group in tooling).
    #[serde(default)]
    pub labels: Vec<String>,
    /// `implicit` — every eligible account is in while a phase runs; `explicit` — the account
    /// joins, and its own window runs `participation_secs` from joining (capped by the cycle).
    #[serde(default = "implicit")]
    pub participation: String,
    #[serde(default)]
    pub participation_secs: Option<i64>,
    #[serde(default)]
    pub max_participants: Option<i64>,
    #[serde(default)]
    pub template: Option<String>,
}

fn implicit() -> String {
    "implicit".into()
}

/// Client-side view options for live events (P0-53).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EventQuery {
    /// Include events / own participations that ended within 30 days.
    pub include_past: bool,
    /// Include events starting later than the default 7-day horizon.
    pub include_future: bool,
    /// Comma-separated labels — keep events carrying any of them.
    pub labels: Option<String>,
}

/// Deterministic 0..10_000 bucket of (key, account) — stable across nodes and restarts.
pub fn bucket(key: &str, account: Uuid) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes().iter().chain(account.as_bytes().iter()) {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    (h % 10_000) as u32
}

fn in_pct(key: &str, account: Uuid, pct: i64) -> bool {
    (bucket(key, account) as i64) < pct.clamp(0, 100) * 100
}

/// Weighted pick, deterministic for (experiment, account).
pub fn pick_variant<'a>(
    exp_id: &str,
    account: Uuid,
    variants: &'a [Variant],
) -> Option<&'a Variant> {
    let total: u64 = variants.iter().map(|v| v.weight as u64).sum();
    if total == 0 {
        return variants.first();
    }
    let roll = (bucket(&format!("{exp_id}/variant"), account) as u64 * total) / 10_000;
    let mut acc = 0u64;
    for v in variants {
        acc += v.weight as u64;
        if roll < acc {
            return Some(v);
        }
    }
    variants.last()
}

fn window_ok(starts: Option<i64>, ends: Option<i64>, now: i64) -> bool {
    starts.is_none_or(|s| s <= now) && ends.is_none_or(|e| now < e)
}

/// Active phase of an event at `now` for an account whose clock is `tz_offset_min` from UTC.
/// Returns (phase index, phase start, phase end, cycle start) in unix seconds, or the next
/// cycle start when nothing is active.
pub fn phase_at(
    ev: &LiveEvent,
    now: i64,
    tz_offset_min: i64,
) -> std::result::Result<(usize, i64, i64, i64), Option<i64>> {
    let shift = if ev.local_time {
        -tz_offset_min * 60
    } else {
        0
    };
    let start = ev.starts_at + shift;
    if let Some(e) = ev.ends_at {
        if now >= e + shift {
            return Err(None);
        }
    }
    let cycle_start = match ev.recurrence_secs.filter(|r| *r > 0) {
        Some(r) if now >= start => start + ((now - start) / r) * r,
        _ => start,
    };
    if now < cycle_start {
        return Err(Some(cycle_start));
    }
    let mut next: Option<i64> = None;
    for (i, p) in ev.phases.iter().enumerate() {
        let ps = cycle_start + p.offset_secs;
        let pe = ps + p.duration_secs;
        if ps <= now && now < pe {
            return Ok((i, ps, pe, cycle_start));
        }
        if ps > now {
            next = Some(next.map_or(ps, |n| n.min(ps)));
        }
    }
    if next.is_none() {
        if let Some(r) = ev.recurrence_secs.filter(|r| *r > 0) {
            next = Some(cycle_start + r);
        }
    }
    Err(next)
}

// ---------------------------------------------------------------- loading

const FLAG_SELECT: &str =
    "select name, description, enabled, default_value, rules, labels from musubi.flags";
type FlagRow = (String, String, bool, Value, Value, Value);

fn flag_of((name, description, enabled, default_value, rules, labels): FlagRow) -> Flag {
    Flag {
        name,
        description,
        enabled,
        default_value,
        rules: serde_json::from_value(rules).unwrap_or_default(),
        labels: serde_json::from_value(labels).unwrap_or_default(),
    }
}

pub async fn load_flags(s: &AppState) -> Result<Vec<Flag>> {
    let rows: Vec<FlagRow> = sqlx::query_as(&format!("{FLAG_SELECT} order by name"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(rows.into_iter().map(flag_of).collect())
}

pub async fn load_flag(s: &AppState, name: &str) -> Result<Option<Flag>> {
    let row: Option<FlagRow> = sqlx::query_as(&format!("{FLAG_SELECT} where name = $1"))
        .bind(name)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(row.map(flag_of))
}

const EXP_SELECT: &str = "select id, name, description, status, audience, rollout_pct, variants, starts_at, ends_at, exclusion_group, metadata, labels from musubi.experiments";
type ExpRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    i32,
    Value,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Value,
    Value,
);

fn exp_of(r: ExpRow) -> Experiment {
    let (
        id,
        name,
        description,
        status,
        audience,
        rollout_pct,
        variants,
        starts_at,
        ends_at,
        exclusion_group,
        metadata,
        labels,
    ) = r;
    Experiment {
        id,
        name,
        description,
        status,
        audience,
        rollout_pct,
        variants: serde_json::from_value(variants).unwrap_or_default(),
        starts_at,
        ends_at,
        exclusion_group,
        metadata,
        labels: serde_json::from_value(labels).unwrap_or_default(),
    }
}

pub async fn load_experiments(s: &AppState, only_running: bool) -> Result<Vec<Experiment>> {
    let q = if only_running {
        format!("{EXP_SELECT} where status = 'running' order by id")
    } else {
        format!("{EXP_SELECT} order by id")
    };
    let rows: Vec<ExpRow> = sqlx::query_as(&q)
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(rows.into_iter().map(exp_of).collect())
}

pub async fn load_experiment(s: &AppState, id: &str) -> Result<Option<Experiment>> {
    let row: Option<ExpRow> = sqlx::query_as(&format!("{EXP_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(row.map(exp_of))
}

const EV_SELECT: &str = "select id, name, description, enabled, phases, starts_at, ends_at, recurrence_secs, local_time, audience, metadata, labels, participation, participation_secs, max_participants, template from musubi.live_events";
type EvRow = (
    String,
    String,
    String,
    bool,
    Value,
    i64,
    Option<i64>,
    Option<i64>,
    bool,
    Option<String>,
    Value,
    Value,
    String,
    Option<i64>,
    Option<i64>,
    Option<String>,
);

fn ev_of(r: EvRow) -> LiveEvent {
    let (
        id,
        name,
        description,
        enabled,
        phases,
        starts_at,
        ends_at,
        recurrence_secs,
        local_time,
        audience,
        metadata,
        labels,
        participation,
        participation_secs,
        max_participants,
        template,
    ) = r;
    LiveEvent {
        id,
        name,
        description,
        enabled,
        phases: serde_json::from_value(phases).unwrap_or_default(),
        starts_at,
        ends_at,
        recurrence_secs,
        local_time,
        audience,
        metadata,
        labels: serde_json::from_value(labels).unwrap_or_default(),
        participation,
        participation_secs,
        max_participants,
        template,
    }
}

pub async fn load_events(s: &AppState) -> Result<Vec<LiveEvent>> {
    let rows: Vec<EvRow> = sqlx::query_as(&format!("{EV_SELECT} order by starts_at, id"))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(rows.into_iter().map(ev_of).collect())
}

pub async fn load_event(s: &AppState, id: &str) -> Result<Option<LiveEvent>> {
    let row: Option<EvRow> = sqlx::query_as(&format!("{EV_SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(&s.db)
        .await
        .map_err(Error::internal)?;
    Ok(row.map(ev_of))
}

// ---------------------------------------------------------------- evaluation

fn now_s() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// One account's variant per running experiment it is eligible for (sticky: the first
/// evaluation stores the assignment and emits `experiment.exposed`).
pub async fn assignments(
    s: &AppState,
    account: AccountId,
    audiences: &[String],
) -> Result<Vec<(Experiment, String)>> {
    let now = now_s();
    let running = load_experiments(s, true).await?;
    if running.is_empty() {
        return Ok(Vec::new());
    }
    let stored: Vec<(String, String)> = sqlx::query_as(
        "select experiment_id, variant from musubi.experiment_assignments where account_id = $1",
    )
    .bind(account.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let stored: HashMap<String, String> = stored.into_iter().collect();
    let mut groups: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for exp in running {
        if let Some(v) = stored.get(&exp.id) {
            if let Some(g) = &exp.exclusion_group {
                groups.insert(g.clone());
            }
            out.push((exp, v.clone()));
            continue;
        }
        if !window_ok(exp.starts_at, exp.ends_at, now)
            || exp
                .audience
                .as_ref()
                .is_some_and(|a| !audiences.contains(a))
            || !in_pct(&exp.id, account.0, exp.rollout_pct as i64)
            || exp
                .exclusion_group
                .as_ref()
                .is_some_and(|g| groups.contains(g))
            || exp.variants.is_empty()
        {
            continue;
        }
        let Some(v) = pick_variant(&exp.id, account.0, &exp.variants) else {
            continue;
        };
        let variant = v.name.clone();
        let inserted = sqlx::query(
            "insert into musubi.experiment_assignments (experiment_id, account_id, variant)
             values ($1, $2, $3) on conflict do nothing",
        )
        .bind(&exp.id)
        .bind(account.0)
        .bind(&variant)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if inserted > 0 {
            s.events.invalidate_experiments(Some(account));
            s.events
                .emit(
                    "experiment.exposed",
                    Some(account),
                    json!({ "experiment_id": exp.id, "variant": variant }),
                )
                .await;
        }
        if let Some(g) = &exp.exclusion_group {
            groups.insert(g.clone());
        }
        out.push((exp, variant));
    }
    Ok(out)
}

/// Native flag values for an account: experiment overrides first, then the first matching rule,
/// then the default. `[{name, value, source}]`.
pub async fn eval_flags(s: &AppState, account: AccountId, names: &[String]) -> Result<Vec<Value>> {
    let flags = load_flags(s).await?;
    if flags.is_empty() {
        return Ok(Vec::new());
    }
    let audiences = s.audiences.of(&s.db, &s.economy, account).await?;
    let assigned = assignments(s, account, &audiences).await?;
    let mut overrides: HashMap<String, (Value, String)> = HashMap::new();
    for (exp, variant) in &assigned {
        if let Some(v) = exp.variants.iter().find(|v| v.name == *variant) {
            for (k, val) in &v.flags {
                overrides
                    .entry(k.clone())
                    .or_insert_with(|| (val.clone(), format!("experiment:{}:{}", exp.id, variant)));
            }
        }
    }
    let now = crate::http::qa_clock::now_for(s, account).await;
    let mut out = Vec::new();
    for f in flags {
        if !f.enabled || (!names.is_empty() && !names.contains(&f.name)) {
            continue;
        }
        let (value, source) = if let Some((v, src)) = overrides.get(&f.name) {
            (v.clone(), src.clone())
        } else if let Some((i, r)) = f.rules.iter().enumerate().find(|(i, r)| {
            window_ok(r.starts_at, r.ends_at, now)
                && r.audience.as_ref().is_none_or(|a| audiences.contains(a))
                && r.rollout_pct
                    .is_none_or(|p| in_pct(&format!("{}/rule{i}", f.name), account.0, p as i64))
        }) {
            (r.value.clone(), format!("rule:{i}"))
        } else {
            (f.default_value.clone(), "default".to_string())
        };
        out.push(json!({ "name": f.name, "value": value, "source": source }));
    }
    Ok(out)
}

async fn tz_offset_min(s: &AppState, account: AccountId) -> i64 {
    sqlx::query_scalar::<_, Option<i64>>(
        "select (metadata->>'tz_offset_min')::bigint from musubi.accounts where id = $1",
    )
    .bind(account.0)
    .fetch_optional(&s.db)
    .await
    .ok()
    .flatten()
    .flatten()
    .unwrap_or(0)
}

/// Native live events for an account: active ones with their phase, plus upcoming ones
/// (next start within 7 days).
pub async fn eval_live_events(s: &AppState, account: AccountId) -> Result<Vec<Value>> {
    eval_live_events_q(s, account, &EventQuery::default()).await
}

pub fn cycle_len(ev: &LiveEvent) -> i64 {
    ev.phases
        .iter()
        .map(|p| p.offset_secs + p.duration_secs)
        .max()
        .unwrap_or(0)
}

/// Rows of `live_event_participants` for one account: (event, cycle) → (joined_at, ends_at).
async fn participations(
    s: &AppState,
    account: AccountId,
) -> Result<HashMap<(String, i64), (i64, i64)>> {
    let rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "select event_id, cycle_started_at, joined_at, ends_at from musubi.live_event_participants where account_id = $1",
    )
    .bind(account.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(rows
        .into_iter()
        .map(|(e, c, j, x)| ((e, c), (j, x)))
        .collect())
}

/// Live events as one account sees them (P0-53): running phases (with the account's own
/// participation window for `explicit` events), upcoming starts, and — on request — recently
/// ended events / participations and starts beyond the 7-day horizon, filtered by labels.
pub async fn eval_live_events_q(
    s: &AppState,
    account: AccountId,
    q: &EventQuery,
) -> Result<Vec<Value>> {
    let events = load_events(s).await?;
    if events.is_empty() {
        return Ok(Vec::new());
    }
    let want: Vec<String> = q
        .labels
        .as_deref()
        .unwrap_or("")
        .split(',')
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    let audiences = s.audiences.of(&s.db, &s.economy, account).await?;
    let tz = tz_offset_min(s, account).await;
    let now = crate::http::qa_clock::now_for(s, account).await;
    let mine = if events.iter().any(|e| e.participation == "explicit") {
        participations(s, account).await?
    } else {
        HashMap::new()
    };
    let mut out = Vec::new();
    for ev in events {
        if !ev.enabled || ev.audience.as_ref().is_some_and(|a| !audiences.contains(a)) {
            continue;
        }
        if !want.is_empty() && !ev.labels.iter().any(|l| want.contains(l)) {
            continue;
        }
        let base = json!({
            "id": ev.id, "name": ev.name, "description": ev.description, "labels": ev.labels,
            "participation": ev.participation, "metadata": ev.metadata,
        });
        let mut v = base;
        match phase_at(&ev, now, tz) {
            Ok((i, ps, pe, cycle)) => {
                let next = ev.phases.get(i + 1).map(|p| p.name.clone());
                v["active"] = json!(true);
                v["phase"] = json!(ev.phases[i].name);
                v["phase_index"] = json!(i);
                v["phase_started_at"] = json!(ps);
                v["phase_ends_at"] = json!(pe);
                v["cycle_started_at"] = json!(cycle);
                v["cycle_ends_at"] = json!(cycle + cycle_len(&ev));
                v["next_phase"] = json!(next);
                if ev.participation == "explicit" {
                    match mine.get(&(ev.id.clone(), cycle)) {
                        Some((j, x)) if *x > now => {
                            v["joined"] = json!(true);
                            v["joined_at"] = json!(j);
                            v["participation_ends_at"] = json!(x);
                        }
                        Some((j, x)) => {
                            if !q.include_past {
                                continue;
                            }
                            v["active"] = json!(false);
                            v["joined"] = json!(true);
                            v["joined_at"] = json!(j);
                            v["ended"] = json!(true);
                            v["ended_at"] = json!(x);
                        }
                        None => {
                            v["joined"] = json!(false);
                            v["can_join"] = json!(true);
                            v["participation_secs"] = json!(ev.participation_secs);
                        }
                    }
                }
                out.push(v);
            }
            Err(Some(next)) if q.include_future || next - now <= 7 * 86_400 => {
                v["active"] = json!(false);
                v["starts_at"] = json!(next);
                out.push(v);
            }
            Err(Some(_)) => {}
            Err(None) if q.include_past => {
                let ended_at = ev
                    .ends_at
                    .unwrap_or(ev.starts_at + cycle_len(&ev))
                    .min(ev.ends_at.unwrap_or(i64::MAX));
                if now - ended_at <= 30 * 86_400 {
                    v["active"] = json!(false);
                    v["ended"] = json!(true);
                    v["ended_at"] = json!(ended_at);
                    out.push(v);
                }
            }
            Err(None) => {}
        }
    }
    Ok(out)
}

/// `explicit` participation: the account opts into the running cycle; its own window ends at
/// `joined_at + participation_secs` or the cycle end, whichever is first.
pub async fn join_live_event(s: &AppState, account: AccountId, id: &str) -> Result<Value> {
    let ev = load_event(s, id)
        .await?
        .ok_or_else(|| Error::NotFound("live event".into()))?;
    if ev.participation != "explicit" {
        return Err(Error::InvalidArgument(
            "live event has implicit participation — every eligible account is already in".into(),
        ));
    }
    let audiences = s.audiences.of(&s.db, &s.economy, account).await?;
    if !ev.enabled || ev.audience.as_ref().is_some_and(|a| !audiences.contains(a)) {
        return Err(Error::PermissionDenied(
            "account is not eligible for this live event".into(),
        ));
    }
    let now = crate::http::qa_clock::now_for(s, account).await;
    let tz = tz_offset_min(s, account).await;
    let Ok((_, _, _, cycle)) = phase_at(&ev, now, tz) else {
        return Err(Error::Conflict("live event is not running".into()));
    };
    let cycle_end = cycle + cycle_len(&ev);
    let ends_at = ev
        .participation_secs
        .map_or(cycle_end, |p| (now + p).min(cycle_end));
    if let Some(max) = ev.max_participants {
        let n: i64 = sqlx::query_scalar(
            "select count(*) from musubi.live_event_participants where event_id = $1 and cycle_started_at = $2",
        )
        .bind(id)
        .bind(cycle)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
        let already: bool = sqlx::query_scalar(
            "select exists(select 1 from musubi.live_event_participants where event_id = $1 and cycle_started_at = $2 and account_id = $3)",
        )
        .bind(id)
        .bind(cycle)
        .bind(account.0)
        .fetch_one(&s.db)
        .await
        .map_err(Error::internal)?;
        if n >= max && !already {
            return Err(Error::Conflict("live event is full".into()));
        }
    }
    let (joined_at, ends_at, inserted): (i64, i64, bool) = sqlx::query_as(
        "insert into musubi.live_event_participants (event_id, account_id, cycle_started_at, joined_at, ends_at)
         values ($1, $2, $3, $4, $5)
         on conflict (event_id, account_id, cycle_started_at) do update set joined_at = musubi.live_event_participants.joined_at
         returning joined_at, ends_at, (xmax = 0)",
    )
    .bind(id)
    .bind(account.0)
    .bind(cycle)
    .bind(now)
    .bind(ends_at)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(json!({
        "id": id, "joined": true, "already": !inserted, "cycle_started_at": cycle,
        "joined_at": joined_at, "participation_ends_at": ends_at,
    }))
}

/// Leave the current cycle of an `explicit` event (re-joining starts a fresh window).
pub async fn leave_live_event(s: &AppState, account: AccountId, id: &str) -> Result<Value> {
    let ev = load_event(s, id)
        .await?
        .ok_or_else(|| Error::NotFound("live event".into()))?;
    let now = crate::http::qa_clock::now_for(s, account).await;
    let tz = tz_offset_min(s, account).await;
    let cycle = phase_at(&ev, now, tz).map(|(_, _, _, c)| c).ok();
    let n = sqlx::query(
        "delete from musubi.live_event_participants where event_id = $1 and account_id = $2 and ($3::bigint is null or cycle_started_at = $3)",
    )
    .bind(id)
    .bind(account.0)
    .bind(cycle)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    Ok(json!({ "id": id, "left": n > 0 }))
}

fn merge_by_name(mut native: Vec<Value>, extra: Vec<Value>) -> Vec<Value> {
    let have: HashSet<String> = native
        .iter()
        .filter_map(|v| v["name"].as_str().map(str::to_string))
        .collect();
    for v in extra {
        let name = v["name"].as_str().or(v["id"].as_str()).unwrap_or("");
        if !have.contains(name) {
            native.push(v);
        }
    }
    native
}

/// Flags as clients see them: native, plus Zanshin's for names musubi does not define.
pub async fn merged_flags(s: &AppState, account: AccountId, names: &[String]) -> Result<Value> {
    let native = eval_flags(s, account, names).await?;
    let extra = match &s.liveops {
        Some(l) => l
            .flags(&account.to_string(), names)
            .await?
            .as_array()
            .cloned()
            .unwrap_or_default(),
        None => Vec::new(),
    };
    Ok(Value::Array(merge_by_name(native, extra)))
}

/// Live events as clients see them (`{live_events: [...]}`), native + Zanshin overlay.
pub async fn merged_live_events(s: &AppState, account: AccountId) -> Result<Value> {
    merged_live_events_q(s, account, &EventQuery::default()).await
}

pub async fn merged_live_events_q(
    s: &AppState,
    account: AccountId,
    q: &EventQuery,
) -> Result<Value> {
    let native = eval_live_events_q(s, account, q).await?;
    let extra = match &s.liveops {
        Some(l) => {
            let v = l.live_events(&account.to_string()).await?;
            v.get("live_events")
                .or(v.get("events"))
                .and_then(Value::as_array)
                .cloned()
                .or_else(|| v.as_array().cloned())
                .unwrap_or_default()
        }
        None => Vec::new(),
    };
    Ok(json!({ "live_events": merge_by_name(native, extra) }))
}

// ---------------------------------------------------------------- scorecards

/// Two-sided p-value of a z statistic (Abramowitz–Stegun 7.1.26 erfc approximation).
pub fn p_value(z: f64) -> f64 {
    let x = z.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erfc = poly * (-x * x).exp();
    (erfc).clamp(0.0, 1.0)
}

/// Per-variant assignment counts and per-metric mean / stddev with a z-test against the first
/// (control) variant.
pub async fn results(s: &AppState, exp: &Experiment) -> Result<Value> {
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "select variant, count(*) from musubi.experiment_assignments where experiment_id = $1 group by variant",
    )
    .bind(&exp.id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let counts: HashMap<String, i64> = counts.into_iter().collect();
    type Agg = (String, String, i64, Option<f64>, Option<f64>, i64);
    let rows: Vec<Agg> = sqlx::query_as(
        "select metric, variant, count(*), avg(value), stddev_samp(value), count(distinct account_id)
         from musubi.experiment_outcomes where experiment_id = $1 group by metric, variant",
    )
    .bind(&exp.id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let control = exp
        .variants
        .first()
        .map(|v| v.name.clone())
        .unwrap_or_default();
    let mut metrics: HashMap<String, Vec<Value>> = HashMap::new();
    let mut by_key: HashMap<(String, String), (i64, f64, f64, i64)> = HashMap::new();
    for (metric, variant, n, mean, sd, users) in rows {
        by_key.insert(
            (metric, variant),
            (n, mean.unwrap_or(0.0), sd.unwrap_or(0.0), users),
        );
    }
    let metric_names: HashSet<String> = by_key.keys().map(|(m, _)| m.clone()).collect();
    for metric in metric_names {
        let ctrl = by_key.get(&(metric.clone(), control.clone())).copied();
        let mut rows_out = Vec::new();
        for v in &exp.variants {
            let Some((n, mean, sd, users)) = by_key.get(&(metric.clone(), v.name.clone())).copied()
            else {
                rows_out.push(json!({ "variant": v.name, "n": 0 }));
                continue;
            };
            let assigned = counts.get(&v.name).copied().unwrap_or(0);
            let mut row = json!({
                "variant": v.name, "n": n, "users": users, "assigned": assigned,
                "mean": mean, "stddev": sd,
                "per_assigned": if assigned > 0 { mean * n as f64 / assigned as f64 } else { 0.0 },
            });
            if let Some((cn, cmean, csd, _)) = ctrl {
                if v.name != control && n > 1 && cn > 1 {
                    let se = (csd * csd / cn as f64 + sd * sd / n as f64).sqrt();
                    if se > 0.0 {
                        let z = (mean - cmean) / se;
                        let p = p_value(z);
                        row["lift"] = json!(if cmean != 0.0 {
                            (mean - cmean) / cmean
                        } else {
                            0.0
                        });
                        row["z"] = json!(z);
                        row["p_value"] = json!(p);
                        row["significant_95"] = json!(p < 0.05);
                    }
                }
            }
            rows_out.push(row);
        }
        metrics.insert(metric, rows_out);
    }
    Ok(json!({
        "experiment_id": exp.id, "status": exp.status, "control": control,
        "assigned": exp.variants.iter().map(|v| json!({ "variant": v.name, "count": counts.get(&v.name).copied().unwrap_or(0) })).collect::<Vec<_>>(),
        "metrics": metrics,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_are_stable_and_spread() {
        let a = Uuid::now_v7();
        assert_eq!(bucket("f", a), bucket("f", a));
        assert_ne!(bucket("f", a), bucket("g", a));
        let mut lo = 0;
        for _ in 0..2000 {
            if in_pct("flag", Uuid::now_v7(), 30) {
                lo += 1;
            }
        }
        assert!((450..750).contains(&lo), "{lo}");
    }

    #[test]
    fn weighted_variants_follow_weights() {
        let vs = vec![
            Variant {
                name: "a".into(),
                weight: 1,
                ..Default::default()
            },
            Variant {
                name: "b".into(),
                weight: 3,
                ..Default::default()
            },
        ];
        let mut b = 0;
        for _ in 0..2000 {
            if pick_variant("x", Uuid::now_v7(), &vs).unwrap().name == "b" {
                b += 1;
            }
        }
        assert!((1350..1650).contains(&b), "{b}");
    }

    #[test]
    fn phases_and_recurrence() {
        let ev = LiveEvent {
            id: "e".into(),
            name: "e".into(),
            description: String::new(),
            enabled: true,
            phases: vec![
                Phase {
                    name: "warmup".into(),
                    offset_secs: 0,
                    duration_secs: 100,
                },
                Phase {
                    name: "main".into(),
                    offset_secs: 100,
                    duration_secs: 200,
                },
            ],
            starts_at: 1000,
            ends_at: None,
            recurrence_secs: Some(1000),
            local_time: false,
            audience: None,
            metadata: Value::Null,
            labels: vec![],
            participation: "implicit".into(),
            participation_secs: None,
            max_participants: None,
            template: None,
        };
        assert_eq!(phase_at(&ev, 500, 0), Err(Some(1000)));
        assert_eq!(phase_at(&ev, 1050, 0).unwrap().0, 0);
        assert_eq!(phase_at(&ev, 1250, 0).unwrap(), (1, 1100, 1300, 1000));
        assert_eq!(phase_at(&ev, 1500, 0), Err(Some(2000)));
        assert_eq!(phase_at(&ev, 2050, 0).unwrap().3, 2000);
        // local time: an account at UTC+2 sees the 1000-wall-clock start 7200 s earlier in UTC
        let mut local = ev.clone();
        local.local_time = true;
        local.recurrence_secs = None;
        assert_eq!(phase_at(&local, 1000 - 7200 + 10, 120).unwrap().0, 0);
        assert!(p_value(1.96) < 0.051 && p_value(1.96) > 0.049);
        assert!(p_value(0.0) > 0.99);
    }
}
