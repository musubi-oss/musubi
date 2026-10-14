//! Chat channels: rooms (ad-hoc, by name), direct messages and group chat. Delivery is the realtime
//! layer's job; this crate owns channel identity, authorization rules and persisted history.

use musubi_core::{AccountId, Error, GroupId, Result};
use musubi_proto::realtime::{ChannelKind, ChannelMessage};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const MAX_CONTENT_BYTES: usize = 4096;
pub const MAX_ROOM_NAME: usize = 64;

/// Canonical channel identity. Encodes as `"<kind>.<key>"`; kind numbers follow Nakama (2 room, 3 direct, 4 group).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Channel {
    Room(String),
    Direct(AccountId, AccountId),
    Group(GroupId),
}

impl Channel {
    pub fn direct(a: AccountId, b: AccountId) -> Self {
        if a.0 <= b.0 {
            Self::Direct(a, b)
        } else {
            Self::Direct(b, a)
        }
    }

    pub fn kind(&self) -> ChannelKind {
        match self {
            Self::Room(_) => ChannelKind::Room,
            Self::Direct(..) => ChannelKind::Direct,
            Self::Group(_) => ChannelKind::Group,
        }
    }

    pub fn id(&self) -> String {
        match self {
            Self::Room(r) => format!("2.{r}"),
            Self::Direct(a, b) => format!("3.{}.{}", a.0, b.0),
            Self::Group(g) => format!("4.{}", g.0),
        }
    }

    pub fn parse(id: &str) -> Result<Self> {
        let bad = || Error::InvalidArgument(format!("bad channel id {id}"));
        let (kind, rest) = id.split_once('.').ok_or_else(bad)?;
        match kind {
            "2" => {
                validate_room(rest)?;
                Ok(Self::Room(rest.to_string()))
            }
            "3" => {
                let (a, b) = rest.split_once('.').ok_or_else(bad)?;
                let a: Uuid = a.parse().map_err(|_| bad())?;
                let b: Uuid = b.parse().map_err(|_| bad())?;
                if a > b {
                    return Err(bad());
                }
                Ok(Self::Direct(AccountId(a), AccountId(b)))
            }
            "4" => Ok(Self::Group(GroupId(rest.parse().map_err(|_| bad())?))),
            _ => Err(bad()),
        }
    }

    /// Members allowed in a direct channel; `None` for rooms/groups.
    pub fn direct_peer(&self, me: AccountId) -> Option<AccountId> {
        match self {
            Self::Direct(a, b) if *a == me => Some(*b),
            Self::Direct(a, b) if *b == me => Some(*a),
            _ => None,
        }
    }
}

pub fn validate_room(name: &str) -> Result<()> {
    if name.is_empty()
        || name.chars().count() > MAX_ROOM_NAME
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | ':' | '#'))
    {
        return Err(Error::InvalidArgument(
            "room name must be 1..=64 chars of letters, digits, _ - : #".into(),
        ));
    }
    Ok(())
}

pub fn validate_content(content: &serde_json::Value) -> Result<()> {
    if !content.is_object() || serde_json::to_vec(content)?.len() > MAX_CONTENT_BYTES {
        return Err(Error::InvalidArgument(format!(
            "content must be a JSON object ≤ {MAX_CONTENT_BYTES} bytes"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct History {
    pub messages: Vec<ChannelMessage>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
}

#[derive(Clone)]
pub struct ChatService {
    db: PgPool,
}

type Row = (
    Uuid,
    String,
    i16,
    Option<Uuid>,
    String,
    i32,
    serde_json::Value,
    OffsetDateTime,
    OffsetDateTime,
);

fn to_msg(r: Row) -> ChannelMessage {
    ChannelMessage {
        channel_id: r.1,
        message_id: r.0.to_string(),
        kind: ChannelKind::from_i16(r.2),
        code: r.5,
        sender_id: r.3.map(AccountId),
        username: r.4,
        content: r.6,
        create_time: r.7.unix_timestamp(),
        update_time: r.8.unix_timestamp(),
        persistent: true,
    }
}

impl ChatService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    /// Can `me` take part in `channel`? Direct: neither side blocked the other. Group: must be a member (rank < 3).
    pub async fn authorize(&self, me: AccountId, channel: &Channel) -> Result<()> {
        match channel {
            Channel::Room(_) => Ok(()),
            Channel::Direct(a, b) => {
                let peer = channel.direct_peer(me).ok_or_else(|| {
                    Error::PermissionDenied("not a party to this direct channel".into())
                })?;
                if peer == me || *a == *b {
                    return Err(Error::InvalidArgument("cannot chat with yourself".into()));
                }
                let blocked: Option<i64> = sqlx::query_scalar(
                    "select 1::bigint from musubi.friends where state = 3 and ((user_id = $1 and friend_id = $2) or (user_id = $2 and friend_id = $1)) limit 1",
                )
                .bind(me.0)
                .bind(peer.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
                let exists: Option<i64> = sqlx::query_scalar(
                    "select 1::bigint from musubi.accounts where id = $1 and deleted_at is null",
                )
                .bind(peer.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
                if exists.is_none() {
                    return Err(Error::NotFound("user".into()));
                }
                if blocked.is_some() {
                    return Err(Error::PermissionDenied("direct channel unavailable".into()));
                }
                Ok(())
            }
            Channel::Group(g) => {
                let member: Option<i16> = sqlx::query_scalar(
                    "select state from musubi.group_members where group_id = $1 and user_id = $2",
                )
                .bind(g.0)
                .bind(me.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
                match member {
                    Some(s) if s < 3 => Ok(()),
                    _ => Err(Error::PermissionDenied("not a group member".into())),
                }
            }
        }
    }

    pub async fn append(
        &self,
        channel: &Channel,
        sender: AccountId,
        username: &str,
        content: serde_json::Value,
        persistent: bool,
    ) -> Result<ChannelMessage> {
        validate_content(&content)?;
        let id = Uuid::now_v7();
        let now = OffsetDateTime::now_utc();
        if persistent {
            sqlx::query("insert into musubi.chat_messages (id, channel_id, kind, sender_id, username, code, content, created_at, updated_at) values ($1, $2, $3, $4, $5, 0, $6, $7, $7)")
                .bind(id)
                .bind(channel.id())
                .bind(channel.kind() as i16)
                .bind(sender.0)
                .bind(username)
                .bind(&content)
                .bind(now)
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
        }
        Ok(ChannelMessage {
            channel_id: channel.id(),
            message_id: id.to_string(),
            kind: channel.kind(),
            code: 0,
            sender_id: Some(sender),
            username: username.to_string(),
            content,
            create_time: now.unix_timestamp(),
            update_time: now.unix_timestamp(),
            persistent,
        })
    }

    /// Only the author may edit; code 1 marks the message as updated.
    pub async fn update(
        &self,
        channel: &Channel,
        sender: AccountId,
        message_id: &str,
        content: serde_json::Value,
    ) -> Result<ChannelMessage> {
        validate_content(&content)?;
        let id: Uuid = message_id
            .parse()
            .map_err(|_| Error::InvalidArgument("bad message id".into()))?;
        let row = sqlx::query_as::<_, Row>(
            "update musubi.chat_messages set content = $4, code = 1, updated_at = now()
             where id = $1 and channel_id = $2 and sender_id = $3
             returning id, channel_id, kind, sender_id, username, code, content, created_at, updated_at",
        )
        .bind(id)
        .bind(channel.id())
        .bind(sender.0)
        .bind(&content)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("message not found or not yours".into()))?;
        Ok(to_msg(row))
    }

    /// Only the author may remove; the row stays with code 2 and empty content (clients render a tombstone).
    pub async fn remove(
        &self,
        channel: &Channel,
        sender: AccountId,
        message_id: &str,
    ) -> Result<ChannelMessage> {
        let id: Uuid = message_id
            .parse()
            .map_err(|_| Error::InvalidArgument("bad message id".into()))?;
        let row = sqlx::query_as::<_, Row>(
            "update musubi.chat_messages set content = '{}'::jsonb, code = 2, updated_at = now()
             where id = $1 and channel_id = $2 and sender_id = $3
             returning id, channel_id, kind, sender_id, username, code, content, created_at, updated_at",
        )
        .bind(id)
        .bind(channel.id())
        .bind(sender.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("message not found or not yours".into()))?;
        Ok(to_msg(row))
    }

    /// History newest-first (`forward = false`) or oldest-first; cursor is a message id.
    pub async fn history(
        &self,
        channel: &Channel,
        limit: usize,
        forward: bool,
        cursor: Option<&str>,
    ) -> Result<History> {
        let limit = limit.clamp(1, 100) as i64;
        let c: Option<Uuid> = match cursor {
            Some(c) => Some(
                c.parse()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))?,
            ),
            None => None,
        };
        let sql = if forward {
            "select id, channel_id, kind, sender_id, username, code, content, created_at, updated_at from musubi.chat_messages where channel_id = $1 and ($2::uuid is null or id > $2) order by id asc limit $3"
        } else {
            "select id, channel_id, kind, sender_id, username, code, content, created_at, updated_at from musubi.chat_messages where channel_id = $1 and ($2::uuid is null or id < $2) order by id desc limit $3"
        };
        let rows = sqlx::query_as::<_, Row>(sql)
            .bind(channel.id())
            .bind(c)
            .bind(limit + 1)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let messages: Vec<ChannelMessage> =
            rows.into_iter().take(limit as usize).map(to_msg).collect();
        let last = messages.last().map(|m| m.message_id.clone());
        let first = messages.first().map(|m| m.message_id.clone());
        Ok(History {
            messages,
            next_cursor: if more { last } else { None },
            prev_cursor: first,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_ids_roundtrip() {
        let a = AccountId(Uuid::from_u128(1));
        let b = AccountId(Uuid::from_u128(2));
        let d = Channel::direct(b, a);
        assert_eq!(d, Channel::Direct(a, b));
        assert_eq!(Channel::parse(&d.id()).unwrap(), d);
        assert_eq!(
            Channel::parse("2.lobby").unwrap(),
            Channel::Room("lobby".into())
        );
        assert!(Channel::parse("2.bad name").is_err());
        assert!(Channel::parse("7.x").is_err());
        assert_eq!(d.direct_peer(a), Some(b));
    }
}
