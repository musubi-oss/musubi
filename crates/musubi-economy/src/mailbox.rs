//! Player mailbox: messages with optional attached reward tables, read/claim/expire, bulk send.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use crate::rewards::RewardEntry;
use crate::EconomyService;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SendMail {
    pub account_ids: Vec<Uuid>,
    /// Send to every account (ignores `account_ids`).
    pub all: bool,
    pub title: String,
    pub body: String,
    pub metadata: Value,
    /// Reward table attached to the message.
    pub rewards: Option<String>,
    pub expires_in_seconds: Option<i64>,
}

impl Default for SendMail {
    fn default() -> Self {
        Self {
            account_ids: Vec::new(),
            all: false,
            title: String::new(),
            body: String::new(),
            metadata: json!({}),
            rewards: None,
            expires_in_seconds: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Mail {
    pub id: Uuid,
    pub title: String,
    pub body: String,
    pub metadata: Value,
    pub rewards: Option<String>,
    pub read: bool,
    pub claimed: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

type MailRow = (
    Uuid,
    String,
    String,
    Value,
    Option<String>,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    Option<OffsetDateTime>,
    OffsetDateTime,
);

fn mail(r: MailRow) -> Mail {
    Mail {
        id: r.0,
        title: r.1,
        body: r.2,
        metadata: r.3,
        rewards: r.4,
        read: r.5.is_some(),
        claimed: r.6.is_some(),
        expires_at: r.7,
        created_at: r.8,
    }
}

const COLS: &str =
    "id, title, body, metadata, rewards, read_at, claimed_at, expires_at, created_at";

impl EconomyService {
    /// Returns the number of messages created.
    pub async fn send_mail(&self, m: SendMail, now: OffsetDateTime) -> Result<i64> {
        if m.title.is_empty() || m.title.len() > 200 || m.body.len() > 16 * 1024 {
            return Err(Error::InvalidArgument(
                "title 1..=200 chars, body <= 16 KiB".into(),
            ));
        }
        if !m.all && (m.account_ids.is_empty() || m.account_ids.len() > 10_000) {
            return Err(Error::InvalidArgument(
                "account_ids 1..=10000 (or all=true)".into(),
            ));
        }
        let expires_at = match m.expires_in_seconds {
            Some(s) if s <= 0 => {
                return Err(Error::InvalidArgument(
                    "expires_in_seconds must be positive".into(),
                ))
            }
            Some(s) => Some(now + Duration::seconds(s)),
            None => None,
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        if let Some(t) = &m.rewards {
            if Self::load_table(&mut tx, t).await?.is_none() {
                return Err(Error::NotFound(format!("reward table {t}")));
            }
        }
        let sql = if m.all {
            "insert into musubi.mail (id, account_id, title, body, metadata, rewards, expires_at, created_at)
             select gen_random_uuid(), a.id, $2, $3, $4, $5, $6, $7 from musubi.accounts a"
        } else {
            "insert into musubi.mail (id, account_id, title, body, metadata, rewards, expires_at, created_at)
             select gen_random_uuid(), a.id, $2, $3, $4, $5, $6, $7 from unnest($1::uuid[]) as a(id)"
        };
        let n = sqlx::query(sql)
            .bind(&m.account_ids)
            .bind(&m.title)
            .bind(&m.body)
            .bind(&m.metadata)
            .bind(&m.rewards)
            .bind(expires_at)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        tx.commit().await.map_err(Error::internal)?;
        Ok(n as i64)
    }

    pub async fn mail(
        &self,
        account: AccountId,
        unread_only: bool,
        limit: i64,
        now: OffsetDateTime,
    ) -> Result<Vec<Mail>> {
        let rows: Vec<MailRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.mail where account_id = $1 and (expires_at is null or expires_at > $4)
             and (not $2 or read_at is null) order by created_at desc, id desc limit $3"
        ))
        .bind(account.0)
        .bind(unread_only)
        .bind(limit.clamp(1, crate::MAX_LEDGER_PAGE))
        .bind(now)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(mail).collect())
    }

    pub async fn read_mail(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<Mail> {
        let row: Option<MailRow> = sqlx::query_as(&format!(
            "update musubi.mail set read_at = coalesce(read_at, $3) where id = $1 and account_id = $2 returning {COLS}"
        ))
        .bind(id)
        .bind(account.0)
        .bind(now)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        row.map(mail).ok_or_else(|| Error::NotFound("mail".into()))
    }

    /// Claims attached rewards (granted in the same transaction) and marks the message read.
    pub async fn claim_mail(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<(Mail, Vec<RewardEntry>)> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let row: Option<MailRow> = sqlx::query_as(&format!(
            "select {COLS} from musubi.mail where id = $1 and account_id = $2 for update"
        ))
        .bind(id)
        .bind(account.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let Some(m) = row.map(mail) else {
            return Err(Error::NotFound("mail".into()));
        };
        if m.claimed {
            return Err(Error::Conflict("mail already claimed".into()));
        }
        if m.expires_at.is_some_and(|e| e <= now) {
            return Err(Error::PermissionDenied("mail expired".into()));
        }
        let mut rewards = Vec::new();
        if let Some(t) = &m.rewards {
            let Some(table) = Self::load_table(&mut tx, t).await? else {
                return Err(Error::NotFound(format!("reward table {t}")));
            };
            Self::apply_rewards(
                &mut tx,
                account,
                &table.entries,
                "mail.claim",
                &id.to_string(),
            )
            .await?;
            rewards = table.entries;
        }
        let row: MailRow = sqlx::query_as(&format!(
            "update musubi.mail set claimed_at = $2, read_at = coalesce(read_at, $2) where id = $1 returning {COLS}"
        ))
        .bind(id)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok((mail(row), rewards))
    }

    /// Deletes a message; refuses while unclaimed rewards are attached (unless expired).
    pub async fn delete_mail(
        &self,
        account: AccountId,
        id: Uuid,
        now: OffsetDateTime,
    ) -> Result<()> {
        let row: Option<(Option<String>, Option<OffsetDateTime>, Option<OffsetDateTime>)> =
            sqlx::query_as("select rewards, claimed_at, expires_at from musubi.mail where id = $1 and account_id = $2")
                .bind(id)
                .bind(account.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        let Some((rewards, claimed, expires)) = row else {
            return Err(Error::NotFound("mail".into()));
        };
        let expired = expires.is_some_and(|e| e <= now);
        if rewards.is_some() && claimed.is_none() && !expired {
            return Err(Error::Conflict("claim the attached rewards first".into()));
        }
        sqlx::query("delete from musubi.mail where id = $1 and account_id = $2")
            .bind(id)
            .bind(account.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }
}
