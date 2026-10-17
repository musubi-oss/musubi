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
);
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
    "/v1/admin/auth/queue",
    "/v1/admin/chat/",
    "/v1/server/chat/",
    "/v1/server/friends/",
    "/v1/server/mail",
    "/v1/server/notifications",
];
const LIVEOPS_WRITES: &[&str] = &[
    "/v1/admin/content",
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
    fn parse(s: &str) -> Option<Self> {
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
}

impl Caller {
    pub fn bootstrap() -> Self {
        Caller {
            id: Uuid::nil(),
            name: "config".into(),
            role: Role::Admin,
        }
    }
}

#[derive(Default)]
pub struct ServerKeys {
    by_hash: DashMap<String, Caller>,
    touched: DashMap<Uuid, Instant>,
}

pub(crate) fn hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

impl ServerKeys {
    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
            "select id, name, role, hash from musubi.server_keys where revoked_at is null",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        self.by_hash.clear();
        for (id, name, role, h) in &rows {
            if let Some(role) = Role::parse(role) {
                self.by_hash.insert(
                    h.clone(),
                    Caller {
                        id: *id,
                        name: name.clone(),
                        role,
                    },
                );
            }
        }
        Ok(rows.len())
    }

    pub fn lookup(&self, secret: &str) -> Option<Caller> {
        self.by_hash.get(&hash(secret)).map(|c| c.clone())
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
            "insert into musubi.server_keys (id, name, role, hash) values ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(name)
        .bind(role.as_str())
        .bind(hash(&secret))
        .execute(db)
        .await
        .map_err(Error::internal)?;
        let caller = Caller {
            id,
            name: name.to_string(),
            role,
        };
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
            "select id, name, role, created_at, revoked_at, last_used_at from musubi.server_keys order by created_at desc",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        Ok(rows
            .into_iter()
            .map(|(id, name, role, c, r, u)| {
                json!({
                    "id": id, "name": name, "role": role,
                    "created_at": c.unix_timestamp(),
                    "revoked_at": r.map(|t| t.unix_timestamp()),
                    "last_used_at": u.map(|t| t.unix_timestamp()),
                })
            })
            .collect())
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
