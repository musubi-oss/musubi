//! Donations (Hiro-style): a player opens a request from a definition — paying its `cost` — and
//! friends, group mates or anyone contribute up to `user_contribution_max_count` each until
//! `max_count` is reached or `duration_secs` run out. Every contribution hands the contributor
//! `contributor_reward` immediately and accrues one `recipient_reward` the requester claims later.
//! Live donations snapshot their definition so a redefinition never changes a running request.

use std::collections::BTreeMap;

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::rewards::RewardEntry;
use crate::EconomyService;

pub const MAX_DURATION_SECS: i64 = 90 * 86_400;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DonationDef {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Currency → amount the requester pays to open the request.
    pub cost: BTreeMap<String, i64>,
    /// Contributions the request asks for (shown as the goal).
    pub count: i32,
    /// Hard ceiling on contributions (≥ `count`).
    pub max_count: i32,
    pub duration_secs: i64,
    /// Contributions one helper may make to one request.
    pub user_contribution_max_count: i32,
    /// Reward table granted to the requester once per contribution (on claim).
    pub recipient_reward: Option<String>,
    /// Reward table granted to the helper per contribution, immediately.
    pub contributor_reward: Option<String>,
    /// Who may contribute: `friends` (mutual friends of the requester), `groups` (shares a group
    /// with the requester) or `anyone`.
    pub audience: String,
    pub enabled: bool,
    pub metadata: Value,
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
}

impl Default for DonationDef {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            cost: BTreeMap::new(),
            count: 1,
            max_count: 1,
            duration_secs: 86_400,
            user_contribution_max_count: 1,
            recipient_reward: None,
            contributor_reward: None,
            audience: "friends".into(),
            enabled: true,
            metadata: Value::Object(Default::default()),
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Contribution {
    pub account_id: AccountId,
    pub count: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct Donation {
    pub id: Uuid,
    pub account_id: AccountId,
    pub def_id: String,
    pub count: i32,
    pub claimed_count: i32,
    pub target_count: i32,
    pub max_count: i32,
    pub user_contribution_max_count: i32,
    pub recipient_reward: Option<String>,
    pub contributor_reward: Option<String>,
    pub audience: String,
    /// `active` | `completed` (ceiling reached) | `expired`.
    pub status: String,
    /// Contributions the requester has not claimed rewards for yet.
    pub claimable: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub expires_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub contributions: Vec<Contribution>,
}

type DefRow = (
    String,
    String,
    String,
    Value,
    i32,
    i32,
    i64,
    i32,
    Option<String>,
    Option<String>,
    String,
    bool,
    Value,
    OffsetDateTime,
);
const DEF_COLS: &str =
    "id, name, description, cost, count, max_count, duration_secs, user_contribution_max_count, \
    recipient_reward, contributor_reward, audience, enabled, metadata, updated_at";

fn def_from(r: DefRow) -> DonationDef {
    DonationDef {
        id: r.0,
        name: r.1,
        description: r.2,
        cost: serde_json::from_value(r.3).unwrap_or_default(),
        count: r.4,
        max_count: r.5,
        duration_secs: r.6,
        user_contribution_max_count: r.7,
        recipient_reward: r.8,
        contributor_reward: r.9,
        audience: r.10,
        enabled: r.11,
        metadata: r.12,
        updated_at: Some(r.13),
    }
}

type DonRow = (
    Uuid,
    Uuid,
    String,
    i32,
    i32,
    i32,
    i32,
    i32,
    Option<String>,
    Option<String>,
    String,
    OffsetDateTime,
    OffsetDateTime,
);
const DON_COLS: &str =
    "id, account_id, def_id, count, claimed_count, target_count, max_count, user_max, \
    recipient_reward, contributor_reward, audience, expires_at, created_at";

fn status(count: i32, max: i32, expires_at: OffsetDateTime, now: OffsetDateTime) -> &'static str {
    if count >= max {
        "completed"
    } else if expires_at <= now {
        "expired"
    } else {
        "active"
    }
}

fn donation_from(r: DonRow, contributions: Vec<Contribution>, now: OffsetDateTime) -> Donation {
    Donation {
        id: r.0,
        account_id: AccountId(r.1),
        def_id: r.2,
        count: r.3,
        claimed_count: r.4,
        target_count: r.5,
        max_count: r.6,
        user_contribution_max_count: r.7,
        recipient_reward: r.8,
        contributor_reward: r.9,
        audience: r.10,
        status: status(r.3, r.6, r.11, now).into(),
        claimable: r.3 - r.4,
        expires_at: r.11,
        created_at: r.12,
        contributions,
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl EconomyService {
    pub async fn upsert_donation_def(&self, mut d: DonationDef) -> Result<DonationDef> {
        if !valid_id(&d.id) {
            return Err(Error::InvalidArgument(
                "donation id: 1..=64 of [a-z0-9_-]".into(),
            ));
        }
        if d.name.len() > 128 || d.description.len() > 2048 {
            return Err(Error::InvalidArgument(
                "name ≤ 128, description ≤ 2048".into(),
            ));
        }
        if d.count < 1 || d.max_count < d.count || d.max_count > 10_000 {
            return Err(Error::InvalidArgument(
                "need 1 <= count <= max_count <= 10000".into(),
            ));
        }
        if d.duration_secs < 60 || d.duration_secs > MAX_DURATION_SECS {
            return Err(Error::InvalidArgument(
                "duration_secs: 60 .. 90 days".into(),
            ));
        }
        if d.user_contribution_max_count < 1 || d.user_contribution_max_count > d.max_count {
            return Err(Error::InvalidArgument(
                "user_contribution_max_count: 1 .. max_count".into(),
            ));
        }
        if d.audience.is_empty() {
            d.audience = "friends".into();
        }
        if !["friends", "groups", "anyone"].contains(&d.audience.as_str()) {
            return Err(Error::InvalidArgument(
                "audience: friends | groups | anyone".into(),
            ));
        }
        if !d.metadata.is_object() {
            if d.metadata.is_null() {
                d.metadata = Value::Object(Default::default());
            } else {
                return Err(Error::InvalidArgument("metadata must be an object".into()));
            }
        }
        for (cur, amount) in &d.cost {
            if *amount <= 0 {
                return Err(Error::InvalidArgument(format!(
                    "cost.{cur} must be positive"
                )));
            }
            let known: Option<(String,)> =
                sqlx::query_as("select code from musubi.currencies where code = $1")
                    .bind(cur)
                    .fetch_optional(&self.db)
                    .await
                    .map_err(Error::internal)?;
            if known.is_none() {
                return Err(Error::InvalidArgument(format!(
                    "cost: unknown currency `{cur}`"
                )));
            }
        }
        for (what, table) in [
            ("recipient_reward", &d.recipient_reward),
            ("contributor_reward", &d.contributor_reward),
        ] {
            if let Some(t) = table.as_deref().filter(|t| !t.is_empty()) {
                let known: Option<(String,)> =
                    sqlx::query_as("select id from musubi.reward_tables where id = $1")
                        .bind(t)
                        .fetch_optional(&self.db)
                        .await
                        .map_err(Error::internal)?;
                if known.is_none() {
                    return Err(Error::InvalidArgument(format!(
                        "{what}: unknown reward table `{t}`"
                    )));
                }
            }
        }
        let norm = |t: &Option<String>| t.clone().filter(|t| !t.is_empty());
        let row: DefRow = sqlx::query_as(&format!(
            "insert into musubi.donation_defs ({DEF_COLS})
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, now())
             on conflict (id) do update set name = excluded.name, description = excluded.description,
                 cost = excluded.cost, count = excluded.count, max_count = excluded.max_count,
                 duration_secs = excluded.duration_secs,
                 user_contribution_max_count = excluded.user_contribution_max_count,
                 recipient_reward = excluded.recipient_reward, contributor_reward = excluded.contributor_reward,
                 audience = excluded.audience, enabled = excluded.enabled, metadata = excluded.metadata,
                 updated_at = now()
             returning {DEF_COLS}"
        ))
        .bind(&d.id)
        .bind(&d.name)
        .bind(&d.description)
        .bind(serde_json::to_value(&d.cost).unwrap_or_default())
        .bind(d.count)
        .bind(d.max_count)
        .bind(d.duration_secs)
        .bind(d.user_contribution_max_count)
        .bind(norm(&d.recipient_reward))
        .bind(norm(&d.contributor_reward))
        .bind(&d.audience)
        .bind(d.enabled)
        .bind(&d.metadata)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(def_from(row))
    }

    pub async fn donation_defs(&self, include_disabled: bool) -> Result<Vec<DonationDef>> {
        let rows: Vec<DefRow> = sqlx::query_as(&format!(
            "select {DEF_COLS} from musubi.donation_defs where $1 or enabled order by id"
        ))
        .bind(include_disabled)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(def_from).collect())
    }

    pub async fn donation_def(&self, id: &str) -> Result<DonationDef> {
        let row: Option<DefRow> = sqlx::query_as(&format!(
            "select {DEF_COLS} from musubi.donation_defs where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(def_from)
            .ok_or_else(|| Error::NotFound(format!("donation definition `{id}`")))
    }

    pub async fn delete_donation_def(&self, id: &str) -> Result<bool> {
        let n = sqlx::query("delete from musubi.donation_defs where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }

    async fn contributions(&self, ids: &[Uuid]) -> Result<Vec<(Uuid, Contribution)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(Uuid, Uuid, i32, OffsetDateTime)> = sqlx::query_as(
            "select donation_id, account_id, count, updated_at from musubi.donation_contributions
             where donation_id = any($1) order by updated_at desc",
        )
        .bind(ids)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                (
                    r.0,
                    Contribution {
                        account_id: AccountId(r.1),
                        count: r.2,
                        updated_at: r.3,
                    },
                )
            })
            .collect())
    }

    async fn assemble(&self, rows: Vec<DonRow>) -> Result<Vec<Donation>> {
        let now = OffsetDateTime::now_utc();
        let ids: Vec<Uuid> = rows.iter().map(|r| r.0).collect();
        let mut contribs = self.contributions(&ids).await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let mine: Vec<Contribution> = contribs
                    .iter()
                    .filter(|(d, _)| *d == r.0)
                    .map(|(_, c)| c.clone())
                    .collect();
                contribs.retain(|(d, _)| *d != r.0);
                donation_from(r, mine, now)
            })
            .collect())
    }

    /// A player's donations; `active_only` hides completed/expired ones that have nothing left
    /// to claim.
    pub async fn donations_of(
        &self,
        account: AccountId,
        active_only: bool,
    ) -> Result<Vec<Donation>> {
        let rows: Vec<DonRow> = sqlx::query_as(&format!(
            "select {DON_COLS} from musubi.donations where account_id = $1
               and (not $2 or (count < max_count and expires_at > now()) or count > claimed_count)
             order by created_at desc limit 200"
        ))
        .bind(account.0)
        .bind(active_only)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        self.assemble(rows).await
    }

    pub async fn donation(&self, id: Uuid) -> Result<Donation> {
        let row: Option<DonRow> = sqlx::query_as(&format!(
            "select {DON_COLS} from musubi.donations where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("donation {id}")));
        };
        self.assemble(vec![row])
            .await?
            .pop()
            .ok_or_else(|| Error::NotFound(format!("donation {id}")))
    }

    /// Opens a request: pays the definition's `cost` and snapshots it. One live request per
    /// definition per player.
    pub async fn request_donation(&self, account: AccountId, def_id: &str) -> Result<Donation> {
        let def = self.donation_def(def_id).await?;
        if !def.enabled {
            return Err(Error::InvalidArgument(format!(
                "donation `{def_id}` is disabled"
            )));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        // No row to lock before the first request: serialise per (account, definition).
        Self::lock_key(
            &mut tx,
            "donation.request",
            &format!("{}:{def_id}", account.0),
        )
        .await?;
        let live: Option<(Uuid,)> = sqlx::query_as(
            "select id from musubi.donations where account_id = $1 and def_id = $2
               and count < max_count and expires_at > now() for update",
        )
        .bind(account.0)
        .bind(def_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if let Some((id,)) = live {
            return Err(Error::InvalidArgument(format!(
                "donation `{def_id}` is already open ({id})"
            )));
        }
        let id = Uuid::now_v7();
        for (cur, amount) in &def.cost {
            Self::adjust_tx(
                &mut tx,
                account,
                cur,
                -amount,
                "donation_request",
                &format!("donation:{id}"),
                None,
                serde_json::json!({ "donation": def_id }),
            )
            .await?;
        }
        let row: DonRow = sqlx::query_as(&format!(
            "insert into musubi.donations (id, account_id, def_id, target_count, max_count, user_max,
                 recipient_reward, contributor_reward, audience, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, now() + make_interval(secs => $10))
             returning {DON_COLS}"
        ))
        .bind(id)
        .bind(account.0)
        .bind(def_id)
        .bind(def.count)
        .bind(def.max_count)
        .bind(def.user_contribution_max_count)
        .bind(&def.recipient_reward)
        .bind(&def.contributor_reward)
        .bind(&def.audience)
        .bind(def.duration_secs as f64)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(donation_from(row, Vec::new(), OffsetDateTime::now_utc()))
    }

    async fn may_contribute(
        &self,
        audience: &str,
        recipient: Uuid,
        contributor: Uuid,
    ) -> Result<bool> {
        let ok: Option<(bool,)> = match audience {
            "anyone" => return Ok(true),
            "friends" => sqlx::query_as(
                "select true from musubi.friends a join musubi.friends b
                    on a.user_id = b.friend_id and a.friend_id = b.user_id
                  where a.user_id = $1 and a.friend_id = $2 and a.state = 0 and b.state = 0",
            ),
            _ => sqlx::query_as(
                "select true from musubi.group_members a join musubi.group_members b on a.group_id = b.group_id
                  where a.user_id = $1 and b.user_id = $2 and a.state <= 2 and b.state <= 2 limit 1",
            ),
        }
        .bind(recipient)
        .bind(contributor)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(ok.is_some())
    }

    /// One contribution by `contributor`; returns the updated donation and what the helper got.
    pub async fn give_donation(
        &self,
        contributor: AccountId,
        id: Uuid,
    ) -> Result<(Donation, Vec<RewardEntry>)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<DonRow> = sqlx::query_as(&format!(
            "select {DON_COLS} from musubi.donations where id = $1 for update"
        ))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("donation {id}")));
        };
        if row.1 == contributor.0 {
            return Err(Error::InvalidArgument(
                "you cannot contribute to your own donation".into(),
            ));
        }
        let now = OffsetDateTime::now_utc();
        match status(row.3, row.6, row.11, now) {
            "completed" => {
                return Err(Error::InvalidArgument(
                    "donation already has everything it asked for".into(),
                ))
            }
            "expired" => return Err(Error::InvalidArgument("donation has expired".into())),
            _ => {}
        }
        if !self.may_contribute(&row.10, row.1, contributor.0).await? {
            return Err(Error::PermissionDenied(format!(
                "donation is open to `{}` of the requester only",
                row.10
            )));
        }
        let mine: Option<(i32,)> = sqlx::query_as(
            "select count from musubi.donation_contributions where donation_id = $1 and account_id = $2",
        )
        .bind(id)
        .bind(contributor.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if mine.map(|m| m.0).unwrap_or(0) >= row.7 {
            return Err(Error::InvalidArgument(format!(
                "you already contributed the maximum ({}) to this donation",
                row.7
            )));
        }
        sqlx::query(
            "insert into musubi.donation_contributions (donation_id, account_id, count) values ($1, $2, 1)
             on conflict (donation_id, account_id) do update
                 set count = musubi.donation_contributions.count + 1, updated_at = now()",
        )
        .bind(id)
        .bind(contributor.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let (n,): (i32,) = sqlx::query_as(
            "update musubi.donations set count = count + 1 where id = $1 returning count",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let rewards = match &row.9 {
            Some(table) => {
                self.grant_table(contributor, table, &format!("donation:{id}:give:{n}"))
                    .await?
            }
            None => Vec::new(),
        };
        Ok((self.donation(id).await?, rewards))
    }

    /// The requester collects `recipient_reward` once per unclaimed contribution.
    pub async fn claim_donation(
        &self,
        account: AccountId,
        id: Uuid,
    ) -> Result<(Donation, Vec<RewardEntry>, i32)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<DonRow> = sqlx::query_as(&format!(
            "select {DON_COLS} from musubi.donations where id = $1 and account_id = $2 for update"
        ))
        .bind(id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(row) = row else {
            return Err(Error::NotFound(format!("donation {id}")));
        };
        let unclaimed = row.3 - row.4;
        if unclaimed <= 0 {
            return Err(Error::InvalidArgument("nothing to claim yet".into()));
        }
        sqlx::query("update musubi.donations set claimed_count = count where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let mut rewards = Vec::new();
        if let Some(table) = &row.8 {
            for k in (row.4 + 1)..=row.3 {
                rewards.extend(
                    self.grant_table(account, table, &format!("donation:{id}:claim:{k}"))
                        .await?,
                );
            }
        }
        Ok((self.donation(id).await?, rewards, unclaimed))
    }

    /// Drops finished donations with nothing left to claim after a grace week.
    pub async fn sweep_donations(&self) -> Result<u64> {
        let n = sqlx::query(
            "delete from musubi.donations
             where count = claimed_count
               and (count >= max_count or expires_at < now())
               and greatest(expires_at, created_at) < now() - interval '7 days'",
        )
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        Ok(n)
    }
}
