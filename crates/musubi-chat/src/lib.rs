//! Chat channels: rooms (ad-hoc, by name), direct messages and group chat. Delivery is the realtime
//! layer's job; this crate owns channel identity, authorization rules and persisted history.

pub mod moderation;
pub use moderation::{ChannelSettings, Filter, FilterAction, Report};

use musubi_core::{AccountId, Error, GroupId, Result};
use musubi_proto::realtime::{ChannelKind, ChannelMessage, Reaction};
use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const MAX_CONTENT_BYTES: usize = 4096;
pub const MAX_ROOM_NAME: usize = 64;
/// Distinct emoji kinds one message may carry (new kinds past this are rejected).
pub const MAX_REACTION_KINDS: i64 = 20;

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
    if let Some(a) = content.get("attachments") {
        validate_attachments(a)?;
    }
    Ok(())
}

pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// `content.attachments`: up to 10 `{url, name?, mime?, size?}` objects — `url` is https (or a
/// UGC/blob reference `ugc:<id>`), ≤ 2048 chars; `size` ≤ 25 MiB. Anything else is rejected
/// so clients can trust the shape.
pub fn validate_attachments(v: &serde_json::Value) -> Result<()> {
    let Some(list) = v.as_array() else {
        return Err(Error::InvalidArgument(
            "attachments must be an array".into(),
        ));
    };
    if list.len() > MAX_ATTACHMENTS {
        return Err(Error::InvalidArgument(format!(
            "at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    for (i, a) in list.iter().enumerate() {
        let Some(o) = a.as_object() else {
            return Err(Error::InvalidArgument(format!(
                "attachment {i}: not an object"
            )));
        };
        let url = o.get("url").and_then(|u| u.as_str()).unwrap_or("");
        if url.is_empty()
            || url.len() > 2048
            || !(url.starts_with("https://") || url.starts_with("ugc:"))
        {
            return Err(Error::InvalidArgument(format!(
                "attachment {i}: url must be https:// or ugc:<id>, ≤ 2048 chars"
            )));
        }
        if o.get("name")
            .is_some_and(|n| !n.is_string() || n.as_str().unwrap().chars().count() > 255)
        {
            return Err(Error::InvalidArgument(format!(
                "attachment {i}: name ≤ 255 chars"
            )));
        }
        if o.get("mime")
            .is_some_and(|m| !m.is_string() || m.as_str().unwrap().len() > 128)
        {
            return Err(Error::InvalidArgument(format!("attachment {i}: bad mime")));
        }
        if let Some(sz) = o.get("size") {
            let ok = sz.as_u64().is_some_and(|n| n <= MAX_ATTACHMENT_BYTES);
            if !ok {
                return Err(Error::InvalidArgument(format!(
                    "attachment {i}: size must be ≤ {MAX_ATTACHMENT_BYTES} bytes"
                )));
            }
        }
    }
    Ok(())
}

/// Per-channel unread counters for one viewer.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Unread {
    pub channel_id: String,
    pub count: u64,
    pub mentions: u64,
    pub last_read: Option<String>,
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
    moderation: std::sync::Arc<moderation::ModerationState>,
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
    Option<Uuid>,
    Vec<Uuid>,
);
const MSG_COLS: &str = "id, channel_id, kind, sender_id, username, code, content, created_at, updated_at, reply_to, mentions";

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
        reactions: Vec::new(),
        reply_to: r.9.map(|u| u.to_string()),
        mentions: r.10.into_iter().map(AccountId).collect(),
    }
}

impl ChatService {
    pub fn new(db: PgPool) -> Self {
        Self {
            db,
            moderation: Default::default(),
        }
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
        self.append_ext(
            channel,
            sender,
            username,
            content,
            persistent,
            None,
            Vec::new(),
        )
        .await
    }

    /// `append` with a thread parent and resolved mentions. `reply_to` must be a persisted message
    /// of the same channel.
    #[allow(clippy::too_many_arguments)]
    pub async fn append_ext(
        &self,
        channel: &Channel,
        sender: AccountId,
        username: &str,
        content: serde_json::Value,
        persistent: bool,
        reply_to: Option<&str>,
        mentions: Vec<AccountId>,
    ) -> Result<ChannelMessage> {
        validate_content(&content)?;
        let reply_to = match reply_to {
            Some(r) => {
                let rid: Uuid = r
                    .parse()
                    .map_err(|_| Error::InvalidArgument("bad reply_to".into()))?;
                let ok: bool = sqlx::query_scalar(
                    "select exists(select 1 from musubi.chat_messages where id = $1 and channel_id = $2 and reply_to is null)",
                )
                .bind(rid)
                .bind(channel.id())
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
                if !ok {
                    return Err(Error::NotFound(
                        "reply_to: no such top-level message in this channel".into(),
                    ));
                }
                Some(rid)
            }
            None => None,
        };
        let id = Uuid::now_v7();
        let now = OffsetDateTime::now_utc();
        if persistent {
            sqlx::query("insert into musubi.chat_messages (id, channel_id, kind, sender_id, username, code, content, created_at, updated_at, reply_to, mentions) values ($1, $2, $3, $4, $5, 0, $6, $7, $7, $8, $9)")
                .bind(id)
                .bind(channel.id())
                .bind(channel.kind() as i16)
                .bind(sender.0)
                .bind(username)
                .bind(&content)
                .bind(now)
                .bind(reply_to)
                .bind(mentions.iter().map(|a| a.0).collect::<Vec<_>>())
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
            reactions: Vec::new(),
            reply_to: reply_to.map(|u| u.to_string()),
            mentions,
        })
    }

    /// Accounts @-mentioned in the message text (`@name`, 2..=32 chars of `[A-Za-z0-9_.-]`),
    /// resolved case-insensitively; unknown names are ignored.
    pub async fn resolve_mentions(&self, content: &serde_json::Value) -> Result<Vec<AccountId>> {
        fn collect<'a>(v: &'a serde_json::Value, out: &mut Vec<&'a str>) {
            match v {
                serde_json::Value::String(s) => out.push(s),
                serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
                serde_json::Value::Object(o) => o.values().for_each(|x| collect(x, out)),
                _ => {}
            }
        }
        let mut texts = Vec::new();
        collect(content, &mut texts);
        let mut names: Vec<String> = Vec::new();
        for t in texts {
            let mut rest = t;
            while let Some(i) = rest.find('@') {
                let after = &rest[i + 1..];
                let end = after
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
                    .unwrap_or(after.len());
                let name = &after[..end];
                if (2..=32).contains(&name.len()) {
                    let l = name.to_ascii_lowercase();
                    if !names.contains(&l) {
                        names.push(l);
                    }
                }
                rest = &after[end..];
            }
        }
        if names.is_empty() {
            return Ok(Vec::new());
        }
        names.truncate(20);
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "select id from musubi.accounts where lower(username) = any($1) and deleted_at is null",
        )
        .bind(&names)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(ids.into_iter().map(AccountId).collect())
    }

    /// Replies to `message_id` in `channel`, oldest first.
    pub async fn replies(
        &self,
        channel: &Channel,
        message_id: &str,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<History> {
        let parent: Uuid = message_id
            .parse()
            .map_err(|_| Error::InvalidArgument("bad message id".into()))?;
        let cursor = cursor
            .map(|c| {
                c.parse::<Uuid>()
                    .map_err(|_| Error::InvalidArgument("bad cursor".into()))
            })
            .transpose()?;
        let limit = limit.clamp(1, 100) as i64;
        let rows: Vec<Row> = sqlx::query_as(&format!(
            "select {MSG_COLS} from musubi.chat_messages where channel_id = $1 and reply_to = $2 and ($3::uuid is null or id > $3) order by id asc limit $4"
        ))
        .bind(channel.id())
        .bind(parent)
        .bind(cursor)
        .bind(limit)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let next_cursor = (rows.len() as i64 == limit).then(|| rows.last().unwrap().0.to_string());
        Ok(History {
            messages: rows.into_iter().map(to_msg).collect(),
            next_cursor,
            prev_cursor: None,
        })
    }

    /// Move `me`'s read cursor in `channel` to `message_id` (or to "now" when omitted).
    pub async fn mark_read(
        &self,
        channel: &Channel,
        me: AccountId,
        message_id: Option<&str>,
    ) -> Result<()> {
        let at = match message_id {
            Some(m) => m
                .parse::<Uuid>()
                .map_err(|_| Error::InvalidArgument("bad message id".into()))?,
            None => Uuid::now_v7(),
        };
        sqlx::query(
            "insert into musubi.chat_read_cursors (channel_id, account_id, last_read) values ($1, $2, $3)
             on conflict (channel_id, account_id) do update
                 set last_read = greatest(musubi.chat_read_cursors.last_read, excluded.last_read), updated_at = now()",
        )
        .bind(channel.id())
        .bind(me.0)
        .bind(at)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Unread counters for `me` in each of `channels`: messages after the read cursor that were
    /// not sent by `me`, and how many of them mention `me`.
    pub async fn unread(&self, me: AccountId, channels: &[String]) -> Result<Vec<Unread>> {
        if channels.is_empty() {
            return Ok(Vec::new());
        }
        let rows: Vec<(String, i64, i64, Option<Uuid>)> = sqlx::query_as(
            "select c.channel_id,
                    count(m.id),
                    count(m.id) filter (where $2 = any(m.mentions)),
                    r.last_read
             from unnest($1::text[]) as c(channel_id)
             left join musubi.chat_read_cursors r on r.channel_id = c.channel_id and r.account_id = $2
             left join musubi.chat_messages m on m.channel_id = c.channel_id and m.code = 0
                  and (m.sender_id is distinct from $2) and (r.last_read is null or m.id > r.last_read)
             group by c.channel_id, r.last_read",
        )
        .bind(channels)
        .bind(me.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(channel_id, count, mentions, last_read)| Unread {
                channel_id,
                count: count as u64,
                mentions: mentions as u64,
                last_read: last_read.map(|u| u.to_string()),
            })
            .collect())
    }

    /// Only the author may edit; code 1 marks the message as updated.
    /// Add (`add = true`) or remove the caller's `emoji` reaction on a message of `channel`.
    /// Returns the new total for that emoji. Idempotent.
    pub async fn react(
        &self,
        channel: &Channel,
        me: AccountId,
        message_id: &str,
        emoji: &str,
        add: bool,
    ) -> Result<u32> {
        let n = emoji.chars().count();
        if n == 0 || n > 32 || emoji.chars().any(char::is_whitespace) {
            return Err(Error::InvalidArgument(
                "emoji must be 1..=32 non-blank chars".into(),
            ));
        }
        let id: Uuid = message_id
            .parse()
            .map_err(|_| Error::InvalidArgument("bad message id".into()))?;
        let exists: bool = sqlx::query_scalar(
            "select exists(select 1 from musubi.chat_messages where id = $1 and channel_id = $2)",
        )
        .bind(id)
        .bind(channel.id())
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        if !exists {
            return Err(Error::NotFound(
                "message (only persisted messages can be reacted to; room messages are not stored)"
                    .into(),
            ));
        }
        if add {
            // a new emoji kind is admitted only while the message has fewer than
            // MAX_REACTION_KINDS distinct ones; joining an existing kind always works
            let inserted = sqlx::query(
                "insert into musubi.chat_reactions (message_id, account_id, emoji)
                 select $1, $2, $3
                 where exists(select 1 from musubi.chat_reactions where message_id = $1 and emoji = $3)
                    or (select count(distinct emoji) from musubi.chat_reactions where message_id = $1) < $4
                 on conflict do nothing",
            )
            .bind(id)
            .bind(me.0)
            .bind(emoji)
            .bind(MAX_REACTION_KINDS)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
            if inserted == 0 {
                let mine: bool = sqlx::query_scalar(
                    "select exists(select 1 from musubi.chat_reactions where message_id = $1 and account_id = $2 and emoji = $3)",
                )
                .bind(id)
                .bind(me.0)
                .bind(emoji)
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
                if !mine {
                    return Err(Error::ResourceExhausted(format!(
                        "a message holds at most {MAX_REACTION_KINDS} distinct reactions"
                    )));
                }
            }
        } else {
            sqlx::query(
                "delete from musubi.chat_reactions where message_id = $1 and account_id = $2 and emoji = $3",
            )
            .bind(id)
            .bind(me.0)
            .bind(emoji)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        }
        let count: i64 = sqlx::query_scalar(
            "select count(*) from musubi.chat_reactions where message_id = $1 and emoji = $2",
        )
        .bind(id)
        .bind(emoji)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(count as u32)
    }

    /// Reactions for a page of messages, aggregated per emoji; `me` marks the viewer's own.
    async fn reactions_for(
        &self,
        ids: &[Uuid],
        viewer: Option<AccountId>,
    ) -> Result<std::collections::HashMap<Uuid, Vec<Reaction>>> {
        let mut out: std::collections::HashMap<Uuid, Vec<Reaction>> = Default::default();
        if ids.is_empty() {
            return Ok(out);
        }
        let rows: Vec<(Uuid, String, i64, bool)> = sqlx::query_as(
            "select message_id, emoji, count(*), bool_or(account_id = $2)
             from musubi.chat_reactions where message_id = any($1)
             group by message_id, emoji order by message_id, min(created_at)",
        )
        .bind(ids)
        .bind(viewer.map(|v| v.0).unwrap_or(Uuid::nil()))
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        for (mid, emoji, count, me) in rows {
            out.entry(mid).or_default().push(Reaction {
                emoji,
                count: count as u32,
                me,
            });
        }
        Ok(out)
    }

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
        let row = sqlx::query_as::<_, Row>(&format!(
            "update musubi.chat_messages set content = $4, code = 1, updated_at = now()
             where id = $1 and channel_id = $2 and sender_id = $3
             returning {MSG_COLS}"
        ))
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
        let row = sqlx::query_as::<_, Row>(&format!(
            "update musubi.chat_messages set content = '{{}}'::jsonb, code = 2, updated_at = now()
             where id = $1 and channel_id = $2 and sender_id = $3
             returning {MSG_COLS}"
        ))
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
        viewer: Option<AccountId>,
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
            &format!("select {MSG_COLS} from musubi.chat_messages where channel_id = $1 and ($2::uuid is null or id > $2) order by id asc limit $3")
        } else {
            &format!("select {MSG_COLS} from musubi.chat_messages where channel_id = $1 and ($2::uuid is null or id < $2) order by id desc limit $3")
        };
        let rows = sqlx::query_as::<_, Row>(sql)
            .bind(channel.id())
            .bind(c)
            .bind(limit + 1)
            .fetch_all(&self.db)
            .await
            .map_err(Error::internal)?;
        let more = rows.len() as i64 > limit;
        let mut messages: Vec<ChannelMessage> =
            rows.into_iter().take(limit as usize).map(to_msg).collect();
        let ids: Vec<Uuid> = messages
            .iter()
            .filter_map(|m| m.message_id.parse().ok())
            .collect();
        let mut reactions = self.reactions_for(&ids, viewer).await?;
        for m in &mut messages {
            if let Some(r) = m
                .message_id
                .parse()
                .ok()
                .and_then(|id| reactions.remove(&id))
            {
                m.reactions = r;
            }
        }
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

/// A mute (cannot send) or ban (cannot join or send) on a channel.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Moderation {
    pub channel_id: String,
    pub account_id: AccountId,
    pub kind: String,
    pub reason: Option<String>,
    pub created_by: Option<String>,
    pub created_at: i64,
    pub until: Option<i64>,
}

impl ChatService {
    /// Mute or ban `account` on `channel`; `until_secs` = None for indefinite. Replaces any existing entry.
    pub async fn moderate(
        &self,
        channel: &Channel,
        account: AccountId,
        kind: &str,
        reason: Option<&str>,
        by: Option<&str>,
        until_secs: Option<u64>,
    ) -> Result<()> {
        if !matches!(kind, "mute" | "ban" | "shadow") {
            return Err(Error::InvalidArgument(
                "kind must be mute, ban or shadow".into(),
            ));
        }
        let until = until_secs.map(|s| {
            time::OffsetDateTime::now_utc()
                + time::Duration::seconds(s.min(10 * 365 * 86_400) as i64)
        });
        sqlx::query(
            "insert into musubi.chat_moderation (channel_id, account_id, kind, reason, created_by, until)
             values ($1, $2, $3, $4, $5, $6)
             on conflict (channel_id, account_id) do update
               set kind = excluded.kind, reason = excluded.reason, created_by = excluded.created_by,
                   created_at = now(), until = excluded.until",
        )
        .bind(channel.id())
        .bind(account.0)
        .bind(kind)
        .bind(reason)
        .bind(by)
        .bind(until)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        self.moderation.forget_restriction(&channel.id(), account.0);
        Ok(())
    }

    pub async fn unmoderate(&self, channel: &Channel, account: AccountId) -> Result<bool> {
        let n = sqlx::query(
            "delete from musubi.chat_moderation where channel_id = $1 and account_id = $2",
        )
        .bind(channel.id())
        .bind(account.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        self.moderation.forget_restriction(&channel.id(), account.0);
        Ok(n > 0)
    }

    pub async fn moderations(&self, channel: &Channel) -> Result<Vec<Moderation>> {
        type Row = (
            String,
            uuid::Uuid,
            String,
            Option<String>,
            Option<String>,
            time::OffsetDateTime,
            Option<time::OffsetDateTime>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select channel_id, account_id, kind, reason, created_by, created_at, until
             from musubi.chat_moderation
             where channel_id = $1 and (until is null or until > now()) order by created_at",
        )
        .bind(channel.id())
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(c, a, k, r, b, ca, u)| Moderation {
                channel_id: c,
                account_id: AccountId(a),
                kind: k,
                reason: r,
                created_by: b,
                created_at: ca.unix_timestamp(),
                until: u.map(|u| u.unix_timestamp()),
            })
            .collect())
    }

    /// Active moderation entry of `account` on `channel` (`mute` / `ban`), if any.
    pub async fn restriction(
        &self,
        channel: &Channel,
        account: AccountId,
    ) -> Result<Option<String>> {
        let id = channel.id();
        if let Some(hit) = self.moderation.cached_restriction(&id, account.0) {
            return Ok(hit);
        }
        let kind: Option<String> = sqlx::query_scalar(
            "select kind from musubi.chat_moderation
             where channel_id = $1 and account_id = $2 and (until is null or until > now())",
        )
        .bind(&id)
        .bind(account.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        self.moderation
            .remember_restriction(&id, account.0, kind.clone());
        Ok(kind)
    }

    /// Delete any message on behalf of a moderator (no sender check); true if it existed.
    pub async fn remove_as_moderator(
        &self,
        channel: &Channel,
        message_id: uuid::Uuid,
    ) -> Result<bool> {
        let n = sqlx::query("delete from musubi.chat_messages where channel_id = $1 and id = $2")
            .bind(channel.id())
            .bind(message_id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
        Ok(n > 0)
    }
}
