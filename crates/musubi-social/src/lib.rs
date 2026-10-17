//! Social graph: friends, invites, blocks, groups.

pub mod groups;
pub mod notifications;
pub use groups::{
    CreateGroup, Group, GroupBan, GroupEvent, GroupFilter, GroupMember, GroupService, MemberState,
    Page, UpdateGroup, UserGroup,
};
pub use notifications::{NewNotification, Notification, NotificationList, NotificationService};

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
    /// Private note about this friend (only the owner sees it).
    pub note: Option<String>,
    pub favorite: bool,
}

/// Someone the viewer recently shared a match with.
#[derive(Debug, Clone, Serialize)]
pub struct RecentPlayer {
    pub user: User,
    pub match_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub seen_at: OffsetDateTime,
}

/// A friend suggestion: friends of friends ranked by mutual friends, then recent teammates.
#[derive(Debug, Clone, Serialize)]
pub struct Suggestion {
    pub user: User,
    pub mutual_friends: i64,
    /// `mutual` or `recent`.
    pub reason: String,
}

/// How many recent players are kept per account.
pub const RECENT_PLAYERS: i64 = 50;

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
    Option<String>,
    bool,
);
type UserRow = (
    Uuid,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    serde_json::Value,
    OffsetDateTime,
);
const USER_COLS: &str =
    "a.id, a.username, a.display_name, a.avatar_url, a.lang, a.location, a.metadata, a.created_at";

fn to_user(r: UserRow) -> User {
    User {
        id: AccountId(r.0),
        username: r.1,
        display_name: r.2,
        avatar_url: r.3,
        lang: r.4,
        location: r.5,
        metadata: r.6,
        created_at: r.7,
    }
}

impl SocialService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Add friends: sends an invite, or accepts an incoming one. Blocked (by either side) is a no-op.
    /// Returns `(other, accepted)` for every relationship that actually changed.
    pub async fn add(&self, me: AccountId, others: &[AccountId]) -> Result<Vec<(AccountId, bool)>> {
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
        let mut changed = Vec::new();
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
                changed.push((*other, true));
            } else {
                upsert(&mut tx, me, *other, 1).await?;
                upsert(&mut tx, *other, me, 2).await?;
                changed.push((*other, false));
            }
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(changed)
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
            "select a.id, a.username, a.display_name, a.avatar_url, a.lang, a.location, a.metadata, a.created_at, f.state, f.updated_at, f.note, f.favorite
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
                note: r.10,
                favorite: r.11,
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
    /// Set the private note / favourite flag on an accepted friend.
    pub async fn annotate(
        &self,
        me: AccountId,
        friend: AccountId,
        note: Option<Option<String>>,
        favorite: Option<bool>,
    ) -> Result<()> {
        if let Some(Some(n)) = &note {
            if n.chars().count() > 256 {
                return Err(Error::InvalidArgument("note must be <= 256 chars".into()));
            }
        }
        let n = sqlx::query(
            "update musubi.friends set note = case when $3 then $4 else note end,
                    favorite = coalesce($5, favorite)
             where user_id = $1 and friend_id = $2 and state = 0",
        )
        .bind(me.0)
        .bind(friend.0)
        .bind(note.is_some())
        .bind(note.flatten())
        .bind(favorite)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("not a friend".into()));
        }
        Ok(())
    }

    /// Remember that `me` shared `match_id` with `others` (both directions), keeping the newest
    /// [`RECENT_PLAYERS`] per account.
    pub async fn recent_seen(
        &self,
        me: AccountId,
        others: &[AccountId],
        match_id: &str,
    ) -> Result<()> {
        let others: Vec<Uuid> = others.iter().map(|a| a.0).filter(|o| *o != me.0).collect();
        if others.is_empty() {
            return Ok(());
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.recent_players (user_id, other_id, match_id)
             select $1, o, $3 from unnest($2::uuid[]) as o
             union all
             select o, $1, $3 from unnest($2::uuid[]) as o
             on conflict (user_id, other_id) do update set match_id = excluded.match_id, seen_at = now()",
        )
        .bind(me.0)
        .bind(&others)
        .bind(match_id)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        let mut touched = others.clone();
        touched.push(me.0);
        sqlx::query(
            "delete from musubi.recent_players r where r.user_id = any($1) and r.other_id in (
                 select other_id from musubi.recent_players x where x.user_id = r.user_id
                 order by seen_at desc offset $2)",
        )
        .bind(&touched)
        .bind(RECENT_PLAYERS)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    /// Recently played with, newest first; accounts `me` blocked are hidden.
    pub async fn recent(&self, me: AccountId, limit: usize) -> Result<Vec<RecentPlayer>> {
        let rows: Vec<(UserRow, String, OffsetDateTime)> = sqlx::query_as::<_, (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            serde_json::Value,
            OffsetDateTime,
            String,
            OffsetDateTime,
        )>(&format!(
            "select {USER_COLS}, r.match_id, r.seen_at
             from musubi.recent_players r join musubi.accounts a on a.id = r.other_id
             where r.user_id = $1 and a.deleted_at is null
               and not exists (select 1 from musubi.friends b where b.user_id = $1 and b.friend_id = r.other_id and b.state = 3)
             order by r.seen_at desc limit $2"
        ))
        .bind(me.0)
        .bind(limit.clamp(1, RECENT_PLAYERS as usize) as i64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?
        .into_iter()
        .map(|r| ((r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7), r.8, r.9))
        .collect();
        Ok(rows
            .into_iter()
            .map(|(u, match_id, seen_at)| RecentPlayer {
                user: to_user(u),
                match_id,
                seen_at,
            })
            .collect())
    }

    /// Friend suggestions: friends of friends ranked by mutual friends, then recent teammates;
    /// never anyone already related to `me` (any state) or who blocked `me`.
    pub async fn suggestions(&self, me: AccountId, limit: usize) -> Result<Vec<Suggestion>> {
        let limit = limit.clamp(1, 50) as i64;
        let rows = sqlx::query_as::<_, (
            Uuid,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
            serde_json::Value,
            OffsetDateTime,
            i64,
            String,
        )>(&format!(
            "with mutual as (
                 select f2.friend_id as id, count(*) as n
                 from musubi.friends f1 join musubi.friends f2 on f2.user_id = f1.friend_id
                 where f1.user_id = $1 and f1.state = 0 and f2.state = 0 and f2.friend_id <> $1
                 group by f2.friend_id
             ), recent as (
                 select other_id as id from musubi.recent_players where user_id = $1
             ), cand as (
                 select id, n, 'mutual' as reason from mutual
                 union all
                 select id, 0, 'recent' from recent where id not in (select id from mutual)
             )
             select {USER_COLS}, c.n, c.reason
             from cand c join musubi.accounts a on a.id = c.id
             where a.deleted_at is null
               and not exists (select 1 from musubi.friends x where x.user_id = $1 and x.friend_id = c.id)
               and not exists (select 1 from musubi.friends y where y.user_id = c.id and y.friend_id = $1 and y.state = 3)
             order by c.n desc, a.username limit $2"
        ))
        .bind(me.0)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| Suggestion {
                user: to_user((r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7)),
                mutual_friends: r.8,
                reason: r.9,
            })
            .collect())
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
