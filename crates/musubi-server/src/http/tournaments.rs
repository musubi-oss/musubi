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
        .route("/v1/server/tournaments/{id}/records", get(records_server))
        .route("/v1/tournaments/{id}/around/{owner}", get(around))
        .route("/v1/server/tournaments", get(list_server).post(create))
        .route("/v1/server/tournaments/{id}", delete(delete_one))
        .route("/v1/server/tournaments/{id}/end", post(end))
        .route("/v1/tournaments/{id}/bracket", get(bracket_get))
        .route(
            "/v1/server/tournaments/{id}/bracket",
            get(bracket_get_server)
                .post(bracket_create)
                .delete(bracket_delete),
        )
        .route(
            "/v1/server/tournaments/{id}/bracket/matches/{mid}",
            post(bracket_report),
        )
        .route(
            "/v1/server/tournaments/{id}/bracket/matches/{mid}/forfeit",
            post(bracket_forfeit),
        )
        .route(
            "/v1/server/tournaments/{id}/bracket/rounds",
            post(bracket_round),
        )
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

/// Same listing for game servers and the console (server key instead of a player bearer).
async fn list_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
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
    group_id: Option<musubi_core::GroupId>,
    #[serde(default)]
    code: Option<String>,
}

async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<String>,
    body: Option<Json<JoinBody>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let code = body.as_ref().and_then(|b| b.0.code.as_deref());
    let group = body.as_ref().and_then(|b| b.0.group_id);
    let (owner, name) = owner_for(&s, &id, me.account_id, group).await?;
    s.tournaments
        .join(&id, owner, name.as_deref(), code)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

/// Who a tournament record belongs to: the caller, or — in a team tournament — the group the
/// caller names and holds the tournament's role bits in.
async fn owner_for(
    s: &AppState,
    id: &str,
    me: AccountId,
    group: Option<musubi_core::GroupId>,
) -> Result<(AccountId, Option<String>), ApiError> {
    let t = s.tournaments.get(id).await?;
    match (t.team, group) {
        (Some(rules), Some(g)) => {
            crate::http::groups::groups(s)
                .check_perm(me, g, rules.perm)
                .await?;
            let name = crate::http::groups::groups(s)
                .get(&[g])
                .await?
                .into_iter()
                .next()
                .map(|g| g.name);
            Ok((AccountId(g.0), name))
        }
        (Some(_), None) => Err(musubi_core::Error::InvalidArgument(
            "team tournament: group_id is required".into(),
        )
        .into()),
        (None, Some(_)) => {
            Err(musubi_core::Error::InvalidArgument("not a team tournament".into()).into())
        }
        (None, None) => Ok((me, username(s, me).await?)),
    }
}

#[derive(Deserialize)]
struct TournamentWrite {
    #[serde(flatten)]
    record: WriteRecord,
    #[serde(default)]
    group_id: Option<musubi_core::GroupId>,
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

/// `GET /v1/server/tournaments/{id}/records` — the same record list for operators (console) under a server key.
async fn records_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
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
    Json(w): Json<TournamentWrite>,
) -> Result<Json<Record>, ApiError> {
    let (owner, name) = owner_for(&s, &id, me.account_id, w.group_id).await?;
    Ok(Json(
        s.tournaments
            .write(
                &s.leaderboards,
                &id,
                owner,
                name.as_deref(),
                w.record,
                false,
            )
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
    let team = s.tournaments.team(id).await?.is_some();
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
        if team {
            let (Some(cur), Some(amount)) = (&p.currency, p.amount) else {
                continue;
            };
            s.economy
                .treasury_grant(r.owner_id.0, cur, amount, &title)
                .await?;
            s.events
                .emit(
                    "tournament.prize",
                    None,
                    json!({ "tournament_id": id, "rank": rank, "group_id": r.owner_id.0, "currency": cur, "amount": amount, "expiry": expiry }),
                )
                .await;
            paid += 1;
            continue;
        }
        s.economy
            .send_mail(
                musubi_economy::SendMail {
                    account_ids: vec![r.owner_id.0],
                    all: false,
                    audience: None,
                    title,
                    body: p.body.clone(),
                    metadata: json!({
                        "tournament_id": id, "rank": rank, "score": r.score,
                        "subscore": r.subscore, "expiry": expiry,
                    }),
                    rewards: Some(p.reward_table.clone()),
                    expires_in_seconds: None,
                    reward_entries: None,
                    available_in_seconds: None,
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

#[derive(serde::Deserialize)]
struct BracketReq {
    format: musubi_leaderboard::bracket::Format,
    /// `score` (current standings, default), `join` (arrival order), `random`, or `list`
    /// (`participants` as given — e.g. team ids for team tournaments).
    #[serde(default)]
    seeding: Option<String>,
    #[serde(default)]
    participants: Vec<uuid::Uuid>,
    /// Cap on the number of participants taken from the standings.
    #[serde(default)]
    size: Option<usize>,
    /// Swiss only; defaults to ⌈log2 n⌉.
    #[serde(default)]
    swiss_rounds: Option<i32>,
    /// Seconds a ready match has before its absent side may be forfeited.
    #[serde(default)]
    deadline_secs: Option<i64>,
}

fn bracket_json(b: &musubi_leaderboard::bracket::Bracket) -> serde_json::Value {
    let mut v = serde_json::to_value(b).unwrap_or(serde_json::Value::Null);
    if let Some(arr) = v["matches"].as_array_mut() {
        for (m, mv) in b.matches.iter().zip(arr.iter_mut()) {
            mv["status"] = json!(m.status());
        }
    }
    v["ready"] = json!(b
        .matches
        .iter()
        .filter(|m| m.ready())
        .map(|m| m.id)
        .collect::<Vec<_>>());
    v["standings"] = serde_json::to_value(musubi_leaderboard::bracket::standings(b))
        .unwrap_or(serde_json::Value::Null);
    v
}

/// Build the bracket from the tournament's current participants (one per tournament; 409 if it
/// exists). Byes are resolved immediately; a Swiss bracket comes with round 1 paired.
async fn bracket_create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<BracketReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    use musubi_leaderboard::bracket::{generate, Participant, MAX_PARTICIPANTS};
    let t = s.tournaments.get(&id).await?;
    let seeding = r.seeding.as_deref().unwrap_or("score");
    let participants: Vec<Participant> = if seeding == "list" {
        r.participants
            .iter()
            .enumerate()
            .map(|(i, p)| Participant {
                id: *p,
                name: String::new(),
                seed: i as i32 + 1,
            })
            .collect()
    } else {
        let mut recs = s
            .leaderboards
            .records(&id, &[], MAX_PARTICIPANTS, None, Some(t.next_reset), None)
            .await?
            .records;
        match seeding {
            "score" => {}
            "join" => recs.sort_by_key(|r| r.create_time),
            "random" => {
                let salt = uuid::Uuid::now_v7().as_u128();
                recs.sort_by_key(|r| r.owner_id.0.as_u128() ^ salt);
            }
            other => {
                return Err(musubi_core::Error::InvalidArgument(format!(
                    "seeding {other:?}: expected score|join|random|list"
                ))
                .into())
            }
        }
        if let Some(n) = r.size {
            recs.truncate(n);
        }
        recs.iter()
            .enumerate()
            .map(|(i, rec)| Participant {
                id: rec.owner_id.0,
                name: rec.username.clone().unwrap_or_default(),
                seed: i as i32 + 1,
            })
            .collect()
    };
    let mut b = generate(r.format, participants, r.swiss_rounds)?;
    if r.deadline_secs.is_some() {
        b.deadline_secs = r.deadline_secs;
        musubi_leaderboard::bracket::stamp_deadlines(&mut b);
    }
    s.tournaments.bracket_create(&id, &b).await?;
    s.events
        .emit(
            "tournament.bracket_created",
            None,
            json!({ "tournament_id": id, "format": r.format, "participants": b.participants.len(),
                "rounds": b.rounds }),
        )
        .await;
    Ok(Json(bracket_json(&b)))
}

async fn bracket_get(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match s.tournaments.bracket_get(&id).await? {
        Some(b) => Ok(Json(bracket_json(&b))),
        None => Err(musubi_core::Error::NotFound(format!("tournament {id} has no bracket")).into()),
    }
}

async fn bracket_get_server(
    State(s): State<AppState>,
    _k: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    match s.tournaments.bracket_get(&id).await? {
        Some(b) => Ok(Json(bracket_json(&b))),
        None => Err(musubi_core::Error::NotFound(format!("tournament {id} has no bracket")).into()),
    }
}

async fn bracket_delete(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let deleted = s.tournaments.bracket_delete(&id).await?;
    Ok(Json(json!({ "deleted": deleted })))
}

#[derive(serde::Deserialize)]
struct BracketResult {
    winner: uuid::Uuid,
    #[serde(default)]
    score: serde_json::Value,
    #[serde(default)]
    match_id: Option<String>,
}

#[derive(Deserialize)]
struct ForfeitReq {
    /// The side that did not show up.
    participant: uuid::Uuid,
}

/// No-show: once the match is past its deadline, the absent participant loses by forfeit.
async fn bracket_forfeit(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, mid)): Path<(String, uuid::Uuid)>,
    Json(r): Json<ForfeitReq>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (b, ready) = s
        .tournaments
        .bracket_update(&id, |b| {
            musubi_leaderboard::bracket::forfeit(b, mid, r.participant)
        })
        .await?;
    let winner = b
        .matches
        .iter()
        .find(|m| m.id == mid)
        .and_then(|m| m.winner)
        .unwrap_or(r.participant);
    emit_bracket(&s, &id, mid, winner, &ready, b.champion).await;
    Ok(Json(
        json!({ "ready": ready, "champion": b.champion, "bracket": bracket_json(&b) }),
    ))
}

/// Decide a bracket match directly (the end-game report can do the same via `bracket_match`).
async fn bracket_report(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, mid)): Path<(String, uuid::Uuid)>,
    Json(r): Json<BracketResult>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (b, ready) = s
        .tournaments
        .bracket_update(&id, |b| {
            musubi_leaderboard::bracket::report(b, mid, r.winner, r.score, r.match_id)
        })
        .await?;
    emit_bracket(&s, &id, mid, r.winner, &ready, b.champion).await;
    Ok(Json(
        json!({ "ready": ready, "champion": b.champion, "bracket": bracket_json(&b) }),
    ))
}

pub(crate) async fn emit_bracket(
    s: &AppState,
    id: &str,
    mid: uuid::Uuid,
    winner: uuid::Uuid,
    ready: &[uuid::Uuid],
    champion: Option<uuid::Uuid>,
) {
    s.events
        .emit(
            "tournament.bracket_match",
            None,
            json!({ "tournament_id": id, "match": mid, "winner": winner, "ready": ready }),
        )
        .await;
    if let Some(c) = champion {
        s.events
            .emit(
                "tournament.bracket_done",
                None,
                json!({ "tournament_id": id, "champion": c }),
            )
            .await;
    }
}

/// Swiss: pair the next round (409 while the current one has unfinished matches).
async fn bracket_round(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let (b, round) = s
        .tournaments
        .bracket_update(&id, musubi_leaderboard::bracket::swiss_next_round)
        .await?;
    s.events
        .emit(
            "tournament.bracket_round",
            None,
            json!({ "tournament_id": id, "round": round }),
        )
        .await;
    Ok(Json(json!({ "round": round, "bracket": bracket_json(&b) })))
}
