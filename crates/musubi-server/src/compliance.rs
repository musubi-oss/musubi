//! Age gate and parental controls (COPPA / GDPR-K / platform age ratings). An account carries an
//! age bracket — `unknown`, `child` (under `child_until`), `teen` (under `adult_from`), `adult` —
//! set from a birthdate the player gives, a platform's age signal, or the game server. Per
//! bracket the config lists features that are closed: `chat`, `friends`, `purchases`, `ugc`,
//! `public_profile`. A verified parent (consent token sent to `parent_email`) switches a child
//! to the `consented_child_restrictions` list. Like bans, the view is in memory on every node and
//! synced over the bus (topic `compliance.age`), so a change bites on the next request.

use std::sync::Arc;

use dashmap::DashMap;
use musubi_core::{config::ComplianceConfig, AccountId, Error, Result};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::Date;
use uuid::Uuid;

pub const AGE_TOPIC: &str = "compliance.age";

pub const FEATURES: &[&str] = &["chat", "friends", "purchases", "ugc", "public_profile"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Bracket {
    #[default]
    Unknown,
    Child,
    Teen,
    Adult,
}

impl Bracket {
    pub fn as_str(self) -> &'static str {
        match self {
            Bracket::Unknown => "unknown",
            Bracket::Child => "child",
            Bracket::Teen => "teen",
            Bracket::Adult => "adult",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unknown" => Bracket::Unknown,
            "child" => Bracket::Child,
            "teen" => Bracket::Teen,
            "adult" => Bracket::Adult,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgeInfo {
    pub bracket: Bracket,
    /// A parent/guardian confirmed the consent token.
    pub consented: bool,
}

#[derive(Serialize, Deserialize)]
struct AgeMsg {
    id: Uuid,
    info: AgeInfo,
}

pub struct AgeGate {
    cfg: ComplianceConfig,
    ids: DashMap<Uuid, AgeInfo>,
}

impl AgeGate {
    pub fn new(cfg: ComplianceConfig) -> Self {
        Self {
            cfg,
            ids: DashMap::new(),
        }
    }

    pub fn config(&self) -> &ComplianceConfig {
        &self.cfg
    }

    pub fn get(&self, id: AccountId) -> AgeInfo {
        self.ids.get(&id.0).map(|e| *e).unwrap_or_default()
    }

    /// Bracket for a birthdate as of `today`.
    pub fn bracket_for(&self, birthdate: Date, today: Date) -> Bracket {
        let mut years = today.year() - birthdate.year();
        if (today.month() as u8, today.day()) < (birthdate.month() as u8, birthdate.day()) {
            years -= 1;
        }
        if years < self.cfg.child_until as i32 {
            Bracket::Child
        } else if years < self.cfg.adult_from as i32 {
            Bracket::Teen
        } else {
            Bracket::Adult
        }
    }

    /// Features currently closed for the account.
    pub fn restrictions(&self, id: AccountId) -> Vec<String> {
        if !self.cfg.age_gate {
            return vec![];
        }
        let info = self.get(id);
        let bracket = match info.bracket {
            Bracket::Unknown if self.cfg.unknown_as_child => Bracket::Child,
            Bracket::Unknown => Bracket::Adult,
            b => b,
        };
        match bracket {
            Bracket::Child if info.consented => self.cfg.consented_child_restrictions.clone(),
            Bracket::Child => self.cfg.child_restrictions.clone(),
            Bracket::Teen => self.cfg.teen_restrictions.clone(),
            _ => vec![],
        }
    }

    pub fn restricted(&self, id: AccountId, feature: &str) -> bool {
        self.cfg.age_gate && self.restrictions(id).iter().any(|f| f == feature)
    }

    pub fn check(&self, id: AccountId, feature: &str) -> Result<()> {
        if self.restricted(id, feature) {
            return Err(Error::PermissionDenied(format!(
                "{feature} is not available for this account's age group"
            )));
        }
        Ok(())
    }

    fn apply(&self, id: Uuid, info: AgeInfo) {
        if info == AgeInfo::default() {
            self.ids.remove(&id);
        } else {
            self.ids.insert(id, info);
        }
    }

    /// Persist, record locally and tell the other nodes. `birthdate` is stored when given.
    pub async fn set(
        &self,
        db: &PgPool,
        id: AccountId,
        info: AgeInfo,
        birthdate: Option<Date>,
        cluster: Option<&Arc<musubi_cluster::Cluster>>,
    ) -> Result<()> {
        sqlx::query(
            "update musubi.accounts
                set age_bracket = $2,
                    parental_consent_at = case when $3 then coalesce(parental_consent_at, now()) end,
                    birthdate = coalesce($4, birthdate),
                    updated_at = now()
              where id = $1",
        )
        .bind(id.0)
        .bind(info.bracket.as_str())
        .bind(info.consented)
        .bind(birthdate)
        .execute(db)
        .await
        .map_err(Error::internal)?;
        self.apply(id.0, info);
        if let Some(c) = cluster {
            if let Err(e) = c.publish(AGE_TOPIC, &AgeMsg { id: id.0, info }).await {
                tracing::warn!(error = %e, "age bracket broadcast failed");
            }
        }
        Ok(())
    }

    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        let rows: Vec<(Uuid, String, bool)> = sqlx::query_as(
            "select id, age_bracket, parental_consent_at is not null
               from musubi.accounts
              where deleted_at is null and (age_bracket <> 'unknown' or parental_consent_at is not null)",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (id, bracket, consented) in &rows {
            self.apply(
                *id,
                AgeInfo {
                    bracket: Bracket::parse(bracket).unwrap_or_default(),
                    consented: *consented,
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
                    Ok(m) if m.topic == AGE_TOPIC && m.origin != self_id => {
                        if let Ok(a) = serde_json::from_value::<AgeMsg>(m.payload) {
                            me.apply(a.id, a.info);
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
