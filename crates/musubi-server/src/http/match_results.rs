//! End-game report (plan 5.7): the game server posts one result per match and Musubi fans it
//! out — rating queue, rank-based reward tables (under grant limits), quest events, progression
//! XP, the bus and modules. Idempotent per match id: a repeat returns the stored outcome.
use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use musubi_core::{AccountId, Error};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/server/matches/{id}/results", get(stored).post(report))
}

#[derive(Deserialize)]
struct Player {
    account_id: Uuid,
    #[serde(default)]
    stats: Value,
}

#[derive(Deserialize)]
struct Team {
    /// 1 = best; equal ranks are a draw. Default: team order.
    rank: Option<u32>,
    players: Vec<Player>,
}

#[derive(Deserialize, Default)]
struct ByRank {
    /// keys: "1", "2", … or "default"
    #[serde(flatten)]
    map: HashMap<String, Value>,
}

#[derive(Deserialize)]
struct Report {
    teams: Vec<Team>,
    /// Rating queue to update (`PUT /v1/server/rating/queues`); absent = no rating change.
    queue: Option<String>,
    /// Reward table per rank (`{"1": "gold_win", "default": "gold_played"}`).
    #[serde(default)]
    rewards: ByRank,
    /// Quest event fired for every player, `{name, delta?}`.
    quest_event: Option<QuestEvent>,
    /// XP per rank on a progression track.
    progression: Option<Progression>,
    /// Decide a tournament bracket match with this result: the rank-1 team's participant wins.
    #[serde(default)]
    bracket_match: Option<BracketRef>,
    /// Fold each player's `stats` object into their player statistics under this scope
    /// (`""` = global slice only); absent = stats are only echoed back.
    #[serde(default)]
    stats_scope: Option<String>,
    #[serde(default)]
    metadata: Value,
}

#[derive(Deserialize)]
struct BracketRef {
    tournament_id: String,
    match_id: Uuid,
}

#[derive(Deserialize)]
struct QuestEvent {
    name: String,
    #[serde(default = "one")]
    delta: i64,
}

#[derive(Deserialize)]
struct Progression {
    track_id: String,
    #[serde(default)]
    xp_by_rank: ByRank,
}

fn one() -> i64 {
    1
}

fn pick(m: &HashMap<String, Value>, rank: u32) -> Option<&Value> {
    m.get(&rank.to_string()).or_else(|| m.get("default"))
}

async fn stored(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row: Option<(Value, Value, time::OffsetDateTime)> = sqlx::query_as(
        "select payload, result, applied_at from musubi.match_results where match_id = $1",
    )
    .bind(&id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((payload, result, at)) = row else {
        return Err(Error::NotFound("match results".into()).into());
    };
    Ok(Json(
        json!({ "match_id": id, "payload": payload, "result": result,
        "applied_at": at.format(&time::format_description::well_known::Rfc3339).ok(), "replayed": true }),
    ))
}

async fn report(
    _k: ServerKey,
    State(s): State<AppState>,
    Path(id): Path<String>,
    Json(raw): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    if id.is_empty() || id.len() > 128 {
        return Err(Error::InvalidArgument("match id".into()).into());
    }
    let r: Report = serde_json::from_value(raw.clone())
        .map_err(|e| Error::InvalidArgument(format!("report: {e}")))?;
    if r.teams.is_empty() || r.teams.iter().any(|t| t.players.is_empty()) {
        return Err(Error::InvalidArgument("teams with players required".into()).into());
    }
    let ranks: Vec<u32> = r
        .teams
        .iter()
        .enumerate()
        .map(|(i, t)| t.rank.unwrap_or(i as u32 + 1))
        .collect();
    if ranks.contains(&0) {
        return Err(Error::InvalidArgument("ranks start at 1".into()).into());
    }
    // claim the match id first: a concurrent or repeated report sees the stored outcome
    let claimed = sqlx::query(
        "insert into musubi.match_results (match_id, payload, result) values ($1, $2, '{}'::jsonb)
         on conflict (match_id) do nothing",
    )
    .bind(&id)
    .bind(&raw)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if claimed == 0 {
        return stored(_k, State(s), Path(id)).await;
    }
    let mut out = json!({ "players": [], "rating": Value::Null, "errors": [] });
    if let Some(q) = &r.queue {
        let mr = musubi_leaderboard::MatchResult {
            teams: r
                .teams
                .iter()
                .map(|t| t.players.iter().map(|p| AccountId(p.account_id)).collect())
                .collect(),
            ranks: Some(ranks.clone()),
            match_id: Some(id.clone()),
        };
        match s.ratings.apply(q, mr).await {
            Ok(a) => out["rating"] = serde_json::to_value(a).map_err(Error::internal)?,
            Err(e) => out["errors"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "step": "rating", "error": e.to_string() })),
        }
    }
    let now = time::OffsetDateTime::now_utc();
    for (ti, t) in r.teams.iter().enumerate() {
        let rank = ranks[ti];
        for p in &t.players {
            let acc = AccountId(p.account_id);
            let mut pj = json!({ "account_id": acc, "team": ti, "rank": rank });
            if let Some(table) = pick(&r.rewards.map, rank).and_then(|v| v.as_str()) {
                let reference = format!("match:{id}");
                let res = async {
                    let booked =
                        crate::grant_limits::check(&s.db, acc, "reward_table", table, 1).await?;
                    match s.economy.grant_table(acc, table, &reference).await {
                        Ok(rw) => Ok::<_, Error>(rw),
                        Err(e) => {
                            crate::grant_limits::release(&s.db, &booked).await?;
                            Err(e)
                        }
                    }
                }
                .await;
                match res {
                    Ok(rw) => pj["rewards"] = json!({ "table": table, "entries": rw }),
                    Err(e) => pj["rewards_error"] = json!(e.to_string()),
                }
            }
            if let Some(qe) = &r.quest_event {
                match s.economy.quest_event(acc, &qe.name, qe.delta, now).await {
                    Ok(res) => pj["quests"] = serde_json::to_value(res).unwrap_or(Value::Null),
                    Err(e) => pj["quests_error"] = json!(e.to_string()),
                }
                pj["challenges"] =
                    crate::http::challenges::fan_out_event(&s, acc, &qe.name, qe.delta).await;
            }
            if let Some(pr) = &r.progression {
                if let Some(xp) = pick(&pr.xp_by_rank.map, rank).and_then(|v| v.as_i64()) {
                    if xp > 0 {
                        match s
                            .economy
                            .add_xp(acc, &pr.track_id, xp, &format!("match:{id}"))
                            .await
                        {
                            Ok(res) => pj["xp"] = serde_json::to_value(res).unwrap_or(Value::Null),
                            Err(e) => pj["xp_error"] = json!(e.to_string()),
                        }
                    }
                }
            }
            pj["stats"] = p.stats.clone();
            if let (Some(scope), Some(obj)) = (&r.stats_scope, p.stats.as_object()) {
                let nums: std::collections::BTreeMap<String, f64> = obj
                    .iter()
                    .filter_map(|(k, v)| v.as_f64().map(|n| (k.clone(), n)))
                    .collect();
                if !nums.is_empty() {
                    match crate::http::stats::apply(&s.db, acc, scope, &nums).await {
                        Ok(res) => pj["stats_applied"] = res,
                        Err(e) => pj["stats_error"] = json!(e.to_string()),
                    }
                }
            }
            out["players"].as_array_mut().unwrap().push(pj);
        }
    }
    if let Some(bm) = &r.bracket_match {
        let best = ranks
            .iter()
            .enumerate()
            .min_by_key(|(_, rk)| **rk)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let ids: Vec<Uuid> = r.teams[best].players.iter().map(|p| p.account_id).collect();
        let res = s
            .tournaments
            .bracket_update(&bm.tournament_id, |b| {
                use musubi_leaderboard::bracket::Slot;
                let m = b
                    .matches
                    .iter()
                    .find(|m| m.id == bm.match_id)
                    .ok_or_else(|| Error::NotFound(format!("bracket match {}", bm.match_id)))?;
                let winner = [m.a, m.b]
                    .into_iter()
                    .find_map(|sl| match sl {
                        Slot::Player(p) if ids.contains(&p) => Some(p),
                        _ => None,
                    })
                    .ok_or_else(|| {
                        Error::InvalidArgument(
                            "the winning team has no participant of this bracket match".into(),
                        )
                    })?;
                musubi_leaderboard::bracket::report(
                    b,
                    bm.match_id,
                    winner,
                    json!({ "ranks": ranks }),
                    Some(id.clone()),
                )
                .map(|ready| (winner, ready))
            })
            .await;
        match res {
            Ok((b, (winner, ready))) => {
                super::tournaments::emit_bracket(
                    &s,
                    &bm.tournament_id,
                    bm.match_id,
                    winner,
                    &ready,
                    b.champion,
                )
                .await;
                out["bracket"] = json!({ "tournament_id": bm.tournament_id, "match": bm.match_id,
                    "winner": winner, "ready": ready, "champion": b.champion });
            }
            Err(e) => out["bracket_error"] = json!(e.to_string()),
        }
    }
    sqlx::query("update musubi.match_results set result = $2 where match_id = $1")
        .bind(&id)
        .bind(&out)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    s.events
        .emit(
            "match.results",
            None,
            json!({ "match_id": id, "queue": r.queue, "ranks": ranks, "metadata": r.metadata,
                "players": out["players"].as_array().map(|a| a.len()).unwrap_or(0) }),
        )
        .await;
    if s.runtime.has_hook("hook_match_results") {
        let rt = s.runtime.clone();
        let payload = json!({ "match_id": id, "report": raw, "result": out });
        tokio::spawn(async move {
            rt.event(
                "match_results",
                musubi_runtime::CallContext::default(),
                payload,
            )
            .await
        });
    }
    out["match_id"] = json!(id);
    out["replayed"] = json!(false);
    Ok(Json(out))
}
