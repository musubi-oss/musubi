//! In-memory set of disabled accounts so a ban bites on the next HTTP request even though access
//! JWTs are verified statelessly. Loaded from `musubi.accounts.disabled_at` at startup and kept in
//! sync across nodes over the bus (topic `admin.ban`).

use std::sync::Arc;

use dashmap::DashSet;
use musubi_core::{AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

pub const BAN_TOPIC: &str = "admin.ban";

#[derive(Serialize, Deserialize)]
struct BanMsg {
    id: Uuid,
    banned: bool,
}

#[derive(Default)]
pub struct Bans {
    ids: DashSet<Uuid>,
}

impl Bans {
    pub fn is_banned(&self, id: AccountId) -> bool {
        self.ids.contains(&id.0)
    }

    pub fn check(&self, id: AccountId) -> Result<()> {
        if self.is_banned(id) {
            return Err(Error::PermissionDenied("account disabled".into()));
        }
        Ok(())
    }

    fn apply(&self, id: Uuid, banned: bool) {
        if banned {
            self.ids.insert(id);
        } else {
            self.ids.remove(&id);
        }
    }

    /// Record locally and tell the other nodes.
    pub async fn set(
        &self,
        id: AccountId,
        banned: bool,
        cluster: Option<&Arc<musubi_cluster::Cluster>>,
    ) {
        self.apply(id.0, banned);
        if let Some(c) = cluster {
            if let Err(e) = c.publish(BAN_TOPIC, &BanMsg { id: id.0, banned }).await {
                tracing::warn!(error = %e, "ban broadcast failed");
            }
        }
    }

    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        let ids: Vec<(Uuid,)> = sqlx::query_as(
            "select id from musubi.accounts where disabled_at is not null and deleted_at is null",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (id,) in &ids {
            self.ids.insert(*id);
        }
        Ok(ids.len())
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
                            me.apply(b.id, b.banned);
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
