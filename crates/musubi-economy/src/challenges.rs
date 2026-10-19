//! Player-created challenges: an operator publishes templates (objectives, duration, prizes per
//! rank); a player instantiates one, invites friends or opens it with a join code, everyone's
//! quest events score it, and when it ends the standings are frozen and prizes become claimable.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::quests::Objective;
use crate::rewards::RewardEntry;
use crate::EconomyService;

pub const MAX_ACTIVE_PER_OWNER: i64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChallengeTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub objectives: Vec<Objective>,
    pub duration_secs: i64,
    pub max_participants: i32,
    /// `{"1": "table", "2": "table", "default": "table"}`.
    pub rewards_by_rank: Map<String, Value>,
    pub enabled: bool,
    pub metadata: Value,
}

impl Default for ChallengeTemplate {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            objectives: Vec::new(),
            duration_secs: 86_400,
            max_participants: 8,
            rewards_by_rank: Map::new(),
            enabled: true,
            metadata: json!({}),
        }
    }
}

impl ChallengeTemplate {
    pub fn table_for_rank(&self, rank: i32) -> Option<String> {
        self.rewards_by_rank
            .get(&rank.to_string())
            .or_else(|| self.rewards_by_rank.get("default"))
            .and_then(|v| v.as_str().map(str::to_string))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Challenge {
    pub id: Uuid,
    pub template_id: String,
    pub owner_id: AccountId,
    pub name: String,
    pub open: bool,
    pub join_code: Option<String>,
    pub max_participants: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub starts_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub ends_at: OffsetDateTime,
    pub state: String,
}

#[derive(Debug, Clone, Default)]
pub struct NewChallenge {
    pub template_id: String,
    pub name: Option<String>,
    pub open: bool,
    pub starts_in_secs: i64,
    pub duration_secs: Option<i64>,
    pub max_participants: Option<i32>,
}

type TemplateRow = (String, String, String, Value, i64, i32, Value, bool, Value);
type ChallengeRow = (
    Uuid,
    String,
    Uuid,
    String,
    bool,
    Option<String>,
    i32,
    OffsetDateTime,
    OffsetDateTime,
    String,
);
type ParticipantRow = (
    Uuid,
    i64,
    Value,
    Option<i32>,
    Option<OffsetDateTime>,
    OffsetDateTime,
);

const T_COLS: &str =
    "id, name, description, objectives, duration_secs, max_participants, rewards_by_rank, enabled, metadata";
const C_COLS: &str =
    "id, template_id, owner_id, name, open, join_code, max_participants, starts_at, ends_at, state";

fn template(r: TemplateRow) -> ChallengeTemplate {
    ChallengeTemplate {
        id: r.0,
        name: r.1,
        description: r.2,
        objectives: serde_json::from_value(r.3).unwrap_or_default(),
        duration_secs: r.4,
        max_participants: r.5,
        rewards_by_rank: r.6.as_object().cloned().unwrap_or_default(),
        enabled: r.7,
        metadata: r.8,
    }
}

fn challenge(r: ChallengeRow) -> Challenge {
    Challenge {
        id: r.0,
        template_id: r.1,
        owner_id: AccountId(r.2),
        name: r.3,
        open: r.4,
        join_code: r.5,
        max_participants: r.6,
        starts_at: r.7,
        ends_at: r.8,
        state: r.9,
    }
}

/// Score = progress capped per objective, summed.
pub fn score(objectives: &[Objective], progress: &Value) -> i64 {
    objectives
        .iter()
        .map(|o| progress[&o.event].as_i64().unwrap_or(0).min(o.target))
        .sum()
}

fn join_code() -> String {
    let b = Uuid::now_v7();
    let b = b.as_bytes();
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    (8..16)
        .map(|i| A[(b[i] as usize) % A.len()] as char)
        .collect()
}

impl EconomyService {
    pub async fn upsert_challenge_template(
        &self,
        t: ChallengeTemplate,
    ) -> Result<ChallengeTemplate> {
        crate::check_code(&t.id)?;
        if t.objectives.is_empty()
            || t.objectives
                .iter()
                .any(|o| o.event.is_empty() || o.target <= 0)
        {
            return Err(Error::InvalidArgument(
                "objectives: non-empty, event + positive target".into(),
            ));
        }
        if !(60..=30 * 86_400).contains(&t.duration_secs) {
            return Err(Error::InvalidArgument("duration_secs: 60..=30 days".into()));
        }
        if !(2..=1000).contains(&t.max_participants) {
            return Err(Error::InvalidArgument("max_participants: 2..=1000".into()));
        }
        if t.rewards_by_rank.values().any(|v| !v.is_string()) {
            return Err(Error::InvalidArgument(
                "rewards_by_rank: {rank|default: reward_table}".into(),
            ));
        }
        let row: TemplateRow = sqlx::query_as(&format!(
            "insert into musubi.challenge_templates (id, name, description, objectives, duration_secs, max_participants, rewards_by_rank, enabled, metadata)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             on conflict (id) do update set name = excluded.name, description = excluded.description, objectives = excluded.objectives,
                 duration_secs = excluded.duration_secs, max_participants = excluded.max_participants,
                 rewards_by_rank = excluded.rewards_by_rank, enabled = excluded.enabled, metadata = excluded.metadata
             returning {T_COLS}"
        ))
        .bind(&t.id)
        .bind(&t.name)
        .bind(&t.description)
        .bind(serde_json::to_value(&t.objectives)?)
        .bind(t.duration_secs)
        .bind(t.max_participants)
        .bind(Value::Object(t.rewards_by_rank.clone()))
        .bind(t.enabled)
        .bind(&t.metadata)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(template(row))
    }

    pub async fn challenge_templates(&self) -> Result<Vec<ChallengeTemplate>> {
        let rows: Vec<TemplateRow> = sqlx::query_as(&format!(
            "select {T_COLS} from musubi.challenge_templates order by id"
        ))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(template).collect())
    }

    pub async fn challenge_template(&self, id: &str) -> Result<ChallengeTemplate> {
        let row: Option<TemplateRow> = sqlx::query_as(&format!(
            "select {T_COLS} from musubi.challenge_templates where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(template)
            .ok_or_else(|| Error::NotFound(format!("challenge template {id}")))
    }

    pub async fn challenge(&self, id: Uuid) -> Result<Challenge> {
        let row: Option<ChallengeRow> = sqlx::query_as(&format!(
            "select {C_COLS} from musubi.challenges where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(challenge)
            .ok_or_else(|| Error::NotFound(format!("challenge {id}")))
    }

    pub async fn create_challenge(
        &self,
        owner: AccountId,
        n: NewChallenge,
        now: OffsetDateTime,
    ) -> Result<Challenge> {
        let t = self.challenge_template(&n.template_id).await?;
        if !t.enabled {
            return Err(Error::NotFound(format!("challenge template {}", t.id)));
        }
        let duration = n.duration_secs.unwrap_or(t.duration_secs);
        if !(60..=t.duration_secs).contains(&duration) {
            return Err(Error::InvalidArgument(format!(
                "duration_secs: 60..={} for this template",
                t.duration_secs
            )));
        }
        let max = n.max_participants.unwrap_or(t.max_participants);
        if !(2..=t.max_participants).contains(&max) {
            return Err(Error::InvalidArgument(format!(
                "max_participants: 2..={} for this template",
                t.max_participants
            )));
        }
        if !(0..=7 * 86_400).contains(&n.starts_in_secs) {
            return Err(Error::InvalidArgument("starts_in_secs: 0..=7 days".into()));
        }
        let name = n.name.unwrap_or_else(|| t.name.clone());
        if name.chars().count() > 64 {
            return Err(Error::InvalidArgument("name: <= 64 chars".into()));
        }
        let active: i64 = sqlx::query_scalar(
            "select count(*) from musubi.challenges where owner_id = $1 and state = 'active'",
        )
        .bind(owner.0)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        if active >= MAX_ACTIVE_PER_OWNER {
            return Err(Error::ResourceExhausted(format!(
                "at most {MAX_ACTIVE_PER_OWNER} active challenges per player"
            )));
        }
        let starts = now + Duration::seconds(n.starts_in_secs);
        let id = Uuid::now_v7();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: ChallengeRow = sqlx::query_as(&format!(
            "insert into musubi.challenges (id, template_id, owner_id, name, open, join_code, max_participants, starts_at, ends_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning {C_COLS}"
        ))
        .bind(id)
        .bind(&t.id)
        .bind(owner.0)
        .bind(&name)
        .bind(n.open)
        .bind(join_code())
        .bind(max)
        .bind(starts)
        .bind(starts + Duration::seconds(duration))
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.challenge_participants (challenge_id, account_id) values ($1, $2)",
        )
        .bind(id)
        .bind(owner.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(challenge(row))
    }

    pub async fn invite_to_challenge(
        &self,
        by: AccountId,
        id: Uuid,
        targets: &[AccountId],
        now: OffsetDateTime,
    ) -> Result<Vec<AccountId>> {
        let c = self.challenge(id).await?;
        if c.state != "active" || c.ends_at <= now {
            return Err(Error::Conflict("challenge is over".into()));
        }
        if !self.is_participant(id, by).await? {
            return Err(Error::PermissionDenied(
                "only participants can invite".into(),
            ));
        }
        if targets.is_empty() || targets.len() > 50 {
            return Err(Error::InvalidArgument("account_ids: 1..=50".into()));
        }
        let mut out = Vec::new();
        for t in targets {
            if *t == by {
                continue;
            }
            let n = sqlx::query(
                "insert into musubi.challenge_invites (challenge_id, account_id, invited_by) values ($1, $2, $3)
                 on conflict do nothing",
            )
            .bind(id)
            .bind(t.0)
            .bind(by.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
            if n > 0 {
                out.push(*t);
            }
        }
        Ok(out)
    }

    async fn is_participant(&self, id: Uuid, account: AccountId) -> Result<bool> {
        let n: i64 = sqlx::query_scalar(
            "select count(*) from musubi.challenge_participants where challenge_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(n > 0)
    }

    pub async fn join_challenge(
        &self,
        account: AccountId,
        id: Uuid,
        code: Option<&str>,
        now: OffsetDateTime,
    ) -> Result<Challenge> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<ChallengeRow> = sqlx::query_as(&format!(
            "select {C_COLS} from musubi.challenges where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(c) = row.map(challenge) else {
            return Err(Error::NotFound(format!("challenge {id}")));
        };
        if c.state != "active" || c.ends_at <= now {
            return Err(Error::Conflict("challenge is over".into()));
        }
        let invited: i64 = sqlx::query_scalar(
            "select count(*) from musubi.challenge_invites where challenge_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let by_code =
            code.is_some_and(|k| c.join_code.as_deref() == Some(&k.trim().to_uppercase()));
        if !(c.open || invited > 0 || by_code) {
            return Err(Error::PermissionDenied(
                "challenge is private: invite or join code required".into(),
            ));
        }
        let count: i64 = sqlx::query_scalar(
            "select count(*) from musubi.challenge_participants where challenge_id = $1",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let n = sqlx::query(
            "insert into musubi.challenge_participants (challenge_id, account_id) values ($1, $2) on conflict do nothing",
        )
        .bind(id)
        .bind(account.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n > 0 && count >= c.max_participants as i64 {
            return Err(Error::ResourceExhausted("challenge is full".into()));
        }
        sqlx::query(
            "delete from musubi.challenge_invites where challenge_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(c)
    }

    pub async fn leave_challenge(&self, account: AccountId, id: Uuid) -> Result<()> {
        let c = self.challenge(id).await?;
        if c.owner_id == account {
            return Err(Error::Conflict(
                "the owner cannot leave; it ends on schedule".into(),
            ));
        }
        if c.state != "active" {
            return Err(Error::Conflict("challenge is over".into()));
        }
        let n = sqlx::query(
            "delete from musubi.challenge_participants where challenge_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("not a participant".into()));
        }
        Ok(())
    }

    /// Progress every running challenge the player takes part in; returns the touched ids.
    pub async fn challenge_event(
        &self,
        account: AccountId,
        event: &str,
        delta: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<Uuid>> {
        if event.is_empty() || delta <= 0 {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, Value, Value)> = sqlx::query_as(
            "select c.id, t.objectives, p.progress from musubi.challenge_participants p
             join musubi.challenges c on c.id = p.challenge_id
             join musubi.challenge_templates t on t.id = c.template_id
             where p.account_id = $1 and c.state = 'active' and c.starts_at <= $2 and c.ends_at > $2",
        )
        .bind(account.0)
        .bind(now)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out = Vec::new();
        for (id, objectives, mut progress) in rows {
            let objectives: Vec<Objective> = serde_json::from_value(objectives).unwrap_or_default();
            let Some(o) = objectives.iter().find(|o| o.event == event) else {
                continue;
            };
            let cur = progress[event].as_i64().unwrap_or(0);
            progress[event] = json!((cur + delta).min(o.target));
            let s = score(&objectives, &progress);
            sqlx::query(
                "update musubi.challenge_participants set progress = $3, score = $4 where challenge_id = $1 and account_id = $2",
            )
            .bind(id)
            .bind(account.0)
            .bind(&progress)
            .bind(s)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
            out.push(id);
        }
        Ok(out)
    }

    /// Freeze the standings of every challenge past its end; returns the settled ids.
    pub async fn settle_challenges(&self, now: OffsetDateTime) -> Result<Vec<Uuid>> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select id from musubi.challenges where state = 'active' and ends_at <= $1",
        )
        .bind(now)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        for id in &ids {
            self.settle_challenge(*id).await?;
        }
        Ok(ids)
    }

    async fn settle_challenge(&self, id: Uuid) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let n = sqlx::query(
            "update musubi.challenges set state = 'settled' where id = $1 and state = 'active'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Ok(());
        }
        sqlx::query(
            "update musubi.challenge_participants p set rank = r.rank from (
                 select account_id, row_number() over (order by score desc, joined_at asc)::int as rank
                 from musubi.challenge_participants where challenge_id = $1) r
             where p.challenge_id = $1 and p.account_id = r.account_id",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    /// Challenge with ordered standings; settles it first when its time is up.
    pub async fn challenge_standings(&self, id: Uuid, now: OffsetDateTime) -> Result<Value> {
        let mut c = self.challenge(id).await?;
        if c.state == "active" && c.ends_at <= now {
            self.settle_challenge(id).await?;
            c.state = "settled".into();
        }
        let t = self.challenge_template(&c.template_id).await?;
        let rows: Vec<ParticipantRow> = sqlx::query_as(
            "select account_id, score, progress, rank, claimed_at, joined_at from musubi.challenge_participants
             where challenge_id = $1 order by score desc, joined_at asc",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let invited: Vec<Uuid> = sqlx::query_scalar(
            "select account_id from musubi.challenge_invites where challenge_id = $1",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let standings: Vec<Value> = rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let rank = r.3.unwrap_or(i as i32 + 1);
                json!({
                    "account_id": r.0, "score": r.1, "progress": r.2, "rank": rank,
                    "claimed": r.4.is_some(),
                    "reward_table": (c.state == "settled").then(|| t.table_for_rank(rank)).flatten(),
                })
            })
            .collect();
        let mut v = serde_json::to_value(&c)?;
        v["template"] = json!({
            "id": t.id, "name": t.name, "description": t.description, "objectives": t.objectives,
            "rewards_by_rank": t.rewards_by_rank, "metadata": t.metadata,
        });
        v["participants"] = json!(standings.len());
        v["standings"] = Value::Array(standings);
        v["invited"] = json!(invited);
        v["running"] = json!(c.state == "active" && c.starts_at <= now);
        Ok(v)
    }

    /// Challenges the player takes part in or is invited to (newest first).
    pub async fn my_challenges(
        &self,
        account: AccountId,
        now: OffsetDateTime,
    ) -> Result<Vec<Value>> {
        self.settle_challenges(now).await?;
        let rows: Vec<(ChallengeRow, String, i64, Option<i32>, i64)> = sqlx::query_as::<_, (
            Uuid, String, Uuid, String, bool, Option<String>, i32, OffsetDateTime, OffsetDateTime, String,
            String, i64, Option<i32>, i64,
        )>(&format!(
            "select {}, x.role, coalesce(p.score, 0), p.rank,
                    (select count(*) from musubi.challenge_participants q where q.challenge_id = c.id)
             from (select challenge_id, 'participant' as role from musubi.challenge_participants where account_id = $1
                   union all
                   select challenge_id, 'invited' from musubi.challenge_invites where account_id = $1) x
             join musubi.challenges c on c.id = x.challenge_id
             left join musubi.challenge_participants p on p.challenge_id = c.id and p.account_id = $1
             order by c.created_at desc limit 100",
            C_COLS.split(", ").map(|c| format!("c.{c}")).collect::<Vec<_>>().join(", ")
        ))
        .bind(account.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?
        .into_iter()
        .map(|r| ((r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7, r.8, r.9), r.10, r.11, r.12, r.13))
        .collect();
        let mut out = Vec::new();
        for (row, role, score, rank, participants) in rows {
            let c = challenge(row);
            let hide_code = role != "participant";
            let mut v = serde_json::to_value(&c)?;
            if hide_code {
                v["join_code"] = Value::Null;
            }
            v["role"] = json!(role);
            v["my_score"] = json!(score);
            v["my_rank"] = json!(rank);
            v["participants"] = json!(participants);
            out.push(v);
        }
        Ok(out)
    }

    pub async fn claim_challenge(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(i32, Vec<RewardEntry>)> {
        let c = self.challenge(id).await?;
        if c.state == "active" && c.ends_at <= now {
            self.settle_challenge(id).await?;
        } else if c.state == "active" {
            return Err(Error::Conflict("challenge still running".into()));
        }
        let t = self.challenge_template(&c.template_id).await?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<(Option<i32>, Option<OffsetDateTime>)> = sqlx::query_as(
            "select rank, claimed_at from musubi.challenge_participants where challenge_id = $1 and account_id = $2 for update",
        )
        .bind(id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some((rank, claimed)) = row else {
            return Err(Error::NotFound("not a participant".into()));
        };
        if claimed.is_some() {
            return Err(Error::Conflict("already claimed".into()));
        }
        let rank = rank.unwrap_or(i32::MAX);
        let mut rewards = Vec::new();
        if let Some(table_id) = t.table_for_rank(rank) {
            let Some(table) = Self::load_table(&mut tx, &table_id).await? else {
                return Err(Error::NotFound(format!("reward table {table_id}")));
            };
            Self::apply_rewards(
                &mut tx,
                account,
                &table.entries,
                "challenge.claim",
                &format!("{id}:{rank}"),
            )
            .await?;
            rewards = table.entries;
        }
        sqlx::query(
            "update musubi.challenge_participants set claimed_at = $3 where challenge_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(account.0)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((rank, rewards))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_caps_each_objective() {
        let o = vec![
            Objective {
                event: "kills".into(),
                target: 10,
            },
            Objective {
                event: "wins".into(),
                target: 3,
            },
        ];
        assert_eq!(score(&o, &json!({"kills": 25, "wins": 2})), 12);
        assert_eq!(score(&o, &json!({})), 0);
    }

    #[test]
    fn rank_tables_fall_back_to_default() {
        let t = ChallengeTemplate {
            rewards_by_rank: json!({"1": "gold", "default": "bronze"})
                .as_object()
                .cloned()
                .unwrap(),
            ..Default::default()
        };
        assert_eq!(t.table_for_rank(1).as_deref(), Some("gold"));
        assert_eq!(t.table_for_rank(7).as_deref(), Some("bronze"));
        assert_eq!(ChallengeTemplate::default().table_for_rank(1), None);
        assert_eq!(join_code().len(), 8);
    }
}
