use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::AccountId;
use musubi_leaderboard::{
    CreateTournament, Record, RecordList, Tournament, TournamentFilter, WriteRecord,
};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/tournaments", get(list))
        .route("/v1/tournaments/{id}", get(get_one))
        .route("/v1/tournaments/{id}/join", post(join))
        .route("/v1/tournaments/{id}/records", get(records).post(write))
        .route("/v1/tournaments/{id}/around/{owner}", get(around))
        .route("/v1/server/tournaments", post(create))
        .route("/v1/server/tournaments/{id}", delete(delete_one))
        .route("/v1/server/tournaments/{id}/end", post(end))
        .route(
            "/v1/server/tournaments/{id}/records/{owner}",
            post(write_as),
        )
}

#[derive(Deserialize)]
struct ListQuery {
    #[serde(default)]
    category_start: Option<i32>,
    #[serde(default)]
    category_end: Option<i32>,
    #[serde(default)]
    start_time: Option<i64>,
    #[serde(default)]
    end_time: Option<i64>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct RecordsQuery {
    #[serde(default)]
    owner_ids: Option<String>,
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

async fn username(s: &AppState, id: AccountId) -> Result<Option<String>, ApiError> {
    Ok(s.accounts
        .get_users(&[id], &[])
        .await?
        .pop()
        .map(|u| u.username))
}

async fn list(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Query(q): Query<ListQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let f = TournamentFilter {
        category_start: q.category_start,
        category_end: q.category_end,
        start_time: q.start_time,
        end_time: q.end_time,
    };
    let (tournaments, cursor) = s.tournaments.list(f, q.limit, q.cursor.as_deref()).await?;
    Ok(Json(
        json!({ "tournaments": tournaments, "cursor": cursor }),
    ))
}

async fn get_one(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
) -> Result<Json<Tournament>, ApiError> {
    Ok(Json(s.tournaments.get(&id).await?))
}

#[derive(Deserialize, Default)]
struct JoinBody {
    #[serde(default)]
    code: Option<String>,
}

async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    body: Option<Json<JoinBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let u = username(&s, me.account_id).await?;
    let code = body.as_ref().and_then(|b| b.0.code.as_deref());
    s.tournaments
        .join(&id, me.account_id, u.as_deref(), code)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

async fn records(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path(id): Path<String>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    let owners = parse_ids(q.owner_ids.as_deref())?;
    Ok(Json(
        s.tournaments
            .records(
                &s.leaderboards,
                &id,
                &owners,
                q.limit,
                q.cursor.as_deref(),
                q.expiry,
            )
            .await?,
    ))
}

async fn around(
    State(s): State<AppState>,
    Bearer(_): Bearer,
    Path((id, owner)): Path<(String, AccountId)>,
    Query(q): Query<RecordsQuery>,
) -> Result<Json<RecordList>, ApiError> {
    Ok(Json(
        s.tournaments
            .around(&s.leaderboards, &id, owner, q.limit, q.expiry)
            .await?,
    ))
}

async fn write(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let u = username(&s, me.account_id).await?;
    Ok(Json(
        s.tournaments
            .write(&s.leaderboards, &id, me.account_id, u.as_deref(), w, false)
            .await?,
    ))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(c): Json<CreateTournament>,
) -> Result<Json<Tournament>, ApiError> {
    Ok(Json(s.tournaments.create(c).await?))
}

async fn delete_one(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    s.tournaments.delete(&id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn write_as(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, owner)): Path<(String, AccountId)>,
    Json(w): Json<WriteRecord>,
) -> Result<Json<Record>, ApiError> {
    let u = username(&s, owner).await?;
    Ok(Json(
        s.tournaments
            .write(&s.leaderboards, &id, owner, u.as_deref(), w, true)
            .await?,
    ))
}

/// Pay the prizes of one finished window: every record in a rank band gets a mail carrying the
/// band's reward table. Claimed once per (tournament, window), so the leader sweep and a manual
/// `end` never pay twice. Returns how many mails went out.
pub async fn pay_prizes(s: &AppState, id: &str, expiry: i64) -> Result<usize, musubi_core::Error> {
    let prizes = s.tournaments.prizes(id).await?;
    if prizes.is_empty() || !s.tournaments.claim_payout(id, expiry).await? {
        return Ok(0);
    }
    let limit = prizes.iter().map(|p| p.to_rank).max().unwrap_or(0) as usize;
    let records = s
        .leaderboards
        .records(id, &[], limit, None, Some(expiry), None)
        .await?
        .records;
    let now = time::OffsetDateTime::now_utc();
    let mut paid = 0;
    for (i, r) in records.iter().enumerate() {
        let rank = i as i32 + 1;
        let Some(p) = prizes
            .iter()
            .find(|p| p.from_rank <= rank && rank <= p.to_rank)
        else {
            continue;
        };
        let title = if p.title.is_empty() {
            format!("Tournament {id}: rank {rank}")
        } else {
            p.title.clone()
        };
        s.economy
            .send_mail(
                musubi_economy::SendMail {
                    account_ids: vec![r.owner_id.0],
                    all: false,
                    title,
                    body: p.body.clone(),
                    metadata: json!({
                        "tournament_id": id, "rank": rank, "score": r.score,
                        "subscore": r.subscore, "expiry": expiry,
                    }),
                    rewards: Some(p.reward_table.clone()),
                    expires_in_seconds: None,
                },
                now,
            )
            .await?;
        s.events
            .emit(
                "tournament.prize",
                Some(r.owner_id),
                json!({ "tournament_id": id, "rank": rank, "reward_table": p.reward_table, "expiry": expiry }),
            )
            .await;
        paid += 1;
    }
    s.tournaments.record_payout(id, expiry, paid as i32).await?;
    Ok(paid)
}

/// Close the active window by hand: fires `tournament_end` to modules (once) and pays the prizes.
async fn end(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    // records of the running window are keyed by its expiry (`next_reset`), as in the sweep
    let t = s.tournaments.get(&id).await?;
    let expiry = t.next_reset;
    if s.leaderboards.claim_end(&id, expiry).await? {
        let records = s
            .leaderboards
            .records(&id, &[], 100, None, Some(expiry), None)
            .await?
            .records;
        let ctx = musubi_runtime::CallContext {
            server: true,
            ..Default::default()
        };
        s.runtime
            .event(
                "tournament_end",
                ctx,
                json!({ "id": id, "expiry": expiry, "size": records.len(), "records": records }),
            )
            .await;
    }
    let paid = pay_prizes(&s, &id, expiry).await?;
    Ok(Json(json!({ "expiry": expiry, "paid": paid })))
}
