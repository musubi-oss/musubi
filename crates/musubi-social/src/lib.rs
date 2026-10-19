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
    /// Strongest signal: `platform`, `mutual`, `group` or `recent`.
    pub reason: String,
    /// Every signal that applies, strongest first; platform ones carry the provider
    /// (`platform:steam`).
    pub reasons: Vec<String>,
    /// Ranking score: platform friend 40, each mutual friend 10, each shared group 6, each recent
    /// match 3 (6 if within a week).
    pub score: i64,
    pub shared_groups: i64,
    pub recent_matches: i64,
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
        self.add_via(me, others, false).await
    }

    /// [`add`](Self::add) with the recipient's friend-request privacy applied to new invites
    /// (accepting an incoming invite is always allowed); `via_code` = the sender presented the
    /// recipient's friend code, which bypasses the policy.
    pub async fn add_via(
        &self,
        me: AccountId,
        others: &[AccountId],
        via_code: bool,
    ) -> Result<Vec<(AccountId, bool)>> {
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
                if !via_code {
                    let policy: Option<String> = sqlx::query_scalar(
                        "select requests from musubi.friend_settings where account_id = $1",
                    )
                    .bind(other.0)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                    match policy.as_deref().unwrap_or("anyone") {
                        "anyone" => {}
                        "friends_of_friends" => {
                            let mutual: Option<i64> = sqlx::query_scalar(
                                "select 1::bigint from musubi.friends a join musubi.friends b on a.friend_id = b.friend_id \
                                 where a.user_id = $1 and a.state = 0 and b.user_id = $2 and b.state = 0 limit 1",
                            )
                            .bind(me.0)
                            .bind(other.0)
                            .fetch_optional(&mut *tx)
                            .await
                            .map_err(Error::internal)?;
                            if mutual.is_none() {
                                return Err(Error::PermissionDenied(format!(
                                    "{other} accepts friend requests from friends of friends only"
                                )));
                            }
                        }
                        p => {
                            return Err(Error::PermissionDenied(format!(
                                "{other} does not accept friend requests ({p})"
                            )))
                        }
                    }
                }
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

    /// Friend suggestions: people the player knows from somewhere, ranked. Signals: imported
    /// platform friends who turned out to have an account, friends of friends (count of mutual
    /// friends), fellow members of the player's groups, recent teammates (recency-weighted).
    /// Never anyone already related to `me` (any state), who blocked `me`, or who hides from
    /// search.
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
            i64,
            i64,
            i64,
            Vec<String>,
        )>(&format!(
            "with mutual as (
                 select f2.friend_id as id, count(*) as n
                 from musubi.friends f1 join musubi.friends f2 on f2.user_id = f1.friend_id
                 where f1.user_id = $1 and f1.state = 0 and f2.state = 0 and f2.friend_id <> $1
                 group by f2.friend_id
             ), platform as (
                 select matched_account as id, array_agg(distinct provider order by provider) as providers
                 from musubi.platform_friends
                 where account_id = $1 and matched_account is not null and matched_account <> $1
                 group by matched_account
             ), groups_shared as (
                 select g2.user_id as id, count(*) as n
                 from musubi.group_members g1
                 join musubi.group_members g2 on g2.group_id = g1.group_id
                 where g1.user_id = $1 and g1.state <= 2 and g2.state <= 2 and g2.user_id <> $1
                 group by g2.user_id
             ), recent as (
                 select other_id as id, 1::bigint as n,
                        case when seen_at > now() - interval '7 days' then 6 else 3 end as pts
                 from musubi.recent_players where user_id = $1
             ), ids as (
                 select id from mutual union select id from platform
                 union select id from groups_shared union select id from recent
             ), cand as (
                 select i.id,
                        coalesce(m.n, 0) as mutual,
                        coalesce(g.n, 0) as groups_n,
                        coalesce(r.n, 0) as recent_n,
                        coalesce(p.providers, '{{}}'::text[]) as providers,
                        (case when p.id is not null then 40 else 0 end)
                          + coalesce(m.n, 0) * 10 + coalesce(g.n, 0) * 6 + coalesce(r.pts, 0) as score
                 from ids i
                 left join mutual m on m.id = i.id
                 left join platform p on p.id = i.id
                 left join groups_shared g on g.id = i.id
                 left join recent r on r.id = i.id
             )
             select {USER_COLS}, c.mutual, c.groups_n, c.recent_n, c.score, c.providers
             from cand c join musubi.accounts a on a.id = c.id
             where a.deleted_at is null
               and not exists (select 1 from musubi.friends x where x.user_id = $1 and x.friend_id = c.id)
               and not exists (select 1 from musubi.friends y where y.user_id = c.id and y.friend_id = $1 and y.state = 3)
             order by c.score desc, c.mutual desc, a.username limit $2"
        ))
        .bind(me.0)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let mut reasons = Vec::new();
                for p in &r.12 {
                    reasons.push(format!("platform:{p}"));
                }
                if r.8 > 0 {
                    reasons.push("mutual".into());
                }
                if r.9 > 0 {
                    reasons.push("group".into());
                }
                if r.10 > 0 {
                    reasons.push("recent".into());
                }
                let reason = if !r.12.is_empty() {
                    "platform".to_string()
                } else {
                    reasons.first().cloned().unwrap_or_default()
                };
                Suggestion {
                    user: to_user((r.0, r.1, r.2, r.3, r.4, r.5, r.6, r.7)),
                    mutual_friends: r.8,
                    reason,
                    reasons,
                    score: r.11,
                    shared_groups: r.9,
                    recent_matches: r.10,
                }
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

pub const FRIEND_REQUEST_POLICIES: [&str; 4] =
    ["anyone", "friends_of_friends", "code_only", "nobody"];

/// A player's shareable friend code (`XXXX-XXXX`), optionally limited in uses or time.
#[derive(Debug, Clone, Serialize)]
pub struct FriendCode {
    pub code: String,
    pub created_at: i64,
    pub rotated_at: Option<i64>,
    pub uses: i32,
    pub max_uses: Option<i32>,
    pub expires_at: Option<i64>,
}

type FriendCodeRow = (
    String,
    OffsetDateTime,
    Option<OffsetDateTime>,
    i32,
    Option<i32>,
    Option<OffsetDateTime>,
);

fn friend_code_from(r: FriendCodeRow) -> FriendCode {
    FriendCode {
        code: r.0,
        created_at: r.1.unix_timestamp(),
        rotated_at: r.2.map(|t| t.unix_timestamp()),
        uses: r.3,
        max_uses: r.4,
        expires_at: r.5.map(|t| t.unix_timestamp()),
    }
}

const FRIEND_CODE_COLS: &str = "code, created_at, rotated_at, uses, max_uses, expires_at";

impl SocialService {
    /// The account's current friend code; `fresh` is used when none exists yet.
    pub async fn friend_code(&self, me: AccountId, fresh: &str) -> Result<FriendCode> {
        let row: Option<FriendCodeRow> = sqlx::query_as(&format!(
            "select {FRIEND_CODE_COLS} from musubi.friend_codes where account_id = $1"
        ))
        .bind(me.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        if let Some(r) = row {
            return Ok(friend_code_from(r));
        }
        let row: FriendCodeRow = sqlx::query_as(&format!(
            "insert into musubi.friend_codes (account_id, code) values ($1, $2) \
             on conflict (account_id) do update set account_id = excluded.account_id returning {FRIEND_CODE_COLS}"
        ))
        .bind(me.0)
        .bind(fresh)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(friend_code_from(row))
    }

    /// Replace the code (old one stops working immediately) with optional use/time limits.
    pub async fn rotate_friend_code(
        &self,
        me: AccountId,
        fresh: &str,
        max_uses: Option<i32>,
        expires_in_secs: Option<i64>,
    ) -> Result<FriendCode> {
        if max_uses.is_some_and(|m| !(1..=100_000).contains(&m)) {
            return Err(Error::InvalidArgument("max_uses must be 1..=100000".into()));
        }
        if expires_in_secs.is_some_and(|e| !(1..=365 * 86_400).contains(&e)) {
            return Err(Error::InvalidArgument(
                "expires_in_secs must be 1..=31536000".into(),
            ));
        }
        let row: FriendCodeRow = sqlx::query_as(&format!(
            "insert into musubi.friend_codes (account_id, code, max_uses, expires_at) \
             values ($1, $2, $3, case when $4::bigint is null then null else now() + make_interval(secs => $4::bigint) end) \
             on conflict (account_id) do update set code = excluded.code, rotated_at = now(), uses = 0, \
               max_uses = excluded.max_uses, expires_at = excluded.expires_at returning {FRIEND_CODE_COLS}"
        ))
        .bind(me.0)
        .bind(fresh)
        .bind(max_uses)
        .bind(expires_in_secs)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(friend_code_from(row))
    }

    /// Owner of a still-valid (not expired / exhausted) friend code.
    pub async fn resolve_friend_code(&self, code: &str) -> Result<Option<AccountId>> {
        let r: Option<(Uuid,)> = sqlx::query_as(
            "select account_id from musubi.friend_codes where code = $1 \
             and (expires_at is null or expires_at > now()) and (max_uses is null or uses < max_uses)",
        )
        .bind(code)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(r.map(|(id,)| AccountId(id)))
    }

    pub async fn consume_friend_code(&self, code: &str) -> Result<()> {
        sqlx::query("update musubi.friend_codes set uses = uses + 1 where code = $1")
            .bind(code)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Who may send this account friend requests (`anyone` by default).
    pub async fn friend_request_policy(&self, me: AccountId) -> Result<String> {
        let r: Option<(String,)> =
            sqlx::query_as("select requests from musubi.friend_settings where account_id = $1")
                .bind(me.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        Ok(r.map(|x| x.0).unwrap_or_else(|| "anyone".into()))
    }

    pub async fn set_friend_request_policy(&self, me: AccountId, policy: &str) -> Result<()> {
        if !FRIEND_REQUEST_POLICIES.contains(&policy) {
            return Err(Error::InvalidArgument(format!(
                "requests must be one of {}",
                FRIEND_REQUEST_POLICIES.join("|")
            )));
        }
        sqlx::query(
            "insert into musubi.friend_settings (account_id, requests) values ($1, $2) \
             on conflict (account_id) do update set requests = excluded.requests, updated_at = now()",
        )
        .bind(me.0)
        .bind(policy)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }
}
