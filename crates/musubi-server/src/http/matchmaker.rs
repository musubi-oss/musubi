//! Server-key API for matchmaker queues: inspect and change queue rules at runtime and read what
//! is waiting. Queues from `[[matchmaker.queues]]` are loaded at boot; changes made here live on
//! this node (the matchmaker leader forms the matches, so apply them where the leader runs or
//! put them in the config of every node).

use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_matchmaker::QueueConfig;
use serde_json::{json, Value};

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/server/matchmaker/queues",
            get(list_queues).put(put_queue),
        )
        .route("/v1/server/matchmaker/queues/{name}", delete(delete_queue))
        .route("/v1/server/matchmaker/stats", get(stats))
        .route("/v1/server/matchmaker/simulate", post(simulate))
        .route(
            "/v1/server/matchmaker/tickets/{id}/trace",
            get(ticket_trace),
        )
        .route("/v1/server/matchmaker/matches", get(matches))
        .route("/v1/server/matchmaker/rejections", get(rejections))
}

async fn list_queues(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "queues": s.matchmaker.queues() })))
}

async fn put_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(q): Json<QueueConfig>,
) -> Result<Json<Value>, ApiError> {
    s.matchmaker.set_queue(q.clone())?;
    Ok(Json(json!({ "queue": q })))
}

async fn delete_queue(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(name): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !s.matchmaker.remove_queue(&name) {
        return Err(musubi_core::Error::NotFound(format!("queue {name} does not exist")).into());
    }
    Ok(Json(json!({ "removed": name })))
}

async fn stats(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "waiting": s.matchmaker.waiting(),
        "waiting_ages": s.matchmaker.waiting_ages(),
        "wait": s.matchmaker.wait_stats(),
        "open_matches": s.matchmaker.open_count(),
        "queues": s.matchmaker.queues(),
    })))
}

#[derive(serde::Deserialize)]
struct SimRegion {
    name: String,
    /// Relative share of arrivals (normalised).
    #[serde(default = "one")]
    share: f64,
    /// Ping to this region for players from here; others get `far_ms`.
    #[serde(default = "thirty")]
    ping_ms: f64,
    #[serde(default = "far")]
    far_ms: f64,
}

fn one() -> f64 {
    1.0
}
fn thirty() -> f64 {
    30.0
}
fn far() -> f64 {
    120.0
}
fn sixty() -> u64 {
    60
}
fn tick() -> u64 {
    1000
}

#[derive(serde::Deserialize)]
struct SimulateReq {
    queue: String,
    /// Arrivals per second (fractional is fine).
    arrivals_per_sec: f64,
    #[serde(default = "sixty")]
    duration_secs: u64,
    #[serde(default = "tick")]
    tick_ms: u64,
    /// Normal MMR distribution; absent = no MMR property.
    #[serde(default)]
    mmr_mean: Option<f64>,
    #[serde(default)]
    mmr_sd: Option<f64>,
    #[serde(default)]
    regions: Vec<SimRegion>,
    /// Party sizes with relative shares, e.g. `{"1": 7, "2": 2, "4": 1}`; default solo.
    #[serde(default)]
    party_sizes: std::collections::BTreeMap<String, f64>,
    #[serde(default)]
    seed: u64,
}

/// Dry-run a queue's rules on virtual time: synthetic arrivals (MMR, region pings, parties)
/// flow through a private matchmaker with the live queue config; back come matches, wait
/// percentiles and match quality — tune `mmr`/`ping` expansion before players feel it.
async fn simulate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<SimulateReq>,
) -> Result<Json<Value>, ApiError> {
    use musubi_matchmaker::{AddTicket, Matchmaker};
    use rand::{Rng, SeedableRng};
    let cfg = s
        .matchmaker
        .queues()
        .into_iter()
        .find(|q| q.name == r.queue)
        .ok_or_else(|| musubi_core::Error::NotFound(format!("matchmaker queue {}", r.queue)))?;
    if !(r.arrivals_per_sec.is_finite() && r.arrivals_per_sec > 0.0)
        || r.duration_secs == 0
        || r.duration_secs > 3600
    {
        return Err(musubi_core::Error::InvalidArgument(
            "arrivals_per_sec > 0 and 1..=3600 duration_secs".into(),
        )
        .into());
    }
    let tick = std::time::Duration::from_millis(r.tick_ms.clamp(100, 10_000));
    let total_arrivals = r.arrivals_per_sec * r.duration_secs as f64;
    if total_arrivals > 50_000.0 {
        return Err(
            musubi_core::Error::InvalidArgument("at most 50 000 arrivals per run".into()).into(),
        );
    }
    let mmr_prop = cfg.mmr.as_ref().map(|m| m.property.clone());
    let ping_prefix = cfg.ping.as_ref().map(|p| p.prefix.clone());
    let mm = Matchmaker::new(std::time::Duration::from_secs(
        s.cfg.matchmaker.min_wait_secs,
    ));
    mm.set_queue(cfg.clone())?;
    let mut rng = rand::rngs::StdRng::seed_from_u64(r.seed);
    let region_total: f64 = r.regions.iter().map(|x| x.share.max(0.0)).sum();
    let parties: Vec<(usize, f64)> = r
        .party_sizes
        .iter()
        .filter_map(|(k, v)| k.parse::<usize>().ok().map(|n| (n.max(1), v.max(0.0))))
        .collect();
    let party_total: f64 = parties.iter().map(|p| p.1).sum();
    let pick = |rng: &mut rand::rngs::StdRng, items: &[(f64,)], total: f64| -> usize {
        let mut x = rng.gen::<f64>() * total;
        for (i, w) in items.iter().enumerate() {
            if x < w.0 {
                return i;
            }
            x -= w.0;
        }
        items.len().saturating_sub(1)
    };
    let region_w: Vec<(f64,)> = r.regions.iter().map(|x| (x.share.max(0.0),)).collect();
    let party_w: Vec<(f64,)> = parties.iter().map(|p| (p.1,)).collect();
    let gauss = |rng: &mut rand::rngs::StdRng| -> f64 {
        let u1: f64 = rng.gen::<f64>().max(1e-12);
        let u2: f64 = rng.gen::<f64>();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    };
    let start = std::time::Instant::now();
    let mut now = start;
    let end = start + std::time::Duration::from_secs(r.duration_secs);
    let mut carry = 0.0_f64;
    let mut arrivals = 0usize;
    let mut arrived_players = 0usize;
    let mut waits: Vec<f32> = Vec::new();
    let mut qualities: Vec<f32> = Vec::new();
    let mut gaps: Vec<f64> = Vec::new();
    let mut pings: Vec<f64> = Vec::new();
    let mut matches = 0usize;
    let mut matched_players = 0usize;
    let mut per_region: std::collections::BTreeMap<String, usize> = Default::default();
    while now < end {
        carry += r.arrivals_per_sec * tick.as_secs_f64();
        while carry >= 1.0 {
            carry -= 1.0;
            arrivals += 1;
            let size = if parties.is_empty() {
                1
            } else {
                parties[pick(&mut rng, &party_w, party_total)].0
            };
            let home = (region_total > 0.0).then(|| pick(&mut rng, &region_w, region_total));
            let party_mmr = r
                .mmr_mean
                .map(|m| m + gauss(&mut rng) * r.mmr_sd.unwrap_or(0.0));
            let mut members = Vec::with_capacity(size);
            for _ in 0..size {
                let mut strings = std::collections::HashMap::new();
                strings.insert("queue".to_string(), r.queue.clone());
                let mut numbers = std::collections::HashMap::new();
                if let (Some(p), Some(m)) = (&mmr_prop, party_mmr) {
                    numbers.insert(p.clone(), m + gauss(&mut rng) * 25.0);
                }
                if let (Some(prefix), Some(h)) = (&ping_prefix, home) {
                    for (i, reg) in r.regions.iter().enumerate() {
                        let base = if i == h { reg.ping_ms } else { reg.far_ms };
                        numbers.insert(
                            format!("{prefix}{}", reg.name),
                            (base + gauss(&mut rng) * base * 0.1).max(1.0),
                        );
                    }
                }
                let id = uuid::Uuid::now_v7();
                members.push(musubi_proto::realtime::MatchmakerUser {
                    presence: musubi_proto::UserPresence {
                        account_id: musubi_core::AccountId(id),
                        session_id: musubi_core::SessionId::new(),
                        username: format!("sim-{}", arrivals),
                        status: None,
                        rich: None,
                        invisible: false,
                    },
                    string_properties: strings,
                    numeric_properties: numbers,
                    team: None,
                });
            }
            arrived_players += size;
            let _ = mm.add_at(
                AddTicket {
                    party_id: (size > 1).then(|| uuid::Uuid::now_v7().to_string()),
                    members,
                    query: "*".into(),
                    min_count: cfg.min_count,
                    max_count: cfg.max_count,
                    count_multiple: 1,
                },
                now,
            );
        }
        for m in mm.process_at(now) {
            matches += 1;
            for t in &m.tickets {
                matched_players += t.members.len();
                waits.push(now.saturating_duration_since(t.created).as_secs_f32());
            }
            if let Some(q) = &m.quality {
                qualities.push(q.score);
                gaps.push(q.team_mmr_gap);
                pings.push(q.ping_ms);
            }
            *per_region
                .entry(m.region.clone().unwrap_or_else(|| "any".into()))
                .or_default() += 1;
        }
        now += tick;
    }
    waits.sort_by(|a, b| a.total_cmp(b));
    let pct = |v: &[f32], p: f64| -> f32 {
        if v.is_empty() {
            0.0
        } else {
            v[(((v.len() - 1) as f64) * p).round() as usize]
        }
    };
    let mean = |v: &[f64]| -> f64 {
        if v.is_empty() {
            0.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let qmean = if qualities.is_empty() {
        0.0
    } else {
        qualities.iter().map(|q| *q as f64).sum::<f64>() / qualities.len() as f64
    };
    let left = mm.waiting_ages_at(now);
    Ok(Json(json!({
        "queue": r.queue,
        "duration_secs": r.duration_secs,
        "arrivals": { "tickets": arrivals, "players": arrived_players },
        "matches": matches,
        "matched_players": matched_players,
        "still_waiting": left.get(&r.queue).cloned().unwrap_or_default(),
        "wait_secs": {
            "p50": pct(&waits, 0.5), "p95": pct(&waits, 0.95), "p99": pct(&waits, 0.99),
            "max": waits.last().copied().unwrap_or(0.0)
        },
        "quality": {
            "mean_score": qmean,
            "mean_team_mmr_gap": mean(&gaps),
            "mean_ping_ms": mean(&pings)
        },
        "matches_by_region": per_region,
    })))
}

/// Every few seconds, move finished ticket traces and formed matches from the matchmaker's
/// memory into `musubi.matchmaker_ticket_log` / `musubi.matchmaker_matches` (observability).
pub fn spawn_drain(db: sqlx::PgPool, mm: std::sync::Arc<musubi_matchmaker::Matchmaker>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            if let Err(e) = matchmaker_drain(&db, &mm).await {
                tracing::warn!(error = %e, "matchmaker trace drain failed");
            }
        }
    });
}

pub async fn matchmaker_drain(
    db: &sqlx::PgPool,
    mm: &musubi_matchmaker::Matchmaker,
) -> musubi_core::Result<()> {
    use musubi_core::Error;
    for t in mm.take_finished_traces() {
        sqlx::query(
            "insert into musubi.matchmaker_ticket_log (ticket, queue, accounts, outcome, wait_ms, events)
             values ($1, $2, $3, $4, $5, $6) on conflict (ticket) do nothing",
        )
        .bind(&t.ticket)
        .bind(&t.queue)
        .bind(t.accounts.iter().map(|a| a.0).collect::<Vec<uuid::Uuid>>())
        .bind(&t.outcome)
        .bind(t.wait_ms as i64)
        .bind(serde_json::to_value(&t.events).unwrap_or_default())
        .execute(db)
        .await
        .map_err(Error::internal)?;
    }
    for m in mm.take_matches() {
        sqlx::query(
            "insert into musubi.matchmaker_matches (token, queue, region, quality, team_mmr_gap, tickets, accounts, wait_ms)
             values ($1, $2, $3, $4, $5, $6, $7, $8) on conflict (token) do nothing",
        )
        .bind(&m.token)
        .bind(&m.queue)
        .bind(&m.region)
        .bind(m.quality)
        .bind(m.team_mmr_gap)
        .bind(&m.tickets)
        .bind(m.accounts.iter().map(|a| a.0).collect::<Vec<uuid::Uuid>>())
        .bind(m.wait_ms.iter().map(|w| *w as i64).collect::<Vec<i64>>())
        .execute(db)
        .await
        .map_err(Error::internal)?;
    }
    Ok(())
}

/// `GET /v1/server/matchmaker/tickets/{id}/trace` — live decision trace of a waiting ticket or the
/// persisted one of a finished ticket.
async fn ticket_trace(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if let Some(t) = s.matchmaker.trace(&id) {
        return Ok(Json(json!({ "live": t.outcome == "waiting", "trace": t })));
    }
    matchmaker_drain(&s.db, &s.matchmaker).await?;
    type R = (
        String,
        Option<String>,
        Vec<uuid::Uuid>,
        String,
        i64,
        Value,
        i64,
    );
    let row: Option<R> = sqlx::query_as(
        "select ticket, queue, accounts, outcome, wait_ms, events, extract(epoch from finished_at)::bigint
         from musubi.matchmaker_ticket_log where ticket = $1",
    )
    .bind(&id)
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let Some((ticket, queue, accounts, outcome, wait_ms, events, at)) = row else {
        return Err(musubi_core::Error::NotFound(format!("ticket {id}")).into());
    };
    Ok(Json(json!({ "live": false, "trace": {
        "ticket": ticket, "queue": queue, "accounts": accounts, "outcome": outcome,
        "wait_ms": wait_ms, "events": events, "finished_at": at,
    }})))
}

#[derive(serde::Deserialize)]
struct MatchesQ {
    token: Option<String>,
    queue: Option<String>,
    account_id: Option<uuid::Uuid>,
    limit: Option<i64>,
}

type MatchRow = (
    String,
    Option<String>,
    Option<String>,
    Option<f32>,
    Option<f64>,
    Vec<String>,
    Vec<uuid::Uuid>,
    Vec<i64>,
    i64,
);

/// `GET /v1/server/matchmaker/matches?token|queue|account_id&limit` — stored matchmaking results.
async fn matches(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    axum::extract::Query(q): axum::extract::Query<MatchesQ>,
) -> Result<Json<Value>, ApiError> {
    matchmaker_drain(&s.db, &s.matchmaker).await?;
    let rows: Vec<MatchRow> = sqlx::query_as(
        "select token, queue, region, quality, team_mmr_gap, tickets, accounts, wait_ms, extract(epoch from created_at)::bigint
         from musubi.matchmaker_matches
         where ($1::text is null or token = $1) and ($2::text is null or queue = $2)
           and ($3::uuid is null or $3 = any(accounts))
         order by created_at desc limit $4",
    )
    .bind(q.token.as_deref())
    .bind(q.queue.as_deref())
    .bind(q.account_id)
    .bind(q.limit.unwrap_or(50).clamp(1, 500))
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let matches: Vec<Value> = rows
        .into_iter()
        .map(|(token, queue, region, quality, gap, tickets, accounts, wait_ms, at)| {
            json!({
                "token": token, "queue": queue, "region": region, "quality": quality, "team_mmr_gap": gap,
                "tickets": tickets, "accounts": accounts, "wait_ms": wait_ms, "created_at": at,
            })
        })
        .collect();
    Ok(Json(json!({ "matches": matches })))
}

/// `GET /v1/server/matchmaker/rejections` — why tickets were kept apart, cumulative per queue × rule.
async fn rejections(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    let _ = s.matchmaker.take_rejections();
    let rejections: Vec<Value> = s
        .matchmaker
        .rejection_totals()
        .into_iter()
        .map(|(q, r, n)| json!({ "queue": q, "reason": r, "count": n }))
        .collect();
    Ok(Json(json!({ "rejections": rejections })))
}
