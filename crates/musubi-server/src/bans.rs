//! In-memory view of banned accounts so a ban bites on the next request even though access JWTs
//! are verified statelessly. Loaded from `musubi.account_bans` (plus legacy `disabled_at`) at
//! startup and kept in sync across nodes over the bus (topic `admin.ban`).
//!
//! A shadow ban is not a denial: the account keeps working, but chat and matchmaking consult
//! [`Bans::is_shadow`] and quietly drop it.

use std::sync::Arc;

use dashmap::DashMap;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

pub const BAN_TOPIC: &str = "admin.ban";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BanInfo {
    #[serde(default)]
    pub reason: Option<String>,
    /// Shown to the player.
    #[serde(default)]
    pub message: Option<String>,
    /// Unix seconds; `None` = permanent.
    #[serde(default)]
    pub until: Option<i64>,
    #[serde(default)]
    pub shadow: bool,
}

impl BanInfo {
    pub fn expired(&self) -> bool {
        self.until
            .is_some_and(|u| u <= OffsetDateTime::now_utc().unix_timestamp())
    }
}

#[derive(Serialize, Deserialize)]
struct BanMsg {
    id: Uuid,
    /// `None` = lifted.
    info: Option<BanInfo>,
}

#[derive(Default)]
pub struct Bans {
    ids: DashMap<Uuid, BanInfo>,
}

impl Bans {
    pub fn get(&self, id: AccountId) -> Option<BanInfo> {
        self.ids
            .get(&id.0)
            .filter(|b| !b.expired())
            .map(|b| b.clone())
    }

    pub fn is_banned(&self, id: AccountId) -> bool {
        self.get(id).is_some_and(|b| !b.shadow)
    }

    pub fn is_shadow(&self, id: AccountId) -> bool {
        self.get(id).is_some_and(|b| b.shadow)
    }

    pub fn check(&self, id: AccountId) -> Result<()> {
        match self.get(id) {
            Some(b) if !b.shadow => Err(Error::PermissionDenied(describe(&b))),
            _ => Ok(()),
        }
    }

    fn apply(&self, id: Uuid, info: Option<BanInfo>) {
        match info {
            Some(b) => {
                self.ids.insert(id, b);
            }
            None => {
                self.ids.remove(&id);
            }
        }
    }

    /// Record locally and tell the other nodes; `None` lifts the ban.
    pub async fn set(
        &self,
        id: AccountId,
        info: Option<BanInfo>,
        cluster: Option<&Arc<musubi_cluster::Cluster>>,
    ) {
        self.apply(id.0, info.clone());
        if let Some(c) = cluster {
            if let Err(e) = c.publish(BAN_TOPIC, &BanMsg { id: id.0, info }).await {
                tracing::warn!(error = %e, "ban broadcast failed");
            }
        }
    }

    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        type Row = (Uuid, Option<String>, Option<String>, Option<i64>, bool);
        let rows: Vec<Row> = sqlx::query_as(
            "select a.id, b.reason, b.message,
                    (extract(epoch from b.until))::bigint, coalesce(b.shadow, false)
             from musubi.accounts a
             left join musubi.account_bans b on b.account_id = a.id
             where a.deleted_at is null
               and (b.account_id is not null or a.disabled_at is not null)
               and (b.until is null or b.until > now())",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (id, reason, message, until, shadow) in &rows {
            self.ids.insert(
                *id,
                BanInfo {
                    reason: reason.clone(),
                    message: message.clone(),
                    until: *until,
                    shadow: *shadow,
                },
            );
        }
        Ok(rows.len())
    }

    pub fn start(self: &Arc<Self>, cluster: &Arc<musubi_cluster::Cluster>) {
        let me = self.clone();
        let mut rx = cluster.subscribe();
        let self_id = cluster.node_id();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) if m.topic == BAN_TOPIC && m.origin != self_id => {
                        if let Ok(b) = serde_json::from_value::<BanMsg>(m.payload) {
                            me.apply(b.id, b.info);
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

/// Player-facing text: `account banned: <message> (until 2026-10-20T12:00:00Z)`.
pub fn describe(b: &BanInfo) -> String {
    let mut s = String::from("account banned");
    if let Some(m) = b.message.as_deref().filter(|m| !m.is_empty()) {
        s.push_str(": ");
        s.push_str(m);
    }
    if let Some(u) = b.until {
        if let Ok(t) = OffsetDateTime::from_unix_timestamp(u) {
            if let Ok(f) = t.format(&time::format_description::well_known::Rfc3339) {
                s.push_str(&format!(" (until {f})"));
            }
        }
    }
    s
}
