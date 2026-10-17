//! First-party integrations: Zanshin (LiveOps — flags, experiments, analytics events) and Aurix
//! (voice — channel tokens). Both are reached over their server-key REST APIs; Musubi account ids
//! are the `external_id` on both sides, so a player is the same identity everywhere.

use std::sync::Arc;
use std::time::Duration;

use musubi_core::{Error, Result};

pub mod email;
pub mod iap;
pub use email::{Mail, Mailer};
pub use iap::{AppleStoreClient, GooglePlayClient, SteamClient, SteamInitTxn, StoreTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;

async fn check(res: reqwest::Response, what: &str) -> Result<Value> {
    let status = res.status().as_u16();
    let text = res.text().await.map_err(Error::internal)?;
    if !(200..300).contains(&status) {
        return Err(Error::Internal(format!(
            "{what}: upstream {status}: {text}"
        )));
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
}

// ---------------------------------------------------------------- Zanshin

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveOpsEvent {
    pub external_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

/// Zanshin server-key client. Events are batched per identity and flushed every `flush_every`
/// (or when the queue reaches `max_batch`); flags are evaluated live.
pub struct ZanshinClient {
    http: reqwest::Client,
    base: String,
    key: String,
    queue: Mutex<Vec<LiveOpsEvent>>,
    pub max_batch: usize,
}

impl ZanshinClient {
    pub fn new(url: &str, server_key: &str) -> Arc<Self> {
        Arc::new(Self {
            http: reqwest::Client::new(),
            base: url.trim_end_matches('/').to_string(),
            key: server_key.to_string(),
            queue: Mutex::new(Vec::new()),
            max_batch: 500,
        })
    }

    /// Queue an analytics event (never blocks the caller on the network).
    pub fn track(self: &Arc<Self>, external_id: &str, name: &str, metadata: Value) {
        let ev = LiveOpsEvent {
            external_id: external_id.to_string(),
            name: name.to_string(),
            value: None,
            metadata,
        };
        let me = self.clone();
        tokio::spawn(async move {
            let full = {
                let mut q = me.queue.lock().await;
                q.push(ev);
                q.len() >= me.max_batch
            };
            if full {
                me.flush().await;
            }
        });
    }

    /// Send everything queued, grouped by identity (`POST /server/v1/events`).
    pub async fn flush(&self) -> usize {
        let batch: Vec<LiveOpsEvent> = std::mem::take(&mut *self.queue.lock().await);
        if batch.is_empty() {
            return 0;
        }
        let mut by_id: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
        for e in &batch {
            by_id.entry(e.external_id.clone()).or_default().push(json!({
                "name": e.name, "value": e.value, "metadata": e.metadata
            }));
        }
        let mut sent = 0;
        for (xid, events) in by_id {
            let n = events.len();
            let res = self
                .http
                .post(format!("{}/server/v1/events", self.base))
                .header("x-api-key", &self.key)
                .json(&json!({ "external_id": xid, "events": events }))
                .send()
                .await;
            match res {
                Ok(r) if r.status().is_success() => sent += n,
                Ok(r) => tracing::warn!(status = %r.status(), "zanshin rejected events"),
                Err(e) => tracing::warn!(error = %e, "zanshin unreachable; events dropped"),
            }
        }
        sent
    }

    pub fn spawn_flusher(self: Arc<Self>, every: Duration) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(every);
            loop {
                iv.tick().await;
                self.flush().await;
            }
        })
    }

    /// Evaluate flags for an identity (`GET /server/v1/flags?external_id=&names=`).
    pub async fn flags(&self, external_id: &str, names: &[String]) -> Result<Value> {
        let mut req = self
            .http
            .get(format!("{}/server/v1/flags", self.base))
            .header("x-api-key", &self.key)
            .query(&[("external_id", external_id)]);
        if !names.is_empty() {
            req = req.query(&[("names", names.join(","))]);
        }
        let res = req.send().await.map_err(Error::internal)?;
        let v = check(res, "zanshin flags").await?;
        Ok(v.get("flags").cloned().unwrap_or(Value::Array(vec![])))
    }

    /// Live events targeting an identity (`GET /server/v1/live-events?external_id=`).
    pub async fn live_events(&self, external_id: &str) -> Result<Value> {
        let res = self
            .http
            .get(format!("{}/server/v1/live-events", self.base))
            .header("x-api-key", &self.key)
            .query(&[("external_id", external_id)])
            .send()
            .await
            .map_err(Error::internal)?;
        check(res, "zanshin live events").await
    }
}

// ---------------------------------------------------------------- Aurix

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceGrant {
    pub token: String,
    pub user_id: String,
    pub expires_at: String,
    pub channel_id: Option<String>,
    pub endpoint: Option<Value>,
}

pub struct AurixClient {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl AurixClient {
    pub fn new(url: &str, api_key: &str) -> Arc<Self> {
        Arc::new(Self {
            http: reqwest::Client::new(),
            base: url.trim_end_matches('/').to_string(),
            key: api_key.to_string(),
        })
    }

    /// Player token for an ad-hoc channel (created on first join, removed when empty).
    /// `kind` is Aurix's channel type (`team`, `positional`, `conference`, …).
    pub async fn token_for_channel(
        &self,
        external_id: &str,
        display_name: &str,
        channel_name: &str,
        kind: &str,
        max_participants: u32,
        speak: bool,
    ) -> Result<VoiceGrant> {
        let res = self
            .http
            .post(format!("{}/v1/tokens", self.base))
            .header("x-api-key", &self.key)
            .json(&json!({
                "external_id": external_id,
                "display_name": display_name,
                "channels": [{
                    "ad_hoc": { "name": channel_name, "channel_type": kind, "max_participants": max_participants },
                    "speak": speak,
                    "receive": true
                }]
            }))
            .send()
            .await
            .map_err(Error::internal)?;
        let v = check(res, "aurix token").await?;
        let channel_id = v["channels"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c.get("channel_id"))
            .and_then(|c| c.as_str())
            .map(str::to_string);
        Ok(VoiceGrant {
            token: v["token"].as_str().unwrap_or_default().to_string(),
            user_id: v["user_id"].as_str().unwrap_or_default().to_string(),
            expires_at: v["expires_at"].as_str().unwrap_or_default().to_string(),
            channel_id,
            endpoint: v.get("endpoint").cloned(),
        })
    }

    async fn post(&self, path: &str, body: Value, what: &str) -> Result<Value> {
        let res = self
            .http
            .post(format!("{}{}", self.base, path))
            .header("x-api-key", &self.key)
            .json(&body)
            .send()
            .await
            .map_err(Error::internal)?;
        check(res, what).await
    }

    /// Aurix's id of a Musubi account (its `external_id` there), if the account has ever been
    /// issued a voice token; `None` means there is nothing in Aurix to act on.
    pub async fn resolve_user(&self, external_id: &str) -> Result<Option<String>> {
        let res = self
            .http
            .get(format!("{}/v1/users", self.base))
            .header("x-api-key", &self.key)
            .query(&[("q", external_id), ("per_page", "20")])
            .send()
            .await
            .map_err(Error::internal)?;
        let v = check(res, "aurix users").await?;
        let items = v
            .as_array()
            .or_else(|| v["items"].as_array())
            .or_else(|| v["users"].as_array())
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .find(|u| u["external_id"].as_str() == Some(external_id))
            .and_then(|u| u["id"].as_str())
            .map(str::to_string))
    }

    /// Account ban mirrored into voice (`POST /v1/moderation/ban`, scope `account`, permanent —
    /// lifted by `unban_user`). `false`: the account never used voice.
    pub async fn ban_user(&self, external_id: &str, reason: &str) -> Result<bool> {
        let Some(uid) = self.resolve_user(external_id).await? else {
            return Ok(false);
        };
        self.post(
            "/v1/moderation/ban",
            json!({ "user_id": uid, "scope": "account", "reason": reason }),
            "aurix ban",
        )
        .await?;
        Ok(true)
    }

    pub async fn unban_user(&self, external_id: &str) -> Result<bool> {
        let Some(uid) = self.resolve_user(external_id).await? else {
            return Ok(false);
        };
        self.post(&format!("/v1/users/{uid}/unban"), json!({}), "aurix unban")
            .await?;
        Ok(true)
    }

    /// A friend-list block also silences the blocked player's voice for the blocker.
    pub async fn block(&self, external_id: &str, blocked_external_id: &str) -> Result<bool> {
        let (Some(a), Some(b)) = (
            self.resolve_user(external_id).await?,
            self.resolve_user(blocked_external_id).await?,
        ) else {
            return Ok(false);
        };
        self.post(
            &format!("/v1/users/{a}/blocks"),
            json!({ "blocked_user_id": b }),
            "aurix block",
        )
        .await?;
        Ok(true)
    }

    /// Player report filed in Aurix (`POST /v1/moderation/report`), where the voice evidence is.
    pub async fn report(
        &self,
        reporter_external_id: &str,
        target_external_id: &str,
        channel_id: Option<&str>,
        reason: &str,
    ) -> Result<Value> {
        let (Some(reporter), Some(target)) = (
            self.resolve_user(reporter_external_id).await?,
            self.resolve_user(target_external_id).await?,
        ) else {
            return Err(Error::NotFound("voice user not found".into()));
        };
        self.post(
            "/v1/moderation/report",
            json!({
                "reporter_user_id": reporter,
                "target_user_id": target,
                "channel_id": channel_id,
                "reason": reason,
            }),
            "aurix report",
        )
        .await
    }

    /// Channel-wide moderation (`POST /v1/moderation/kick-all`), e.g. when a match ends.
    pub async fn kick_all(&self, channel_id: &str, reason: &str) -> Result<()> {
        let res = self
            .http
            .post(format!("{}/v1/moderation/kick-all", self.base))
            .header("x-api-key", &self.key)
            .json(&json!({ "channel_id": channel_id, "reason": reason }))
            .send()
            .await
            .map_err(Error::internal)?;
        check(res, "aurix kick-all").await.map(|_| ())
    }
}
