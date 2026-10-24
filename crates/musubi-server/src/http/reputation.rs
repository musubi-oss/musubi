//! Server reputation (Steamworks `GetServerReputation` / `GetGameplayStats` parity): game servers
//! report gameplay statistics about themselves (`POST /v1/gameserver/gameplay_stats`, cumulative
//! deltas) and operators read a reputation card per server or fleet — player ratings from the
//! server browser, the gameplay totals and a 0..100 score.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_core::Error;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::{ApiError, GameServerAuth, ServerKey};
use crate::app::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/gameserver/gameplay_stats", post(report))
        .route(
            "/v1/server/gameservers/{id}/reputation",
            get(server_reputation),
        )
        .route("/v1/server/fleets/{id}/reputation", get(fleet_reputation))
}

/// Largest delta one report may add per counter (~31 years of `play_secs`): a misbehaving server
/// cannot push the bigint totals to overflow and wedge every later report with a DB error.
const MAX_DELTA: i64 = 1_000_000_000_000;

#[derive(Deserialize, Default)]
#[serde(default)]
struct Report {
    /// Deltas since the previous report (all ≥ 0).
    rounds: i64,
    players: i64,
    play_secs: i64,
    kicks: i64,
    crashes: i64,
}

/// `POST /v1/gameserver/gameplay_stats {rounds, players, play_secs, kicks, crashes}` with
/// `X-Musubi-Server-Id` — the server adds what happened since its last report.
async fn report(
    State(s): State<AppState>,
    GameServerAuth(sid): GameServerAuth,
    body: Option<Json<Report>>,
) -> Result<Json<Value>, ApiError> {
    let id = sid;
    let b = body.map(|b| b.0).unwrap_or_default();
    if [b.rounds, b.players, b.play_secs, b.kicks, b.crashes]
        .iter()
        .any(|v| !(0..=MAX_DELTA).contains(v))
    {
        return Err(
            Error::InvalidArgument(format!("gameplay stats must be in 0..={MAX_DELTA}")).into(),
        );
    }
    let fleet: Option<(Uuid,)> = sqlx::query_as(
        "select fleet_id from musubi.game_servers where id = $1 and state <> 'terminated'",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    let Some((fleet_id,)) = fleet else {
        return Err(Error::NotFound("game server".into()).into());
    };
    let row: (i64, i64, i64, i64, i64, i32) = sqlx::query_as(
        "insert into musubi.server_gameplay_stats (server_id, fleet_id, rounds, players_served, play_secs, kicks, crashes, reports)
         values ($1, $2, $3, $4, $5, $6, $7, 1)
         on conflict (server_id) do update set
             rounds = least(musubi.server_gameplay_stats.rounds::numeric + excluded.rounds, 9223372036854775807)::bigint,
             players_served = least(musubi.server_gameplay_stats.players_served::numeric + excluded.players_served, 9223372036854775807)::bigint,
             play_secs = least(musubi.server_gameplay_stats.play_secs::numeric + excluded.play_secs, 9223372036854775807)::bigint,
             kicks = least(musubi.server_gameplay_stats.kicks::numeric + excluded.kicks, 9223372036854775807)::bigint,
             crashes = least(musubi.server_gameplay_stats.crashes::numeric + excluded.crashes, 9223372036854775807)::bigint,
             reports = least(musubi.server_gameplay_stats.reports + 1, 2147483647), updated_at = now()
         returning rounds, players_served, play_secs, kicks, crashes, reports",
    )
    .bind(id)
    .bind(fleet_id)
    .bind(b.rounds)
    .bind(b.players)
    .bind(b.play_secs)
    .bind(b.kicks)
    .bind(b.crashes)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "server_id": id, "rounds": row.0, "players_served": row.1, "play_secs": row.2,
        "kicks": row.3, "crashes": row.4, "reports": row.5 }),
    ))
}

/// 0..100: 60 % player rating (1..5 → 0..1; no votes = neutral 0.6), 25 % stability
/// (crashes per 10 rounds), 15 % moderation (kicks per 100 players served).
pub fn score(
    avg_rating: Option<f64>,
    votes: i64,
    rounds: i64,
    crashes: i64,
    players: i64,
    kicks: i64,
) -> f64 {
    let rating = match avg_rating {
        Some(a) if votes > 0 => ((a - 1.0) / 4.0).clamp(0.0, 1.0),
        _ => 0.6,
    };
    let stability = if rounds == 0 {
        1.0
    } else {
        (1.0 - (crashes as f64 * 10.0 / rounds as f64)).clamp(0.0, 1.0)
    };
    let moderation = if players == 0 {
        1.0
    } else {
        (1.0 - (kicks as f64 * 100.0 / players as f64) / 50.0).clamp(0.0, 1.0)
    };
    ((rating * 0.6 + stability * 0.25 + moderation * 0.15) * 100.0).round()
}

/// address, port, rating avg, votes, rounds, players, play_secs, kicks, crashes, reports
type CardRow = (String, i32, Option<f64>, i64, i64, i64, i64, i64, i64, i32);

async fn card(s: &AppState, server_id: Uuid) -> Result<Option<Value>, ApiError> {
    let row: Option<CardRow> = sqlx::query_as(
        "select g.address::text, g.port, r.avg, r.votes,
                coalesce(st.rounds, 0), coalesce(st.players_served, 0), coalesce(st.play_secs, 0),
                coalesce(st.kicks, 0), coalesce(st.crashes, 0), coalesce(st.reports, 0)
         from musubi.game_servers g
         left join lateral (select avg(score)::float8 as avg, count(*)::bigint as votes
                            from musubi.server_ratings sr where sr.key = g.address || ':' || g.port) r on true
         left join musubi.server_gameplay_stats st on st.server_id = g.id
         where g.id = $1",
    )
    .bind(server_id)
    .fetch_optional(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(row.map(
        |(address, port, avg, votes, rounds, players, play_secs, kicks, crashes, reports)| {
            json!({
                "server_id": server_id, "address": address, "port": port,
                "rating": { "avg": avg, "votes": votes },
                "gameplay": { "rounds": rounds, "players_served": players, "play_secs": play_secs,
                              "kicks": kicks, "crashes": crashes, "reports": reports },
                "score": score(avg, votes, rounds, crashes, players, kicks),
            })
        },
    ))
}

/// `GET /v1/server/gameservers/{id}/reputation`
async fn server_reputation(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    card(&s, id)
        .await?
        .map(Json)
        .ok_or_else(|| Error::NotFound("game server".into()).into())
}

/// `GET /v1/server/fleets/{id}/reputation` — every live server's card plus fleet totals.
async fn fleet_reputation(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.fleets.get(id).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select id from musubi.game_servers where fleet_id = $1 and state <> 'terminated' order by created_at",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut servers = Vec::new();
    for sid in ids {
        if let Some(c) = card(&s, sid).await? {
            servers.push(c);
        }
    }
    let totals: (i64, i64, i64, i64, i64) = sqlx::query_as(
        "select coalesce(sum(rounds), 0)::bigint, coalesce(sum(players_served), 0)::bigint, coalesce(sum(play_secs), 0)::bigint,
                coalesce(sum(kicks), 0)::bigint, coalesce(sum(crashes), 0)::bigint
         from musubi.server_gameplay_stats where fleet_id = $1",
    )
    .bind(id)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let avg_score = if servers.is_empty() {
        None
    } else {
        Some(
            servers
                .iter()
                .filter_map(|c| c["score"].as_f64())
                .sum::<f64>()
                / servers.len() as f64,
        )
    };
    Ok(Json(json!({
        "fleet_id": id, "servers": servers, "score": avg_score,
        "gameplay": { "rounds": totals.0, "players_served": totals.1, "play_secs": totals.2,
                      "kicks": totals.3, "crashes": totals.4 },
    })))
}
