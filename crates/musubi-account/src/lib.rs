//! Accounts and public user profiles.

use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// Full view of one's own account.
#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: AccountId,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub lang: String,
    pub timezone: Option<String>,
    pub location: Option<String>,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    pub identities: Vec<Identity>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub provider: String,
    /// Subject is masked for secret-bearing providers (email → `a***@host`).
    pub subject: String,
    pub verified: bool,
}

/// What other players see.
#[derive(Debug, Clone, Serialize)]
pub struct User {
    pub id: AccountId,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub lang: String,
    pub location: Option<String>,
    pub metadata: serde_json::Value,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAccount {
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub lang: Option<String>,
    pub timezone: Option<String>,
    pub location: Option<String>,
    /// Replaces metadata wholesale (≤ 16 KB object).
    pub metadata: Option<serde_json::Value>,
}

/// What `AccountService::merge` moved into the target.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MergeReport {
    pub identities: u64,
    pub wallets: u64,
    pub inventory: u64,
    pub storage_moved: u64,
    /// Source objects dropped because the target already had the same collection/key.
    pub storage_kept: u64,
    pub friends: u64,
    pub groups: u64,
}

async fn exec(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sql: &str,
    target: Uuid,
    source: Uuid,
) -> Result<u64> {
    Ok(sqlx::query(sql)
        .bind(target)
        .bind(source)
        .execute(&mut **tx)
        .await
        .map_err(Error::internal)?
        .rows_affected())
}

#[derive(Clone)]
pub struct AccountService {
    db: PgPool,
}

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
    "id, username, display_name, avatar_url, lang, location, metadata, created_at";

impl AccountService {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }

    pub async fn get(&self, id: AccountId) -> Result<Account> {
        // one round-trip: the identities ride along as a JSON array
        let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, String, Option<String>, Option<String>, serde_json::Value, OffsetDateTime, OffsetDateTime, serde_json::Value)>(
            "select a.username, a.display_name, a.avatar_url, a.lang, a.timezone, a.location, a.metadata, a.created_at, a.updated_at,
                    coalesce((select json_agg(json_build_array(i.provider, i.subject, i.verified) order by i.created_at)
                              from musubi.identities i where i.account_id = a.id), '[]'::json)
             from musubi.accounts a where a.id = $1 and a.deleted_at is null",
        )
        .bind(id.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("account".into()))?;
        let identities = serde_json::from_value::<Vec<(String, String, bool)>>(row.9)
            .map_err(Error::internal)?
            .into_iter()
            .map(|(provider, subject, verified)| Identity {
                subject: mask_subject(&provider, &subject),
                provider,
                verified,
            })
            .collect();
        Ok(Account {
            id,
            username: row.0,
            display_name: row.1,
            avatar_url: row.2,
            lang: row.3,
            timezone: row.4,
            location: row.5,
            metadata: row.6,
            created_at: row.7,
            updated_at: row.8,
            identities,
        })
    }

    pub async fn update(&self, id: AccountId, u: UpdateAccount) -> Result<()> {
        if let Some(name) = &u.username {
            validate_username(name)?;
        }
        for (v, max, what) in [
            (&u.display_name, 64, "display_name"),
            (&u.avatar_url, 512, "avatar_url"),
            (&u.lang, 16, "lang"),
            (&u.timezone, 64, "timezone"),
            (&u.location, 128, "location"),
        ] {
            if v.as_ref().is_some_and(|s| s.chars().count() > max) {
                return Err(Error::InvalidArgument(format!(
                    "{what} longer than {max} chars"
                )));
            }
        }
        if let Some(m) = &u.metadata {
            if !m.is_object() || serde_json::to_vec(m)?.len() > 16 * 1024 {
                return Err(Error::InvalidArgument(
                    "metadata must be an object ≤ 16 KB".into(),
                ));
            }
        }
        let res = sqlx::query(
            "update musubi.accounts set
                username = coalesce($2, username), display_name = coalesce($3, display_name),
                avatar_url = coalesce($4, avatar_url), lang = coalesce($5, lang),
                timezone = coalesce($6, timezone), location = coalesce($7, location),
                metadata = coalesce($8, metadata), updated_at = now()
             where id = $1 and deleted_at is null",
        )
        .bind(id.0)
        .bind(&u.username)
        .bind(&u.display_name)
        .bind(&u.avatar_url)
        .bind(&u.lang)
        .bind(&u.timezone)
        .bind(&u.location)
        .bind(&u.metadata)
        .execute(&self.db)
        .await;
        match res {
            Ok(r) if r.rows_affected() == 0 => Err(Error::NotFound("account".into())),
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(Error::AlreadyExists("username taken".into()))
            }
            Err(e) => Err(Error::internal(e)),
        }
    }

    /// Soft-delete: the row stays for ledgers/leaderboards, identities and sessions go.
    pub async fn delete(&self, id: AccountId) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let r = sqlx::query("update musubi.accounts set deleted_at = now(), disabled_at = now(), updated_at = now() where id = $1 and deleted_at is null")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("account".into()));
        }
        sqlx::query("delete from musubi.identities where account_id = $1")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query("update musubi.sessions set revoked_at = now() where account_id = $1 and revoked_at is null").bind(id.0).execute(&mut *tx).await.map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)
    }

    /// Fold `source` into `target` (guest → social login): identities move over (an email stays
    /// behind if the target already has one), wallets are summed (ledger rows `reason = merge`),
    /// inventory, friends, group memberships, recent players and notifications are unioned,
    /// storage objects move unless the target already has the same collection/key (target wins);
    /// leaderboard records and ratings stay with the target. The source is then closed like
    /// `delete`. Everything runs in one transaction and is journaled in `account_merges`.
    pub async fn merge(&self, target: AccountId, source: AccountId) -> Result<MergeReport> {
        if target == source {
            return Err(Error::InvalidArgument(
                "cannot merge an account into itself".into(),
            ));
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let alive: Vec<Uuid> = sqlx::query_scalar(
            "select id from musubi.accounts where id = any($1) and deleted_at is null for update",
        )
        .bind(vec![target.0, source.0])
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if alive.len() != 2 {
            return Err(Error::NotFound("account".into()));
        }
        let t = target.0;
        let s = source.0;
        let identities = exec(&mut tx,
            "update musubi.identities set account_id = $1, updated_at = now()
             where account_id = $2 and not (provider = 'email' and exists
               (select 1 from musubi.identities x where x.account_id = $1 and x.provider = 'email'))", t, s).await?;
        let mut rep = MergeReport {
            identities,
            ..MergeReport::default()
        };

        let wallets: Vec<(String, i64)> = sqlx::query_as(
            "select currency, balance from musubi.wallets where account_id = $1 and balance <> 0",
        )
        .bind(s)
        .fetch_all(&mut *tx)
        .await
        .map_err(Error::internal)?;
        for (currency, delta) in wallets {
            let after: i64 = sqlx::query_scalar(
                "insert into musubi.wallets (account_id, currency, balance) values ($1, $2, $3)
                 on conflict (account_id, currency) do update
                   set balance = musubi.wallets.balance + excluded.balance, updated_at = now()
                 returning balance",
            )
            .bind(t)
            .bind(&currency)
            .bind(delta)
            .fetch_one(&mut *tx)
            .await
            .map_err(Error::internal)?;
            sqlx::query(
                "insert into musubi.wallet_ledger (id, account_id, currency, delta, balance_after, reason, ref)
                 values ($1, $2, $3, $4, $5, 'merge', $6)",
            )
            .bind(Uuid::now_v7())
            .bind(t)
            .bind(&currency)
            .bind(delta)
            .bind(after)
            .bind(s.to_string())
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
            rep.wallets += 1;
        }
        exec(
            &mut tx,
            "delete from musubi.wallets where account_id = $2 and $1 = $1",
            t,
            s,
        )
        .await?;

        rep.inventory = exec(
            &mut tx,
            "update musubi.inventory set account_id = $1, updated_at = now() where account_id = $2",
            t,
            s,
        )
        .await?;

        rep.storage_moved = exec(&mut tx,
            "update musubi.storage_objects o set owner_id = $1, updated_at = now()
             where o.owner_id = $2 and not exists
               (select 1 from musubi.storage_objects x where x.owner_id = $1 and x.collection = o.collection and x.key = o.key)", t, s).await?;
        rep.storage_kept = exec(
            &mut tx,
            "delete from musubi.storage_objects where owner_id = $2 and $1 = $1",
            t,
            s,
        )
        .await?;

        rep.friends = exec(
            &mut tx,
            "update musubi.friends f set user_id = $1, updated_at = now()
             where f.user_id = $2 and f.friend_id <> $1 and not exists
               (select 1 from musubi.friends x where x.user_id = $1 and x.friend_id = f.friend_id)",
            t,
            s,
        )
        .await?;
        exec(
            &mut tx,
            "update musubi.friends f set friend_id = $1, updated_at = now()
             where f.friend_id = $2 and f.user_id <> $1 and not exists
               (select 1 from musubi.friends x where x.user_id = f.user_id and x.friend_id = $1)",
            t,
            s,
        )
        .await?;
        exec(
            &mut tx,
            "delete from musubi.friends where (user_id = $2 or friend_id = $2) and $1 = $1",
            t,
            s,
        )
        .await?;

        exec(
            &mut tx,
            "update musubi.groups set creator_id = $1 where creator_id = $2",
            t,
            s,
        )
        .await?;
        rep.groups = exec(&mut tx,
            "update musubi.group_members m set user_id = $1, updated_at = now()
             where m.user_id = $2 and not exists
               (select 1 from musubi.group_members x where x.group_id = m.group_id and x.user_id = $1)", t, s).await?;
        exec(&mut tx,
            "update musubi.groups g set edge_count = edge_count - 1
             where g.id in (select group_id from musubi.group_members where user_id = $2 and state < 3) and $1 = $1", t, s).await?;
        exec(
            &mut tx,
            "delete from musubi.group_members where user_id = $2 and $1 = $1",
            t,
            s,
        )
        .await?;

        exec(&mut tx,
            "update musubi.recent_players r set user_id = $1 where r.user_id = $2 and r.other_id <> $1 and not exists
               (select 1 from musubi.recent_players x where x.user_id = $1 and x.other_id = r.other_id)", t, s).await?;
        exec(&mut tx,
            "update musubi.recent_players r set other_id = $1 where r.other_id = $2 and r.user_id <> $1 and not exists
               (select 1 from musubi.recent_players x where x.user_id = r.user_id and x.other_id = $1)", t, s).await?;
        exec(
            &mut tx,
            "delete from musubi.recent_players where (user_id = $2 or other_id = $2) and $1 = $1",
            t,
            s,
        )
        .await?;
        exec(
            &mut tx,
            "update musubi.notifications set user_id = $1 where user_id = $2",
            t,
            s,
        )
        .await?;

        exec(&mut tx,
            "update musubi.accounts set deleted_at = now(), disabled_at = now(), updated_at = now() where id = $2 and $1 = $1", t, s).await?;
        exec(
            &mut tx,
            "delete from musubi.identities where account_id = $2 and $1 = $1",
            t,
            s,
        )
        .await?;
        exec(&mut tx,
            "update musubi.sessions set revoked_at = now() where account_id = $2 and revoked_at is null and $1 = $1", t, s).await?;
        sqlx::query("insert into musubi.account_merges (id, target_id, source_id, report) values ($1, $2, $3, $4)")
            .bind(Uuid::now_v7())
            .bind(t)
            .bind(s)
            .bind(serde_json::to_value(&rep)?)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        Ok(rep)
    }

    pub async fn get_users(&self, ids: &[AccountId], usernames: &[String]) -> Result<Vec<User>> {
        if ids.len() + usernames.len() > 100 {
            return Err(Error::InvalidArgument(
                "at most 100 users per request".into(),
            ));
        }
        let ids: Vec<Uuid> = ids.iter().map(|i| i.0).collect();
        let names: Vec<String> = usernames.iter().map(|n| n.to_lowercase()).collect();
        let rows = sqlx::query_as::<_, UserRow>(&format!(
            "select {USER_COLS} from musubi.accounts where deleted_at is null and (id = any($1) or lower(username) = any($2))"
        ))
        .bind(&ids)
        .bind(&names)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(to_user).collect())
    }

    /// Users whose username starts with `prefix` (case-insensitive), shortest/alphabetical first.
    pub async fn search_users(&self, prefix: &str, limit: usize) -> Result<Vec<User>> {
        let prefix = prefix.trim().to_lowercase();
        if prefix.is_empty() || prefix.chars().count() > 32 {
            return Err(Error::InvalidArgument("q must be 1..=32 chars".into()));
        }
        let pattern = format!(
            "{}%",
            prefix
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let rows = sqlx::query_as::<_, UserRow>(&format!(
            "select {USER_COLS} from musubi.accounts
             where deleted_at is null and lower(username) like $1
             order by length(username), lower(username) limit $2"
        ))
        .bind(pattern)
        .bind(limit.clamp(1, 50) as i64)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows.into_iter().map(to_user).collect())
    }
}

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

pub fn validate_username(u: &str) -> Result<()> {
    let ok = (3..=32).contains(&u.chars().count())
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if !ok {
        return Err(Error::InvalidArgument(
            "username must be 3..=32 chars of [A-Za-z0-9_.-]".into(),
        ));
    }
    Ok(())
}

fn mask_subject(provider: &str, subject: &str) -> String {
    match provider {
        "email" => match subject.split_once('@') {
            Some((local, host)) => format!("{}***@{host}", local.chars().next().unwrap_or('*')),
            None => "***".into(),
        },
        "device" => subject.to_string(),
        _ => format!("{}…", subject.chars().take(6).collect::<String>()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_email() {
        assert_eq!(
            mask_subject("email", "alice@example.com"),
            "a***@example.com"
        );
        assert_eq!(mask_subject("steam", "76561198000000000"), "765611…");
    }
}

/// Every table that stores rows about an account, with the column that names it. Used by the
/// admin export and the GDPR purge; `identities` and `sessions` cascade from `accounts`.
pub const ACCOUNT_TABLES: &[(&str, &str)] = &[
    ("identities", "account_id"),
    ("sessions", "account_id"),
    ("account_limits", "account_id"),
    ("storage_objects", "owner_id"),
    ("friends", "user_id"),
    ("group_members", "user_id"),
    ("notifications", "user_id"),
    ("chat_messages", "sender_id"),
    ("leaderboard_records", "owner_id"),
    ("wallets", "account_id"),
    ("wallet_ledger", "account_id"),
    ("inventory", "account_id"),
    ("purchases", "account_id"),
    ("iap_receipts", "account_id"),
    ("iap_orders", "account_id"),
    ("reward_rolls", "account_id"),
    ("reward_pity", "account_id"),
    ("progress", "account_id"),
    ("energy_state", "account_id"),
    ("streak_state", "account_id"),
    ("mail", "account_id"),
    ("achievement_progress", "account_id"),
    ("quest_progress", "account_id"),
    ("season_state", "account_id"),
    ("market_listings", "seller_id"),
    ("market_sales", "seller_id"),
    ("market_sales", "buyer_id"),
    ("trades", "from_id"),
    ("trades", "to_id"),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    pub id: AccountId,
    pub username: String,
    pub display_name: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub disabled_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub deleted_at: Option<OffsetDateTime>,
}

impl AccountService {
    /// Admin search: `q` matches a username prefix (case-insensitive) or an exact id.
    pub async fn search(
        &self,
        q: Option<&str>,
        include_deleted: bool,
        limit: i64,
        cursor: Option<Uuid>,
    ) -> Result<Vec<AccountSummary>> {
        self.search_by(q, None, include_deleted, limit, cursor)
            .await
    }

    /// `search` narrowed to accounts holding a linked identity `provider:subject` (device id,
    /// e-mail, Steam id, `oidc:<name>` subject, passkey credential id …) when `identity` is set.
    /// Accounts holding any of `subjects` under `provider` (platform friends import): live
    /// accounts only, in the order of `subjects`, unknown subjects skipped.
    pub async fn by_identities(
        &self,
        provider: &str,
        subjects: &[String],
    ) -> Result<Vec<(String, AccountSummary)>> {
        if provider.is_empty() || provider.len() > 64 {
            return Err(Error::InvalidArgument("provider".into()));
        }
        if subjects.len() > 500 {
            return Err(Error::InvalidArgument("at most 500 subjects".into()));
        }
        type Row = (
            String,
            Uuid,
            String,
            Option<String>,
            OffsetDateTime,
            Option<OffsetDateTime>,
            Option<OffsetDateTime>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select i.subject, a.id, a.username, a.display_name, a.created_at, a.disabled_at, a.deleted_at
             from musubi.identities i
             join musubi.accounts a on a.id = i.account_id
             where i.provider = $1 and i.subject = any($2) and a.deleted_at is null",
        )
        .bind(provider)
        .bind(subjects)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut by_subject: std::collections::HashMap<String, AccountSummary> = rows
            .into_iter()
            .map(
                |(s, id, username, display_name, created_at, disabled_at, deleted_at)| {
                    (
                        s,
                        AccountSummary {
                            id: AccountId(id),
                            username,
                            display_name,
                            created_at,
                            disabled_at,
                            deleted_at,
                        },
                    )
                },
            )
            .collect();
        Ok(subjects
            .iter()
            .filter_map(|s| by_subject.remove(s).map(|a| (s.clone(), a)))
            .collect())
    }

    pub async fn search_by(
        &self,
        q: Option<&str>,
        identity: Option<&str>,
        include_deleted: bool,
        limit: i64,
        cursor: Option<Uuid>,
    ) -> Result<Vec<AccountSummary>> {
        let limit = limit.clamp(1, 200);
        let q = q.unwrap_or("").trim();
        let (provider, subject) = match identity.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => {
                let (p, sub) = s.rsplit_once(':').ok_or_else(|| {
                    Error::InvalidArgument("identity must be provider:subject".into())
                })?;
                (Some(p.to_string()), Some(sub.to_string()))
            }
            None => (None, None),
        };
        let by_id = Uuid::parse_str(q).ok();
        let pattern = format!(
            "{}%",
            q.to_lowercase().replace('%', "\\%").replace('_', "\\_")
        );
        let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, OffsetDateTime, Option<OffsetDateTime>, Option<OffsetDateTime>)>(
            "select id, username, display_name, created_at, disabled_at, deleted_at from musubi.accounts a
             where ($1 = '' or lower(username) like $2 or id = $3)
               and ($4 or deleted_at is null)
               and ($5::uuid is null or id > $5)
               and ($7::text is null or exists (select 1 from musubi.identities i
                      where i.account_id = a.id and i.provider = $7 and i.subject = $8))
             order by id limit $6",
        )
        .bind(q)
        .bind(&pattern)
        .bind(by_id)
        .bind(include_deleted)
        .bind(cursor)
        .bind(limit)
        .bind(provider)
        .bind(subject)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(
                |(id, username, display_name, created_at, disabled_at, deleted_at)| {
                    AccountSummary {
                        id: AccountId(id),
                        username,
                        display_name,
                        created_at,
                        disabled_at,
                        deleted_at,
                    }
                },
            )
            .collect())
    }

    pub async fn status(&self, id: AccountId) -> Result<AccountSummary> {
        self.search(Some(&id.0.to_string()), true, 1, None)
            .await?
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| Error::NotFound("account".into()))
    }

    /// Ban (`disabled = true`) or unban. Banning revokes every refresh session so the player
    /// cannot mint new access tokens; live sockets are kicked by the caller.
    pub async fn set_disabled(&self, id: AccountId, disabled: bool) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let r = if disabled {
            sqlx::query("update musubi.accounts set disabled_at = coalesce(disabled_at, now()), updated_at = now() where id = $1 and deleted_at is null")
        } else {
            sqlx::query("update musubi.accounts set disabled_at = null, updated_at = now() where id = $1 and deleted_at is null")
        }
        .bind(id.0)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("account".into()));
        }
        if disabled {
            sqlx::query("update musubi.sessions set revoked_at = now() where account_id = $1 and revoked_at is null")
                .bind(id.0)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
        }
        tx.commit().await.map_err(Error::internal)
    }

    /// Everything stored about the account, table by table (secrets stripped). GDPR access
    /// request / support tooling.
    pub async fn export(&self, id: AccountId) -> Result<serde_json::Value> {
        let account: Option<serde_json::Value> =
            sqlx::query_scalar("select to_jsonb(a) from musubi.accounts a where id = $1")
                .bind(id.0)
                .fetch_optional(&self.db)
                .await
                .map_err(Error::internal)?;
        let Some(account) = account else {
            return Err(Error::NotFound("account".into()));
        };
        let mut tables = serde_json::Map::new();
        for (table, col) in ACCOUNT_TABLES {
            let rows: serde_json::Value = sqlx::query_scalar(&format!(
                "select coalesce(jsonb_agg(to_jsonb(t) - 'secret_hash' - 'refresh_hash'), '[]'::jsonb) from musubi.{table} t where t.{col} = $1"
            ))
            .bind(id.0)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
            let entry = tables
                .entry(table.to_string())
                .or_insert_with(|| serde_json::Value::Array(vec![]));
            if let (Some(dst), Some(src)) = (entry.as_array_mut(), rows.as_array()) {
                dst.extend(src.iter().cloned());
            }
        }
        Ok(serde_json::json!({ "account": account, "tables": tables }))
    }

    /// Point-in-time restore from an `export` bundle (support tooling: "roll my save back",
    /// undo a bad purge). Tables are restored row by row through `jsonb_populate_record`, so
    /// the bundle format is the export format; `replace` first clears the account's rows of each
    /// restored table, `merge` keeps existing rows and adds the missing ones (conflicts on keys
    /// leave the live row). `sessions` are never restored (their secrets are stripped on
    /// export); unknown tables are skipped and reported. The account row itself is updated from
    /// `account` (every column but `id`/`created_at`; with an explicit `tables` list only when
    /// it names `account`) or recreated when it is gone and `create_missing` is set. One
    /// transaction: a failing row rolls everything back.
    pub async fn restore(
        &self,
        id: AccountId,
        bundle: &serde_json::Value,
        tables: Option<&[String]>,
        replace: bool,
        create_missing: bool,
    ) -> Result<serde_json::Value> {
        let Some(bundle_tables) = bundle.get("tables").and_then(|t| t.as_object()) else {
            return Err(Error::InvalidArgument(
                "bundle has no `tables` (use the output of the account export)".into(),
            ));
        };
        if let Some(acc) = bundle.get("account").filter(|a| !a.is_null()) {
            if acc.get("id").and_then(|v| v.as_str()) != Some(&id.0.to_string()) {
                return Err(Error::InvalidArgument(
                    "bundle `account.id` does not match the account being restored".into(),
                ));
            }
        }
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from musubi.accounts where id = $1)")
                .bind(id.0)
                .fetch_one(&mut *tx)
                .await
                .map_err(Error::internal)?;
        let mut out = serde_json::Map::new();
        let mut account_action = "unchanged";
        // with an explicit table list the account row is restored only when "account" is listed
        let account_wanted = tables.is_none_or(|ts| ts.iter().any(|t| t == "account"));
        let account = bundle
            .get("account")
            .filter(|a| a.is_object() && account_wanted);
        match (exists, account) {
            (true, Some(acc)) => {
                let cols: Vec<String> = sqlx::query_scalar(
                    "select column_name::text from information_schema.columns where table_schema = 'musubi' and table_name = 'accounts' and column_name not in ('id', 'created_at') order by ordinal_position",
                )
                .fetch_all(&mut *tx)
                .await
                .map_err(Error::internal)?;
                let sets: Vec<String> = cols
                    .iter()
                    .filter(|c| acc.get(c.as_str()).is_some())
                    .map(|c| format!("\"{c}\" = r.\"{c}\""))
                    .collect();
                if !sets.is_empty() {
                    sqlx::query(&format!(
                        "update musubi.accounts a set {} from jsonb_populate_record(null::musubi.accounts, $1) r where a.id = $2",
                        sets.join(", ")
                    ))
                    .bind(acc)
                    .bind(id.0)
                    .execute(&mut *tx)
                    .await
                    .map_err(Error::internal)?;
                    account_action = "updated";
                }
            }
            (false, Some(acc)) if create_missing => {
                sqlx::query(
                    "insert into musubi.accounts select * from jsonb_populate_record(null::musubi.accounts, $1)",
                )
                .bind(acc)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
                account_action = "created";
            }
            (false, _) => return Err(Error::NotFound("account".into())),
            (true, None) => {}
        }
        let mut skipped: Vec<String> = Vec::new();
        let wanted = |t: &str| tables.is_none_or(|ts| ts.iter().any(|x| x == t));
        for name in bundle_tables.keys() {
            let known = ACCOUNT_TABLES.iter().any(|(t, _)| t == name);
            if !known || name == "sessions" || !wanted(name) {
                if wanted(name) {
                    skipped.push(name.clone());
                }
                continue;
            }
        }
        let mut done: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (table, col) in ACCOUNT_TABLES {
            if *table == "sessions" || !wanted(table) || !done.insert(table) {
                continue;
            }
            let Some(rows) = bundle_tables.get(*table).and_then(|r| r.as_array()) else {
                continue;
            };
            let mut deleted = 0u64;
            if replace {
                // every column that names the account (market_sales: seller_id and buyer_id)
                for (t, c) in ACCOUNT_TABLES.iter().filter(|(t, _)| t == table) {
                    let _ = c;
                    let r = sqlx::query(&format!("delete from musubi.{t} where {} = $1", c))
                        .bind(id.0)
                        .execute(&mut *tx)
                        .await
                        .map_err(Error::internal)?;
                    deleted += r.rows_affected();
                }
            }
            let _ = col;
            let mut inserted = 0u64;
            for row in rows {
                let r = sqlx::query(&format!(
                    "insert into musubi.{table} select * from jsonb_populate_record(null::musubi.{table}, $1) on conflict do nothing"
                ))
                .bind(row)
                .execute(&mut *tx)
                .await
                .map_err(|e| Error::InvalidArgument(format!("{table}: {e}")))?;
                inserted += r.rows_affected();
            }
            out.insert(
                table.to_string(),
                serde_json::json!({ "rows": rows.len(), "inserted": inserted, "deleted": deleted }),
            );
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(serde_json::json!({
            "account": account_action,
            "mode": if replace { "replace" } else { "merge" },
            "tables": out,
            "skipped": skipped,
        }))
    }

    /// Hard delete: the account row and every row in `ACCOUNT_TABLES` (GDPR erasure). Rows that
    /// only reference the account as a counter-party (sales, trades) go too.
    pub async fn purge(&self, id: AccountId) -> Result<u64> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let mut removed = 0u64;
        for (table, col) in ACCOUNT_TABLES {
            let r = sqlx::query(&format!("delete from musubi.{table} where {col} = $1"))
                .bind(id.0)
                .execute(&mut *tx)
                .await
                .map_err(Error::internal)?;
            removed += r.rows_affected();
        }
        let r = sqlx::query("delete from musubi.accounts where id = $1")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Err(Error::NotFound("account".into()));
        }
        tx.commit().await.map_err(Error::internal)?;
        Ok(removed + 1)
    }
}
