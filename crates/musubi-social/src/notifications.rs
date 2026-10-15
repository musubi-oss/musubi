//! In-app notifications: persisted per user, optionally pushed live by the caller.

use musubi_core::{AccountId, Error, Result};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// System notification codes (negative, reserved; app codes are positive).
pub mod code {
    pub const FRIEND_REQUEST: i32 = -2;
    pub const FRIEND_ACCEPTED: i32 = -3;
    pub const GROUP_JOIN_REQUEST: i32 = -4;
    pub const GROUP_JOIN_ACCEPTED: i32 = -5;
    pub const FRIEND_JOINED_GAME: i32 = -6;
    pub const SINGLE_SOCKET: i32 = -7;
}

#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    pub id: Uuid,
    pub subject: String,
    pub content: serde_json::Value,
    pub code: i32,
    pub sender_id: Option<AccountId>,
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,
    pub persistent: bool,
    /// When the recipient marked it read; absent while unread.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub read_time: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NewNotification {
    pub user_id: AccountId,
    pub sender_id: Option<AccountId>,
    pub subject: String,
    pub content: serde_json::Value,
    pub code: i32,
    pub persistent: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NotificationList {
    pub notifications: Vec<Notification>,
    pub cursor: Option<String>,
    /// Unread notifications of the recipient (all of them, not just this page).
    pub unread_count: i64,
}

pub const MAX_BATCH: usize = 100;

#[derive(Clone)]
pub struct NotificationService {
    db: PgPool,
}

type Row = (
    Uuid,
    String,
    serde_json::Value,
    i32,
    Option<Uuid>,
    OffsetDateTime,
    Option<OffsetDateTime>,
);

impl NotificationService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Persist what must be persisted and return every notification (with ids) for live delivery.
    pub async fn send(
        &self,
        batch: Vec<NewNotification>,
    ) -> Result<Vec<(AccountId, Notification)>> {
        if batch.is_empty() || batch.len() > MAX_BATCH {
            return Err(Error::InvalidArgument(format!(
                "1..={MAX_BATCH} notifications per call"
            )));
        }
        let mut out = Vec::with_capacity(batch.len());
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        for n in batch {
            if n.subject.chars().count() > 256 || serde_json::to_vec(&n.content)?.len() > 16 * 1024
            {
                return Err(Error::InvalidArgument(
                    "subject ≤ 256 chars, content ≤ 16 KB".into(),
                ));
            }
            let id = Uuid::now_v7();
            let created = if n.persistent {
                sqlx::query_scalar::<_, OffsetDateTime>(
                    "insert into musubi.notifications (id, user_id, sender_id, subject, content, code) values ($1, $2, $3, $4, $5, $6) returning created_at",
                )
                .bind(id)
                .bind(n.user_id.0)
                .bind(n.sender_id.map(|s| s.0))
                .bind(&n.subject)
                .bind(&n.content)
                .bind(n.code)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?
            } else {
                OffsetDateTime::now_utc()
            };
            out.push((
                n.user_id,
                Notification {
                    id,
                    subject: n.subject,
                    content: n.content,
                    code: n.code,
                    sender_id: n.sender_id,
                    create_time: created,
                    persistent: n.persistent,
                    read_time: None,
                },
            ));
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(out)
    }

    /// Newest first; cursor is the id of the last returned row.
    pub async fn list(
        &self,
        user: AccountId,
        limit: usize,
        cursor: Option<&str>,
        unread_only: bool,
    ) -> Result<NotificationList> {
        let limit = limit.clamp(1, 100) as i64;
        let before: Option<Uuid> = match cursor {
            Some(c) => Some(
                c.parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?,
            ),
            None => None,
        };
        let rows = sqlx::query_as::<_, Row>(
            "select id, subject, content, code, sender_id, created_at, read_at from musubi.notifications
             where user_id = $1 and ($2::uuid is null or id < $2) and (not $4 or read_at is null)
             order by id desc limit $3",
        )
        .bind(user.0)
        .bind(before)
        .bind(limit + 1)
        .bind(unread_only)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let notifications: Vec<Notification> = rows
            .into_iter()
            .take(limit as usize)
            .map(|r| Notification {
                id: r.0,
                subject: r.1,
                content: r.2,
                code: r.3,
                sender_id: r.4.map(AccountId),
                create_time: r.5,
                persistent: true,
                read_time: r.6,
            })
            .collect();
        let cursor = more
            .then(|| notifications.last().map(|n| n.id.to_string()))
            .flatten();
        let unread_count: i64 = sqlx::query_scalar(
            "select count(*) from musubi.notifications where user_id = $1 and read_at is null",
        )
        .bind(user.0)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(NotificationList {
            notifications,
            cursor,
            unread_count,
        })
    }

    /// Mark notifications read; `None` = everything unread. Returns how many changed.
    pub async fn mark_read(&self, user: AccountId, ids: Option<&[Uuid]>) -> Result<u64> {
        let r = match ids {
            Some(ids) => {
                if ids.is_empty() || ids.len() > MAX_BATCH {
                    return Err(Error::InvalidArgument(format!("1..={MAX_BATCH} ids")));
                }
                sqlx::query(
                    "update musubi.notifications set read_at = now()
                     where user_id = $1 and id = any($2) and read_at is null",
                )
                .bind(user.0)
                .bind(ids)
                .execute(&self.db)
                .await
            }
            None => {
                sqlx::query(
                    "update musubi.notifications set read_at = now() where user_id = $1 and read_at is null",
                )
                .bind(user.0)
                .execute(&self.db)
                .await
            }
        }
        .map_err(Error::internal)?;
        Ok(r.rows_affected())
    }

    pub async fn delete(&self, user: AccountId, ids: &[Uuid]) -> Result<u64> {
        if ids.is_empty() || ids.len() > MAX_BATCH {
            return Err(Error::InvalidArgument(format!("1..={MAX_BATCH} ids")));
        }
        let r = sqlx::query("delete from musubi.notifications where user_id = $1 and id = any($2)")
            .bind(user.0)
            .bind(ids)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected())
    }
}
