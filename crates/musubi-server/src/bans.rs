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
    /// Non-empty = a feature ban: the account keeps working except for these features
    /// (`chat`, `matchmaking`, `groups`, `ugc`, `trading`, `leaderboards`, `voice`, `friends`).
    #[serde(default)]
    pub features: Vec<String>,
}

pub const FEATURES: &[&str] = &[
    "chat",
    "matchmaking",
    "groups",
    "ugc",
    "trading",
    "leaderboards",
    "voice",
    "friends",
];

impl BanInfo {
    /// A full ban denies every request; shadow and feature bans do not.
    pub fn is_full(&self) -> bool {
        !self.shadow && self.features.is_empty()
    }

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
        self.get(id).is_some_and(|b| b.is_full())
    }

    /// Deny when the account is fully banned or feature-banned for `feature`.
    pub fn check_feature(&self, id: AccountId, feature: &str) -> Result<()> {
        match self.get(id) {
            Some(b) if b.is_full() => Err(Error::PermissionDenied(describe(&b))),
            Some(b) if b.features.iter().any(|f| f == feature) => {
                Err(Error::PermissionDenied(describe_feature(&b, feature)))
            }
            _ => Ok(()),
        }
    }

    pub fn is_shadow(&self, id: AccountId) -> bool {
        self.get(id).is_some_and(|b| b.shadow)
    }

    pub fn check(&self, id: AccountId) -> Result<()> {
        match self.get(id) {
            Some(b) if b.is_full() => Err(Error::PermissionDenied(describe(&b))),
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
        type Row = (
            Uuid,
            Option<String>,
            Option<String>,
            Option<i64>,
            bool,
            Option<Vec<String>>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select a.id, b.reason, b.message,
                    (extract(epoch from b.until))::bigint, coalesce(b.shadow, false), b.features
             from musubi.accounts a
             left join musubi.account_bans b on b.account_id = a.id
             where a.deleted_at is null
               and (b.account_id is not null or a.disabled_at is not null)
               and (b.until is null or b.until > now())",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (id, reason, message, until, shadow, features) in &rows {
            self.ids.insert(
                *id,
                BanInfo {
                    reason: reason.clone(),
                    message: message.clone(),
                    until: *until,
                    shadow: *shadow,
                    features: features.clone().unwrap_or_default(),
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

/// Player-facing text for a feature ban: `chat banned: <message> (until …)`.
pub fn describe_feature(b: &BanInfo, feature: &str) -> String {
    let mut s = format!("{feature} banned");
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

// ---------------------------------------------------------------------------------------------
// Device / IP / CIDR rules

pub const RULE_TOPIC: &str = "admin.ban_rule";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub id: Uuid,
    /// `device` | `ip` | `cidr`
    pub kind: String,
    pub value: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// Unix seconds; `None` = permanent.
    #[serde(default)]
    pub until: Option<i64>,
}

impl Rule {
    pub fn expired(&self) -> bool {
        self.until
            .is_some_and(|u| u <= OffsetDateTime::now_utc().unix_timestamp())
    }

    fn info(&self) -> BanInfo {
        BanInfo {
            reason: self.reason.clone(),
            message: self.message.clone(),
            until: self.until,
            shadow: false,
            features: Vec::new(),
        }
    }
}

/// `a.b.c.d/n` or `::/n`; a bare address is a /32 or /128.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IpNet {
    addr: std::net::IpAddr,
    prefix: u8,
}

impl IpNet {
    pub fn parse(s: &str) -> Option<IpNet> {
        let (a, p) = match s.split_once('/') {
            Some((a, p)) => (a, Some(p)),
            None => (s, None),
        };
        let addr: std::net::IpAddr = a.trim().parse().ok()?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match p {
            Some(p) => p.trim().parse::<u8>().ok().filter(|p| *p <= max)?,
            None => max,
        };
        Some(IpNet { addr, prefix })
    }

    pub fn contains(&self, ip: std::net::IpAddr) -> bool {
        match (self.addr, ip) {
            (std::net::IpAddr::V4(n), std::net::IpAddr::V4(a)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u32::MAX << (32 - self.prefix as u32)
                };
                (u32::from(n) & mask) == (u32::from(a) & mask)
            }
            (std::net::IpAddr::V6(n), std::net::IpAddr::V6(a)) => {
                let mask = if self.prefix == 0 {
                    0
                } else {
                    u128::MAX << (128 - self.prefix as u32)
                };
                (u128::from(n) & mask) == (u128::from(a) & mask)
            }
            _ => false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct RuleMsg {
    id: Uuid,
    /// `None` = removed.
    rule: Option<Rule>,
}

/// Cluster-wide cache of device / IP / CIDR bans, consulted before authentication (IP at the
/// `/v1/auth/*` and `/ws` gate, device id at login).
#[derive(Default)]
pub struct BanRules {
    devices: DashMap<String, Rule>,
    nets: parking_lot::RwLock<Vec<(IpNet, Rule)>>,
}

impl BanRules {
    pub fn validate(kind: &str, value: &str) -> Result<()> {
        match kind {
            "device" if !value.is_empty() && value.len() <= 256 => Ok(()),
            "ip" if value.parse::<std::net::IpAddr>().is_ok() => Ok(()),
            "cidr" if IpNet::parse(value).is_some() && value.contains('/') => Ok(()),
            "device" => Err(Error::InvalidArgument("device id: 1..256 chars".into())),
            "ip" => Err(Error::InvalidArgument("ip: not an address".into())),
            "cidr" => Err(Error::InvalidArgument("cidr: expected a.b.c.d/n".into())),
            _ => Err(Error::InvalidArgument("kind: device | ip | cidr".into())),
        }
    }

    pub fn check_device(&self, device: &str) -> Result<()> {
        match self.devices.get(device) {
            Some(r) if !r.expired() => Err(Error::PermissionDenied(format!(
                "device banned{}",
                describe(&r.info()).trim_start_matches("account banned")
            ))),
            _ => Ok(()),
        }
    }

    pub fn check_ip(&self, ip: &str) -> Result<()> {
        let Ok(addr) = ip.parse::<std::net::IpAddr>() else {
            return Ok(());
        };
        let nets = self.nets.read();
        match nets.iter().find(|(n, r)| !r.expired() && n.contains(addr)) {
            Some((_, r)) => Err(Error::PermissionDenied(format!(
                "address banned{}",
                describe(&r.info()).trim_start_matches("account banned")
            ))),
            None => Ok(()),
        }
    }

    pub fn len(&self) -> usize {
        self.devices.len() + self.nets.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn apply(&self, id: Uuid, rule: Option<Rule>) {
        // drop any previous version of this rule
        self.devices.retain(|_, r| r.id != id);
        self.nets.write().retain(|(_, r)| r.id != id);
        if let Some(r) = rule {
            if r.kind == "device" {
                self.devices.insert(r.value.clone(), r);
            } else if let Some(n) = IpNet::parse(&r.value) {
                self.nets.write().push((n, r));
            }
        }
    }

    pub async fn set(
        &self,
        id: Uuid,
        rule: Option<Rule>,
        cluster: Option<&Arc<musubi_cluster::Cluster>>,
    ) {
        self.apply(id, rule.clone());
        if let Some(c) = cluster {
            if let Err(e) = c.publish(RULE_TOPIC, &RuleMsg { id, rule }).await {
                tracing::warn!(error = %e, "ban rule broadcast failed");
            }
        }
    }

    pub async fn load(&self, db: &PgPool) -> Result<usize> {
        type Row = (
            Uuid,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
        );
        let rows: Vec<Row> = sqlx::query_as(
            "select id, kind, value, reason, message, (extract(epoch from until))::bigint
             from musubi.ban_rules where until is null or until > now()",
        )
        .fetch_all(db)
        .await
        .map_err(Error::internal)?;
        for (id, kind, value, reason, message, until) in rows.iter().cloned() {
            self.apply(
                id,
                Some(Rule {
                    id,
                    kind,
                    value,
                    reason,
                    message,
                    until,
                }),
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
                    Ok(m) if m.topic == RULE_TOPIC && m.origin != self_id => {
                        if let Ok(b) = serde_json::from_value::<RuleMsg>(m.payload) {
                            me.apply(b.id, b.rule);
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

/// Registry upkeep after a login: remember the device (from the session row) and the address.
pub async fn note_login(db: &PgPool, session: Uuid, account: AccountId, ip: &str) -> Result<()> {
    sqlx::query(
        "insert into musubi.account_devices (account_id, device_id, user_agent, last_ip)
         select account_id, device_id, user_agent, ip from musubi.sessions
         where id = $1 and device_id is not null and device_id <> ''
         on conflict (account_id, device_id) do update set
            last_seen_at = now(), logins = musubi.account_devices.logins + 1,
            user_agent = coalesce(excluded.user_agent, musubi.account_devices.user_agent),
            last_ip = coalesce(excluded.last_ip, musubi.account_devices.last_ip)",
    )
    .bind(session)
    .execute(db)
    .await
    .map_err(Error::internal)?;
    if ip.parse::<std::net::IpAddr>().is_ok() {
        sqlx::query(
            "insert into musubi.account_ips (account_id, ip) values ($1, $2::inet)
             on conflict (account_id, ip) do update set
                last_seen_at = now(), logins = musubi.account_ips.logins + 1",
        )
        .bind(account.0)
        .bind(ip)
        .execute(db)
        .await
        .map_err(Error::internal)?;
    }
    Ok(())
}

/// Axum gate for `/v1/auth/*` and `/ws`: a banned address never reaches a handler.
pub async fn ip_gate(
    axum::extract::State(s): axum::extract::State<crate::app::AppState>,
    ip: crate::http::ClientIp,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    if let Err(e) = s.ban_rules.check_ip(&ip.0) {
        return axum::response::IntoResponse::into_response(crate::http::ApiError::from(e));
    }
    next.run(req).await
}

#[cfg(test)]
mod rule_tests {
    use super::*;

    #[test]
    fn cidr_matching_and_validation() {
        let n = IpNet::parse("10.1.0.0/16").unwrap();
        assert!(n.contains("10.1.200.3".parse().unwrap()));
        assert!(!n.contains("10.2.0.1".parse().unwrap()));
        assert!(!n.contains("::1".parse().unwrap()));
        let v6 = IpNet::parse("2001:db8::/32").unwrap();
        assert!(v6.contains("2001:db8:1::5".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));
        assert!(IpNet::parse("1.2.3.4")
            .unwrap()
            .contains("1.2.3.4".parse().unwrap()));
        assert!(IpNet::parse("1.2.3.4/33").is_none());
        assert!(BanRules::validate("cidr", "10.0.0.0/8").is_ok());
        assert!(BanRules::validate("cidr", "10.0.0.1").is_err());
        assert!(BanRules::validate("ip", "nope").is_err());
        assert!(BanRules::validate("device", "").is_err());
        assert!(BanRules::validate("hwid", "x").is_err());
    }

    #[test]
    fn rules_match_expire_and_replace() {
        let r = BanRules::default();
        let id = Uuid::now_v7();
        r.apply(
            id,
            Some(Rule {
                id,
                kind: "cidr".into(),
                value: "192.168.0.0/24".into(),
                reason: None,
                message: Some("vpn range".into()),
                until: None,
            }),
        );
        assert!(r.check_ip("192.168.0.77").is_err());
        assert!(r.check_ip("192.168.1.1").is_ok());
        assert!(r.check_ip("unknown").is_ok());
        let d = Uuid::now_v7();
        r.apply(
            d,
            Some(Rule {
                id: d,
                kind: "device".into(),
                value: "dev-1".into(),
                reason: None,
                message: None,
                until: Some(1),
            }),
        );
        assert!(
            r.check_device("dev-1").is_ok(),
            "expired rule does not bite"
        );
        r.apply(id, None);
        assert!(r.check_ip("192.168.0.77").is_ok());
        assert_eq!(r.len(), 1);
    }
}
