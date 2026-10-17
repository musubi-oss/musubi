use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use musubi_actor::MatchInfo;
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/matches", get(list))
        .route("/v1/server/matches", post(create))
        .route("/v1/server/matches/{id}", get(get_one).delete(terminate))
        .route("/v1/matches/{id}/replay", get(replay))
        .route(
            "/v1/server/matches/{id}/replay",
            axum::routing::delete(delete_replay),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    min_size: Option<u32>,
    #[serde(default)]
    max_size: Option<u32>,
    #[serde(default = "default_limit")]
    limit: usize,
    /// Matchmaker-style query over the label, e.g. `+label.mode:ranked +label.open:1`.
    #[serde(default)]
    query: Option<String>,
}

fn default_limit() -> usize {
    50
}

#[derive(Deserialize)]
struct CreateReq {
    module: String,
    #[serde(default)]
    params: serde_json::Value,
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let query = q
        .query
        .as_deref()
        .filter(|q| !q.trim().is_empty() && q.trim() != "*")
        .map(musubi_matchmaker::Query::parse)
        .transpose()?;
    let matches: Vec<MatchInfo> = s
        .matches
        .list_query(
            q.label.as_deref(),
            query.as_ref(),
            q.min_size,
            q.max_size,
            q.limit.clamp(1, 100),
        )
        .await;
    Ok(Json(json!({ "matches": matches })))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateReq>,
) -> Result<Json<MatchInfo>, ApiError> {
    let h = s.matches.create(s.hub.clone(), &r.module, r.params)?;
    Ok(Json(h.info().await))
}

async fn get_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<MatchInfo>, ApiError> {
    let h = s
        .matches
        .get(&id)
        .ok_or_else(|| musubi_core::Error::NotFound("match".into()))?;
    Ok(Json(h.info().await))
}

async fn terminate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let h = s
        .matches
        .get(&id)
        .ok_or_else(|| musubi_core::Error::NotFound("match".into()))?;
    h.terminate(0);
    Ok(Json(json!({ "ok": true })))
}

/// Replay of a recorded match (`params.record = true` at creation): the recording header and
/// every kept frame in tick order — `snapshot` frames carry the module state (base64), `journal`
/// frames the processed inputs/membership steps between snapshots. Available while the match
/// is running (frames so far) and after it ended (`ended_at` set), until retention
/// (`[retention] replay_days`) removes it.
async fn replay(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mid: uuid::Uuid = id
        .parse()
        .map_err(|_| musubi_core::Error::NotFound("replay".into()))?;
    let head = sqlx::query_as::<_, (String, String, serde_json::Value, String, Option<String>, i64)>(
        "select module, label, params, to_char(started_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),
                to_char(ended_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'), last_tick
         from musubi.match_recordings where match_id = $1",
    )
    .bind(mid)
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?
    .ok_or_else(|| musubi_core::Error::NotFound("replay".into()))?;
    let frames =
        sqlx::query_as::<_, (String, i64, i64, Option<Vec<u8>>, Option<serde_json::Value>)>(
            "select kind, tick_from, tick_to, state, entries from musubi.match_recording_frames
         where match_id = $1 order by tick_from, case kind when 'snapshot' then 0 else 1 end",
        )
        .bind(mid)
        .fetch_all(&s.db)
        .await
        .map_err(musubi_core::Error::internal)?;
    use base64::Engine as _;
    let frames: Vec<serde_json::Value> = frames
        .into_iter()
        .map(|(kind, from, to, state, entries)| {
            let mut f = json!({ "kind": kind, "tick_from": from, "tick_to": to });
            if let Some(st) = state {
                f["state"] = json!(base64::engine::general_purpose::STANDARD.encode(st));
            }
            if let Some(e) = entries {
                f["entries"] = e;
            }
            f
        })
        .collect();
    Ok(Json(json!({
        "match_id": id, "module": head.0, "label": head.1, "params": head.2,
        "started_at": head.3, "ended_at": head.4, "last_tick": head.5,
        "live": s.matches.get(&id).is_some(), "frames": frames,
    })))
}

async fn delete_replay(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mid: uuid::Uuid = id
        .parse()
        .map_err(|_| musubi_core::Error::NotFound("replay".into()))?;
    let n = sqlx::query("delete from musubi.match_recordings where match_id = $1")
        .bind(mid)
        .execute(&s.db)
        .await
        .map_err(musubi_core::Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(musubi_core::Error::NotFound("replay".into()).into());
    }
    Ok(Json(json!({ "ok": true })))
}
