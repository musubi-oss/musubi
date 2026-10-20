use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_leaderboard::{CreateLeaderboard, Leaderboard, Record, RecordList, WriteRecord};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/leaderboards", get(list))
        .route("/v1/leaderboards/{id}", get(get_board))
        .route(
            "/v1/leaderboards/{id}/records",
            get(records).post(write).delete(delete_own),
        )
        .route("/v1/leaderboards/{id}/around/{owner}", get(around))
        .route("/v1/leaderboards/{id}/windows", get(windows))
        .route("/v1/leaderboards/{id}/tier", get(tier))
        .route("/v1/leaderboards/{id}/cohort", get(cohort))
        .route("/v1/leaderboards/{id}/reroll", post(reroll))
        // server-to-server
        .route("/v1/server/leaderboards", post(create))
        .route("/v1/server/leaderboards/{id}", delete(delete_board))
        .route("/v1/server/leaderboards/{id}/rollover", post(rollover))
        .route(
            "/v1/server/leaderboards/{id}/records/{owner}",
            post(write_as).delete(delete_as),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RecordsQuery {
    #[serde(default)]
    owner_ids: Option<String>,
    /// Only the caller's friends (their records, ranked on the full board).
    #[serde(default)]
    friends: bool,
    /// Only members of this group.
    #[serde(default)]
    group_id: Option<uuid::Uuid>,
    /// Rank range (1-based, inclusive) instead of cursor paging.
    #[serde(default)]
    rank_from: Option<i64>,
    #[serde(default)]
    rank_to: Option<i64>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    expiry: Option<i64>,
}

fn default_limit() -> usize {
    50
}

fn parse_ids(s: Option<&str>) -> Result<Vec<AccountId>, ApiError> {
    s.unwrap_or("")
        .split(',')
        .filter(|x| !x.is_empty())
        .map(|x| {
            x.parse::<AccountId>()
                .map_err(|_| musubi_core::Error::InvalidArgument(format!("bad id {x}")).into())
        })
        .collect()
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (leaderboards, cursor) = s.leaderboards.list(q.limit, q.cursor.as_deref()).await?;
    Ok(Json(
        json!({ "leaderboards": leaderboards, "cursor": cursor }),
    ))
}

async fn get_board(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Leaderboard>, ApiError> {
    Ok(Json(s.leaderboards.get(&id).await?))
}

async fn records(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    let mut owners = parse_ids(q.owner_ids.as_deref())?;
    let filtered = q.friends || q.group_id.is_some();
    if q.friends {
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "select friend_id from musubi.friends where user_id = $1 and state = 0 limit 1000",
        )
        .bind(me.account_id.0)
        .fetch_all(&s.db)
        .await
        .map_err(musubi_core::Error::internal)?;
        owners.push(me.account_id);
        owners.extend(ids.into_iter().map(AccountId));
    }
    if let Some(g) = q.group_id {
        let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
            "select user_id from musubi.group_members where group_id = $1 and state < 3 limit 10000",
        )
        .bind(g)
        .fetch_all(&s.db)
        .await
        .map_err(musubi_core::Error::internal)?;
        owners.extend(ids.into_iter().map(AccountId));
    }
    let (mut limit, mut cursor) = (q.limit, q.cursor.clone());
    if let Some(from) = q.rank_from {
        if from < 1 {
            return Err(musubi_core::Error::InvalidArgument("rank_from starts at 1".into()).into());
        }
        let to = q.rank_to.unwrap_or(from + q.limit as i64 - 1).max(from);
        limit = ((to - from + 1) as usize).min(100);
        cursor = Some((from - 1).to_string());
    }
    let mut list = s
        .leaderboards
        .records(
            &id,
            &owners,
            limit,
            cursor.as_deref(),
            q.expiry,
            Some(me.account_id),
        )
        .await?;
    if filtered {
        // the page is the filtered set itself, each record keeping its rank on the full board
        let mut page = std::mem::take(&mut list.owner_records);
        page.sort_by_key(|r| r.rank);
        page.truncate(q.limit.clamp(1, 100));
        list.records = page;
        list.next_cursor = None;
        list.prev_cursor = None;
    }
    Ok(Json(list))
}

/// Past windows of a board (newest first) so a client can ask for `expiry=` of a finished
/// season; the current window comes first with `current: true`.
async fn windows(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let board = s.leaderboards.get(&id).await?;
    let rows: Vec<(time::OffsetDateTime, i64, Option<time::OffsetDateTime>)> = sqlx::query_as(
        "select r.expiry_time, count(*)::bigint, max(e.fired_at)
         from musubi.leaderboard_records r
         left join musubi.leaderboard_ends e
           on e.leaderboard_id = r.leaderboard_id and e.expiry_time = r.expiry_time
         where r.leaderboard_id = $1 group by r.expiry_time order by r.expiry_time desc limit 100",
    )
    .bind(&id)
    .fetch_all(&s.db)
    .await
    .map_err(musubi_core::Error::internal)?;
    let now = time::OffsetDateTime::now_utc();
    let windows: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(exp, n, ended)| {
            let e = exp.unix_timestamp();
            serde_json::json!({
                "expiry": e,
                "records": n,
                "current": e == 0 || exp > now,
                "ended_at": ended.map(|t| t.unix_timestamp()),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "leaderboard_id": board.id,
        "reset_schedule": board.reset_schedule,
        "windows": windows,
    })))
}

async fn around(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path((id, owner)): Path<(String, AccountId)>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    Ok(Json(
        s.leaderboards.around(&id, owner, q.limit, q.expiry).await?,
    ))
}

async fn write(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    s.banned.check_feature(me.account_id, "leaderboards")?;
    let username = s
        .accounts
        .get_users(&[me.account_id], &[])
        .await?
        .pop()
        .map(|u| u.username);
    let (r, closed) = s
        .leaderboards
        .write_ext(&id, me.account_id, username.as_deref(), w, false)
        .await?;
    s.events
        .emit(
            "leaderboard.record",
            Some(me.account_id),
            json!({ "leaderboard_id": id, "record": r }),
        )
        .await;
    if closed {
        s.events
            .emit(
                "leaderboard.cohort_finished",
                Some(me.account_id),
                json!({ "leaderboard_id": id, "tier": r.tier, "bucket": r.bucket, "expiry_time": r.expiry_time }),
            )
            .await;
    }
    Ok(Json(r))
}

/// The caller's race cohort: members, finishers, closed, time left, rolls.
async fn cohort(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<musubi_leaderboard::Cohort>, ApiError> {
    Ok(Json(s.leaderboards.cohort(&id, me.account_id).await?))
}

/// Re-roll into a fresh cohort once the current one is over (finished cohort, target reached
/// or time limit out), within `max_rolls` and after the cooldown.
async fn reroll(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<musubi_leaderboard::Cohort>, ApiError> {
    s.banned.check_feature(me.account_id, "leaderboards")?;
    let c = s.leaderboards.reroll(&id, me.account_id).await?;
    s.events
        .emit(
            "leaderboard.reroll",
            Some(me.account_id),
            json!({ "leaderboard_id": id, "tier": c.tier, "bucket": c.bucket, "rolls": c.rolls }),
        )
        .await;
    Ok(Json(c))
}

async fn delete_own(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.leaderboards
        .delete_record(&id, me.account_id, false)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<CreateLeaderboard>,
) -> Result<Json<Leaderboard>, ApiError> {
    Ok(Json(s.leaderboards.create(c).await?))
}

async fn delete_board(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.leaderboards.delete(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn write_as(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, owner)): Path<(String, AccountId)>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let username = s
        .accounts
        .get_users(&[owner], &[])
        .await?
        .pop()
        .map(|u| u.username);
    let r = s
        .leaderboards
        .write(&id, owner, username.as_deref(), w, true)
        .await?;
    s.events
        .emit(
            "leaderboard.record",
            Some(owner),
            json!({ "leaderboard_id": id, "record": r, "server": true }),
        )
        .await;
    Ok(Json(r))
}

async fn delete_as(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, owner)): Path<(String, AccountId)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.leaderboards.delete_record(&id, owner, true).await?;
    Ok(Json(json!({ "ok": true })))
}

/// League card of the caller: tier, window phase (resting / ending soon), rank in the group and
/// the projected move at the end of the window.
async fn tier(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(
        s.leaderboards
            .tier_status(&id, me.account_id, time::OffsetDateTime::now_utc())
            .await?,
    ))
}

#[derive(Deserialize, Default)]
struct RolloverBody {
    #[serde(default)]
    expiry: Option<i64>,
}

/// Close a league window now (default: the last finished one; `expiry` of the current window ends
/// the season early). Returns null when that window was already rolled over.
async fn rollover(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    body: Option<Json<RolloverBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let expiry = body.and_then(|b| b.0.expiry);
    let Some(mut r) = s.leaderboards.rollover(&id, expiry).await? else {
        return Ok(Json(json!({ "rollover": null })));
    };
    let grants = std::mem::take(&mut r.rewards);
    let mut rewarded = 0u32;
    for (account, table) in grants {
        if s.economy
            .grant_table(account, &table, &format!("league:{id}:{}", r.expiry_time))
            .await
            .is_ok()
        {
            rewarded += 1;
        }
    }
    let mut v = serde_json::to_value(&r).map_err(musubi_core::Error::internal)?;
    v["rewarded"] = json!(rewarded);
    s.events
        .emit(
            "league.rollover",
            None,
            json!({ "leaderboard_id": id, "expiry_time": r.expiry_time, "promoted": r.promoted,
                    "demoted": r.demoted, "idle_demoted": r.idle_demoted, "rewarded": rewarded }),
        )
        .await;
    Ok(Json(json!({ "rollover": v })))
}
