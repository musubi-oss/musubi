//! Social graph: friends, invites, blocks. Groups and parties live in their own modules.

use musubi_account::User;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// Relationship state as seen from the requesting user (Nakama-compatible numbering).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "i16", try_from = "i16")]
#[repr(i16)]
pub enum FriendState {
    Friend = 0,
    InviteSent = 1,
    InviteReceived = 2,
    Blocked = 3,
}

impl From<FriendState> for i16 {
    fn from(s: FriendState) -> i16 {
        s as i16
    }
}

impl TryFrom<i16> for FriendState {
    type Error = String;
    fn try_from(v: i16) -> std::result::Result<Self, String> {
        Ok(match v {
            0 => Self::Friend,
            1 => Self::InviteSent,
            2 => Self::InviteReceived,
            3 => Self::Blocked,
            _ => return Err(format!("bad friend state {v}")),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Friend {
    pub user: User,
    pub state: FriendState,
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct FriendList {
    pub friends: Vec<Friend>,
    pub cursor: Option<String>,
}

pub const MAX_FRIENDS: i64 = 1000;
pub const MAX_BATCH: usize = 100;

#[derive(Clone)]
pub struct SocialService {
    db: PgPool,
}

type FriendRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    serde_json::Value,
    OffsetDateTime,
    i16,
    OffsetDateTime,
);

impl SocialService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Add friends: sends an invite, or accepts an incoming one. Blocked (by either side) is a no-op.
    pub async fn add(&self, me: AccountId, others: &[AccountId]) -> Result<()> {
        check_batch(me, others)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let count: i64 = sqlx::query_scalar(
            "select count(*) from musubi.friends where user_id = $1 and state in (0, 1)",
        )
        .bind(me.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if count + others.len() as i64 > MAX_FRIENDS {
            return Err(Error::ResourceExhausted(format!(
                "friend limit {MAX_FRIENDS}"
            )));
        }
        for other in others {
            let exists: Option<i64> = sqlx::query_scalar(
                "select 1::bigint from musubi.accounts where id = $1 and deleted_at is null",
            )
            .bind(other.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if exists.is_none() {
                return Err(Error::NotFound(format!("user {other}")));
            }
            let states = sqlx::query_as::<_, (Uuid, i16)>(
                "select user_id, state from musubi.friends where (user_id = $1 and friend_id = $2) or (user_id = $2 and friend_id = $1) for update",
            )
            .bind(me.0)
            .bind(other.0)
            .fetch_all(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let mine = states.iter().find(|(u, _)| *u == me.0).map(|(_, s)| *s);
            let theirs = states.iter().find(|(u, _)| *u == other.0).map(|(_, s)| *s);
            if mine == Some(3) || theirs == Some(3) || mine == Some(0) || mine == Some(1) {
                continue;
            }
            if theirs == Some(1) {
                // They invited me → both become friends.
                upsert(&mut tx, me, *other, 0).await?;
                upsert(&mut tx, *other, me, 0).await?;
            } else {
                upsert(&mut tx, me, *other, 1).await?;
                upsert(&mut tx, *other, me, 2).await?;
            }
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Remove friends, cancel sent invites, reject received invites, or unblock.
    pub async fn delete(&self, me: AccountId, others: &[AccountId]) -> Result<()> {
        check_batch(me, others)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        for other in others {
            let mine: Option<i16> = sqlx::query_scalar(
                "select state from musubi.friends where user_id = $1 and friend_id = $2 for update",
            )
            .bind(me.0)
            .bind(other.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some(mine) = mine else { continue };
            sqlx::query("delete from musubi.friends where user_id = $1 and friend_id = $2")
                .bind(me.0)
                .bind(other.0)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            if mine != 3 {
                // Mirror row goes too (friend / pending invite), unless the other side blocked us.
                sqlx::query("delete from musubi.friends where user_id = $1 and friend_id = $2 and state <> 3")
                    .bind(other.0)
                    .bind(me.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
            }
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Block: drops any friendship/invite in both directions and records state 3 on my side.
    pub async fn block(&self, me: AccountId, others: &[AccountId]) -> Result<()> {
        check_batch(me, others)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        for other in others {
            sqlx::query(
                "delete from musubi.friends where user_id = $1 and friend_id = $2 and state <> 3",
            )
            .bind(other.0)
            .bind(me.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            upsert(&mut tx, me, *other, 3).await?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    pub async fn is_blocked(&self, by: AccountId, target: AccountId) -> Result<bool> {
        let r: Option<i16> = sqlx::query_scalar(
            "select state from musubi.friends where user_id = $1 and friend_id = $2 and state = 3",
        )
        .bind(by.0)
        .bind(target.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.is_some())
    }

    /// List relationships, optionally filtered by state; cursor = `updated_at|friend_id` of the last row.
    pub async fn list(
        &self,
        me: AccountId,
        state: Option<FriendState>,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<FriendList> {
        let limit = limit.clamp(1, 100) as i64;
        let (c_time, c_id) = match cursor {
            Some(c) => {
                let (t, id) = c
                    .split_once('|')
                    .ok_or_else(|| Error::InvalidArgument("bad cursor".into()))?;
                let t = OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339)
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?;
                let id: Uuid = id
                    .parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?;
                (Some(t), Some(id))
            }
            None => (None, None),
        };
        let rows = sqlx::query_as::<_, FriendRow>(
            "select a.id, a.username, a.display_name, a.avatar_url, a.lang, a.location, a.metadata, a.created_at, f.state, f.updated_at
             from musubi.friends f join musubi.accounts a on a.id = f.friend_id
             where f.user_id = $1 and a.deleted_at is null
               and ($2::smallint is null or f.state = $2)
               and ($3::timestamptz is null or (f.updated_at, f.friend_id) < ($3, $4))
             order by f.updated_at desc, f.friend_id desc limit $5",
        )
        .bind(me.0)
        .bind(state.map(|s| s as i16))
        .bind(c_time)
        .bind(c_id)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let friends: Vec<Friend> = rows
            .into_iter()
            .take(limit as usize)
            .map(|r| Friend {
                user: User {
                    id: AccountId(r.0),
                    username: r.1,
                    display_name: r.2,
                    avatar_url: r.3,
                    lang: r.4,
                    location: r.5,
                    metadata: r.6,
                    created_at: r.7,
                },
                state: FriendState::try_from(r.8).unwrap_or(FriendState::Blocked),
                update_time: r.9,
            })
            .collect();
        let cursor = if more {
            friends.last().map(|f| {
                format!(
                    "{}|{}",
                    f.update_time
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default(),
                    f.user.id.0
                )
            })
        } else {
            None
        };
        Ok(FriendList { friends, cursor })
    }
}

async fn upsert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: AccountId,
    friend: AccountId,
    state: i16,
) -> Result<()> {
    sqlx::query(
        "insert into musubi.friends (user_id, friend_id, state) values ($1, $2, $3)
         on conflict (user_id, friend_id) do update set state = excluded.state, updated_at = now()",
    )
    .bind(user.0)
    .bind(friend.0)
    .bind(state)
    .execute(&mut **tx)
    .await
    .map_err(Error::internal)?;
    Ok(())
}

fn check_batch(me: AccountId, others: &[AccountId]) -> Result<()> {
    if others.is_empty() || others.len() > MAX_BATCH {
        return Err(Error::InvalidArgument(format!(
            "1..={MAX_BATCH} ids required"
        )));
    }
    if others.contains(&me) {
        return Err(Error::InvalidArgument("cannot befriend yourself".into()));
    }
    Ok(())
}
