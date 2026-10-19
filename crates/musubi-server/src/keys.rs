//! Scoped server keys. `http.server_key` from the config is the bootstrap admin key; further keys
//! live hashed in `musubi.server_keys` with a role: `server` (`/v1/server/*`, `/metrics`) or
//! `admin` (everything, including `/v1/admin/*`). Revocation is immediate cluster-wide via the
//! `admin.keys` bus topic.

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use musubi_core::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub const KEYS_TOPIC: &str = "admin.keys";
/// (id, name, role, created_at, revoked_at, last_used_at)
type KeyRow = (
    Uuid,
    String,
    String,
    time::OffsetDateTime,
    Option<time::OffsetDateTime>,
    Option<time::OffsetDateTime>,
    Option<time::OffsetDateTime>,
    Option<String>,
    Option<i32>,
    Option<i32>,
    Option<i64>,
);

/// Rate limit of one server key: requests per second with a burst, plus a daily budget
/// (0 / None = the `[limits] key_*` defaults).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct KeyLimit {
    #[serde(default)]
    pub per_second: Option<u32>,
    #[serde(default)]
    pub burst: Option<u32>,
    #[serde(default)]
    pub daily: Option<u64>,
}

impl KeyLimit {
    fn from_row(p: Option<i32>, b: Option<i32>, d: Option<i64>) -> Option<Self> {
        if p.is_none() && b.is_none() && d.is_none() {
            return None;
        }
        Some(KeyLimit {
            per_second: p.map(|v| v.max(0) as u32),
            burst: b.map(|v| v.max(0) as u32),
            daily: d.map(|v| v.max(0) as u64),
        })
    }
}
const TOUCH_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Game servers and trusted services: everything outside `/v1/admin/`.
    Server,
    /// Everything.
    Admin,
    /// Player support and moderation: reads everything; writes accounts, bans, reports, login queue.
    Support,
    /// LiveOps: reads everything; writes content bundles, economy and competition definitions.
    Liveops,
    /// Operations: reads everything; writes fleets, worlds, cluster, webhooks.
    Ops,
}

/// Write prefixes of the scoped operator roles (any method other than GET/HEAD).
const SUPPORT_WRITES: &[&str] = &[
    "/v1/admin/accounts/",
    "/v1/admin/reports",
    "/v1/admin/ugc",
    "/v1/admin/auth/queue",
    "/v1/admin/chat/",
    "/v1/server/chat/",
    "/v1/server/friends/",
    "/v1/server/mail",
    "/v1/server/notifications",
];
const LIVEOPS_WRITES: &[&str] = &[
    "/v1/admin/content",
    "/v1/admin/storage_schemas",
    "/v1/server/currencies",
    "/v1/server/items",
    "/v1/server/inventory/",
    "/v1/server/wallet/",
    "/v1/server/rewards",
    "/v1/server/progression",
    "/v1/server/quests",
    "/v1/server/seasons",
    "/v1/server/store",
    "/v1/server/entitlements",
    "/v1/server/grant_limits",
    "/v1/server/rating/",
    "/v1/server/leaderboards",
    "/v1/server/tournaments",
    "/v1/server/mail",
    "/v1/server/notifications",
];
const OPS_WRITES: &[&str] = &[
    "/v1/admin/fleets",
    "/v1/admin/worlds",
    "/v1/admin/cluster",
    "/v1/admin/webhooks",
    "/v1/admin/integrations",
    "/v1/server/fleets",
    "/v1/server/worlds",
    "/v1/server/builds",
];

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Server => "server",
            Role::Admin => "admin",
            Role::Support => "support",
            Role::Liveops => "liveops",
            Role::Ops => "ops",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "server" => Some(Role::Server),
            "admin" => Some(Role::Admin),
            "support" => Some(Role::Support),
            "liveops" => Some(Role::Liveops),
            "ops" => Some(Role::Ops),
            _ => None,
        }
    }

    /// May a key with this role perform `method` on `path`? Admin may do anything, a server key
    /// anything outside `/v1/admin/`; operator roles read everything and write their own area.
    pub fn allows(self, method: &str, path: &str) -> bool {
        let read = matches!(method, "GET" | "HEAD");
        let writes: &[&str] = match self {
            Role::Admin => return true,
            Role::Server => return !path.starts_with("/v1/admin/"),
            Role::Support => SUPPORT_WRITES,
            Role::Liveops => LIVEOPS_WRITES,
            Role::Ops => OPS_WRITES,
        };
        if read {
            return true;
        }
        // keys are issued by admins only, whatever the caller's area
        if path.starts_with("/v1/admin/keys") {
            return false;
        }
        writes.iter().any(|p| path.starts_with(p))
    }
}

/// Who presented a valid key.
#[derive(Debug, Clone, Serialize)]
pub struct Caller {
    pub id: Uuid,
    pub name: String,
    pub role: Role,
    /// Per-key rate limit override (see `KeyLimit`).
    pub limit: Option<KeyLimit>,
    /// Resource scopes (`crate::scopes`); empty = whatever the role allows.
    pub scopes: Vec<String>,
}

impl Caller {
    pub fn bootstrap() -> Self {
        Caller {
            id: Uuid::nil(),
            name: "config".into(),
            role: Role::Admin,
            limit: None,
            scopes: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct ServerKeys {
    by_hash: DashMap<String, Caller>,
    touched: DashMap<Uuid, Instant>,
    expires: DashMap<Uuid, time::OffsetDateTime>,
}

pub(crate) fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

impl ServerKeys {
    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        #[allow(clippy::type_complexity)]
        let rows: Vec<(
            Uuid,
            String,
            String,
            String,
            Option<time::OffsetDateTime>,
            Option<i32>,
            Option<i32>,
            Option<i64>,
        Vec<String>,
        )> = sqlx::query_as(
            "select id, name, role, hash, expires_at, per_second, burst, daily, scopes from musubi.server_keys
             where revoked_at is null and (expires_at is null or expires_at > now())",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        self.by_hash.clear();
        self.expires.clear();
        for (id, name, role, h, exp, p, b, d, sc) in &rows {
            if let Some(role) = Role::parse(role) {
                self.by_hash.insert(
                    h.clone(),
                    Caller {
                        id: *id,
                        name: name.clone(),
                        role,
                        limit: KeyLimit::from_row(*p, *b, *d),
                        scopes: sc.clone(),
                    },
                );
                if let Some(exp) = exp {
                    self.expires.insert(*id, *exp);
                }
            }
        }
        Ok(rows.len())
    }

    pub fn lookup(&self, secret: &str) -> Option<Caller> {
        let h = hash(secret);
        let c = self.by_hash.get(&h).map(|c| c.clone())?;
        if let Some(exp) = self.expires.get(&c.id) {
            if *exp <= time::OffsetDateTime::now_utc() {
                drop(exp);
                self.expires.remove(&c.id);
                self.by_hash.remove(&h);
                return None;
            }
        }
        Some(c)
    }

    /// Record use at most once a minute per key, off the request path.
    pub fn touch(&self, db: Option<PgPool>, id: Uuid) {
        let Some(db) = db else { return };
        let now = Instant::now();
        let due = self
            .touched
            .get(&id)
            .is_none_or(|t| now.duration_since(*t) >= TOUCH_EVERY);
        if !due {
            return;
        }
        self.touched.insert(id, now);
        tokio::spawn(async move {
            let _ = sqlx::query("update musubi.server_keys set last_used_at = now() where id = $1")
                .bind(id)
                .execute(&db)
                .await;
        });
    }

    /// Create a key; the secret is returned exactly once.
    pub async fn issue(&self, db: &PgPool, name: &str, role: Role) -> Result<(Caller, String)> {
        self.issue_with(db, name, role, None, None, &[]).await
    }

    /// A key that stops working at `expires_at` (operator SSO sessions) and remembers the
    /// identity it was minted for.
    pub async fn issue_with(
        &self,
        db: &PgPool,
        name: &str,
        role: Role,
        expires_at: Option<time::OffsetDateTime>,
        subject: Option<&str>,
        scopes: &[String],
    ) -> Result<(Caller, String)> {
        crate::scopes::validate(scopes)?;
        if name.is_empty() || name.len() > 64 {
            return Err(Error::InvalidArgument("name 1..=64 chars".into()));
        }
        // 256 bits from two v7 uuids (74 random bits each) plus the clock, whitened through sha256
        let seed = format!(
            "{}{}{:?}",
            Uuid::now_v7().simple(),
            Uuid::now_v7().simple(),
            std::time::SystemTime::now()
        );
        let secret = format!("msk_{}", hash(&seed));
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into musubi.server_keys (id, name, role, hash, expires_at, subject, scopes) values ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(name)
        .bind(role.as_str())
        .bind(hash(&secret))
        .bind(expires_at)
        .bind(subject)
        .bind(scopes)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        let caller = Caller {
            id,
            name: name.to_string(),
            role,
            limit: None,
            scopes: scopes.to_vec(),
        };
        if let Some(exp) = expires_at {
            self.expires.insert(id, exp);
        }
        self.by_hash.insert(hash(&secret), caller.clone());
        Ok((caller, secret))
    }

    pub async fn revoke(&self, db: &PgPool, id: Uuid) -> Result<bool> {
        let r = sqlx::query(
            "update musubi.server_keys set revoked_at = now() where id = $1 and revoked_at is null",
        )
        .bind(id)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.by_hash.retain(|_, c| c.id != id);
        Ok(r.rows_affected() > 0)
    }

    pub async fn list(db: &PgPool) -> Result<Vec<Value>> {
        let rows: Vec<KeyRow> = sqlx::query_as(
            "select id, name, role, created_at, revoked_at, last_used_at, expires_at, subject, per_second, burst, daily from musubi.server_keys order by created_at desc",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(id, name, role, c, r, u, e, subject, p, b, d)| {
                json!({
                    "id": id, "name": name, "role": role,
                    "created_at": c.unix_timestamp(),
                    "revoked_at": r.map(|t| t.unix_timestamp()),
                    "last_used_at": u.map(|t| t.unix_timestamp()),
                    "expires_at": e.map(|t| t.unix_timestamp()),
                    "subject": subject,
                    "limit": KeyLimit::from_row(p, b, d),
                })
            })
            .collect())
    }

    /// Set (or with `None` clear) the per-key rate limit; live callers on this node are updated
    /// at once, other nodes on `admin.keys`.
    pub async fn set_limit(&self, db: &PgPool, id: Uuid, limit: Option<KeyLimit>) -> Result<bool> {
        let l = limit.unwrap_or_default();
        let r = sqlx::query(
            "update musubi.server_keys set per_second = $2, burst = $3, daily = $4 where id = $1 and revoked_at is null",
        )
        .bind(id)
        .bind(l.per_second.map(|v| v as i32))
        .bind(l.burst.map(|v| v as i32))
        .bind(l.daily.map(|v| v as i64))
        .execute(db)
        .await
        .map_err(Error::internal)?;
        if r.rows_affected() == 0 {
            return Ok(false);
        }
        for mut c in self.by_hash.iter_mut() {
            if c.id == id {
                c.limit = limit;
            }
        }
        Ok(true)
    }

    /// Reload on `admin.keys` from any node (issue/revoke elsewhere).
    pub fn start(self: &Arc<Self>, cluster: &Arc<musubi_cluster::Cluster>, db: PgPool) {
        let me = self.clone();
        let mut rx = cluster.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) if m.topic == KEYS_TOPIC => {
                        if let Err(e) = me.load(&db).await {
                            tracing::warn!(error = %e, "server keys reload failed");
                        }
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(_) => break,
                }
            }
        });
    }
}
