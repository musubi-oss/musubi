//! Group challenges and wars (plan 5.4): a server-defined competition between groups over a
//! window — `war` pits named groups against each other, `challenge` is open to every group whose
//! members contribute. The game reports contributions (`POST …/score`), standings are the sum of
//! member contributions per group, and when the window closes the node holding the `schedules`
//! lease settles it: the winning group's treasury is credited, its members (and optionally every
//! contributor) receive reward tables, and `group_challenge.settled` goes to the event bus.

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::app::AppState;
use crate::http::{ApiError, Bearer, ServerKey};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/server/group_challenges", post(create).get(list_server))
        .route(
            "/v1/server/group_challenges/{id}",
            get(get_server).delete(remove),
        )
        .route("/v1/server/group_challenges/{id}/score", post(score))
        .route("/v1/server/group_challenges/{id}/settle", post(settle_now))
        .route("/v1/group_challenges", get(list_mine))
        .route("/v1/group_challenges/{id}", get(standings))
        .route("/v1/group_challenges/{id}/me", get(my_contribution))
        .route(
            "/v1/server/group_challenges/sweep",
            delete(sweep_noop).post(sweep_now),
        )
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Rewards {
    /// Credited to the winning group's treasury.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub treasury: Option<TreasuryPrize>,
    /// Reward table granted to every active member of the winning group.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winner_table: Option<String>,
    /// Reward table granted to every account that contributed at least `min_contribution`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub participant_table: Option<String>,
    #[serde(default)]
    pub min_contribution: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreasuryPrize {
    pub currency: String,
    pub amount: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateReq {
    pub name: String,
    /// `war` (only `groups` take part) or `challenge` (open: every group whose members score).
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default = "default_metric")]
    pub metric: String,
    #[serde(default)]
    pub groups: Vec<Uuid>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub starts_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub ends_at: Option<OffsetDateTime>,
    /// Alternative to `ends_at`.
    #[serde(default)]
    pub duration_secs: Option<i64>,
    #[serde(default)]
    pub rewards: Rewards,
    #[serde(default)]
    pub metadata: Value,
}

fn default_kind() -> String {
    "challenge".into()
}
fn default_metric() -> String {
    "score".into()
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Challenge {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub metric: String,
    pub groups: Vec<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ends_at: OffsetDateTime,
    pub rewards: Value,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub settled_at: Option<OffsetDateTime>,
    pub winner: Option<Uuid>,
}

impl Challenge {
    fn status(&self, now: OffsetDateTime) -> &'static str {
        if self.settled_at.is_some() {
            "settled"
        } else if now < self.starts_at {
            "scheduled"
        } else if now >= self.ends_at {
            "ended"
        } else {
            "active"
        }
    }

    fn view(&self, now: OffsetDateTime) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        v["status"] = json!(self.status(now));
        v
    }
}

const SELECT: &str = "select id, name, kind, metric, groups, starts_at, ends_at, rewards, metadata, settled_at, winner from musubi.group_challenges";

async fn load(db: &PgPool, id: Uuid) -> Result<Challenge> {
    sqlx::query_as::<_, Challenge>(&format!("{SELECT} where id = $1"))
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("group challenge".into()))
}

async fn create(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Json(r): Json<CreateReq>,
) -> std::result::Result<Json<Value>, ApiError> {
    if r.name.trim().is_empty() || r.name.len() > 128 {
        return Err(Error::InvalidArgument("name: 1..128 characters".into()).into());
    }
    if r.kind != "war" && r.kind != "challenge" {
        return Err(Error::InvalidArgument("kind: war | challenge".into()).into());
    }
    if r.kind == "war" && r.groups.len() < 2 {
        return Err(Error::InvalidArgument("a war needs at least two groups".into()).into());
    }
    let now = OffsetDateTime::now_utc();
    let starts_at = r.starts_at.unwrap_or(now);
    let ends_at = match (r.ends_at, r.duration_secs) {
        (Some(e), _) => e,
        (None, Some(d)) if d > 0 => starts_at + time::Duration::seconds(d),
        _ => return Err(Error::InvalidArgument("ends_at or duration_secs required".into()).into()),
    };
    if ends_at <= starts_at {
        return Err(Error::InvalidArgument("ends_at must be after starts_at".into()).into());
    }
    if let Some(t) = &r.rewards.treasury {
        if t.amount <= 0 || t.currency.is_empty() {
            return Err(Error::InvalidArgument(
                "rewards.treasury: currency and positive amount".into(),
            )
            .into());
        }
    }
    if !r.groups.is_empty() {
        let known: i64 =
            sqlx::query_scalar("select count(*) from musubi.groups where id = any($1)")
                .bind(&r.groups)
                .fetch_one(&s.db)
                .await
                .map_err(Error::internal)?;
        if known as usize != r.groups.len() {
            return Err(Error::NotFound("one of the groups".into()).into());
        }
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "insert into musubi.group_challenges (id, name, kind, metric, groups, starts_at, ends_at, rewards, metadata)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(r.name.trim())
    .bind(&r.kind)
    .bind(&r.metric)
    .bind(&r.groups)
    .bind(starts_at)
    .bind(ends_at)
    .bind(serde_json::to_value(&r.rewards).unwrap_or_default())
    .bind(if r.metadata.is_null() { json!({}) } else { r.metadata })
    .execute(&s.db)
    .await
    .map_err(Error::internal)?;
    let c = load(&s.db, id).await?;
    s.events
        .emit(
            "group_challenge.created",
            None,
            json!({ "id": id, "kind": c.kind, "groups": c.groups }),
        )
        .await;
    Ok(Json(c.view(now)))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// scheduled | active | ended | settled
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

async fn list_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Query(q): Query<ListQuery>,
) -> std::result::Result<Json<Value>, ApiError> {
    let now = OffsetDateTime::now_utc();
    let rows = sqlx::query_as::<_, Challenge>(&format!("{SELECT} order by ends_at desc limit $1"))
        .bind(q.limit.clamp(1, 500))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
    let items: Vec<Value> = rows
        .iter()
        .filter(|c| q.status.as_deref().is_none_or(|st| c.status(now) == st))
        .map(|c| c.view(now))
        .collect();
    Ok(Json(json!({ "challenges": items })))
}

async fn get_server(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, id).await?;
    Ok(Json(
        standings_of(&s.db, &c, OffsetDateTime::now_utc(), 10).await?,
    ))
}

async fn remove(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let n = sqlx::query("delete from musubi.group_challenges where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
    Ok(Json(json!({ "removed": n > 0 })))
}

#[derive(Debug, Deserialize)]
pub struct ScoreReq {
    pub account_id: Uuid,
    /// Contribution to add (negative corrects an earlier report; totals never go below zero).
    pub delta: i64,
    /// Group to credit; default: the account's participating group (error when ambiguous).
    #[serde(default)]
    pub group_id: Option<Uuid>,
}

/// Active groups (superadmin/admin/member) of an account.
async fn groups_of(db: &PgPool, account: Uuid) -> Result<Vec<Uuid>> {
    sqlx::query_scalar(
        "select group_id from musubi.group_members where user_id = $1 and state <= 2 order by group_id",
    )
    .bind(account)
    .fetch_all(db)
    .await
    .map_err(Error::internal)
}

async fn score(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
    Json(r): Json<ScoreReq>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, id).await?;
    let now = OffsetDateTime::now_utc();
    if c.status(now) != "active" {
        return Err(Error::Conflict(format!("challenge is {}", c.status(now))).into());
    }
    let mine = groups_of(&s.db, r.account_id).await?;
    let eligible: Vec<Uuid> = mine
        .iter()
        .copied()
        .filter(|g| c.groups.is_empty() || c.groups.contains(g))
        .collect();
    let group = match r.group_id {
        Some(g) if eligible.contains(&g) => g,
        Some(_) => {
            return Err(Error::PermissionDenied(
                "account is not a member of that participating group".into(),
            )
            .into())
        }
        None => match eligible.as_slice() {
            [g] => *g,
            [] => {
                return Err(
                    Error::PermissionDenied("account is in no participating group".into()).into(),
                )
            }
            _ => {
                return Err(Error::InvalidArgument(
                    "account is in several participating groups; pass group_id".into(),
                )
                .into())
            }
        },
    };
    let total: i64 = sqlx::query_scalar(
        "insert into musubi.group_challenge_scores (challenge_id, group_id, account_id, score)
         values ($1, $2, $3, greatest($4, 0))
         on conflict (challenge_id, group_id, account_id)
         do update set score = greatest(musubi.group_challenge_scores.score + $4, 0), updated_at = now()
         returning score",
    )
    .bind(id)
    .bind(group)
    .bind(r.account_id)
    .bind(r.delta)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    let group_total: i64 = sqlx::query_scalar(
        "select coalesce(sum(score), 0)::bigint from musubi.group_challenge_scores where challenge_id = $1 and group_id = $2",
    )
    .bind(id)
    .bind(group)
    .fetch_one(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "group_id": group, "contribution": total, "group_total": group_total }),
    ))
}

async fn standings_of(db: &PgPool, c: &Challenge, now: OffsetDateTime, top: i64) -> Result<Value> {
    let rows: Vec<(Uuid, i64, i64)> = sqlx::query_as(
        "select group_id, sum(score)::bigint, count(*)::bigint from musubi.group_challenge_scores
         where challenge_id = $1 group by group_id order by 2 desc, 1 asc",
    )
    .bind(c.id)
    .fetch_all(db)
    .await
    .map_err(Error::internal)?;
    let mut standings: Vec<Value> = Vec::new();
    for (rank, (g, total, contributors)) in rows.iter().enumerate() {
        let leaders: Vec<(Uuid, i64)> = sqlx::query_as(
            "select account_id, score from musubi.group_challenge_scores
             where challenge_id = $1 and group_id = $2 order by score desc, account_id limit $3",
        )
        .bind(c.id)
        .bind(g)
        .bind(top)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        standings.push(json!({
            "rank": rank + 1, "group_id": g, "total": total, "contributors": contributors,
            "top": leaders.iter().map(|(a, sc)| json!({ "account_id": a, "score": sc })).collect::<Vec<_>>(),
        }));
    }
    // declared participants that have not scored yet still show up (wars)
    for g in &c.groups {
        if !rows.iter().any(|(x, _, _)| x == g) {
            standings.push(json!({ "rank": standings.len() + 1, "group_id": g, "total": 0, "contributors": 0, "top": [] }));
        }
    }
    let mut v = c.view(now);
    v["standings"] = json!(standings);
    Ok(v)
}

async fn standings(
    State(s): State<AppState>,
    Bearer(_me): Bearer,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, id).await?;
    Ok(Json(
        standings_of(&s.db, &c, OffsetDateTime::now_utc(), 10).await?,
    ))
}

/// Challenges the player's groups can take part in: open ones plus wars naming one of them;
/// scheduled, active and recently settled.
async fn list_mine(
    State(s): State<AppState>,
    Bearer(me): Bearer,
) -> std::result::Result<Json<Value>, ApiError> {
    let now = OffsetDateTime::now_utc();
    let mine = groups_of(&s.db, me.account_id.0).await?;
    let rows = sqlx::query_as::<_, Challenge>(&format!(
        "{SELECT} where ends_at > now() - interval '7 days' and (cardinality(groups) = 0 or groups && $1) order by starts_at desc limit 100"
    ))
    .bind(&mine)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(
        json!({ "challenges": rows.iter().map(|c| c.view(now)).collect::<Vec<_>>() }),
    ))
}

async fn my_contribution(
    State(s): State<AppState>,
    Bearer(me): Bearer,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, id).await?;
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(
        "select group_id, score from musubi.group_challenge_scores where challenge_id = $1 and account_id = $2",
    )
    .bind(id)
    .bind(me.account_id.0)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    Ok(Json(json!({
        "challenge_id": c.id,
        "status": c.status(OffsetDateTime::now_utc()),
        "contributions": rows.iter().map(|(g, sc)| json!({ "group_id": g, "score": sc })).collect::<Vec<_>>(),
    })))
}

/// Pay out one ended challenge. Returns the winner (none when nobody scored).
pub async fn settle(s: &AppState, c: &Challenge) -> Result<Option<Uuid>> {
    let claimed = sqlx::query(
        "update musubi.group_challenges set settled_at = now() where id = $1 and settled_at is null",
    )
    .bind(c.id)
    .execute(&s.db)
    .await
    .map_err(Error::internal)?
    .rows_affected();
    if claimed == 0 {
        return Err(Error::Conflict("already settled".into()));
    }
    let rewards: Rewards = serde_json::from_value(c.rewards.clone()).unwrap_or_default();
    let totals: Vec<(Uuid, i64)> = sqlx::query_as(
        "select group_id, sum(score)::bigint from musubi.group_challenge_scores
         where challenge_id = $1 group by group_id order by 2 desc, 1 asc",
    )
    .bind(c.id)
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let winner = totals.first().filter(|(_, t)| *t > 0).map(|(g, _)| *g);
    let reference = format!("group_challenge:{}", c.id);
    let mut granted = 0usize;
    if let Some(w) = winner {
        sqlx::query("update musubi.group_challenges set winner = $2 where id = $1")
            .bind(c.id)
            .bind(w)
            .execute(&s.db)
            .await
            .map_err(Error::internal)?;
        if let Some(t) = &rewards.treasury {
            s.economy
                .treasury_grant(w, &t.currency, t.amount, &reference)
                .await?;
        }
        if let Some(table) = &rewards.winner_table {
            let members: Vec<Uuid> = sqlx::query_scalar(
                "select user_id from musubi.group_members where group_id = $1 and state <= 2",
            )
            .bind(w)
            .fetch_all(&s.db)
            .await
            .map_err(Error::internal)?;
            for m in members {
                if let Err(e) = s
                    .economy
                    .grant_table_detailed(AccountId(m), table, &format!("{reference}:winner"))
                    .await
                {
                    tracing::warn!(challenge = %c.id, account = %m, error = %e, "winner reward failed");
                } else {
                    granted += 1;
                }
            }
        }
    }
    if let Some(table) = &rewards.participant_table {
        let contributors: Vec<Uuid> = sqlx::query_scalar(
            "select distinct account_id from musubi.group_challenge_scores where challenge_id = $1 and score >= $2",
        )
        .bind(c.id)
        .bind(rewards.min_contribution.max(1))
        .fetch_all(&s.db)
        .await
        .map_err(Error::internal)?;
        for a in contributors {
            if let Err(e) = s
                .economy
                .grant_table_detailed(AccountId(a), table, &format!("{reference}:participant"))
                .await
            {
                tracing::warn!(challenge = %c.id, account = %a, error = %e, "participant reward failed");
            } else {
                granted += 1;
            }
        }
    }
    s.events
        .emit(
            "group_challenge.settled",
            None,
            json!({
                "id": c.id, "kind": c.kind, "winner": winner,
                "standings": totals.iter().map(|(g, t)| json!({ "group_id": g, "total": t })).collect::<Vec<_>>(),
                "rewards_granted": granted,
            }),
        )
        .await;
    Ok(winner)
}

/// Settle every challenge whose window closed; called each second by the `schedules` runner.
pub async fn settle_due(s: &AppState) -> Result<usize> {
    let due = sqlx::query_as::<_, Challenge>(&format!(
        "{SELECT} where settled_at is null and ends_at <= now() order by ends_at limit 50"
    ))
    .fetch_all(&s.db)
    .await
    .map_err(Error::internal)?;
    let mut n = 0;
    for c in &due {
        match settle(s, c).await {
            Ok(_) => n += 1,
            Err(Error::Conflict(_)) => {}
            Err(e) => {
                tracing::warn!(challenge = %c.id, error = %e, "group challenge settlement failed")
            }
        }
    }
    Ok(n)
}

async fn settle_now(
    State(s): State<AppState>,
    ServerKey: ServerKey,
    Path(id): Path<Uuid>,
) -> std::result::Result<Json<Value>, ApiError> {
    let c = load(&s.db, id).await?;
    if c.settled_at.is_some() {
        return Err(Error::Conflict("already settled".into()).into());
    }
    // closing early: the window ends now
    sqlx::query("update musubi.group_challenges set ends_at = least(ends_at, now()) where id = $1")
        .bind(id)
        .execute(&s.db)
        .await
        .map_err(Error::internal)?;
    let winner = settle(&s, &c).await?;
    let c = load(&s.db, id).await?;
    let mut v = standings_of(&s.db, &c, OffsetDateTime::now_utc(), 10).await?;
    v["winner"] = json!(winner);
    Ok(Json(v))
}

async fn sweep_now(
    State(s): State<AppState>,
    ServerKey: ServerKey,
) -> std::result::Result<Json<Value>, ApiError> {
    Ok(Json(json!({ "settled": settle_due(&s).await? })))
}

async fn sweep_noop(ServerKey: ServerKey) -> Json<Value> {
    Json(json!({ "ok": true }))
}
