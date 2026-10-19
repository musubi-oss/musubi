//! Quest pools (randomized goal rotations), player-created challenges and streak operator tools.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use musubi_economy::challenges::{ChallengeTemplate, NewChallenge};
use musubi_economy::quest_pools::QuestPool;
use musubi_social::notifications::NewNotification;
use serde::Deserialize;
use serde_json::{json, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub const CHALLENGE_INVITE_CODE: i32 = 31;

pub fn routes() -> Router<AppState> {
    Router::new()
        // quest pools
        .route("/v1/server/quests/pools", get(list_pools).post(upsert_pool))
        .route(
            "/v1/server/quests/pools/{id}",
            get(get_pool).delete(delete_pool),
        )
        .route("/v1/server/quests/pools/{id}/accelerate", post(accelerate))
        .route("/v1/server/quests/pools/{id}/retire", post(retire))
        .route(
            "/v1/server/quests/pools/{id}/preview/{account_id}",
            get(preview_pool),
        )
        .route("/v1/quests/pools", get(my_pools))
        .route(
            "/v1/quests/pools/{id}/slots/{slot}/reroll",
            post(reroll_slot),
        )
        .route("/v1/quests/pools/{id}/slots/{slot}/claim", post(claim_slot))
        // challenges
        .route(
            "/v1/server/challenge_templates",
            get(list_templates).post(upsert_template),
        )
        .route("/v1/server/challenges/sweep", post(sweep))
        .route("/v1/challenges", get(my_challenges).post(create_challenge))
        .route("/v1/challenges/{id}", get(standings))
        .route("/v1/challenges/{id}/join", post(join))
        .route("/v1/challenges/{id}/leave", post(leave))
        .route("/v1/challenges/{id}/invite", post(invite))
        .route("/v1/challenges/{id}/claim", post(claim_challenge))
        // streak operator tools
        .route(
            "/v1/server/streaks/{id}/state/{account_id}",
            get(streak_state),
        )
        .route("/v1/server/streaks/{id}/reset", post(streak_reset))
        .route("/v1/server/streaks/{id}", delete(delete_streak))
}

/// Player's UTC offset in minutes (`accounts.metadata.tz_offset_min`, as live events use it).
pub async fn tz_offset_min(s: &AppState, account: AccountId) -> i64 {
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

fn account(id: &str) -> Result<AccountId, ApiError> {
    Uuid::parse_str(id)
        .map(AccountId)
        .map_err(|_| Error::InvalidArgument("bad account_id".into()).into())
}

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

// ----- quest pools -----

async fn list_pools(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "pools": s.economy.quest_pools().await? })))
}

async fn upsert_pool(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(p): Json<QuestPool>,
) -> Result<Json<Value>, ApiError> {
    let p = s.economy.upsert_quest_pool(p).await?;
    s.events
        .emit("quest_pool.updated", None, json!({ "pool_id": p.id }))
        .await;
    Ok(Json(serde_json::to_value(p).map_err(Error::internal)?))
}

async fn get_pool(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.quest_pool(&id).await?).map_err(Error::internal)?,
    ))
}

async fn delete_pool(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    s.economy.delete_quest_pool(&id).await?;
    Ok(Json(json!({ "deleted": true })))
}

async fn accelerate(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = s
        .economy
        .control_quest_pool(&id, "accelerate", now())
        .await?;
    s.events
        .emit(
            "quest_pool.accelerated",
            None,
            json!({ "pool_id": p.id, "generation": p.generation }),
        )
        .await;
    Ok(Json(serde_json::to_value(p).map_err(Error::internal)?))
}

async fn retire(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let p = s.economy.control_quest_pool(&id, "retire", now()).await?;
    s.events
        .emit("quest_pool.retired", None, json!({ "pool_id": p.id }))
        .await;
    Ok(Json(serde_json::to_value(p).map_err(Error::internal)?))
}

#[derive(Deserialize)]
struct PreviewQuery {
    /// Evaluate as if the clock read this unix time.
    at: Option<i64>,
}

/// What a given player holds in a pool right now (support view).
async fn preview_pool(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, account_id)): Path<(String, String)>,
    Query(q): Query<PreviewQuery>,
) -> Result<Json<Value>, ApiError> {
    let acc = account(&account_id)?;
    let at = match q.at {
        Some(t) => OffsetDateTime::from_unix_timestamp(t)
            .map_err(|_| Error::InvalidArgument("at: unix seconds".into()))?,
        None => now(),
    };
    let tz = tz_offset_min(&s, acc).await;
    let views = s.economy.quest_pool_views(acc, at, tz).await?;
    let Some(v) = views.into_iter().find(|v| v["pool_id"] == id) else {
        return Err(Error::NotFound(format!("quest pool {id} is not live")).into());
    };
    Ok(Json(v))
}

async fn my_pools(State(s): State<AppState>, Bearer(me): Bearer) -> Result<Json<Value>, ApiError> {
    let tz = tz_offset_min(&s, me.account_id).await;
    Ok(Json(json!({
        "pools": s.economy.quest_pool_views(me.account_id, now(), tz).await?
    })))
}

async fn reroll_slot(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, slot)): Path<(String, i32)>,
) -> Result<Json<Value>, ApiError> {
    let tz = tz_offset_min(&s, me.account_id).await;
    Ok(Json(
        s.economy
            .reroll_pool_slot(me.account_id, &id, slot, now(), tz)
            .await?,
    ))
}

async fn claim_slot(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path((id, slot)): Path<(String, i32)>,
) -> Result<Json<Value>, ApiError> {
    let tz = tz_offset_min(&s, me.account_id).await;
    let (goal_id, rewards) = s
        .economy
        .claim_pool_slot(me.account_id, &id, slot, now(), tz)
        .await?;
    s.events
        .emit(
            "quest_pool.claimed",
            Some(me.account_id),
            json!({ "pool_id": id, "slot": slot, "goal_id": goal_id, "rewards": rewards }),
        )
        .await;
    Ok(Json(json!({ "goal_id": goal_id, "rewards": rewards })))
}

/// Fan a quest event out to pools and challenges too (called from the quest event route and
/// match results).
pub async fn fan_out_event(s: &AppState, account: AccountId, event: &str, delta: i64) -> Value {
    let at = now();
    let tz = tz_offset_min(s, account).await;
    let pools = s
        .economy
        .quest_pool_event(account, event, delta, at, tz)
        .await
        .unwrap_or_else(|_| musubi_economy::quests::QuestEventResult {
            updated: Vec::new(),
            completed: Vec::new(),
        });
    for id in &pools.completed {
        s.events
            .emit(
                "quest_pool.completed",
                Some(account),
                json!({ "assignment": id, "event": event }),
            )
            .await;
    }
    let challenges = s
        .economy
        .challenge_event(account, event, delta, at)
        .await
        .unwrap_or_default();
    json!({
        "pools": { "updated": pools.updated, "completed": pools.completed },
        "challenges": challenges,
    })
}

// ----- challenges -----

async fn list_templates(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        json!({ "templates": s.economy.challenge_templates().await? }),
    ))
}

async fn upsert_template(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(t): Json<ChallengeTemplate>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(
        serde_json::to_value(s.economy.upsert_challenge_template(t).await?)
            .map_err(Error::internal)?,
    ))
}

async fn sweep(State(s): State<AppState>, ServerKey: ServerKey) -> Result<Json<Value>, ApiError> {
    let settled = s.economy.settle_challenges(now()).await?;
    for id in &settled {
        s.events
            .emit("challenge.settled", None, json!({ "challenge_id": id }))
            .await;
    }
    Ok(Json(json!({ "settled": settled })))
}

#[derive(Deserialize)]
struct CreateReq {
    template_id: String,
    name: Option<String>,
    #[serde(default)]
    open: bool,
    #[serde(default)]
    starts_in_secs: i64,
    duration_secs: Option<i64>,
    max_participants: Option<i32>,
    #[serde(default)]
    invite: Vec<String>,
}

async fn create_challenge(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Json(r): Json<CreateReq>,
) -> Result<Json<Value>, ApiError> {
    let c = s
        .economy
        .create_challenge(
            me.account_id,
            NewChallenge {
                template_id: r.template_id,
                name: r.name,
                open: r.open,
                starts_in_secs: r.starts_in_secs,
                duration_secs: r.duration_secs,
                max_participants: r.max_participants,
            },
            now(),
        )
        .await?;
    s.events
        .emit(
            "challenge.created",
            Some(me.account_id),
            json!({ "challenge_id": c.id, "template_id": c.template_id, "open": c.open }),
        )
        .await;
    let mut invited = Vec::new();
    if !r.invite.is_empty() {
        let targets: Vec<AccountId> = r
            .invite
            .iter()
            .map(|a| account(a))
            .collect::<Result<_, _>>()?;
        invited = do_invite(&s, me.account_id, c.id, &c.name, &targets).await?;
    }
    let mut v = serde_json::to_value(&c).map_err(Error::internal)?;
    v["invited"] = json!(invited);
    Ok(Json(v))
}

async fn do_invite(
    s: &AppState,
    by: AccountId,
    id: Uuid,
    name: &str,
    targets: &[AccountId],
) -> Result<Vec<AccountId>, ApiError> {
    let invited = s
        .economy
        .invite_to_challenge(by, id, targets, now())
        .await?;
    if !invited.is_empty() {
        let batch: Vec<NewNotification> = invited
            .iter()
            .map(|t| NewNotification {
                user_id: *t,
                sender_id: Some(by),
                subject: "challenge_invite".into(),
                content: json!({ "challenge_id": id, "name": name, "from": by }),
                code: CHALLENGE_INVITE_CODE,
                persistent: true,
                ttl_secs: Some(7 * 86_400),
            })
            .collect();
        let _ = s.notify.send(batch).await;
        for t in &invited {
            s.events
                .emit(
                    "challenge.invited",
                    Some(*t),
                    json!({ "challenge_id": id, "by": by }),
                )
                .await;
        }
    }
    Ok(invited)
}

async fn my_challenges(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "challenges": s.economy.my_challenges(me.account_id, now()).await?
    })))
}

async fn standings(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut v = s.economy.challenge_standings(id, now()).await?;
    let mine = v["standings"]
        .as_array()
        .is_some_and(|a| a.iter().any(|p| p["account_id"] == json!(me.account_id)));
    if !mine {
        v["join_code"] = Value::Null;
    }
    Ok(Json(v))
}

#[derive(Deserialize, Default)]
struct JoinReq {
    code: Option<String>,
}

async fn join(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    body: Option<Json<JoinReq>>,
) -> Result<Json<Value>, ApiError> {
    let code = body.and_then(|Json(b)| b.code);
    let c = s
        .economy
        .join_challenge(me.account_id, id, code.as_deref(), now())
        .await?;
    s.events
        .emit(
            "challenge.joined",
            Some(me.account_id),
            json!({ "challenge_id": id }),
        )
        .await;
    Ok(Json(serde_json::to_value(c).map_err(Error::internal)?))
}

async fn leave(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    s.economy.leave_challenge(me.account_id, id).await?;
    Ok(Json(json!({ "left": true })))
}

#[derive(Deserialize)]
struct InviteReq {
    account_ids: Vec<String>,
}

async fn invite(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
    Json(r): Json<InviteReq>,
) -> Result<Json<Value>, ApiError> {
    let c = s.economy.challenge(id).await?;
    let targets: Vec<AccountId> = r
        .account_ids
        .iter()
        .map(|a| account(a))
        .collect::<Result<_, _>>()?;
    let invited = do_invite(&s, me.account_id, id, &c.name, &targets).await?;
    Ok(Json(json!({ "invited": invited })))
}

async fn claim_challenge(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let (rank, rewards) = s.economy.claim_challenge(me.account_id, id, now()).await?;
    s.events
        .emit(
            "challenge.claimed",
            Some(me.account_id),
            json!({ "challenge_id": id, "rank": rank, "rewards": rewards }),
        )
        .await;
    Ok(Json(json!({ "rank": rank, "rewards": rewards })))
}

// ----- streak operator tools -----

async fn streak_state(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path((id, account_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let acc = account(&account_id)?;
    let tz = tz_offset_min(&s, acc).await;
    Ok(Json(s.economy.streak_state(acc, &id, now(), tz).await?))
}

#[derive(Deserialize)]
struct ResetReq {
    account_id: String,
    count: Option<i32>,
}

async fn streak_reset(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
    Json(r): Json<ResetReq>,
) -> Result<Json<Value>, ApiError> {
    let acc = account(&r.account_id)?;
    s.economy.reset_streak(acc, &id, r.count).await?;
    s.events
        .emit(
            "streak.reset",
            Some(acc),
            json!({ "streak_id": id, "count": r.count.unwrap_or(0) }),
        )
        .await;
    let tz = tz_offset_min(&s, acc).await;
    Ok(Json(s.economy.streak_state(acc, &id, now(), tz).await?))
}

async fn delete_streak(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.streaks where id = $1")
        .bind(&id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    if n == 0 {
        return Err(Error::NotFound(format!("streak {id}")).into());
    }
    Ok(Json(json!({ "deleted": true })))
}
