//! Any orchestrator behind a small HTTP contract (`provider = "webhook"`): GameFabric, a custom
//! scheduler, a bare-metal panel. `POST {url}/servers` starts a server and answers with where it
//! listens (or `202` + an id to poll at `GET {url}/servers/{id}` until `status: "ready"`),
//! `DELETE {url}/servers/{id}` stops it.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::config::WebhookFleetConfig;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

pub struct WebhookProvider {
    cfg: WebhookFleetConfig,
    client: reqwest::Client,
    ids: Arc<DashMap<Uuid, String>>,
}

impl WebhookProvider {
    pub fn new(cfg: WebhookFleetConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            ids: Arc::new(DashMap::new()),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.cfg.url.trim_end_matches('/'))
    }

    fn auth(&self, r: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.cfg.token.is_empty() {
            r
        } else {
            r.header("authorization", format!("Bearer {}", self.cfg.token))
        }
    }

    fn started(v: &Value) -> Option<Started> {
        let address = v["address"].as_str().or(v["host"].as_str())?;
        let port = v["port"].as_i64()?;
        Some(Started {
            address: address.to_string(),
            port: port as i32,
            pid: None,
        })
    }

    async fn stop_id(&self, id: &str) -> Result<()> {
        let r = self
            .auth(self.client.delete(self.url(&format!("/servers/{id}"))))
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("webhook stop: {e}")))?;
        if !r.status().is_success() && r.status().as_u16() != 404 {
            return Err(Error::Unavailable(format!("webhook stop {}", r.status())));
        }
        Ok(())
    }
}

impl Provider for WebhookProvider {
    fn name(&self) -> &'static str {
        "webhook"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let mut env = req.fleet.spec.env.clone();
            env.insert("MUSUBI_SERVER_ID".into(), req.server_id.to_string());
            env.insert("MUSUBI_FLEET_ID".into(), req.fleet.id.to_string());
            env.insert("MUSUBI_PORT".into(), req.port.to_string());
            env.insert("MUSUBI_API".into(), req.api_url.to_string());
            env.insert("MUSUBI_SERVER_KEY".into(), req.server_key.to_string());
            let body = json!({
                "server_id": req.server_id,
                "port": req.port,
                "fleet": {
                    "id": req.fleet.id,
                    "name": req.fleet.name,
                    "region": req.fleet.region,
                    "pool": req.fleet.pool,
                    "image": req.fleet.spec.image,
                    "command": req.fleet.spec.command,
                    "vcpu": req.fleet.spec.vcpu,
                },
                "env": env,
            });
            let r = self
                .auth(self.client.post(self.url("/servers")))
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Unavailable(format!("webhook start: {e}")))?;
            let st = r.status();
            let out: Value = r.json().await.unwrap_or(Value::Null);
            if !st.is_success() {
                return Err(Error::Unavailable(format!("webhook start {st}: {out}")));
            }
            let id = out["id"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| req.server_id.to_string());
            self.ids.insert(req.server_id, id.clone());
            if let Some(s) = Self::started(&out) {
                return Ok(s);
            }
            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(self.cfg.ready_timeout_secs.max(5));
            loop {
                tokio::time::sleep(Duration::from_secs(self.cfg.poll_secs.max(1))).await;
                let r = self
                    .auth(self.client.get(self.url(&format!("/servers/{id}"))))
                    .send()
                    .await
                    .map_err(|e| Error::Unavailable(format!("webhook status: {e}")))?;
                let st = r.status();
                let s: Value = r.json().await.unwrap_or(Value::Null);
                if !st.is_success() {
                    self.ids.remove(&req.server_id);
                    return Err(Error::Unavailable(format!("webhook status {st}: {s}")));
                }
                let status = s["status"].as_str().unwrap_or("");
                if status == "ready" || (status.is_empty() && Self::started(&s).is_some()) {
                    if let Some(started) = Self::started(&s) {
                        return Ok(started);
                    }
                    return Err(Error::internal(format!(
                        "webhook server {id} ready without address/port: {s}"
                    )));
                }
                if status == "error" || status == "stopped" {
                    self.ids.remove(&req.server_id);
                    return Err(Error::Unavailable(format!(
                        "webhook server {id} {status}: {}",
                        s["error"]
                    )));
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = self.stop_id(&id).await;
                    self.ids.remove(&req.server_id);
                    return Err(Error::Unavailable(format!(
                        "webhook server {id} not ready in time ({status})"
                    )));
                }
            }
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some((_, id)) = self.ids.remove(&server_id) {
                if let Err(e) = self.stop_id(&id).await {
                    tracing::warn!(server = %server_id, id = %id, error = %e, "webhook stop failed");
                }
            }
        })
    }

    fn alive(&self, _server_id: Uuid) -> Option<bool> {
        None
    }
}
