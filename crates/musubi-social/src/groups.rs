//! Groups / clans: creation, membership states, moderation and listing.

use musubi_account::User;
use musubi_core::{AccountId, Error, GroupId, Result};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Member role, Nakama-compatible numbering. Lower = more power.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "i16", try_from = "i16")]
#[repr(i16)]
pub enum MemberState {
    Superadmin = 0,
    Admin = 1,
    Member = 2,
    JoinRequest = 3,
}

impl From<MemberState> for i16 {
    fn from(s: MemberState) -> i16 {
        s as i16
    }
}

impl TryFrom<i16> for MemberState {
    type Error = String;
    fn try_from(v: i16) -> std::result::Result<Self, String> {
        Ok(match v {
            0 => Self::Superadmin,
            1 => Self::Admin,
            2 => Self::Member,
            3 => Self::JoinRequest,
            _ => return Err(format!("bad member state {v}")),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub id: GroupId,
    pub creator_id: Option<AccountId>,
    pub name: String,
    pub description: String,
    pub avatar_url: Option<String>,
    pub lang: String,
    pub metadata: serde_json::Value,
    pub open: bool,
    pub edge_count: i32,
    pub max_count: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub create_time: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,
    /// Pinned announcement from an admin, if any.
    pub announcement: Option<String>,
    pub announcement_at: Option<i64>,
    pub xp: i64,
    pub level: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AllianceMember {
    pub group_id: GroupId,
    pub name: String,
    /// 0 member, 1 invited
    pub state: i16,
}

#[derive(Debug, Clone, Serialize)]
pub struct Alliance {
    pub id: Uuid,
    pub name: String,
    pub leader_group: GroupId,
    pub members: Vec<AllianceMember>,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupBan {
    pub user_id: AccountId,
    pub username: String,
    pub banned_by: Option<AccountId>,
    pub reason: String,
    /// Unix seconds; `None` = permanent.
    pub until: Option<i64>,
    pub created_at: i64,
}

/// One line of a group's journal (`joined`, `join_requested`, `kicked`, `banned`, `unbanned`,
/// `announcement`, …).
#[derive(Debug, Clone, Serialize)]
pub struct GroupEvent {
    pub id: String,
    pub kind: String,
    pub actor_id: Option<AccountId>,
    pub subject_id: Option<AccountId>,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateGroup {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub lang: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default = "default_open")]
    pub open: bool,
    #[serde(default)]
    pub max_count: Option<i32>,
}

fn default_open() -> bool {
    true
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateGroup {
    pub name: Option<String>,
    pub description: Option<String>,
    pub avatar_url: Option<String>,
    pub lang: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub open: Option<bool>,
    pub max_count: Option<i32>,
}

/// Permission bits a group role may hold (plan 5.4). The owner (rank 0) holds all of them.
pub mod perm {
    pub const INVITE: i64 = 1;
    pub const KICK: i64 = 2;
    pub const BAN: i64 = 4;
    pub const ANNOUNCE: i64 = 8;
    pub const EDIT: i64 = 16;
    pub const PROMOTE: i64 = 32;
    pub const TREASURY_DEPOSIT: i64 = 64;
    pub const TREASURY_WITHDRAW: i64 = 128;
    pub const ALL: i64 = 255;
    /// Defaults when a group has not customised the role: admins do everything, members may pay in.
    pub fn default_for(rank: i16) -> i64 {
        match rank {
            0 | 1 => ALL,
            2 => TREASURY_DEPOSIT,
            _ => 0,
        }
    }
    pub fn default_name(rank: i16) -> &'static str {
        match rank {
            0 => "owner",
            1 => "admin",
            2 => "member",
            _ => "custom",
        }
    }
}

/// A rank in the group's hierarchy: lower rank = more senior. Ranks 0..=2 double as the legacy
/// superadmin/admin/member states; 3..=7 are extra tiers a group defines itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupRole {
    pub rank: i16,
    pub name: String,
    pub permissions: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupMember {
    pub user: User,
    pub state: MemberState,
    pub rank: i16,
    #[serde(with = "time::serde::rfc3339")]
    pub update_time: OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserGroup {
    pub group: Group,
    pub state: MemberState,
}

#[derive(Debug, Clone, Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupFilter<'a> {
    pub name_prefix: Option<&'a str>,
    pub lang: Option<&'a str>,
    pub open: Option<bool>,
    pub members_min: Option<i32>,
    pub members_max: Option<i32>,
}

pub const MAX_GROUPS_PER_USER: i64 = 100;

#[derive(Clone)]
pub struct GroupService {
    db: PgPool,
    level_xp: Vec<i64>,
}

type GroupRow = (
    Uuid,
    Option<Uuid>,
    String,
    String,
    Option<String>,
    String,
    serde_json::Value,
    bool,
    i32,
    i32,
    OffsetDateTime,
    OffsetDateTime,
    Option<String>,
    Option<OffsetDateTime>,
    i64,
    i32,
);

type BanRow = (
    Uuid,
    String,
    Option<Uuid>,
    String,
    Option<OffsetDateTime>,
    OffsetDateTime,
);
type EventRow = (
    Uuid,
    String,
    Option<Uuid>,
    Option<Uuid>,
    serde_json::Value,
    OffsetDateTime,
);

const GROUP_COLS: &str = "g.id, g.creator_id, g.name, g.description, g.avatar_url, g.lang, g.metadata, g.open, g.edge_count, g.max_count, g.created_at, g.updated_at, g.announcement, g.announcement_at, g.xp, g.level";

fn to_group(r: GroupRow) -> Group {
    Group {
        id: GroupId(r.0),
        creator_id: r.1.map(AccountId),
        name: r.2,
        description: r.3,
        avatar_url: r.4,
        lang: r.5,
        metadata: r.6,
        open: r.7,
        edge_count: r.8,
        max_count: r.9,
        create_time: r.10,
        update_time: r.11,
        announcement: r.12,
        announcement_at: r.13.map(|t| t.unix_timestamp()),
        xp: r.14,
        level: r.15,
    }
}

fn validate_name(name: &str) -> Result<()> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 64 || n != name {
        return Err(Error::InvalidArgument(
            "group name must be 1..=64 chars without surrounding whitespace".into(),
        ));
    }
    Ok(())
}

fn validate_metadata(m: &Option<serde_json::Value>) -> Result<()> {
    if let Some(m) = m {
        if !m.is_object() || serde_json::to_vec(m)?.len() > 16 * 1024 {
            return Err(Error::InvalidArgument(
                "metadata must be an object ≤ 16 KB".into(),
            ));
        }
    }
    Ok(())
}

impl GroupService {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            level_xp: vec![
                1_000, 3_000, 6_000, 10_000, 15_000, 21_000, 28_000, 36_000, 45_000,
            ],
        }
    }

    pub async fn create(&self, me: AccountId, c: CreateGroup) -> Result<Group> {
        validate_name(&c.name)?;
        validate_metadata(&c.metadata)?;
        if c.description.chars().count() > 1024 {
            return Err(Error::InvalidArgument("description ≤ 1024 chars".into()));
        }
        let max = c.max_count.unwrap_or(100);
        if !(1..=10000).contains(&max) {
            return Err(Error::InvalidArgument("max_count must be 1..=10000".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let owned: i64 = sqlx::query_scalar(
            "select count(*) from musubi.group_members where user_id = $1 and state < 3",
        )
        .bind(me.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if owned >= MAX_GROUPS_PER_USER {
            return Err(Error::ResourceExhausted(format!(
                "user is in {MAX_GROUPS_PER_USER} groups already"
            )));
        }
        let id = GroupId::new();
        let res = sqlx::query_as::<_, GroupRow>(&format!(
            "insert into musubi.groups as g (id, creator_id, name, description, avatar_url, lang, metadata, open, edge_count, max_count)
             values ($1, $2, $3, $4, $5, $6, coalesce($7, '{{}}'::jsonb), $8, 1, $9) returning {GROUP_COLS}"
        ))
        .bind(id.0)
        .bind(me.0)
        .bind(&c.name)
        .bind(&c.description)
        .bind(&c.avatar_url)
        .bind(c.lang.as_deref().unwrap_or("en"))
        .bind(&c.metadata)
        .bind(c.open)
        .bind(max)
        .fetch_one(&mut *tx)
        .await;
        let row = match res {
            Ok(r) => r,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(Error::AlreadyExists("group name taken".into()))
            }
            Err(e) => return Err(Error::internal(e)),
        };
        sqlx::query(
            "insert into musubi.group_members (group_id, user_id, state, rank) values ($1, $2, 0, 0)",
        )
        .bind(id.0)
        .bind(me.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(to_group(row))
    }

    pub async fn get(&self, ids: &[GroupId]) -> Result<Vec<Group>> {
        if ids.len() > 100 {
            return Err(Error::InvalidArgument("≤100 ids".into()));
        }
        let ids: Vec<Uuid> = ids.iter().map(|i| i.0).collect();
        let rows = sqlx::query_as::<_, GroupRow>(&format!(
            "select {GROUP_COLS} from musubi.groups g where g.id = any($1)"
        ))
        .bind(&ids)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(to_group).collect())
    }

    pub async fn update(&self, me: AccountId, group: GroupId, u: UpdateGroup) -> Result<Group> {
        if let Some(n) = &u.name {
            validate_name(n)?;
        }
        validate_metadata(&u.metadata)?;
        if let Some(m) = u.max_count {
            if !(1..=10000).contains(&m) {
                return Err(Error::InvalidArgument("max_count must be 1..=10000".into()));
            }
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_perm(&mut tx, group, me, perm::EDIT).await?;
        let res = sqlx::query_as::<_, GroupRow>(&format!(
            "update musubi.groups g set name = coalesce($2, name), description = coalesce($3, description),
                avatar_url = coalesce($4, avatar_url), lang = coalesce($5, lang), metadata = coalesce($6, metadata),
                open = coalesce($7, open), max_count = greatest(coalesce($8, max_count), edge_count), updated_at = now()
             where id = $1 returning {GROUP_COLS}"
        ))
        .bind(group.0)
        .bind(&u.name)
        .bind(&u.description)
        .bind(&u.avatar_url)
        .bind(&u.lang)
        .bind(&u.metadata)
        .bind(u.open)
        .bind(u.max_count)
        .fetch_one(&mut *tx)
        .await;
        let row = match res {
            Ok(r) => r,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                return Err(Error::AlreadyExists("group name taken".into()))
            }
            Err(e) => return Err(Error::internal(e)),
        };
        tx.commit().await.map_err(Error::internal)?;
        Ok(to_group(row))
    }

    pub async fn delete(&self, me: AccountId, group: GroupId) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_role(&mut tx, group, me, MemberState::Superadmin).await?;
        sqlx::query("delete from musubi.groups where id = $1")
            .bind(group.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)
    }

    /// Open group → member immediately; closed → join request (admins see it in state 3).
    pub async fn join(&self, me: AccountId, group: GroupId) -> Result<MemberState> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let g = lock_group(&mut tx, group).await?;
        let existing: Option<i16> = sqlx::query_scalar(
            "select state from musubi.group_members where group_id = $1 and user_id = $2",
        )
        .bind(group.0)
        .bind(me.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if let Some(s) = existing {
            return MemberState::try_from(s).map_err(Error::Internal);
        }
        if let Some(until) = sqlx::query_scalar::<_, Option<OffsetDateTime>>(
            "select until from musubi.group_bans where group_id = $1 and user_id = $2",
        )
        .bind(group.0)
        .bind(me.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(Error::internal)?
        {
            match until {
                Some(t) if t <= OffsetDateTime::now_utc() => {
                    sqlx::query(
                        "delete from musubi.group_bans where group_id = $1 and user_id = $2",
                    )
                    .bind(group.0)
                    .bind(me.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                }
                _ => return Err(Error::PermissionDenied("banned from this group".into())),
            }
        }
        let count: i64 = sqlx::query_scalar(
            "select count(*) from musubi.group_members where user_id = $1 and state < 3",
        )
        .bind(me.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if count >= MAX_GROUPS_PER_USER {
            return Err(Error::ResourceExhausted(format!(
                "user is in {MAX_GROUPS_PER_USER} groups already"
            )));
        }
        let state = if g.open {
            if g.edge_count >= g.max_count {
                return Err(Error::ResourceExhausted("group is full".into()));
            }
            MemberState::Member
        } else {
            MemberState::JoinRequest
        };
        insert_member(&mut tx, group, me, state).await?;
        let kind = if state == MemberState::Member {
            "joined"
        } else {
            "join_requested"
        };
        log_event(
            &mut tx,
            group,
            kind,
            Some(me),
            Some(me),
            serde_json::json!({}),
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(state)
    }

    /// Leave (or withdraw a join request). The last superadmin may only leave if nobody else is left,
    /// in which case the group is deleted.
    pub async fn leave(&self, me: AccountId, group: GroupId) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        let mine = member_state(&mut tx, group, me)
            .await?
            .ok_or_else(|| Error::NotFound("not a member".into()))?;
        if mine == MemberState::Superadmin {
            let admins: i64 = sqlx::query_scalar(
                "select count(*) from musubi.group_members where group_id = $1 and state = 0",
            )
            .bind(group.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let members: i64 = sqlx::query_scalar(
                "select count(*) from musubi.group_members where group_id = $1 and state < 3",
            )
            .bind(group.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            if admins == 1 && members > 1 {
                return Err(Error::Conflict(
                    "promote another superadmin before leaving".into(),
                ));
            }
            if members == 1 {
                sqlx::query("delete from musubi.groups where id = $1")
                    .bind(group.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                return tx.commit().await.map_err(Error::internal);
            }
        }
        remove_member(&mut tx, group, me, mine).await?;
        tx.commit().await.map_err(Error::internal)
    }

    /// Admin+: accept join requests or add users directly as members.
    pub async fn add(&self, me: AccountId, group: GroupId, users: &[AccountId]) -> Result<()> {
        check_users(users)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let g = lock_group(&mut tx, group).await?;
        require_perm(&mut tx, group, me, perm::INVITE).await?;
        let mut edge = g.edge_count;
        for u in users {
            match member_state(&mut tx, group, *u).await? {
                Some(MemberState::JoinRequest) => {
                    if edge >= g.max_count {
                        return Err(Error::ResourceExhausted("group is full".into()));
                    }
                    set_state(&mut tx, group, *u, MemberState::Member).await?;
                    edge += 1;
                }
                Some(_) => {}
                None => {
                    if edge >= g.max_count {
                        return Err(Error::ResourceExhausted("group is full".into()));
                    }
                    let exists: Option<i64> = sqlx::query_scalar("select 1::bigint from musubi.accounts where id = $1 and deleted_at is null")
                        .bind(u.0)
                        .fetch_optional(&mut *tx)
                        .await
                        .map_err(Error::internal)?;
                    if exists.is_none() {
                        return Err(Error::NotFound(format!("user {u}")));
                    }
                    insert_member(&mut tx, group, *u, MemberState::Member).await?;
                    edge += 1;
                }
            }
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Admin+: remove members/requests ranked strictly below the caller.
    pub async fn kick(&self, me: AccountId, group: GroupId, users: &[AccountId]) -> Result<()> {
        check_users(users)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        require_perm(&mut tx, group, me, perm::KICK).await?;
        for u in users {
            let Some(theirs) = member_state(&mut tx, group, *u).await? else {
                continue;
            };
            if *u == me || !outranks(&mut tx, group, me, *u).await? {
                return Err(Error::PermissionDenied(format!("cannot kick {u}")));
            }
            remove_member(&mut tx, group, *u, theirs).await?;
            log_event(
                &mut tx,
                group,
                "kicked",
                Some(me),
                Some(*u),
                serde_json::json!({}),
            )
            .await?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Admin+: ban users (kicking them if they are in); a banned account cannot join or request to
    /// join until `until` passes (`None` = permanent). Nobody of equal or higher rank can be banned.
    pub async fn ban(
        &self,
        me: AccountId,
        group: GroupId,
        users: &[AccountId],
        reason: &str,
        until: Option<OffsetDateTime>,
    ) -> Result<()> {
        check_users(users)?;
        if reason.chars().count() > 512 {
            return Err(Error::InvalidArgument("reason must be <= 512 chars".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        require_perm(&mut tx, group, me, perm::BAN).await?;
        for u in users {
            if *u == me {
                return Err(Error::PermissionDenied("cannot ban yourself".into()));
            }
            if let Some(theirs) = member_state(&mut tx, group, *u).await? {
                if !outranks(&mut tx, group, me, *u).await? {
                    return Err(Error::PermissionDenied(format!("cannot ban {u}")));
                }
                remove_member(&mut tx, group, *u, theirs).await?;
            }
            sqlx::query(
                "insert into musubi.group_bans (group_id, user_id, banned_by, reason, until) values ($1, $2, $3, $4, $5)
                 on conflict (group_id, user_id) do update set banned_by = $3, reason = $4, until = $5, created_at = now()",
            )
            .bind(group.0)
            .bind(u.0)
            .bind(me.0)
            .bind(reason)
            .bind(until)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            log_event(
                &mut tx,
                group,
                "banned",
                Some(me),
                Some(*u),
                serde_json::json!({ "reason": reason, "until": until.map(|t| t.unix_timestamp()) }),
            )
            .await?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Admin+: lift bans.
    pub async fn unban(&self, me: AccountId, group: GroupId, users: &[AccountId]) -> Result<()> {
        check_users(users)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        require_perm(&mut tx, group, me, perm::BAN).await?;
        for u in users {
            let n =
                sqlx::query("delete from musubi.group_bans where group_id = $1 and user_id = $2")
                    .bind(group.0)
                    .bind(u.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?
                    .rows_affected();
            if n > 0 {
                log_event(
                    &mut tx,
                    group,
                    "unbanned",
                    Some(me),
                    Some(*u),
                    serde_json::json!({}),
                )
                .await?;
            }
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Admin+: current bans, newest first.
    pub async fn bans(&self, me: AccountId, group: GroupId, limit: usize) -> Result<Vec<GroupBan>> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_perm(&mut tx, group, me, perm::BAN).await?;
        let rows: Vec<BanRow> = sqlx::query_as(
            "select b.user_id, a.username, b.banned_by, b.reason, b.until, b.created_at
                 from musubi.group_bans b join musubi.accounts a on a.id = b.user_id
                 where b.group_id = $1 and (b.until is null or b.until > now())
                 order by b.created_at desc limit $2",
        )
        .bind(group.0)
        .bind(limit.clamp(1, 100) as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| GroupBan {
                user_id: AccountId(r.0),
                username: r.1,
                banned_by: r.2.map(AccountId),
                reason: r.3,
                until: r.4.map(|t| t.unix_timestamp()),
                created_at: r.5.unix_timestamp(),
            })
            .collect())
    }

    /// Admin+: pin (or clear with `None`) the group announcement. Returns the members to notify.
    pub async fn announce(
        &self,
        me: AccountId,
        group: GroupId,
        text: Option<&str>,
    ) -> Result<Vec<AccountId>> {
        if text.is_some_and(|t| t.chars().count() > 2048) {
            return Err(Error::InvalidArgument(
                "announcement must be <= 2048 chars".into(),
            ));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        require_perm(&mut tx, group, me, perm::ANNOUNCE).await?;
        sqlx::query(
            "update musubi.groups set announcement = $2, announcement_at = case when $2 is null then null else now() end, updated_at = now() where id = $1",
        )
        .bind(group.0)
        .bind(text)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        log_event(
            &mut tx,
            group,
            "announcement",
            Some(me),
            None,
            serde_json::json!({ "text": text }),
        )
        .await?;
        let members: Vec<Uuid> = sqlx::query_scalar(
            "select user_id from musubi.group_members where group_id = $1 and state < 3 and user_id <> $2",
        )
        .bind(group.0)
        .bind(me.0)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(members.into_iter().map(AccountId).collect())
    }

    /// Members only: the group's journal, newest first; `cursor` = id of the last event seen.
    pub async fn events(
        &self,
        me: AccountId,
        group: GroupId,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Vec<GroupEvent>> {
        let cursor: Option<Uuid> = cursor
            .map(|c| {
                c.parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))
            })
            .transpose()?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        match member_state(&mut tx, group, me).await? {
            Some(st) if st != MemberState::JoinRequest => {}
            _ => return Err(Error::PermissionDenied("members only".into())),
        }
        let rows: Vec<EventRow> = sqlx::query_as(
            "select id, kind, actor_id, subject_id, payload, created_at from musubi.group_events
                 where group_id = $1 and ($2::uuid is null or id < $2) order by id desc limit $3",
        )
        .bind(group.0)
        .bind(cursor)
        .bind(limit.clamp(1, 100) as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|r| GroupEvent {
                id: r.0.to_string(),
                kind: r.1,
                actor_id: r.2.map(AccountId),
                subject_id: r.3.map(AccountId),
                payload: r.4,
                created_at: r.5.unix_timestamp(),
            })
            .collect())
    }

    /// Admin+: raise users one rank (member → admin, admin → superadmin), never above the caller.
    /// XP thresholds for levels 2, 3, … (`[social] group_level_xp`).
    pub fn with_level_xp(mut self, level_xp: Vec<i64>) -> Self {
        self.level_xp = level_xp;
        self
    }

    fn level_for(&self, xp: i64) -> i32 {
        1 + self.level_xp.iter().filter(|t| xp >= **t).count() as i32
    }

    /// Server-side XP grant for a group (quests, wars, events); returns the group with its level
    /// recomputed from the thresholds and whether it went up.
    pub async fn grant_xp(
        &self,
        group: GroupId,
        delta: i64,
        reason: &str,
    ) -> Result<(Group, bool)> {
        if delta <= 0 {
            return Err(Error::InvalidArgument("delta must be positive".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let before: Option<(i64, i32)> =
            sqlx::query_as("select xp, level from musubi.groups where id = $1 for update")
                .bind(group.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let Some((xp, level)) = before else {
            return Err(Error::NotFound("group".into()));
        };
        let xp = xp.saturating_add(delta);
        let new_level = self.level_for(xp);
        let row: GroupRow = sqlx::query_as(&format!(
            "update musubi.groups g set xp = $2, level = $3, updated_at = now() where g.id = $1 returning {GROUP_COLS}"
        ))
        .bind(group.0)
        .bind(xp)
        .bind(new_level)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if new_level > level {
            log_event(
                &mut tx,
                group,
                "level_up",
                None,
                None,
                serde_json::json!({ "level": new_level, "xp": xp, "reason": reason }),
            )
            .await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok((to_group(row), new_level > level))
    }

    /// Owner of `group` founds an alliance; the group becomes its leader and first member.
    pub async fn create_alliance(
        &self,
        me: AccountId,
        group: GroupId,
        name: &str,
    ) -> Result<Alliance> {
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidArgument(
                "alliance name must be 1..=64 chars".into(),
            ));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_role(&mut tx, group, me, MemberState::Superadmin).await?;
        let id = Uuid::now_v7();
        sqlx::query("insert into musubi.alliances (id, name, leader_group) values ($1, $2, $3)")
            .bind(id)
            .bind(name)
            .bind(group.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| match e {
                sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                    Error::AlreadyExists("alliance name taken".into())
                }
                e => Error::internal(e),
            })?;
        sqlx::query(
            "insert into musubi.alliance_members (alliance_id, group_id, state) values ($1, $2, 0)",
        )
        .bind(id)
        .bind(group.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                Error::AlreadyExists("group is already in an alliance".into())
            }
            e => Error::internal(e),
        })?;
        log_event(
            &mut tx,
            group,
            "alliance_founded",
            Some(me),
            None,
            serde_json::json!({ "alliance_id": id, "name": name }),
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        self.alliance(id).await
    }

    /// Owner of the leader group invites another group (its owner accepts).
    pub async fn invite_to_alliance(
        &self,
        me: AccountId,
        alliance: Uuid,
        group: GroupId,
    ) -> Result<Alliance> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let leader = alliance_leader(&mut tx, alliance).await?;
        require_role(&mut tx, leader, me, MemberState::Superadmin).await?;
        sqlx::query(
            "insert into musubi.alliance_members (alliance_id, group_id, state) values ($1, $2, 1)",
        )
        .bind(alliance)
        .bind(group.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                Error::AlreadyExists("group is already in or invited to an alliance".into())
            }
            sqlx::Error::Database(ref d) if d.is_foreign_key_violation() => {
                Error::NotFound("group".into())
            }
            e => Error::internal(e),
        })?;
        log_event(
            &mut tx,
            group,
            "alliance_invited",
            Some(me),
            None,
            serde_json::json!({ "alliance_id": alliance }),
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        self.alliance(alliance).await
    }

    /// Owner of an invited group accepts the invitation.
    pub async fn accept_alliance(
        &self,
        me: AccountId,
        alliance: Uuid,
        group: GroupId,
    ) -> Result<Alliance> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_role(&mut tx, group, me, MemberState::Superadmin).await?;
        let n = sqlx::query(
            "update musubi.alliance_members set state = 0, joined_at = now() where alliance_id = $1 and group_id = $2 and state = 1",
        )
        .bind(alliance)
        .bind(group.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("invitation".into()));
        }
        log_event(
            &mut tx,
            group,
            "alliance_joined",
            Some(me),
            None,
            serde_json::json!({ "alliance_id": alliance }),
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        self.alliance(alliance).await
    }

    /// Owner takes the group out (declining an invitation works the same way). When the leader
    /// group leaves, the alliance is dissolved.
    pub async fn leave_alliance(
        &self,
        me: AccountId,
        alliance: Uuid,
        group: GroupId,
    ) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_role(&mut tx, group, me, MemberState::Superadmin).await?;
        let leader = alliance_leader(&mut tx, alliance).await?;
        if leader == group {
            sqlx::query("delete from musubi.alliances where id = $1")
                .bind(alliance)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            log_event(
                &mut tx,
                group,
                "alliance_dissolved",
                Some(me),
                None,
                serde_json::json!({ "alliance_id": alliance }),
            )
            .await?;
        } else {
            let n = sqlx::query(
                "delete from musubi.alliance_members where alliance_id = $1 and group_id = $2",
            )
            .bind(alliance)
            .bind(group.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?
            .rows_affected();
            if n == 0 {
                return Err(Error::NotFound("membership".into()));
            }
            log_event(
                &mut tx,
                group,
                "alliance_left",
                Some(me),
                None,
                serde_json::json!({ "alliance_id": alliance }),
            )
            .await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    pub async fn alliance(&self, id: Uuid) -> Result<Alliance> {
        let head: Option<(String, Uuid, OffsetDateTime)> = sqlx::query_as(
            "select name, leader_group, created_at from musubi.alliances where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some((name, leader, created_at)) = head else {
            return Err(Error::NotFound("alliance".into()));
        };
        let members: Vec<(Uuid, String, i16)> = sqlx::query_as(
            "select m.group_id, g.name, m.state from musubi.alliance_members m join musubi.groups g on g.id = m.group_id
             where m.alliance_id = $1 order by m.state, m.joined_at",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(Alliance {
            id,
            name,
            leader_group: GroupId(leader),
            members: members
                .into_iter()
                .map(|(g, name, state)| AllianceMember {
                    group_id: GroupId(g),
                    name,
                    state,
                })
                .collect(),
            created_at: created_at.unix_timestamp(),
        })
    }

    /// The alliance a group belongs to or is invited to, if any.
    pub async fn group_alliance(&self, group: GroupId) -> Result<Option<Alliance>> {
        let id: Option<Uuid> = sqlx::query_scalar(
            "select alliance_id from musubi.alliance_members where group_id = $1",
        )
        .bind(group.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        match id {
            Some(id) => Ok(Some(self.alliance(id).await?)),
            None => Ok(None),
        }
    }

    /// Does `me`'s role in `group` hold `bit`? (owner always does) — for callers outside the
    /// group service, e.g. the treasury.
    pub async fn check_perm(&self, me: AccountId, group: GroupId, bit: i64) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_perm(&mut tx, group, me, bit).await?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    /// The group's role table: every rank 0..=7 with its name and permission bits (defaults for
    /// ranks the group has not customised).
    pub async fn roles(&self, group: GroupId) -> Result<Vec<GroupRole>> {
        let rows: Vec<(i16, String, i64)> = sqlx::query_as(
            "select rank, name, permissions from musubi.group_roles where group_id = $1",
        )
        .bind(group.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok((0..=7i16)
            .map(|rank| {
                rows.iter()
                    .find(|r| r.0 == rank)
                    .map(|r| GroupRole {
                        rank,
                        name: r.1.clone(),
                        permissions: r.2,
                    })
                    .unwrap_or(GroupRole {
                        rank,
                        name: perm::default_name(rank).into(),
                        permissions: perm::default_for(rank),
                    })
            })
            .collect())
    }

    /// Owner only: define names and permissions for ranks 1..=7 (rank 0 always holds everything).
    pub async fn set_roles(
        &self,
        me: AccountId,
        group: GroupId,
        roles: &[GroupRole],
    ) -> Result<Vec<GroupRole>> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_role(&mut tx, group, me, MemberState::Superadmin).await?;
        for r in roles {
            if !(1..=7).contains(&r.rank) {
                return Err(Error::InvalidArgument("rank must be 1..=7".into()));
            }
            if r.name.is_empty() || r.name.len() > 32 {
                return Err(Error::InvalidArgument(
                    "role name must be 1..=32 chars".into(),
                ));
            }
            if r.permissions & !perm::ALL != 0 {
                return Err(Error::InvalidArgument("unknown permission bits".into()));
            }
            sqlx::query(
                "insert into musubi.group_roles (group_id, rank, name, permissions) values ($1, $2, $3, $4)
                 on conflict (group_id, rank) do update set name = $3, permissions = $4",
            )
            .bind(group.0)
            .bind(r.rank)
            .bind(&r.name)
            .bind(r.permissions)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        }
        log_event(
            &mut tx,
            group,
            "roles_updated",
            Some(me),
            None,
            serde_json::json!({ "ranks": roles.iter().map(|r| r.rank).collect::<Vec<_>>() }),
        )
        .await?;
        tx.commit().await.map_err(Error::internal)?;
        self.roles(group).await
    }

    /// Move members to `rank` (1..=7). Needs PROMOTE; the caller can only hand out ranks junior
    /// to their own and only touch members junior to them. Ranks 1..=2 also set the legacy state.
    pub async fn set_rank(
        &self,
        me: AccountId,
        group: GroupId,
        users: &[AccountId],
        rank: i16,
    ) -> Result<()> {
        if !(1..=7).contains(&rank) {
            return Err(Error::InvalidArgument("rank must be 1..=7".into()));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        require_perm(&mut tx, group, me, perm::PROMOTE).await?;
        let my_rank: i16 = sqlx::query_scalar(
            "select rank from musubi.group_members where group_id = $1 and user_id = $2",
        )
        .bind(group.0)
        .bind(me.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if rank <= my_rank {
            return Err(Error::PermissionDenied(
                "cannot grant a rank at or above your own".into(),
            ));
        }
        for u in users {
            let theirs: Option<(i16, i16)> = sqlx::query_as(
                "select state, rank from musubi.group_members where group_id = $1 and user_id = $2 for update",
            )
            .bind(group.0)
            .bind(u.0)
            .fetch_optional(&mut *tx)
            .await
            .map_err(Error::internal)?;
            let Some((state, their_rank)) = theirs else {
                return Err(Error::NotFound("member".into()));
            };
            if state == i16::from(MemberState::JoinRequest) {
                return Err(Error::InvalidArgument("member has not joined".into()));
            }
            if their_rank <= my_rank {
                return Err(Error::PermissionDenied(
                    "cannot change a senior member".into(),
                ));
            }
            sqlx::query(
                "update musubi.group_members set rank = $3, state = least($3, 2), updated_at = now() where group_id = $1 and user_id = $2",
            )
            .bind(group.0)
            .bind(u.0)
            .bind(rank)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            log_event(
                &mut tx,
                group,
                "rank_changed",
                Some(me),
                Some(*u),
                serde_json::json!({ "rank": rank, "from": their_rank }),
            )
            .await?;
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(())
    }

    pub async fn promote(&self, me: AccountId, group: GroupId, users: &[AccountId]) -> Result<()> {
        check_users(users)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        let mine = require_perm(&mut tx, group, me, perm::PROMOTE).await?;
        for u in users {
            let theirs = member_state(&mut tx, group, *u)
                .await?
                .ok_or_else(|| Error::NotFound(format!("user {u} is not a member")))?;
            let next = match theirs {
                MemberState::Member => MemberState::Admin,
                MemberState::Admin => MemberState::Superadmin,
                MemberState::Superadmin => continue,
                MemberState::JoinRequest => {
                    return Err(Error::InvalidArgument(
                        "accept the join request first".into(),
                    ))
                }
            };
            if next < mine {
                return Err(Error::PermissionDenied(
                    "cannot promote above your own rank".into(),
                ));
            }
            set_state(&mut tx, group, *u, next).await?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Admin+: lower users one rank (superadmin → admin, admin → member); only users strictly below the caller,
    /// except a superadmin may demote another superadmin when at least one superadmin remains.
    pub async fn demote(&self, me: AccountId, group: GroupId, users: &[AccountId]) -> Result<()> {
        check_users(users)?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        lock_group(&mut tx, group).await?;
        let mine = require_perm(&mut tx, group, me, perm::PROMOTE).await?;
        for u in users {
            let theirs = member_state(&mut tx, group, *u)
                .await?
                .ok_or_else(|| Error::NotFound(format!("user {u} is not a member")))?;
            let allowed = theirs > mine
                || (theirs == MemberState::Superadmin
                    && mine == MemberState::Superadmin
                    && *u != me);
            if !allowed {
                return Err(Error::PermissionDenied(format!("cannot demote {u}")));
            }
            let next = match theirs {
                MemberState::Superadmin => MemberState::Admin,
                MemberState::Admin => MemberState::Member,
                _ => continue,
            };
            set_state(&mut tx, group, *u, next).await?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    pub async fn list(
        &self,
        f: GroupFilter<'_>,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Page<Group>> {
        let limit = limit.clamp(1, 100) as i64;
        let after: Option<Uuid> = match cursor {
            Some(c) => Some(
                c.parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?,
            ),
            None => None,
        };
        let prefix = f.name_prefix.map(|p| {
            format!(
                "{}%",
                p.to_lowercase().replace('%', "\\%").replace('_', "\\_")
            )
        });
        let rows = sqlx::query_as::<_, GroupRow>(&format!(
            "select {GROUP_COLS} from musubi.groups g
             where ($1::text is null or lower(g.name) like $1)
               and ($2::text is null or g.lang = $2)
               and ($3::boolean is null or g.open = $3)
               and ($4::int is null or g.edge_count >= $4)
               and ($5::int is null or g.edge_count <= $5)
               and ($6::uuid is null or g.id > $6)
             order by g.id limit $7"
        ))
        .bind(prefix)
        .bind(f.lang)
        .bind(f.open)
        .bind(f.members_min)
        .bind(f.members_max)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let items: Vec<Group> = rows
            .into_iter()
            .take(limit as usize)
            .map(to_group)
            .collect();
        let cursor = more
            .then(|| items.last().map(|g| g.id.0.to_string()))
            .flatten();
        Ok(Page { items, cursor })
    }

    /// Members of a group. Join requests (state 3) are only visible to admins.
    pub async fn members(
        &self,
        me: AccountId,
        group: GroupId,
        state: Option<MemberState>,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Page<GroupMember>> {
        let limit = limit.clamp(1, 100) as i64;
        let mine = sqlx::query_scalar::<_, i16>(
            "select state from musubi.group_members where group_id = $1 and user_id = $2",
        )
        .bind(group.0)
        .bind(me.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let is_admin = mine.is_some_and(|s| s <= 1);
        let after: Option<Uuid> = match cursor {
            Some(c) => Some(
                c.parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?,
            ),
            None => None,
        };
        let rows = sqlx::query_as::<_, MemberRow>(
            "select a.id, a.username, a.display_name, a.avatar_url, a.lang, a.location, a.metadata, a.created_at, m.state, m.updated_at, m.rank
             from musubi.group_members m join musubi.accounts a on a.id = m.user_id
             where m.group_id = $1 and a.deleted_at is null
               and ($2::smallint is null or m.state = $2)
               and ($3::boolean or m.state < 3)
               and ($4::uuid is null or m.user_id > $4)
             order by m.user_id limit $5",
        )
        .bind(group.0)
        .bind(state.map(i16::from))
        .bind(is_admin)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let items: Vec<GroupMember> = rows
            .into_iter()
            .take(limit as usize)
            .map(|r| GroupMember {
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
                state: MemberState::try_from(r.8).unwrap_or(MemberState::JoinRequest),
                rank: r.10,
                update_time: r.9,
            })
            .collect();
        let cursor = more
            .then(|| items.last().map(|m| m.user.id.0.to_string()))
            .flatten();
        Ok(Page { items, cursor })
    }

    pub async fn user_groups(
        &self,
        user: AccountId,
        state: Option<MemberState>,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<Page<UserGroup>> {
        let limit = limit.clamp(1, 100) as i64;
        let after: Option<Uuid> = match cursor {
            Some(c) => Some(
                c.parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?,
            ),
            None => None,
        };
        let rows = sqlx::query(&format!(
            "select {GROUP_COLS}, m.state from musubi.group_members m join musubi.groups g on g.id = m.group_id
             where m.user_id = $1 and ($2::smallint is null or m.state = $2) and ($3::uuid is null or g.id > $3)
             order by g.id limit $4"
        ))
        .bind(user.0)
        .bind(state.map(i16::from))
        .bind(after)
        .bind(limit + 1)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let mut items: Vec<UserGroup> = Vec::with_capacity(rows.len());
        for r in rows.into_iter().take(limit as usize) {
            use sqlx::Row;
            let get = |e: sqlx::Error| Error::internal(e);
            let state: i16 = r.try_get(16).map_err(get)?;
            let row: GroupRow = (
                r.try_get(0).map_err(get)?,
                r.try_get(1).map_err(get)?,
                r.try_get(2).map_err(get)?,
                r.try_get(3).map_err(get)?,
                r.try_get(4).map_err(get)?,
                r.try_get(5).map_err(get)?,
                r.try_get(6).map_err(get)?,
                r.try_get(7).map_err(get)?,
                r.try_get(8).map_err(get)?,
                r.try_get(9).map_err(get)?,
                r.try_get(10).map_err(get)?,
                r.try_get(11).map_err(get)?,
                r.try_get(12).map_err(get)?,
                r.try_get(13).map_err(get)?,
                r.try_get(14).map_err(get)?,
                r.try_get(15).map_err(get)?,
            );
            items.push(UserGroup {
                state: MemberState::try_from(state).unwrap_or(MemberState::JoinRequest),
                group: to_group(row),
            });
        }
        let cursor = more
            .then(|| items.last().map(|g| g.group.id.0.to_string()))
            .flatten();
        Ok(Page { items, cursor })
    }
}

type MemberRow = (
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
    i16,
);

struct Locked {
    open: bool,
    edge_count: i32,
    max_count: i32,
}

async fn log_event(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    kind: &str,
    actor: Option<AccountId>,
    subject: Option<AccountId>,
    payload: serde_json::Value,
) -> Result<()> {
    sqlx::query(
        "insert into musubi.group_events (id, group_id, kind, actor_id, subject_id, payload) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(Uuid::now_v7())
    .bind(group.0)
    .bind(kind)
    .bind(actor.map(|a| a.0))
    .bind(subject.map(|a| a.0))
    .bind(&payload)
    .execute(&mut **tx)
    .await
    .map_err(Error::internal)?;
    Ok(())
}

async fn lock_group(tx: &mut Transaction<'_, Postgres>, group: GroupId) -> Result<Locked> {
    let r = sqlx::query_as::<_, (bool, i32, i32)>(
        "select open, edge_count, max_count from musubi.groups where id = $1 for update",
    )
    .bind(group.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Error::internal)?
    .ok_or_else(|| Error::NotFound("group".into()))?;
    Ok(Locked {
        open: r.0,
        edge_count: r.1,
        max_count: r.2,
    })
}

async fn member_state(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    user: AccountId,
) -> Result<Option<MemberState>> {
    let s: Option<i16> = sqlx::query_scalar(
        "select state from musubi.group_members where group_id = $1 and user_id = $2 for update",
    )
    .bind(group.0)
    .bind(user.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Error::internal)?;
    s.map(|s| MemberState::try_from(s).map_err(Error::Internal))
        .transpose()
}

/// Caller must be a member with rank ≤ `at_least`. Returns the caller's actual rank.
async fn require_role(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    me: AccountId,
    at_least: MemberState,
) -> Result<MemberState> {
    let exists: Option<i64> =
        sqlx::query_scalar("select 1::bigint from musubi.groups where id = $1")
            .bind(group.0)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
    if exists.is_none() {
        return Err(Error::NotFound("group".into()));
    }
    let mine = member_state(tx, group, me).await?;
    match mine {
        Some(s) if s <= at_least => Ok(s),
        _ => Err(Error::PermissionDenied("insufficient group rank".into())),
    }
}

async fn insert_member(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    user: AccountId,
    state: MemberState,
) -> Result<()> {
    sqlx::query(
        "insert into musubi.group_members (group_id, user_id, state, rank) values ($1, $2, $3, least($3, 2))",
    )
    .bind(group.0)
    .bind(user.0)
    .bind(i16::from(state))
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
    if state < MemberState::JoinRequest {
        bump_edges(tx, group, 1).await?;
    }
    Ok(())
}

/// Caller must be a member whose role holds `bit` (the owner always does). Returns the caller's
/// legacy state so rank comparisons keep working.
async fn require_perm(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    me: AccountId,
    bit: i64,
) -> Result<MemberState> {
    let row: Option<(i16, i16)> = sqlx::query_as(
        "select m.state, m.rank from musubi.group_members m where m.group_id = $1 and m.user_id = $2 for update",
    )
    .bind(group.0)
    .bind(me.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Error::internal)?;
    let Some((state, rank)) = row else {
        let exists: Option<i64> =
            sqlx::query_scalar("select 1::bigint from musubi.groups where id = $1")
                .bind(group.0)
                .fetch_optional(&mut **tx)
                .await
                .map_err(Error::internal)?;
        return Err(if exists.is_none() {
            Error::NotFound("group".into())
        } else {
            Error::PermissionDenied("not a member".into())
        });
    };
    let state = MemberState::try_from(state).map_err(Error::Internal)?;
    if state == MemberState::JoinRequest {
        return Err(Error::PermissionDenied("not a member".into()));
    }
    if rank == 0
        || state == MemberState::Superadmin
        || role_permissions(tx, group, rank).await? & bit != 0
    {
        Ok(state)
    } else {
        Err(Error::PermissionDenied("role lacks permission".into()))
    }
}

/// `me` is strictly senior to `other` in the rank hierarchy (lower rank wins; the owner beats all).
async fn outranks(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    me: AccountId,
    other: AccountId,
) -> Result<bool> {
    let rank = |u: AccountId| {
        sqlx::query_scalar::<_, i16>(
            "select rank from musubi.group_members where group_id = $1 and user_id = $2",
        )
        .bind(group.0)
        .bind(u.0)
    };
    let mine: i16 = rank(me)
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
    let theirs: Option<i16> = rank(other)
        .fetch_optional(&mut **tx)
        .await
        .map_err(Error::internal)?;
    Ok(theirs.is_none_or(|t| t > mine))
}

async fn alliance_leader(tx: &mut Transaction<'_, Postgres>, alliance: Uuid) -> Result<GroupId> {
    let g: Option<Uuid> =
        sqlx::query_scalar("select leader_group from musubi.alliances where id = $1")
            .bind(alliance)
            .fetch_optional(&mut **tx)
            .await
            .map_err(Error::internal)?;
    g.map(GroupId)
        .ok_or_else(|| Error::NotFound("alliance".into()))
}

async fn role_permissions(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    rank: i16,
) -> Result<i64> {
    let p: Option<i64> = sqlx::query_scalar(
        "select permissions from musubi.group_roles where group_id = $1 and rank = $2",
    )
    .bind(group.0)
    .bind(rank)
    .fetch_optional(&mut **tx)
    .await
    .map_err(Error::internal)?;
    Ok(p.unwrap_or_else(|| perm::default_for(rank)))
}

async fn set_state(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    user: AccountId,
    state: MemberState,
) -> Result<()> {
    let prev: i16 = sqlx::query_scalar("update musubi.group_members m set state = $3, rank = case when $3 < 2 then $3 else greatest(m.rank, 2) end, updated_at = now() from (select state from musubi.group_members where group_id = $1 and user_id = $2) o where m.group_id = $1 and m.user_id = $2 returning o.state")
        .bind(group.0)
        .bind(user.0)
        .bind(i16::from(state))
        .fetch_one(&mut **tx)
        .await
        .map_err(Error::internal)?;
    if prev == 3 && state < MemberState::JoinRequest {
        bump_edges(tx, group, 1).await?;
    }
    Ok(())
}

async fn remove_member(
    tx: &mut Transaction<'_, Postgres>,
    group: GroupId,
    user: AccountId,
    state: MemberState,
) -> Result<()> {
    sqlx::query("delete from musubi.group_members where group_id = $1 and user_id = $2")
        .bind(group.0)
        .bind(user.0)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?;
    if state < MemberState::JoinRequest {
        bump_edges(tx, group, -1).await?;
    }
    Ok(())
}

async fn bump_edges(tx: &mut Transaction<'_, Postgres>, group: GroupId, delta: i32) -> Result<()> {
    sqlx::query(
        "update musubi.groups set edge_count = edge_count + $2, updated_at = now() where id = $1",
    )
    .bind(group.0)
    .bind(delta)
    .execute(&mut **tx)
    .await
    .map_err(Error::internal)?;
    Ok(())
}

fn check_users(users: &[AccountId]) -> Result<()> {
    if users.is_empty() || users.len() > 100 {
        return Err(Error::InvalidArgument("1..=100 user ids required".into()));
    }
    Ok(())
}
