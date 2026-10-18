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
        .route("/v1/server/matches/{id}/backfill", post(backfill))
        .route("/v1/matches/replays", get(replays))
        .route("/v1/matches/{id}/replay", get(replay))
        .route("/v1/matches/{id}/replay/state", get(replay_state))
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

#[derive(serde::Deserialize)]
struct BackfillReq {
    /// Seats to offer to waiting matchmaker tickets (`0` closes the match for backfill).
    slots: usize,
}

/// `POST /v1/server/matches/{id}/backfill {slots}` — a running match formed by the matchmaker
/// asks for `slots` more players (members dropped, a bot seat opened): compatible waiting tickets
/// join it with the original token. `slots: 0` ends backfill for the match. Matches the
/// matchmaker did not form, or formed before the current leader, are not re-opened.
async fn backfill(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(req): Json<BackfillReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let known = s.relayed.token_match_known(&id);
    if !known {
        return Err(musubi_core::Error::NotFound("match formed by the matchmaker".into()).into());
    }
    s.fanout
        .mm_release(&id, (req.slots > 0).then_some(req.slots));
    Ok(Json(json!({ "ok": true, "slots": req.slots })))
}

/// Replay of a recorded match (`params.record = true` at creation): the recording header and
/// every kept frame in tick order — `snapshot` frames carry the module state (base64), `journal`
/// frames the processed inputs/membership steps between snapshots. Available while the match
/// is running (frames so far) and after it ended (`ended_at` set), until retention
/// (`[retention] replay_days`) removes it.
/// (match_id, module, label, params, started_at, ended_at, last_tick, participants)
type ReplayRow = (
    uuid::Uuid,
    String,
    String,
    serde_json::Value,
    String,
    Option<String>,
    i64,
    Vec<uuid::Uuid>,
);

#[derive(Deserialize, Default)]
struct ReplaysQuery {
    module: Option<String>,
    /// Only matches this account played in (default: the caller; `any` for everyone's).
    account_id: Option<String>,
    /// Only recordings started at or after / before this RFC 3339 time.
    since: Option<String>,
    before: Option<String>,
    limit: Option<i64>,
}

/// `GET /v1/matches/replays?module&account_id&since&before&limit` — recorded matches, newest
/// first: the caller's own by default, anyone's with `account_id` (or `any`), narrowed by module
/// and start time. Each row carries what `GET /v1/matches/{id}/replay` has in its header.
async fn replays(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Query(q): Query<ReplaysQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let account: Option<uuid::Uuid> = match q.account_id.as_deref() {
        None => Some(me.account_id.0),
        Some("any") => None,
        Some(a) => Some(
            a.parse()
                .map_err(|_| musubi_core::Error::InvalidArgument("account_id".into()))?,
        ),
    };
    let parse = |t: &Option<String>| -> Result<Option<time::OffsetDateTime>, ApiError> {
        Ok(match t {
            None => None,
            Some(t) => Some(
                time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                    .map_err(|_| musubi_core::Error::InvalidArgument("time".into()))?,
            ),
        })
    };
    let (since, before) = (parse(&q.since)?, parse(&q.before)?);
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    let rows: Vec<ReplayRow> = sqlx::query_as(
        "select match_id, module, label, params,
                    to_char(started_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),
                    to_char(ended_at at time zone 'utc', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),
                    last_tick, participants
             from musubi.match_recordings
             where ($1::uuid is null or participants @> array[$1::uuid])
               and ($2::text is null or module = $2)
               and ($3::timestamptz is null or started_at >= $3)
               and ($4::timestamptz is null or started_at < $4)
             order by started_at desc limit $5",
    )
    .bind(account)
    .bind(&q.module)
    .bind(since)
    .bind(before)
    .bind(limit)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let replays: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, module, label, params, started, ended, last_tick, participants)| {
                json!({
                    "match_id": id, "module": module, "label": label, "params": params,
                    "started_at": started, "ended_at": ended, "last_tick": last_tick,
                    "participants": participants, "live": s.matches.get(&id.to_string()).is_some(),
                })
            },
        )
        .collect();
    Ok(Json(json!({ "replays": replays })))
}

#[derive(Deserialize, Default)]
struct ReplayQuery {
    /// Only frames overlapping `[from_tick, to_tick]` (both inclusive, default: everything).
    from_tick: Option<i64>,
    to_tick: Option<i64>,
    /// At most this many frames (default 1000, max 10000); `next_tick` says where to resume.
    limit: Option<i64>,
}

async fn replay(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<String>,
    Query(q): Query<ReplayQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let from = q.from_tick.unwrap_or(0);
    let to = q.to_tick.unwrap_or(i64::MAX);
    let limit = q.limit.unwrap_or(1000).clamp(1, 10_000);
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
         where match_id = $1 and tick_to >= $2 and tick_from <= $3
         order by tick_from, case kind when 'snapshot' then 0 else 1 end limit $4",
        )
        .bind(mid)
        .bind(from)
        .bind(to)
        .bind(limit + 1)
        .fetch_all(&s.db)
        .await
        .map_err(musubi_core::Error::internal)?;
    let truncated = frames.len() as i64 > limit;
    let frames: Vec<_> = frames.into_iter().take(limit as usize).collect();
    let next_tick = truncated.then(|| frames.last().map(|f| f.2 + 1)).flatten();
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
        "live": s.matches.get(&id).is_some(), "frames": frames, "next_tick": next_tick,
    })))
}

#[derive(Deserialize)]
struct ReplayStateQuery {
    tick: i64,
}

/// `GET /v1/matches/{id}/replay/state?tick=N` — the module state of a recorded match as it was
/// after tick `N`, rebuilt on the server from the last snapshot at or before `N` plus the journal
/// up to it (the same replay the failover uses): scrubbing a replay needs no client-side module
/// and no download of every frame. Needs the module loaded on this node.
async fn replay_state(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<String>,
    Query(q): Query<ReplayStateQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mid: uuid::Uuid = id
        .parse()
        .map_err(|_| musubi_core::Error::NotFound("replay".into()))?;
    let (module, label0): (String, String) =
        sqlx::query_as("select module, label from musubi.match_recordings where match_id = $1")
            .bind(mid)
            .fetch_optional(&s.db)
            .await
            .map_err(musubi_core::Error::internal)?
            .ok_or_else(|| musubi_core::Error::NotFound("replay".into()))?;
    let handler = s.matches.handler(&module).ok_or_else(|| {
        musubi_core::Error::Unavailable(format!("module {module} is not loaded here"))
    })?;
    let snap: (i64, Vec<u8>) = sqlx::query_as(
        "select tick_from, state from musubi.match_recording_frames
         where match_id = $1 and kind = 'snapshot' and tick_from <= $2 and state is not null
         order by tick_from desc limit 1",
    )
    .bind(mid)
    .bind(q.tick)
    .fetch_optional(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?
    .ok_or_else(|| musubi_core::Error::NotFound("no snapshot at or before that tick".into()))?;
    let chunks: Vec<(serde_json::Value,)> = sqlx::query_as(
        "select entries from musubi.match_recording_frames
         where match_id = $1 and kind = 'journal' and tick_to > $2 and tick_from <= $3
         order by tick_from",
    )
    .bind(mid)
    .bind(snap.0)
    .bind(q.tick)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let mut entries: Vec<musubi_actor::JournalEntry> = Vec::new();
    for (v,) in chunks {
        if let Ok(mut e) = serde_json::from_value::<Vec<musubi_actor::JournalEntry>>(v) {
            e.retain(|e| e.tick as i64 <= q.tick);
            entries.append(&mut e);
        }
    }
    let applied = entries.len();
    let (state, label, tick) =
        musubi_actor::replay(handler.as_ref(), snap.1, label0, snap.0 as u64, &entries);
    use base64::Engine as _;
    Ok(Json(json!({
        "match_id": id, "module": module, "label": label, "tick": tick,
        "snapshot_tick": snap.0, "applied": applied,
        "state": base64::engine::general_purpose::STANDARD.encode(state),
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
