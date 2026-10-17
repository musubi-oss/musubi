//! `musubi migrate nakama` (plan 6.5): import a Nakama 3.x database (Postgres or CockroachDB)
//! straight into Musubi's schema. Accounts, linked identities, devices, storage, friends, groups,
//! leaderboards, notifications and chat history map row for row (the schemas agree on states and
//! permissions); a Nakama wallet (one JSON blob per user) becomes currencies + wallet rows + one
//! ledger entry; passwords stay bcrypt and keep working through `musubi_auth::password::verify`.
//! Rows already present are kept unless `replace`; the import is idempotent, so it can be re-run on
//! migration day to pick up what changed since the rehearsal.

use anyhow::{Context, Result};
use futures::StreamExt;
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::types::time::OffsetDateTime;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Count {
    pub written: u64,
    pub kept: u64,
}

pub type Report = Vec<(&'static str, Count)>;

const IMPORT_REASON: &str = "nakama import";

fn text(r: &PgRow, c: &str) -> Option<String> {
    r.try_get::<Option<String>, _>(c).ok().flatten()
}

fn json(r: &PgRow, c: &str) -> Value {
    r.try_get::<Option<Value>, _>(c)
        .ok()
        .flatten()
        .unwrap_or_else(|| Value::Object(Default::default()))
}

fn uuid(r: &PgRow, c: &str) -> Option<Uuid> {
    r.try_get::<Option<Uuid>, _>(c).ok().flatten()
}

fn int(r: &PgRow, c: &str) -> Option<i64> {
    if let Ok(v) = r.try_get::<Option<i64>, _>(c) {
        return v;
    }
    if let Ok(v) = r.try_get::<Option<i32>, _>(c) {
        return v.map(i64::from);
    }
    r.try_get::<Option<i16>, _>(c).ok().flatten().map(i64::from)
}

/// Nakama writes the zero time instead of NULL for "never".
fn time(r: &PgRow, c: &str) -> Option<OffsetDateTime> {
    r.try_get::<Option<OffsetDateTime>, _>(c)
        .ok()
        .flatten()
        .filter(|t| t.unix_timestamp() > 0)
}

async fn has_table(src: &PgPool, name: &str) -> Result<bool> {
    sqlx::query_scalar(
        "select exists (select 1 from information_schema.tables
          where table_name = $1 and table_schema = any (current_schemas(false)))",
    )
    .bind(name)
    .fetch_one(src)
    .await
    .with_context(|| format!("looking for Nakama table {name}"))
}

async fn exec(
    q: sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    dst: &PgPool,
    c: &mut Count,
) -> Result<bool> {
    let n = q.execute(dst).await?.rows_affected();
    if n > 0 {
        c.written += 1;
    } else {
        c.kept += 1;
    }
    Ok(n > 0)
}

fn conflict(replace: bool, key: &str, set: &str) -> String {
    if replace {
        format!("on conflict ({key}) do update set {set}")
    } else {
        format!("on conflict ({key}) do nothing")
    }
}

pub async fn import(src: &PgPool, dst: &PgPool, replace: bool) -> Result<Report> {
    let mut report = Report::new();
    let (mut accounts, mut identities, mut wallets) =
        (Count::default(), Count::default(), Count::default());
    if has_table(src, "users").await? {
        let sql = format!(
            "insert into musubi.accounts
               (id, username, display_name, avatar_url, lang, timezone, location, metadata,
                created_at, updated_at, disabled_at)
             values ($1, $2, $3, $4, coalesce($5, 'en'), $6, $7, $8,
                     coalesce($9, now()), coalesce($10, now()), $11) {}",
            conflict(
                replace,
                "id",
                "username = excluded.username, display_name = excluded.display_name,
                 avatar_url = excluded.avatar_url, lang = excluded.lang, timezone = excluded.timezone,
                 location = excluded.location, metadata = excluded.metadata,
                 updated_at = excluded.updated_at, disabled_at = excluded.disabled_at"
            )
        );
        let mut rows = sqlx::query("select * from users where id <> $1")
            .bind(Uuid::nil())
            .fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama users")?;
            let Some(id) = uuid(&r, "id") else { continue };
            let username = text(&r, "username").unwrap_or_else(|| id.simple().to_string());
            exec(
                sqlx::query(&sql)
                    .bind(id)
                    .bind(&username)
                    .bind(text(&r, "display_name"))
                    .bind(text(&r, "avatar_url"))
                    .bind(text(&r, "lang_tag"))
                    .bind(text(&r, "timezone"))
                    .bind(text(&r, "location"))
                    .bind(json(&r, "metadata"))
                    .bind(time(&r, "create_time"))
                    .bind(time(&r, "update_time"))
                    .bind(time(&r, "disable_time")),
                dst,
                &mut accounts,
            )
            .await
            .with_context(|| format!("importing account {id} ({username})"))?;

            // linked identities; the email keeps its bcrypt hash
            let mut linked: Vec<(&str, Option<String>, Option<String>)> = vec![
                ("steam", text(&r, "steam_id"), None),
                ("google", text(&r, "google_id"), None),
                ("apple", text(&r, "apple_id"), None),
                ("facebook", text(&r, "facebook_id"), None),
                ("gamecenter", text(&r, "gamecenter_id"), None),
                ("custom", text(&r, "custom_id"), None),
            ];
            if let Some(email) = text(&r, "email") {
                let hash = r
                    .try_get::<Option<Vec<u8>>, _>("password")
                    .ok()
                    .flatten()
                    .and_then(|b| String::from_utf8(b).ok())
                    .filter(|h| !h.is_empty());
                linked.push(("email", Some(email.to_lowercase()), hash));
            }
            let verified = time(&r, "verify_time").is_some();
            for (provider, subject, hash) in linked {
                let Some(subject) = subject.filter(|s| !s.is_empty()) else {
                    continue;
                };
                exec(
                    sqlx::query(
                        "insert into musubi.identities (id, account_id, provider, subject, secret_hash, verified)
                         values ($1, $2, $3, $4, $5, $6) on conflict (provider, subject) do nothing",
                    )
                    .bind(Uuid::now_v7())
                    .bind(id)
                    .bind(provider)
                    .bind(&subject)
                    .bind(&hash)
                    .bind(provider == "email" && verified || provider != "email"),
                    dst,
                    &mut identities,
                )
                .await?;
            }

            // wallet: {"coins": 120} → currency + balance + one ledger line (first import only)
            if let Value::Object(w) = json(&r, "wallet") {
                let wsql = format!(
                    "insert into musubi.wallets (account_id, currency, balance) values ($1, $2, $3) {}",
                    conflict(replace, "account_id, currency", "balance = excluded.balance, updated_at = now()")
                );
                for (code, v) in w {
                    let Some(raw) = v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)) else {
                        continue;
                    };
                    if raw < 0 {
                        tracing::warn!(account = %id, currency = %code, balance = raw, "negative Nakama wallet balance clamped to 0");
                    }
                    let balance = raw.max(0);
                    sqlx::query("insert into musubi.currencies (code, name) values ($1, $1) on conflict (code) do nothing")
                        .bind(&code)
                        .execute(dst)
                        .await?;
                    let fresh = exec(
                        sqlx::query(&wsql).bind(id).bind(&code).bind(balance),
                        dst,
                        &mut wallets,
                    )
                    .await?;
                    if fresh {
                        sqlx::query(
                            "insert into musubi.wallet_ledger (id, account_id, currency, delta, balance_after, reason, ref)
                             values ($1, $2, $3, $4, $4, $5, 'nakama')",
                        )
                        .bind(Uuid::now_v7())
                        .bind(id)
                        .bind(&code)
                        .bind(balance)
                        .bind(IMPORT_REASON)
                        .execute(dst)
                        .await?;
                    }
                }
            }
        }
    }
    if has_table(src, "user_device").await? {
        let mut rows = sqlx::query("select id, user_id from user_device").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama devices")?;
            let (Some(device), Some(user)) = (text(&r, "id"), uuid(&r, "user_id")) else {
                continue;
            };
            exec(
                sqlx::query(
                    "insert into musubi.identities (id, account_id, provider, subject, verified)
                     select $1, $2, 'device', $3, true where exists (select 1 from musubi.accounts where id = $2)
                     on conflict (provider, subject) do nothing",
                )
                .bind(Uuid::now_v7())
                .bind(user)
                .bind(device),
                dst,
                &mut identities,
            )
            .await?;
        }
    }
    report.push(("accounts", accounts));
    report.push(("identities", identities));
    report.push(("wallets", wallets));

    let mut storage = Count::default();
    if has_table(src, "storage").await? {
        let sql = format!(
            "insert into musubi.storage_objects
               (collection, key, owner_id, value, version, read_permission, write_permission, created_at, updated_at)
             values ($1, $2, $3, $4, $5, $6, $7, coalesce($8, now()), coalesce($9, now())) {}",
            conflict(
                replace,
                "collection, key, owner_id",
                "value = excluded.value, version = excluded.version, read_permission = excluded.read_permission,
                 write_permission = excluded.write_permission, updated_at = excluded.updated_at"
            )
        );
        let mut rows = sqlx::query("select * from storage").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama storage")?;
            let (Some(collection), Some(key)) = (text(&r, "collection"), text(&r, "key")) else {
                continue;
            };
            exec(
                sqlx::query(&sql)
                    .bind(collection)
                    .bind(key)
                    .bind(uuid(&r, "user_id").unwrap_or(Uuid::nil()))
                    .bind(json(&r, "value"))
                    .bind(text(&r, "version").unwrap_or_default())
                    .bind(int(&r, "read").unwrap_or(1) as i16)
                    .bind(int(&r, "write").unwrap_or(1) as i16)
                    .bind(time(&r, "create_time"))
                    .bind(time(&r, "update_time")),
                dst,
                &mut storage,
            )
            .await?;
        }
    }
    report.push(("storage_objects", storage));

    let mut friends = Count::default();
    if has_table(src, "user_edge").await? {
        let mut rows = sqlx::query("select * from user_edge").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama friends")?;
            let (Some(a), Some(b)) = (uuid(&r, "source_id"), uuid(&r, "destination_id")) else {
                continue;
            };
            if a == b || a.is_nil() || b.is_nil() {
                continue;
            }
            exec(
                sqlx::query(
                    "insert into musubi.friends (user_id, friend_id, state, created_at, updated_at)
                     select $1, $2, $3, coalesce($4, now()), coalesce($4, now())
                     where exists (select 1 from musubi.accounts where id = $1)
                       and exists (select 1 from musubi.accounts where id = $2)
                     on conflict (user_id, friend_id) do nothing",
                )
                .bind(a)
                .bind(b)
                .bind(int(&r, "state").unwrap_or(0).clamp(0, 3) as i16)
                .bind(time(&r, "update_time")),
                dst,
                &mut friends,
            )
            .await?;
        }
    }
    report.push(("friends", friends));

    let (mut groups, mut members) = (Count::default(), Count::default());
    if has_table(src, "groups").await? {
        let sql = format!(
            "insert into musubi.groups
               (id, creator_id, name, description, avatar_url, lang, metadata, open, edge_count, max_count, created_at, updated_at)
             values ($1, (select id from musubi.accounts where id = $2), $3, coalesce($4, ''), $5, coalesce($6, 'en'), $7, $8, $9, $10,
                     coalesce($11, now()), coalesce($12, now())) {}",
            conflict(
                replace,
                "id",
                "name = excluded.name, description = excluded.description, avatar_url = excluded.avatar_url,
                 lang = excluded.lang, metadata = excluded.metadata, open = excluded.open,
                 max_count = excluded.max_count, updated_at = excluded.updated_at"
            )
        );
        let mut rows = sqlx::query("select * from groups").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama groups")?;
            let (Some(id), Some(name)) = (uuid(&r, "id"), text(&r, "name")) else {
                continue;
            };
            if time(&r, "disable_time").is_some() {
                groups.kept += 1;
                continue;
            }
            exec(
                sqlx::query(&sql)
                    .bind(id)
                    .bind(uuid(&r, "creator_id").unwrap_or(Uuid::nil()))
                    .bind(&name)
                    .bind(text(&r, "description"))
                    .bind(text(&r, "avatar_url"))
                    .bind(text(&r, "lang_tag"))
                    .bind(json(&r, "metadata"))
                    .bind(int(&r, "state").unwrap_or(0) == 0)
                    .bind(int(&r, "edge_count").unwrap_or(0).max(0) as i32)
                    .bind(int(&r, "max_count").unwrap_or(100).clamp(1, 10_000) as i32)
                    .bind(time(&r, "create_time"))
                    .bind(time(&r, "update_time")),
                dst,
                &mut groups,
            )
            .await
            .with_context(|| format!("importing group {id} ({name})"))?;
        }
        if has_table(src, "group_edge").await? {
            // Nakama keeps both directions; group → user rows carry the membership state
            let mut rows = sqlx::query(
                "select e.source_id, e.destination_id, e.state, e.update_time
                 from group_edge e join groups g on g.id = e.source_id",
            )
            .fetch(src);
            while let Some(r) = rows.next().await {
                let r = r.context("reading Nakama group members")?;
                let (Some(g), Some(u)) = (uuid(&r, "source_id"), uuid(&r, "destination_id")) else {
                    continue;
                };
                exec(
                    sqlx::query(
                        "insert into musubi.group_members (group_id, user_id, state, created_at, updated_at)
                         select $1, $2, $3, coalesce($4, now()), coalesce($4, now())
                         where exists (select 1 from musubi.groups where id = $1)
                           and exists (select 1 from musubi.accounts where id = $2)
                         on conflict (group_id, user_id) do nothing",
                    )
                    .bind(g)
                    .bind(u)
                    .bind(int(&r, "state").unwrap_or(2).clamp(0, 3) as i16)
                    .bind(time(&r, "update_time")),
                    dst,
                    &mut members,
                )
                .await?;
            }
        }
    }
    report.push(("groups", groups));
    report.push(("group_members", members));

    let (mut boards, mut records) = (Count::default(), Count::default());
    if has_table(src, "leaderboard").await? {
        let sql = format!(
            "insert into musubi.leaderboards (id, authoritative, sort_order, operator, reset_schedule, metadata, created_at)
             values ($1, $2, $3, $4, $5, $6, coalesce($7, now())) {}",
            conflict(
                replace,
                "id",
                "authoritative = excluded.authoritative, sort_order = excluded.sort_order, operator = excluded.operator,
                 reset_schedule = excluded.reset_schedule, metadata = excluded.metadata"
            )
        );
        let mut rows = sqlx::query("select * from leaderboard").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama leaderboards")?;
            let Some(id) = text(&r, "id") else { continue };
            exec(
                sqlx::query(&sql)
                    .bind(id)
                    .bind(
                        r.try_get::<Option<bool>, _>("authoritative")
                            .ok()
                            .flatten()
                            .unwrap_or(false),
                    )
                    .bind(int(&r, "sort_order").unwrap_or(1).clamp(0, 1) as i16)
                    .bind(int(&r, "operator").unwrap_or(0).clamp(0, 3) as i16)
                    .bind(text(&r, "reset_schedule").filter(|s| !s.is_empty()))
                    .bind(json(&r, "metadata"))
                    .bind(time(&r, "create_time")),
                dst,
                &mut boards,
            )
            .await?;
        }
        if has_table(src, "leaderboard_record").await? {
            let sql = format!(
                "insert into musubi.leaderboard_records
                   (leaderboard_id, owner_id, username, score, subscore, num_score, metadata, expiry_time, created_at, updated_at)
                 select $1, $2, $3, $4, $5, $6, $7, coalesce($8, 'epoch'::timestamptz), coalesce($9, now()), coalesce($10, now())
                 where exists (select 1 from musubi.leaderboards where id = $1) {}",
                conflict(
                    replace,
                    "leaderboard_id, expiry_time, owner_id",
                    "username = excluded.username, score = excluded.score, subscore = excluded.subscore,
                     num_score = excluded.num_score, metadata = excluded.metadata, updated_at = excluded.updated_at"
                )
            );
            let mut rows = sqlx::query("select * from leaderboard_record").fetch(src);
            while let Some(r) = rows.next().await {
                let r = r.context("reading Nakama leaderboard records")?;
                let (Some(board), Some(owner)) = (text(&r, "leaderboard_id"), uuid(&r, "owner_id"))
                else {
                    continue;
                };
                exec(
                    sqlx::query(&sql)
                        .bind(board)
                        .bind(owner)
                        .bind(text(&r, "username"))
                        .bind(int(&r, "score").unwrap_or(0))
                        .bind(int(&r, "subscore").unwrap_or(0))
                        .bind(int(&r, "num_score").unwrap_or(1) as i32)
                        .bind(json(&r, "metadata"))
                        .bind(time(&r, "expiry_time"))
                        .bind(time(&r, "create_time"))
                        .bind(time(&r, "update_time")),
                    dst,
                    &mut records,
                )
                .await?;
            }
        }
    }
    report.push(("leaderboards", boards));
    report.push(("leaderboard_records", records));

    let mut notifications = Count::default();
    if has_table(src, "notification").await? {
        let mut rows = sqlx::query("select * from notification").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama notifications")?;
            let (Some(id), Some(user)) = (uuid(&r, "id"), uuid(&r, "user_id")) else {
                continue;
            };
            exec(
                sqlx::query(
                    "insert into musubi.notifications (id, user_id, sender_id, subject, content, code, created_at)
                     select $1, $2, (select id from musubi.accounts where id = $3), coalesce($4, ''), $5, $6, coalesce($7, now())
                     where exists (select 1 from musubi.accounts where id = $2)
                     on conflict (id) do nothing",
                )
                .bind(id)
                .bind(user)
                .bind(uuid(&r, "sender_id").unwrap_or(Uuid::nil()))
                .bind(text(&r, "subject"))
                .bind(json(&r, "content"))
                .bind(int(&r, "code").unwrap_or(0) as i32)
                .bind(time(&r, "create_time")),
                dst,
                &mut notifications,
            )
            .await?;
        }
    }
    report.push(("notifications", notifications));

    let mut messages = Count::default();
    if has_table(src, "message").await? {
        let mut rows = sqlx::query("select * from message").fetch(src);
        while let Some(r) = rows.next().await {
            let r = r.context("reading Nakama chat messages")?;
            let Some(id) = uuid(&r, "id") else { continue };
            let mode = int(&r, "stream_mode").unwrap_or(0);
            let subject = uuid(&r, "stream_subject").unwrap_or(Uuid::nil());
            let other = uuid(&r, "stream_descriptor").unwrap_or(Uuid::nil());
            // same encoding as `musubi_chat::Channel::id`
            let channel = match mode {
                2 => match text(&r, "stream_label").filter(|l| !l.is_empty()) {
                    Some(room) => format!("2.{room}"),
                    None => continue,
                },
                3 => format!("3.{}.{}", subject.min(other), subject.max(other)),
                4 => format!("4.{subject}"),
                _ => continue,
            };
            exec(
                sqlx::query(
                    "insert into musubi.chat_messages (id, channel_id, kind, sender_id, username, code, content, created_at, updated_at)
                     values ($1, $2, $3, (select id from musubi.accounts where id = $4), coalesce($5, ''), $6, $7,
                             coalesce($8, now()), coalesce($9, now()))
                     on conflict (id) do nothing",
                )
                .bind(id)
                .bind(channel)
                .bind(mode as i16)
                .bind(uuid(&r, "sender_id").unwrap_or(Uuid::nil()))
                .bind(text(&r, "username"))
                .bind(int(&r, "code").unwrap_or(0) as i32)
                .bind(json(&r, "content"))
                .bind(time(&r, "create_time"))
                .bind(time(&r, "update_time")),
                dst,
                &mut messages,
            )
            .await?;
        }
    }
    report.push(("chat_messages", messages));
    Ok(report)
}
