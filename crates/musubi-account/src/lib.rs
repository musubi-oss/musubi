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
        let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, String, Option<String>, Option<String>, serde_json::Value, OffsetDateTime, OffsetDateTime)>(
            "select username, display_name, avatar_url, lang, timezone, location, metadata, created_at, updated_at from musubi.accounts where id = $1 and deleted_at is null",
        )
        .bind(id.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .ok_or_else(|| Error::NotFound("account".into()))?;
        let identities = sqlx::query_as::<_, (String, String, bool)>(
            "select provider, subject, verified from musubi.identities where account_id = $1 order by created_at",
        )
        .bind(id.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?
        .into_iter()
        .map(|(provider, subject, verified)| Identity { subject: mask_subject(&provider, &subject), provider, verified })
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
