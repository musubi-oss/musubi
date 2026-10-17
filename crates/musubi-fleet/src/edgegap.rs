//! Edgegap as a fleet provider: a game server is an Edgegap deployment (`POST /v1/deploy`) of the
//! fleet's app/version, polled until `Status.READY`, stopped with `DELETE /v1/stop/{id}`.

use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use musubi_core::config::EdgegapConfig;
use musubi_core::{Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

pub struct EdgegapProvider {
    cfg: EdgegapConfig,
    client: reqwest::Client,
    deployments: Arc<DashMap<Uuid, String>>,
}

impl EdgegapProvider {
    pub fn new(cfg: EdgegapConfig) -> Self {
        Self {
            cfg,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
            deployments: Arc::new(DashMap::new()),
        }
    }

    fn app_version(&self, req: &StartRequest<'_>) -> Result<(String, String)> {
        if let Some(img) = &req.fleet.spec.image {
            if let Some((a, v)) = img.split_once(':') {
                return Ok((a.to_string(), v.to_string()));
            }
        }
        if self.cfg.app.is_empty() || self.cfg.version.is_empty() {
            return Err(Error::InvalidArgument(
                "edgegap fleet needs spec.image = \"app:version\" or [fleet.edgegap] app/version"
                    .into(),
            ));
        }
        Ok((self.cfg.app.clone(), self.cfg.version.clone()))
    }

    async fn status(&self, request_id: &str) -> Result<Value> {
        let r = self
            .client
            .get(format!(
                "{}/v1/status/{request_id}",
                self.cfg.url.trim_end_matches('/')
            ))
            .header("authorization", &self.cfg.token)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("edgegap status: {e}")))?;
        let st = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        if !st.is_success() {
            return Err(Error::Unavailable(format!("edgegap status {st}: {body}")));
        }
        Ok(body)
    }
}

impl Provider for EdgegapProvider {
    fn name(&self) -> &'static str {
        "edgegap"
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let (app, version) = self.app_version(&req)?;
            let mut env: Vec<Value> = vec![
                json!({ "key": "MUSUBI_SERVER_ID", "value": req.server_id.to_string(), "is_secret": false }),
                json!({ "key": "MUSUBI_FLEET_ID", "value": req.fleet.id.to_string(), "is_secret": false }),
                json!({ "key": "MUSUBI_PORT", "value": req.port.to_string(), "is_secret": false }),
                json!({ "key": "MUSUBI_API", "value": req.api_url, "is_secret": false }),
                json!({ "key": "MUSUBI_SERVER_KEY", "value": req.server_key, "is_secret": true }),
            ];
            for (k, v) in &req.fleet.spec.env {
                env.push(json!({ "key": k, "value": v, "is_secret": false }));
            }
            let mut tags = vec![
                format!("musubi:{}", req.server_id),
                format!("fleet:{}", req.fleet.name),
            ];
            if !req.fleet.region.is_empty() {
                tags.push(format!("region:{}", req.fleet.region));
            }
            let body = json!({ "app_name": app, "version_name": version, "ip_list": [], "env_vars": env, "tags": tags });
            let r = self
                .client
                .post(format!("{}/v1/deploy", self.cfg.url.trim_end_matches('/')))
                .header("authorization", &self.cfg.token)
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Unavailable(format!("edgegap deploy: {e}")))?;
            let st = r.status();
            let out: Value = r.json().await.unwrap_or(Value::Null);
            if !st.is_success() {
                return Err(Error::Unavailable(format!("edgegap deploy {st}: {out}")));
            }
            let request_id = out["request_id"]
                .as_str()
                .ok_or_else(|| {
                    Error::internal(format!("edgegap deploy without request_id: {out}"))
                })?
                .to_string();
            self.deployments.insert(req.server_id, request_id.clone());
            let deadline = tokio::time::Instant::now()
                + Duration::from_secs(self.cfg.ready_timeout_secs.max(5));
            loop {
                let s = self.status(&request_id).await?;
                let status = s["current_status"].as_str().unwrap_or("");
                if status == "Status.READY" {
                    let address = s["fqdn"]
                        .as_str()
                        .or(s["public_ip"].as_str())
                        .ok_or_else(|| Error::internal(format!("edgegap ready without fqdn: {s}")))?
                        .to_string();
                    let ports = s["ports"].as_object();
                    let port = ports
                        .and_then(|m| {
                            m.get("game")
                                .or_else(|| m.values().next())
                                .and_then(|p| p["external"].as_i64())
                        })
                        .ok_or_else(|| {
                            Error::internal(format!("edgegap ready without ports: {s}"))
                        })?;
                    return Ok(Started {
                        address,
                        port: port as i32,
                        pid: None,
                    });
                }
                if status == "Status.ERROR" || status == "Status.TERMINATED" {
                    self.deployments.remove(&req.server_id);
                    return Err(Error::Unavailable(format!(
                        "edgegap deployment {request_id} {status}: {}",
                        s["error"]
                    )));
                }
                if tokio::time::Instant::now() >= deadline {
                    let _ = self.stop_request(&request_id).await;
                    self.deployments.remove(&req.server_id);
                    return Err(Error::Unavailable(format!(
                        "edgegap deployment {request_id} not ready in time ({status})"
                    )));
                }
                tokio::time::sleep(Duration::from_secs(self.cfg.poll_secs.max(1))).await;
            }
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some((_, request_id)) = self.deployments.remove(&server_id) {
                if let Err(e) = self.stop_request(&request_id).await {
                    tracing::warn!(server = %server_id, request = %request_id, error = %e, "edgegap stop failed");
                }
            }
        })
    }

    fn alive(&self, _server_id: Uuid) -> Option<bool> {
        None
    }
}

impl EdgegapProvider {
    async fn stop_request(&self, request_id: &str) -> Result<()> {
        let r = self
            .client
            .delete(format!(
                "{}/v1/stop/{request_id}",
                self.cfg.url.trim_end_matches('/')
            ))
            .header("authorization", &self.cfg.token)
            .send()
            .await
            .map_err(|e| Error::Unavailable(format!("edgegap stop: {e}")))?;
        if !r.status().is_success() && r.status().as_u16() != 404 {
            return Err(Error::Unavailable(format!("edgegap stop {}", r.status())));
        }
        Ok(())
    }
}
