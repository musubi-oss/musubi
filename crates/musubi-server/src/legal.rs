//! Legal agreements: versioned documents (`musubi.legal_documents`), consents and the gate that
//! turns every bearer call into `403 consent_required: …` while a mandatory document's latest
//! version is unaccepted (after its grace period). Documents and per-account acceptance are cached
//! here; changes are broadcast over the cluster bus so every node re-reads.

use std::collections::HashSet;
use std::sync::Arc;

use dashmap::DashMap;
use musubi_core::{AccountId, Error, Result};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

const TOPIC: &str = "legal";

type DocRow = (String, i32, Vec<String>, i64, Option<OffsetDateTime>);

/// A published, mandatory document version that can gate accounts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Required {
    pub key: String,
    pub version: i32,
    pub countries: Vec<String>,
    /// Unix seconds from which the gate enforces this version.
    pub required_from: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Accepted {
    pub country: String,
    pub versions: HashSet<(String, i32)>,
}

#[derive(Default)]
pub struct Legal {
    required: RwLock<Vec<Required>>,
    accepted: DashMap<Uuid, Accepted>,
}

#[derive(Serialize, Deserialize)]
struct Msg {
    account: Option<Uuid>,
}

/// Paths a gated account may still call: see what is pending, accept, read documents, leave.
pub fn exempt(path: &str) -> bool {
    path.starts_with("/v1/legal")
        || path.starts_with("/v1/account/consents")
        || path.starts_with("/v1/auth/")
        || path == "/v1/account/export"
        || path == "/v1/account/delete"
        || path == "/v1/account"
}

impl Legal {
    /// Latest published, non-retired, mandatory version per key, with its enforcement time.
    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        let rows: Vec<DocRow> = sqlx::query_as(
            "select distinct on (key) key, version, countries, grace_secs, published_at
             from musubi.legal_documents
             where published and retired_at is null and mandatory
             order by key, version desc",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let req: Vec<Required> = rows
            .into_iter()
            .map(|(key, version, countries, grace, at)| Required {
                key,
                version,
                countries,
                required_from: at.map_or(0, |t| t.unix_timestamp()) + grace,
            })
            .collect();
        let n = req.len();
        *self.required.write() = req;
        self.accepted.clear();
        Ok(n)
    }

    pub fn required(&self) -> Vec<Required> {
        self.required.read().clone()
    }

    async fn accepted_of(&self, db: &PgPool, account: AccountId) -> Result<Accepted> {
        if let Some(a) = self.accepted.get(&account.0) {
            return Ok(a.clone());
        }
        let country: Option<(String,)> =
            sqlx::query_as("select country from musubi.legal_profiles where account_id = $1")
                .bind(account.0)
                .fetch_optional(db)
                .await
                .map_err(Error::internal)?;
        let rows: Vec<(String, i32)> = sqlx::query_as(
            "select key, version from musubi.legal_consents where account_id = $1 and accepted",
        )
        .bind(account.0)
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        let a = Accepted {
            country: country.map(|c| c.0).unwrap_or_default(),
            versions: rows.into_iter().collect(),
        };
        self.accepted.insert(account.0, a.clone());
        Ok(a)
    }

    /// Documents the account must still accept: `(required, enforced_now)`.
    pub async fn pending(
        &self,
        db: &PgPool,
        account: AccountId,
        country: Option<&str>,
    ) -> Result<Vec<(Required, bool)>> {
        let acc = self.accepted_of(db, account).await?;
        let country = country.unwrap_or(&acc.country);
        let now = OffsetDateTime::now_utc().unix_timestamp();
        Ok(self
            .required()
            .into_iter()
            .filter(|r| {
                r.countries.is_empty()
                    || (!country.is_empty()
                        && r.countries.iter().any(|c| c.eq_ignore_ascii_case(country)))
            })
            .filter(|r| !acc.versions.contains(&(r.key.clone(), r.version)))
            .map(|r| {
                let enforced = r.required_from <= now;
                (r, enforced)
            })
            .collect())
    }

    /// The gate used by the bearer extractor.
    pub async fn check(&self, db: &PgPool, account: AccountId) -> Result<()> {
        if self.required.read().is_empty() {
            return Ok(());
        }
        let due: Vec<String> = self
            .pending(db, account, None)
            .await?
            .into_iter()
            .filter(|(_, enforced)| *enforced)
            .map(|(r, _)| format!("{} v{}", r.key, r.version))
            .collect();
        if due.is_empty() {
            Ok(())
        } else {
            Err(Error::PermissionDenied(format!(
                "consent_required: {}",
                due.join(", ")
            )))
        }
    }

    /// Forget one account's cached acceptance (after a consent change) or everything (after a
    /// document change), here and on every other node.
    pub async fn invalidate(
        &self,
        account: Option<AccountId>,
        cluster: Option<&Arc<musubi_cluster::Cluster>>,
        db: &PgPool,
    ) {
        self.apply(account, db).await;
        if let Some(c) = cluster {
            let msg = Msg {
                account: account.map(|a| a.0),
            };
            if let Err(e) = c.publish(TOPIC, &msg).await {
                tracing::warn!(error = %e, "legal broadcast failed");
            }
        }
    }

    async fn apply(&self, account: Option<AccountId>, db: &PgPool) {
        match account {
            Some(a) => {
                self.accepted.remove(&a.0);
            }
            None => {
                if let Err(e) = self.load(db).await {
                    tracing::warn!(error = %e, "legal documents reload failed");
                }
            }
        }
    }

    pub fn start(self: &Arc<Self>, cluster: &Arc<musubi_cluster::Cluster>, db: PgPool) {
        let me = self.clone();
        let mut rx = cluster.subscribe();
        let self_id = cluster.node_id();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(m) if m.topic == TOPIC && m.origin != self_id => {
                        if let Ok(b) = serde_json::from_value::<Msg>(m.payload) {
                            me.apply(b.account.map(AccountId), &db).await;
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
