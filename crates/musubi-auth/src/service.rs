use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use musubi_core::config::SessionConfig;
use musubi_core::{AccountId, Error, IdentityId, Result, SessionId};
use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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

type SessionRow = (
    Uuid,
    Uuid,
    Option<String>,
    Option<String>,
    Option<String>,
    OffsetDateTime,
    OffsetDateTime,
    OffsetDateTime,
);

/// Outcome of a password login: a session, or a second-factor challenge.
#[derive(Debug, Clone)]
pub enum Login {
    Session(Session),
    /// The account has TOTP enabled: finish with `AuthService::mfa_complete`.
    MfaRequired {
        account_id: AccountId,
        mfa_token: String,
    },
}

/// How long a pending second-factor token stays valid.
pub const MFA_TOKEN_SECS: i64 = 300;
pub const RECOVERY_CODES: usize = 8;

/// One login/device of an account as seen in `GET /v1/account/sessions`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub family: Uuid,
    pub device_id: Option<String>,
    pub user_agent: Option<String>,
    pub ip: Option<String>,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub expires_at: i64,
}

/// Result of verifying a bearer token on a request.
#[derive(Debug, Clone)]
pub struct Authenticated {
    pub account_id: AccountId,
    pub session_id: SessionId,
    pub vars: serde_json::Map<String, serde_json::Value>,
    /// From the token when it carries one; older tokens fall back to an account read.
    pub username: Option<String>,
}

#[derive(Clone)]
pub struct AuthService {
    db: PgPool,
    signer: TokenSigner,
    refresh_ttl_secs: u64,
    max_sessions: u32,
    lockout: Lockout,
    policy: password::Policy,
    captcha: Arc<std::sync::RwLock<Option<CaptchaVerifier>>>,
    queue: Arc<std::sync::RwLock<Option<LoginQueue>>>,
}

/// Login queue (plan 5.1): once `online()` reports `capacity` players, further logins get a
/// ticket and wait in arrival order; `admit_per_sec` paces the drain when room frees up.
#[derive(Clone)]
pub struct LoginQueue {
    pub capacity: usize,
    pub admit_per_sec: u32,
    pub ttl_secs: u64,
    /// Accounts with a live socket anywhere in the cluster.
    pub online: Arc<dyn Fn() -> usize + Send + Sync>,
}

/// How long a queue ticket stays valid regardless of polling.
pub const QUEUE_TICKET_SECS: i64 = 3600;
/// An admitted ticket counts against the capacity for this long, so the newly admitted player's
/// socket has time to show up in `online()` before the slot is handed to someone else.
const ADMITTED_HOLD_SECS: i64 = 10;

#[derive(Debug, Clone, Serialize)]
pub struct QueueStats {
    pub capacity: usize,
    pub online: usize,
    pub waiting: i64,
    pub admitted_recently: i64,
    pub admit_per_sec: u32,
}

#[derive(Clone, Copy)]
struct Lockout {
    threshold: u32,
    base_secs: u64,
    max_secs: u64,
    captcha_after: u32,
}

/// Captcha backend for the brute-force gate. `configured` says whether anything can verify a
/// token right now (module hook loaded or provider set); when it is false the gate is skipped and
/// only the lockout applies. `verify(token, client_ip)` → `Ok(true)` lets the login proceed.
pub type CaptchaFuture = Pin<Box<dyn Future<Output = Result<bool>> + Send>>;
pub type CaptchaVerifyFn = Arc<dyn Fn(String, Option<String>) -> CaptchaFuture + Send + Sync>;

#[derive(Clone)]
pub struct CaptchaVerifier {
    pub configured: Arc<dyn Fn() -> bool + Send + Sync>,
    pub verify: CaptchaVerifyFn,
}

/// Current lock on an email address after too many failed logins.
#[derive(Debug, Clone, Serialize)]
pub struct LoginLock {
    pub email: String,
    pub failures: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub locked_until: OffsetDateTime,
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
            max_sessions: cfg.max_sessions_per_account,
            policy: password::Policy {
                min_length: cfg.password_min_length,
                require_classes: cfg.password_require_classes,
                deny_common: cfg.password_deny_common,
            },
            lockout: Lockout {
                threshold: cfg.lockout_threshold,
                base_secs: cfg.lockout_base_secs.max(1),
                max_secs: cfg.lockout_max_secs.max(cfg.lockout_base_secs.max(1)),
                captcha_after: cfg.captcha_after,
            },
            captcha: Arc::new(std::sync::RwLock::new(None)),
            queue: Arc::new(std::sync::RwLock::new(None)),
        })
    }

    pub fn set_login_queue(&self, q: Option<LoginQueue>) {
        *self.queue.write().unwrap() = q;
    }

    fn login_queue(&self) -> Option<LoginQueue> {
        self.queue
            .read()
            .unwrap()
            .clone()
            .filter(|q| q.capacity > 0)
    }

    async fn queue_prune(&self, q: &LoginQueue) -> Result<()> {
        sqlx::query(
            "delete from musubi.login_queue
             where (admitted_at is null and polled_at < now() - make_interval(secs => $1))
                or admitted_at < now() - make_interval(secs => $2)",
        )
        .bind(q.ttl_secs as f64)
        .bind(ADMITTED_HOLD_SECS as f64)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    async fn queue_counts(&self) -> Result<(i64, i64, i64)> {
        sqlx::query_as(
            "select count(*) filter (where admitted_at is null),
                    count(*) filter (where admitted_at is not null),
                    count(*) filter (where admitted_at > now() - interval '1 second')
             from musubi.login_queue",
        )
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)
    }

    fn queue_eta(q: &LoginQueue, position: i64) -> i64 {
        let rate = q.admit_per_sec.max(1) as i64;
        ((position + 1) + rate - 1) / rate
    }

    fn queued(&self, q: &LoginQueue, ticket: String, position: i64, exp: i64) -> Error {
        Error::LoginQueued {
            ticket,
            position,
            eta_secs: Self::queue_eta(q, position),
            expires_in: (exp - OffsetDateTime::now_utc().unix_timestamp()).max(0),
        }
    }

    /// Park a verified login: a row keeps the place in line, the signed ticket carries what
    /// `issue` needs so the poll can finish the login without re-authenticating.
    async fn queue_enter(
        &self,
        q: &LoginQueue,
        account_id: AccountId,
        created: bool,
        vars: serde_json::Map<String, serde_json::Value>,
        dev: &Device<'_>,
    ) -> Result<Error> {
        let id = Uuid::now_v7();
        sqlx::query("insert into musubi.login_queue (id, account_id) values ($1, $2)")
            .bind(id)
            .bind(account_id.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        let position: i64 = sqlx::query_scalar(
            "select count(*) from musubi.login_queue where admitted_at is null and id < $1",
        )
        .bind(id)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut pending = serde_json::Map::new();
        pending.insert("queue".into(), serde_json::Value::String("pending".into()));
        pending.insert("created".into(), serde_json::Value::Bool(created));
        pending.insert("pending_vars".into(), serde_json::Value::Object(vars));
        if let Some(d) = dev.device_id {
            pending.insert("device_id".into(), serde_json::Value::String(d.into()));
        }
        if let Some(u) = dev.user_agent {
            pending.insert("user_agent".into(), serde_json::Value::String(u.into()));
        }
        let exp = now + QUEUE_TICKET_SECS;
        let ticket = self.signer.sign(&Claims {
            sub: account_id,
            sid: SessionId(id),
            iat: now,
            exp,
            vars: pending,
            name: None,
        })?;
        Ok(self.queued(q, ticket, position, exp))
    }

    /// Poll a queue ticket: the session once the place is reached (and the admit rate allows),
    /// otherwise `Error::LoginQueued` again with the fresh position.
    pub async fn queue_poll(&self, ticket: &str) -> Result<Session> {
        let c = self.signer.verify(ticket)?;
        if c.vars.get("queue").and_then(|v| v.as_str()) != Some("pending") {
            return Err(Error::Unauthenticated("not a queue ticket".into()));
        }
        let id = c.sid.0;
        let created = c
            .vars
            .get("created")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let vars = c
            .vars
            .get("pending_vars")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let dev = Device {
            device_id: c.vars.get("device_id").and_then(|v| v.as_str()),
            user_agent: c.vars.get("user_agent").and_then(|v| v.as_str()),
        };
        let Some(q) = self.login_queue() else {
            // queue switched off while waiting: everyone goes straight in
            let _ = sqlx::query("delete from musubi.login_queue where id = $1")
                .bind(id)
                .execute(&self.db)
                .await;
            return self.issue(c.sub, None, created, vars, dev).await;
        };
        self.queue_prune(&q).await?;
        let row: Option<(Option<OffsetDateTime>,)> = sqlx::query_as(
            "update musubi.login_queue set polled_at = now() where id = $1 returning admitted_at",
        )
        .bind(id)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let admitted = match row {
            None => {
                return Err(Error::Unauthenticated(
                    "queue ticket expired: log in again".into(),
                ))
            }
            Some((admitted,)) => admitted.is_some(),
        };
        if !admitted {
            let position: i64 = sqlx::query_scalar(
                "select count(*) from musubi.login_queue where admitted_at is null and id < $1",
            )
            .bind(id)
            .fetch_one(&self.db)
            .await
            .map_err(Error::internal)?;
            let (_, held, last_second) = self.queue_counts().await?;
            let online = (q.online)() as i64 + held;
            let free = (q.capacity as i64 - online).max(0);
            if position >= free || last_second >= q.admit_per_sec as i64 {
                return Err(self.queued(&q, ticket.to_string(), position, c.exp));
            }
            let n = sqlx::query(
                "update musubi.login_queue set admitted_at = now() where id = $1 and admitted_at is null",
            )
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?
            .rows_affected();
            if n == 0 {
                return Err(self.queued(&q, ticket.to_string(), position, c.exp));
            }
        }
        self.issue(c.sub, None, created, vars, dev).await
    }

    pub async fn queue_stats(&self) -> Result<QueueStats> {
        let q = self.login_queue();
        let (waiting, held, _) = self.queue_counts().await?;
        Ok(QueueStats {
            capacity: q.as_ref().map_or(0, |q| q.capacity),
            online: q.as_ref().map_or(0, |q| (q.online)()),
            waiting,
            admitted_recently: held,
            admit_per_sec: q.as_ref().map_or(0, |q| q.admit_per_sec),
        })
    }

    /// Drop every ticket: waiting clients get "log in again" on their next poll.
    pub async fn queue_flush(&self) -> Result<u64> {
        sqlx::query("delete from musubi.login_queue")
            .execute(&self.db)
            .await
            .map(|r| r.rows_affected())
            .map_err(Error::internal)
    }

    /// Install the captcha verifier (module hook or provider siteverify). Without one, the
    /// captcha gate is logged and skipped: lockout still applies.
    pub fn set_captcha_verifier(&self, v: Option<CaptchaVerifier>) {
        *self.captcha.write().unwrap() = v;
    }

    /// Failed logins counted against an address in the last day (0 for unknown addresses).
    pub async fn login_failures(&self, email: &str) -> Result<u32> {
        let email = normalize_email(email)?;
        let n: Option<i32> = sqlx::query_scalar(
            "select failures from musubi.login_attempts where email = $1 and updated_at > now() - interval '1 day'",
        )
        .bind(&email)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(n.unwrap_or(0).max(0) as u32)
    }

    /// Stamp the client IP on a freshly opened session and say whether this login came from an
    /// address the account has never been seen at (accounts with no history are never suspicious).
    pub async fn note_login(&self, session: SessionId, ip: &str) -> Result<bool> {
        if ip.parse::<std::net::IpAddr>().is_err() {
            return Ok(false);
        }
        let (history, seen): (bool, bool) = sqlx::query_as(
            "select exists(select 1 from musubi.sessions s where s.account_id = x.account_id and s.id <> x.id),
                    exists(select 1 from musubi.sessions s where s.account_id = x.account_id and s.id <> x.id and s.ip = $2::inet)
             from musubi.sessions x where x.id = $1",
        )
        .bind(session.0)
        .bind(ip)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?
        .unwrap_or((false, false));
        sqlx::query("update musubi.sessions set ip = $2::inet where id = $1")
            .bind(session.0)
            .bind(ip)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(history && !seen)
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
        client_ip: Option<&str>,
    ) -> Result<Login> {
        let email = normalize_email(email)?;
        password::validate(pass)?;
        if let Some(lock) = self.login_lock(&email).await? {
            let secs = (lock.locked_until - OffsetDateTime::now_utc())
                .whole_seconds()
                .max(1);
            return Err(Error::ResourceExhausted(format!(
                "too many failed logins; retry in {secs}s"
            )));
        }
        if self.lockout.captcha_after > 0
            && self.login_failures(&email).await? >= self.lockout.captcha_after
        {
            let verifier = self.captcha.read().unwrap().clone();
            match verifier.filter(|v| (v.configured)()) {
                None => tracing::warn!(
                    "captcha gate reached but no verifier is configured (hook_auth_captcha or captcha_verify_url)"
                ),
                Some(v) => {
                    let Some(token) = opts.captcha.clone().filter(|t| !t.is_empty()) else {
                        return Err(Error::PermissionDenied("captcha_required".into()));
                    };
                    if !(v.verify)(token, client_ip.map(str::to_string)).await? {
                        return Err(Error::PermissionDenied("captcha_invalid".into()));
                    }
                }
            }
        }
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
                    return Err(self.record_login_failure(&email).await?);
                }
                self.clear_login_failures(&email).await?;
                (AccountId(acc), false)
            }
            Some((_, None)) => return Err(self.record_login_failure(&email).await?),
            None => {
                if !opts.create {
                    return Err(self.record_login_failure(&email).await?);
                }
                self.policy.check(pass, Some(&email))?;
                let hash = password::hash(pass.to_owned()).await?;
                self.find_or_create(provider::EMAIL, &email, Some(&hash), &opts)
                    .await?
            }
        };
        self.finish_login(account_id, created, opts.vars).await
    }

    /// Active lockout for a (normalised) email address, if any.
    pub async fn login_lock(&self, email: &str) -> Result<Option<LoginLock>> {
        if self.lockout.threshold == 0 {
            return Ok(None);
        }
        let email = normalize_email(email)?;
        let row = sqlx::query_as::<_, (i32, Option<OffsetDateTime>)>(
            "select failures, locked_until from musubi.login_attempts where email = $1",
        )
        .bind(&email)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(match row {
            Some((failures, Some(until))) if until > OffsetDateTime::now_utc() => Some(LoginLock {
                email,
                failures: failures.max(0) as u32,
                locked_until: until,
            }),
            _ => None,
        })
    }

    /// Lift a lockout (admin action).
    pub async fn clear_login_lock(&self, email: &str) -> Result<bool> {
        let email = normalize_email(email)?;
        let r = sqlx::query("delete from musubi.login_attempts where email = $1")
            .bind(&email)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(r.rows_affected() > 0)
    }

    /// Count a failed login; past the threshold the address locks for base * 2^(excess), capped.
    /// Unknown addresses are counted too so lock responses never reveal whether an account exists.
    async fn record_login_failure(&self, email: &str) -> Result<Error> {
        let invalid = Error::Unauthenticated("invalid credentials".into());
        if self.lockout.threshold == 0 {
            return Ok(invalid);
        }
        let failures: i32 = sqlx::query_scalar(
            "insert into musubi.login_attempts (email, failures, updated_at) values ($1, 1, now())
             on conflict (email) do update set
               failures = case when musubi.login_attempts.updated_at < now() - interval '1 day' then 1
                               else musubi.login_attempts.failures + 1 end,
               updated_at = now()
             returning failures",
        )
        .bind(email)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)?;
        let excess = failures.max(0) as u32;
        if excess < self.lockout.threshold {
            return Ok(invalid);
        }
        let factor = 1u64 << (excess - self.lockout.threshold).min(20);
        let secs = self
            .lockout
            .base_secs
            .saturating_mul(factor)
            .min(self.lockout.max_secs);
        sqlx::query(
            "update musubi.login_attempts set locked_until = now() + make_interval(secs => $2) where email = $1",
        )
        .bind(email)
        .bind(secs as f64)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(Error::ResourceExhausted(format!(
            "too many failed logins; retry in {secs}s"
        )))
    }

    async fn clear_login_failures(&self, email: &str) -> Result<()> {
        sqlx::query("delete from musubi.login_attempts where email = $1")
            .bind(email)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Verify an access token. Stateless: no DB hit on the hot path.
    pub fn verify_access(&self, token: &str) -> Result<Authenticated> {
        let c = self.signer.verify(token)?;
        if c.vars.contains_key("mfa") {
            return Err(Error::Unauthenticated("second factor pending".into()));
        }
        Ok(Authenticated {
            account_id: c.sub,
            session_id: c.sid,
            vars: c.vars,
            username: c.name,
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

    /// Live refresh families of an account (one row per device/login), newest activity first.
    pub async fn sessions(&self, account_id: AccountId) -> Result<Vec<SessionInfo>> {
        let rows: Vec<SessionRow> = sqlx::query_as(
            "select distinct on (family) id, family, device_id, user_agent, host(ip), created_at, last_seen_at, expires_at
             from musubi.sessions
             where account_id = $1 and revoked_at is null and expires_at > now()
             order by family, created_at desc",
        )
        .bind(account_id.0)
        .fetch_all(&self.db)
        .await
        .map_err(Error::internal)?;
        let mut out: Vec<SessionInfo> = rows
            .into_iter()
            .map(|r| SessionInfo {
                id: SessionId(r.0),
                family: r.1,
                device_id: r.2,
                user_agent: r.3,
                ip: r.4,
                created_at: r.5.unix_timestamp(),
                last_seen_at: r.6.unix_timestamp(),
                expires_at: r.7.unix_timestamp(),
            })
            .collect();
        out.sort_by_key(|s| std::cmp::Reverse(s.last_seen_at));
        Ok(out)
    }

    /// Revoke one device's refresh family (`session` = any id of that family). The access token
    /// already issued stays valid until it expires. Returns false when nothing matched.
    pub async fn revoke_family(&self, account_id: AccountId, session: SessionId) -> Result<bool> {
        let n = sqlx::query(
            "update musubi.sessions set revoked_at = now()
             where account_id = $1 and revoked_at is null
               and family = (select family from musubi.sessions where id = $2 and account_id = $1)",
        )
        .bind(account_id.0)
        .bind(session.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        Ok(n > 0)
    }

    async fn finish_login(
        &self,
        account_id: AccountId,
        created: bool,
        vars: serde_json::Map<String, serde_json::Value>,
    ) -> Result<Login> {
        if !self.totp_required(account_id).await? {
            return Ok(Login::Session(
                self.open_session(
                    account_id,
                    created,
                    vars,
                    Device {
                        device_id: None,
                        user_agent: None,
                    },
                )
                .await?,
            ));
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let mut pending = serde_json::Map::new();
        pending.insert("mfa".into(), serde_json::Value::String("pending".into()));
        pending.insert("created".into(), serde_json::Value::Bool(created));
        pending.insert("pending_vars".into(), serde_json::Value::Object(vars));
        let mfa_token = self.signer.sign(&Claims {
            sub: account_id,
            sid: SessionId(Uuid::nil()),
            iat: now,
            exp: now + MFA_TOKEN_SECS,
            vars: pending,
            name: None,
        })?;
        Ok(Login::MfaRequired {
            account_id,
            mfa_token,
        })
    }

    /// Does this account have a verified authenticator?
    pub async fn totp_required(&self, account_id: AccountId) -> Result<bool> {
        sqlx::query_scalar(
            "select exists(select 1 from musubi.identities where account_id = $1 and provider = 'totp' and verified)",
        )
        .bind(account_id.0)
        .fetch_one(&self.db)
        .await
        .map_err(Error::internal)
    }

    /// Start enrolling an authenticator: returns (base32 secret, otpauth URI, recovery codes).
    /// Nothing is enforced until `totp_enable` confirms a code. Recovery codes are stored hashed.
    pub async fn totp_begin(
        &self,
        account_id: AccountId,
        issuer: &str,
        label: &str,
    ) -> Result<(String, String, Vec<String>)> {
        if self.totp_required(account_id).await? {
            return Err(Error::AlreadyExists("authenticator already enabled".into()));
        }
        let secret = crate::totp::new_secret();
        let recovery: Vec<String> = (0..RECOVERY_CODES)
            .map(|_| {
                use rand::RngCore;
                let mut b = [0u8; 5];
                rand::thread_rng().fill_bytes(&mut b);
                hex::encode(b)
            })
            .collect();
        let hashes: Vec<serde_json::Value> = recovery
            .iter()
            .map(|c| serde_json::Value::String(hex::encode(Sha256::digest(c.as_bytes()))))
            .collect();
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        sqlx::query("delete from musubi.identities where account_id = $1 and provider = 'totp'")
            .bind(account_id.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        sqlx::query(
            "insert into musubi.identities (id, account_id, provider, subject, secret_hash, verified, metadata)
             values ($1, $2, 'totp', $3, $4, false, $5)",
        )
        .bind(IdentityId::new().0)
        .bind(account_id.0)
        .bind(account_id.0.to_string())
        .bind(&secret)
        .bind(serde_json::json!({ "recovery": hashes }))
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)?;
        let uri = crate::totp::otpauth_uri(issuer, label, &secret);
        Ok((secret, uri, recovery))
    }

    /// Confirm enrolment with a code from the app; from now on password logins need a second factor.
    pub async fn totp_enable(&self, account_id: AccountId, code: &str) -> Result<()> {
        let secret: Option<String> = sqlx::query_scalar(
            "select secret_hash from musubi.identities where account_id = $1 and provider = 'totp' and not verified",
        )
        .bind(account_id.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some(secret) = secret else {
            return Err(Error::NotFound(
                "no authenticator enrolment in progress".into(),
            ));
        };
        if !crate::totp::verify(&secret, code, OffsetDateTime::now_utc().unix_timestamp()) {
            return Err(Error::Unauthenticated("bad code".into()));
        }
        sqlx::query(
            "update musubi.identities set verified = true, updated_at = now() where account_id = $1 and provider = 'totp'",
        )
        .bind(account_id.0)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(())
    }

    /// Accept an authenticator code or an unused recovery code (which is burned).
    async fn totp_check(&self, account_id: AccountId, code: &str) -> Result<()> {
        let row: Option<(Uuid, String, serde_json::Value)> = sqlx::query_as(
            "select id, secret_hash, metadata from musubi.identities where account_id = $1 and provider = 'totp' and verified",
        )
        .bind(account_id.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let Some((id, secret, mut meta)) = row else {
            return Err(Error::NotFound("authenticator not enabled".into()));
        };
        if crate::totp::verify(&secret, code, OffsetDateTime::now_utc().unix_timestamp()) {
            return Ok(());
        }
        let want = hex::encode(Sha256::digest(code.trim().to_ascii_lowercase().as_bytes()));
        let codes = meta.get_mut("recovery").and_then(|r| r.as_array_mut());
        if let Some(codes) = codes {
            if let Some(pos) = codes.iter().position(|c| c.as_str() == Some(want.as_str())) {
                codes.remove(pos);
                let n = sqlx::query(
                    "update musubi.identities set metadata = $2, updated_at = now() where id = $1 and metadata = $3",
                )
                .bind(id)
                .bind(&meta)
                .bind(
                    sqlx::query_scalar::<_, serde_json::Value>(
                        "select metadata from musubi.identities where id = $1",
                    )
                    .bind(id)
                    .fetch_one(&self.db)
                    .await
                    .map_err(Error::internal)?,
                )
                .execute(&self.db)
                .await
                .map_err(Error::internal)?
                .rows_affected();
                if n == 1 {
                    return Ok(());
                }
            }
        }
        Err(Error::Unauthenticated("bad code".into()))
    }

    /// Turn the authenticator off (needs a valid code or recovery code).
    pub async fn totp_disable(&self, account_id: AccountId, code: &str) -> Result<()> {
        self.totp_check(account_id, code).await?;
        sqlx::query("delete from musubi.identities where account_id = $1 and provider = 'totp'")
            .bind(account_id.0)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        Ok(())
    }

    /// Second step of a password login: the pending token from `Login::MfaRequired` plus a code.
    pub async fn mfa_complete(&self, mfa_token: &str, code: &str) -> Result<Session> {
        let c = self.signer.verify(mfa_token)?;
        if c.vars.get("mfa").and_then(|v| v.as_str()) != Some("pending") {
            return Err(Error::Unauthenticated("not a pending login".into()));
        }
        self.totp_check(c.sub, code).await?;
        let created = c
            .vars
            .get("created")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let vars = c
            .vars
            .get("pending_vars")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        self.open_session(
            c.sub,
            created,
            vars,
            Device {
                device_id: None,
                user_agent: None,
            },
        )
        .await
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

    pub async fn link_device(&self, account_id: AccountId, device_id: &str) -> Result<IdentityId> {
        if device_id.len() < 10 || device_id.len() > 128 {
            return Err(Error::InvalidArgument(
                "device id must be 10..=128 chars".into(),
            ));
        }
        self.link(account_id, provider::DEVICE, device_id, None)
            .await
    }

    pub async fn link_email(
        &self,
        account_id: AccountId,
        email: &str,
        pass: &str,
    ) -> Result<IdentityId> {
        let email = normalize_email(email)?;
        self.policy.check(pass, Some(&email))?;
        let hash = password::hash(pass.to_owned()).await?;
        self.link(account_id, provider::EMAIL, &email, Some(&hash))
            .await
    }

    /// Sign in with a subject already verified by a third party (steam id, OIDC `sub`).
    pub async fn authenticate_external(
        &self,
        provider: &str,
        subject: &str,
        opts: AuthOptions,
    ) -> Result<Session> {
        if subject.is_empty() || subject.len() > 256 {
            return Err(Error::InvalidArgument(
                "subject must be 1..=256 chars".into(),
            ));
        }
        let (account_id, created) = self.find_or_create(provider, subject, None, &opts).await?;
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

    /// Trusted identity minted by the game server (its own id / token already verified there).
    pub async fn authenticate_custom(&self, subject: &str, opts: AuthOptions) -> Result<Session> {
        self.authenticate_external(provider::CUSTOM, subject, opts)
            .await
    }

    /// Account a magic link is minted for: the owner of `email`, or a fresh account when
    /// `create` (the address is verified when the link is used).
    pub async fn magic_account(&self, email: &str, create: bool) -> Result<Option<AccountId>> {
        let email = email.trim().to_lowercase();
        if !email.contains('@') || email.len() > 254 {
            return Err(Error::InvalidArgument("invalid email".into()));
        }
        if let Some(a) = self.account_by_email(&email).await? {
            return Ok(Some(a));
        }
        if !create {
            return Ok(None);
        }
        let opts: AuthOptions = serde_json::from_value(serde_json::json!({}))?;
        let (a, _) = self
            .find_or_create(provider::EMAIL, &email, None, &opts)
            .await?;
        Ok(Some(a))
    }

    /// Opens a session for a consumed magic link; proves ownership, so the email is verified.
    pub async fn authenticate_magic(
        &self,
        account_id: AccountId,
        email: &str,
        opts: AuthOptions,
    ) -> Result<Session> {
        self.mark_email_verified(account_id, email).await?;
        self.open_session(
            account_id,
            false,
            opts.vars,
            Device {
                device_id: None,
                user_agent: None,
            },
        )
        .await
    }

    pub async fn link_external(
        &self,
        account_id: AccountId,
        provider: &str,
        subject: &str,
    ) -> Result<IdentityId> {
        if subject.is_empty() || subject.len() > 256 {
            return Err(Error::InvalidArgument(
                "subject must be 1..=256 chars".into(),
            ));
        }
        self.link(account_id, provider, subject, None).await
    }

    /// The account's email identity: (address, verified).
    pub async fn email_of(&self, account_id: AccountId) -> Result<Option<(String, bool)>> {
        sqlx::query_as::<_, (String, bool)>(
            "select subject, verified from musubi.identities where provider = $1 and account_id = $2",
        )
        .bind(provider::EMAIL)
        .bind(account_id.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)
    }

    pub async fn account_by_email(&self, email: &str) -> Result<Option<AccountId>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "select account_id from musubi.identities where provider = $1 and subject = $2",
        )
        .bind(provider::EMAIL)
        .bind(email.trim().to_lowercase())
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        Ok(row.map(|r| AccountId(r.0)))
    }

    pub async fn mark_email_verified(&self, account_id: AccountId, email: &str) -> Result<bool> {
        let n = sqlx::query(
            "update musubi.identities set verified = true, updated_at = now()
             where provider = $1 and account_id = $2 and subject = $3",
        )
        .bind(provider::EMAIL)
        .bind(account_id.0)
        .bind(email)
        .execute(&self.db)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        Ok(n > 0)
    }

    /// Replace the password and revoke every refresh session of the account (password reset).
    pub async fn set_email_password(
        &self,
        account_id: AccountId,
        email: &str,
        new_password: &str,
    ) -> Result<()> {
        self.policy.check(new_password, Some(email))?;
        let hash = password::hash(new_password.to_owned()).await?;
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        let n = sqlx::query(
            "update musubi.identities set secret_hash = $4, updated_at = now()
             where provider = $1 and account_id = $2 and subject = $3",
        )
        .bind(provider::EMAIL)
        .bind(account_id.0)
        .bind(email)
        .bind(&hash)
        .execute(&mut *tx)
        .await
        .map_err(Error::internal)?
        .rows_affected();
        if n == 0 {
            return Err(Error::NotFound("email identity".into()));
        }
        sqlx::query("update musubi.sessions set revoked_at = now() where account_id = $1 and revoked_at is null")
            .bind(account_id.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        tx.commit().await.map_err(Error::internal)
    }

    pub async fn unlink(&self, account_id: AccountId, provider: &str, subject: &str) -> Result<()> {
        let mut tx = self.db.begin().await.map_err(Error::internal)?;
        // Serialize unlinks per account so two concurrent unlinks cannot both pass the check.
        sqlx::query("select 1 from musubi.accounts where id = $1 for update")
            .bind(account_id.0)
            .execute(&mut *tx)
            .await
            .map_err(Error::internal)?;
        let count: i64 =
            sqlx::query_scalar("select count(*) from musubi.identities where account_id = $1")
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

    /// Player-facing text for a disabled account: the moderator's message and expiry from
    /// `musubi.account_bans` when there is one.
    async fn ban_text(&self, account_id: AccountId) -> String {
        let row: Option<(Option<String>, Option<OffsetDateTime>)> =
            sqlx::query_as("select message, until from musubi.account_bans where account_id = $1")
                .bind(account_id.0)
                .fetch_optional(&self.db)
                .await
                .ok()
                .flatten();
        let mut s = String::from("account banned");
        if let Some((message, until)) = row {
            if let Some(m) = message.filter(|m| !m.is_empty()) {
                s.push_str(": ");
                s.push_str(&m);
            }
            if let Some(u) = until {
                if let Ok(f) = u.format(&time::format_description::well_known::Rfc3339) {
                    s.push_str(&format!(" (until {f})"));
                }
            }
        }
        s
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
        if let Some(q) = self.login_queue() {
            self.queue_prune(&q).await?;
            let (waiting, held, _) = self.queue_counts().await?;
            if waiting > 0 || (q.online)() + held as usize >= q.capacity {
                return Err(self
                    .queue_enter(&q, account_id, created, vars, &dev)
                    .await?);
            }
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
        let row: Option<(Option<OffsetDateTime>, String)> = sqlx::query_as(
            "select disabled_at, username from musubi.accounts where id = $1 and deleted_at is null",
        )
        .bind(account_id.0)
        .fetch_optional(&self.db)
        .await
        .map_err(Error::internal)?;
        let username = match row {
            None => return Err(Error::NotFound("account".into())),
            Some((Some(_), _)) => {
                return Err(Error::PermissionDenied(self.ban_text(account_id).await));
            }
            Some((None, username)) => username,
        };
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
        if self.max_sessions > 0 {
            sqlx::query(
                "delete from musubi.sessions where account_id = $1 and id in (
                   select id from musubi.sessions where account_id = $1 order by expires_at desc, id desc offset $2)",
            )
            .bind(account_id.0)
            .bind(self.max_sessions as i64)
            .execute(&self.db)
            .await
            .map_err(Error::internal)?;
        }
        let claims = Claims {
            sub: account_id,
            sid: session_id,
            iat: now.unix_timestamp(),
            exp: now.unix_timestamp() + self.signer.access_ttl_secs as i64,
            vars,
            name: Some(username),
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
