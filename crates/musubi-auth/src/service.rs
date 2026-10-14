use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use musubi_core::config::SessionConfig;
use musubi_core::{AccountId, Error, IdentityId, Result, SessionId};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::token::{Claims, TokenSigner};
use crate::{password, provider, AuthOptions};

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub account_id: AccountId,
    pub session_id: SessionId,
    pub access_token: String,
    pub refresh_token: String,
    pub created: bool,
}

/// Result of verifying a bearer token on a request.
#[derive(Debug, Clone)]
pub struct Authenticated {
    pub account_id: AccountId,
    pub session_id: SessionId,
    pub vars: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone)]
pub struct AuthService {
    db: PgPool,
    signer: TokenSigner,
    refresh_ttl_secs: u64,
}

struct Device<'a> {
    device_id: Option<&'a str>,
    user_agent: Option<&'a str>,
}

impl AuthService {
    pub fn new(db: PgPool, cfg: &SessionConfig) -> Result<Self> {
        Ok(Self {
            db,
            signer: TokenSigner::from_config(cfg)?,
            refresh_ttl_secs: cfg.refresh_ttl_secs,
        })
    }

    pub fn signer(&self) -> &TokenSigner {
        &self.signer
    }

    pub async fn authenticate_device(&self, device_id: &str, opts: AuthOptions) -> Result<Session> {
        if device_id.len() < 10 || device_id.len() > 128 {
            return Err(Error::InvalidArgument(
                "device id must be 10..=128 chars".into(),
            ));
        }
        let (account_id, created) = self
            .find_or_create(provider::DEVICE, device_id, None, &opts)
            .await?;
        self.open_session(
            account_id,
            created,
            opts.vars,
            Device {
                device_id: Some(device_id),
                user_agent: None,
            },
        )
        .await
    }

    pub async fn authenticate_email(
        &self,
        email: &str,
        pass: &str,
        opts: AuthOptions,
    ) -> Result<Session> {
        let email = normalize_email(email)?;
        password::validate(pass)?;
        let existing = sqlx::query_as::<_, (Uuid, Option<String>)>(
            "select account_id, secret_hash from musubi.identities where provider = $1 and subject = $2",
        )
        .bind(provider::EMAIL)
        .bind(&email)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;

        let (account_id, created) = match existing {
            Some((acc, Some(hash))) => {
                if !password::verify(pass.to_owned(), hash).await? {
                    return Err(Error::Unauthenticated("invalid credentials".into()));
                }
                (AccountId(acc), false)
            }
            Some((_, None)) => return Err(Error::Unauthenticated("invalid credentials".into())),
            None => {
                if !opts.create {
                    return Err(Error::NotFound("account not found".into()));
                }
                let hash = password::hash(pass.to_owned()).await?;
                self.find_or_create(provider::EMAIL, &email, Some(&hash), &opts)
                    .await?
            }
        };
        self.open_session(
            account_id,
            created,
            opts.vars,
            Device {
                device_id: None,
                user_agent: None,
            },
        )
        .await
    }

    /// Verify an access token. Stateless: no DB hit on the hot path.
    pub fn verify_access(&self, token: &str) -> Result<Authenticated> {
        let c = self.signer.verify(token)?;
        Ok(Authenticated {
            account_id: c.sub,
            session_id: c.sid,
            vars: c.vars,
        })
    }

    /// Rotate a refresh token. Reusing an already-rotated token revokes the entire family.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Session> {
        let hash = hash_refresh(refresh_token);
        let row = sqlx::query_as::<_, (Uuid, Uuid, Uuid, Option<OffsetDateTime>, OffsetDateTime, serde_json::Value, Option<String>, Option<String>)>(
            "select id, account_id, family, revoked_at, expires_at, vars, device_id, user_agent from musubi.sessions where refresh_hash = $1",
        )
        .bind(&hash)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;

        let Some((id, account_id, family, revoked_at, expires_at, vars, device_id, user_agent)) =
            row
        else {
            return Err(Error::Unauthenticated("unknown refresh token".into()));
        };
        if revoked_at.is_some() {
            sqlx::query("update musubi.sessions set revoked_at = now() where family = $1 and revoked_at is null")
                .bind(family)
                .execute(&self.db)
                .await
                .map_err(Error::internal)?;
            tracing::warn!(%account_id, %family, "refresh token reuse detected; family revoked");
            return Err(Error::Unauthenticated("refresh token reused".into()));
        }
        if expires_at < OffsetDateTime::now_utc() {
            return Err(Error::Unauthenticated("refresh token expired".into()));
        }
        sqlx::query("update musubi.sessions set revoked_at = now() where id = $1")
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        let vars = vars.as_object().cloned().unwrap_or_default();
        self.issue(
            AccountId(account_id),
            Some(family),
            false,
            vars,
            Device {
                device_id: device_id.as_deref(),
                user_agent: user_agent.as_deref(),
            },
        )
        .await
    }

    pub async fn logout(&self, session_id: SessionId, all_devices: bool) -> Result<()> {
        let q = if all_devices {
            "update musubi.sessions set revoked_at = now() where account_id = (select account_id from musubi.sessions where id = $1) and revoked_at is null"
        } else {
            "update musubi.sessions set revoked_at = now() where id = $1 and revoked_at is null"
        };
        sqlx::query(q)
            .bind(session_id.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    pub async fn link(
        &self,
        account_id: AccountId,
        provider: &str,
        subject: &str,
        secret_hash: Option<&str>,
    ) -> Result<IdentityId> {
        let id = IdentityId::new();
        let res = sqlx::query(
            "insert into musubi.identities (id, account_id, provider, subject, secret_hash) values ($1, $2, $3, $4, $5)",
        )
        .bind(id.0)
        .bind(account_id.0)
        .bind(provider)
        .bind(subject)
        .bind(secret_hash)
        .execute(&self.db)
        .await;
        match res {
            Ok(_) => Ok(id),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(Error::AlreadyExists(
                "identity already linked to an account".into(),
            )),
            Err(e) => Err(Error::internal(e)),
        }
    }

    pub async fn unlink(&self, account_id: AccountId, provider: &str, subject: &str) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let count: i64 = sqlx::query_scalar(
            "select count(*) from musubi.identities where account_id = $1 for update",
        )
        .bind(account_id.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(Error::internal)?;
        if count <= 1 {
            return Err(Error::Conflict("cannot unlink the last identity".into()));
        }
        let done = sqlx::query("delete from musubi.identities where account_id = $1 and provider = $2 and subject = $3")
            .bind(account_id.0)
            .bind(provider)
            .bind(subject)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        if done.rows_affected() == 0 {
            return Err(Error::NotFound("identity not found".into()));
        }
        tx.commit().await.map_err(Error::internal)
    }

    async fn find_or_create(
        &self,
        provider: &str,
        subject: &str,
        secret_hash: Option<&str>,
        opts: &AuthOptions,
    ) -> Result<(AccountId, bool)> {
        if let Some(acc) = sqlx::query_scalar::<_, Uuid>(
            "select account_id from musubi.identities where provider = $1 and subject = $2",
        )
        .bind(provider)
        .bind(subject)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        {
            return Ok((AccountId(acc), false));
        }
        if !opts.create {
            return Err(Error::NotFound("account not found".into()));
        }
        let account_id = AccountId::new();
        let username = match &opts.username {
            Some(u) => validate_username(u)?.to_owned(),
            None => generate_username(),
        };
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let ins = sqlx::query("insert into musubi.accounts (id, username) values ($1, $2)")
            .bind(account_id.0)
            .bind(&username)
            .execute(&mut *tx)
            .await;
        if let Err(sqlx::Error::Database(e)) = &ins {
            if e.is_unique_violation() {
                return Err(Error::AlreadyExists("username taken".into()));
            }
        }
        ins.map_err(Error::internal)?;
        let ins = sqlx::query(
            "insert into musubi.identities (id, account_id, provider, subject, secret_hash, verified) values ($1, $2, $3, $4, $5, $6)",
        )
        .bind(IdentityId::new().0)
        .bind(account_id.0)
        .bind(provider)
        .bind(subject)
        .bind(secret_hash)
        .bind(provider == provider::DEVICE)
        .execute(&mut *tx)
        .await;
        match ins {
            // Lost a race with a concurrent first login for the same identity: use theirs.
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                tx.rollback().await.map_err(Error::internal)?;
                let acc = sqlx::query_scalar::<_, Uuid>(
                    "select account_id from musubi.identities where provider = $1 and subject = $2",
                )
                .bind(provider)
                .bind(subject)
                .fetch_one(&self.db)
                .await
                .map_err(Error::internal)?;
                Ok((AccountId(acc), false))
            }
            Err(e) => Err(Error::internal(e)),
            Ok(_) => {
                tx.commit().await.map_err(Error::internal)?;
                Ok((account_id, true))
            }
        }
    }

    async fn open_session(
        &self,
        account_id: AccountId,
        created: bool,
        vars: serde_json::Map<String, serde_json::Value>,
        dev: Device<'_>,
    ) -> Result<Session> {
        if serde_json::to_vec(&vars).map(|v| v.len()).unwrap_or(0) > 4096 {
            return Err(Error::InvalidArgument("session vars exceed 4 KB".into()));
        }
        self.issue(account_id, None, created, vars, dev).await
    }

    async fn issue(
        &self,
        account_id: AccountId,
        family: Option<Uuid>,
        created: bool,
        vars: serde_json::Map<String, serde_json::Value>,
        dev: Device<'_>,
    ) -> Result<Session> {
        let session_id = SessionId::new();
        let family = family.unwrap_or_else(Uuid::now_v7);
        let refresh = new_refresh_token();
        let now = OffsetDateTime::now_utc();
        let expires = now + time::Duration::seconds(self.refresh_ttl_secs as i64);
        sqlx::query(
            "insert into musubi.sessions (id, account_id, family, refresh_hash, device_id, user_agent, vars, expires_at) values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(session_id.0)
        .bind(account_id.0)
        .bind(family)
        .bind(hash_refresh(&refresh))
        .bind(dev.device_id)
        .bind(dev.user_agent)
        .bind(serde_json::Value::Object(vars.clone()))
        .bind(expires)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        let claims = Claims {
            sub: account_id,
            sid: session_id,
            iat: now.unix_timestamp(),
            exp: now.unix_timestamp() + self.signer.access_ttl_secs as i64,
            vars,
        };
        Ok(Session {
            account_id,
            session_id,
            access_token: self.signer.sign(&claims)?,
            refresh_token: refresh,
            created,
        })
    }
}

fn new_refresh_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

fn hash_refresh(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

fn normalize_email(e: &str) -> Result<String> {
    let e = e.trim().to_ascii_lowercase();
    let ok = e.len() >= 3
        && e.len() <= 254
        && e.contains('@')
        && !e.starts_with('@')
        && !e.ends_with('@');
    if !ok {
        return Err(Error::InvalidArgument("invalid email".into()));
    }
    Ok(e)
}

fn validate_username(u: &str) -> Result<&str> {
    let ok = (3..=32).contains(&u.chars().count())
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if !ok {
        return Err(Error::InvalidArgument(
            "username must be 3..=32 chars of [A-Za-z0-9_.-]".into(),
        ));
    }
    Ok(u)
}

fn generate_username() -> String {
    let mut b = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut b);
    format!(
        "player_{}",
        URL_SAFE_NO_PAD
            .encode(b)
            .to_ascii_lowercase()
            .replace(['-', '_'], "x")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames() {
        assert!(validate_username("ab").is_err());
        assert!(validate_username("good_name.1-x").is_ok());
        assert!(validate_username("bad name").is_err());
        let g = generate_username();
        assert!(validate_username(&g).is_ok(), "{g}");
    }

    #[test]
    fn emails() {
        assert_eq!(normalize_email(" A@B.co ").unwrap(), "a@b.co");
        assert!(normalize_email("nope").is_err());
    }
}
