//! `kubernetes` and `agones` providers over the raw Kubernetes REST API (no client-go-sized
//! dependency): one Pod with a hostPort per game server, or one Agones `GameServer` CR whose
//! dynamic port and address Agones fills in. Credentials come from the service-account mount when
//! the node runs in a pod, or from `[fleet.kubernetes]`.

use std::time::Duration;

use musubi_core::{Error, Result};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{Provider, StartRequest, Started};

const SA_DIR: &str = "/var/run/secrets/kubernetes.io/serviceaccount";

#[derive(Debug, Clone)]
pub struct KubeConfig {
    pub url: String,
    pub token: Option<String>,
    pub namespace: String,
    pub ca_cert_pem: Option<Vec<u8>>,
    pub insecure: bool,
}

impl KubeConfig {
    /// Service-account discovery; `None` outside a pod.
    pub fn in_cluster() -> Option<Self> {
        let host = std::env::var("KUBERNETES_SERVICE_HOST").ok()?;
        let port = std::env::var("KUBERNETES_SERVICE_PORT").unwrap_or_else(|_| "443".into());
        let read = |f: &str| std::fs::read_to_string(format!("{SA_DIR}/{f}")).ok();
        Some(Self {
            url: format!("https://{host}:{port}"),
            token: read("token").map(|t| t.trim().to_string()),
            namespace: read("namespace")
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|| "default".into()),
            ca_cert_pem: std::fs::read(format!("{SA_DIR}/ca.crt")).ok(),
            insecure: false,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Plain pods: `hostPort` = the fleet port, address = the node's host IP.
    Pods,
    /// Agones `GameServer` CRs: dynamic port, address from `status.address`.
    Agones,
}

pub struct KubernetesProvider {
    client: reqwest::Client,
    cfg: KubeConfig,
    mode: Mode,
    poll: Duration,
    ready_timeout: Duration,
}

fn name_of(id: Uuid) -> String {
    format!("musubi-gs-{}", id.simple())
}

impl KubernetesProvider {
    pub fn new(cfg: KubeConfig, mode: Mode) -> Result<Self> {
        let mut b = reqwest::Client::builder().timeout(Duration::from_secs(15));
        if let Some(pem) = &cfg.ca_cert_pem {
            b = b.add_root_certificate(
                reqwest::Certificate::from_pem(pem).map_err(Error::internal)?,
            );
        }
        if cfg.insecure {
            b = b.danger_accept_invalid_certs(true);
        }
        Ok(Self {
            client: b.build().map_err(Error::internal)?,
            cfg,
            mode,
            poll: Duration::from_millis(500),
            ready_timeout: Duration::from_secs(90),
        })
    }

    /// How often to poll for the address, and how long before a scheduling attempt is abandoned.
    pub fn with_timeouts(mut self, poll: Duration, ready_timeout: Duration) -> Self {
        self.poll = poll;
        self.ready_timeout = ready_timeout;
        self
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self
            .client
            .request(m, format!("{}{}", self.cfg.url.trim_end_matches('/'), path));
        if let Some(t) = &self.cfg.token {
            r = r.bearer_auth(t);
        }
        r
    }

    fn path(&self, name: Option<&str>) -> String {
        let ns = &self.cfg.namespace;
        let base = match self.mode {
            Mode::Pods => format!("/api/v1/namespaces/{ns}/pods"),
            Mode::Agones => format!("/apis/agones.dev/v1/namespaces/{ns}/gameservers"),
        };
        match name {
            Some(n) => format!("{base}/{n}"),
            None => base,
        }
    }

    fn manifest(&self, req: &StartRequest<'_>) -> Result<Value> {
        let image = req.fleet.spec.image.clone().ok_or_else(|| {
            Error::InvalidArgument(format!("{} fleet needs spec.image", self.name()))
        })?;
        let name = name_of(req.server_id);
        let port = req.port;
        let env: Vec<Value> = [
            ("MUSUBI_SERVER_ID", req.server_id.to_string()),
            ("MUSUBI_FLEET_ID", req.fleet.id.to_string()),
            ("MUSUBI_PORT", port.to_string()),
            ("MUSUBI_API", req.api_url.to_string()),
            ("MUSUBI_SERVER_KEY", req.server_key.to_string()),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .chain(
            req.fleet
                .spec
                .env
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        )
        .map(|(k, v)| json!({ "name": k, "value": v }))
        .collect();
        let labels = json!({
            "app.kubernetes.io/managed-by": "musubi",
            "musubi.fleet": req.fleet.id.to_string(),
            "musubi.server": req.server_id.to_string(),
        });
        let mut container = json!({
            "name": "gameserver",
            "image": image,
            "args": req.fleet.spec.command,
            "env": env,
        });
        if let Some(res) = req.fleet.spec.resources.kubernetes() {
            container["resources"] = res;
        }
        Ok(match self.mode {
            Mode::Pods => {
                let publish = !crate::port_policy_none(&req.fleet.spec);
                let ports: Vec<Value> = crate::port_bindings(&req.fleet.spec, port)
                    .into_iter()
                    .map(|(n, host, container, proto)| {
                        let mut p = json!({ "name": format!("{n}-{proto}"), "containerPort": container, "protocol": proto.to_uppercase() });
                        if publish {
                            p["hostPort"] = json!(host);
                        }
                        p
                    })
                    .collect();
                container["ports"] = json!(ports);
                json!({
                    "apiVersion": "v1",
                    "kind": "Pod",
                    "metadata": { "name": name, "labels": labels },
                    "spec": { "restartPolicy": "Never", "containers": [container] },
                })
            }
            Mode::Agones => json!({
                "apiVersion": "agones.dev/v1",
                "kind": "GameServer",
                "metadata": { "name": name, "labels": labels },
                "spec": {
                    "ports": crate::port_bindings(&req.fleet.spec, port).into_iter().map(|(n, _, container, proto)| json!({
                        "name": format!("{n}-{proto}"),
                        "portPolicy": if crate::port_policy_none(&req.fleet.spec) { "None" } else { "Dynamic" },
                        "containerPort": container, "protocol": proto.to_uppercase() })).collect::<Vec<Value>>(),
                    "health": { "disabled": true },
                    "template": { "metadata": { "labels": labels }, "spec": { "containers": [container] } },
                },
            }),
        })
    }

    /// `Ok(Some)` once reachable, `Ok(None)` while scheduling, `Err` on a terminal state.
    fn endpoint(&self, obj: &Value, fleet_port: i32) -> Result<Option<(String, i32)>> {
        match self.mode {
            Mode::Pods => {
                let phase = obj["status"]["phase"].as_str().unwrap_or("");
                if phase == "Failed" || phase == "Succeeded" {
                    return Err(Error::Unavailable(format!("pod {phase}")));
                }
                Ok(obj["status"]["hostIP"]
                    .as_str()
                    .filter(|ip| !ip.is_empty())
                    .map(|ip| (ip.to_string(), fleet_port)))
            }
            Mode::Agones => {
                let state = obj["status"]["state"].as_str().unwrap_or("");
                if matches!(state, "Error" | "Unhealthy" | "Shutdown") {
                    return Err(Error::Unavailable(format!("gameserver {state}")));
                }
                let addr = obj["status"]["address"].as_str().filter(|a| !a.is_empty());
                let port = obj["status"]["ports"][0]["port"].as_i64();
                Ok(match (addr, port) {
                    (Some(a), Some(p)) => Some((a.to_string(), p as i32)),
                    _ => None,
                })
            }
        }
    }

    async fn delete(&self, name: &str) {
        let r = tokio::time::timeout(
            Duration::from_secs(15),
            self.req(reqwest::Method::DELETE, &self.path(Some(name)))
                .send(),
        )
        .await;
        if let Ok(Err(e)) = r {
            tracing::warn!(name, error = %e, "kubernetes delete failed");
        }
    }
}

impl Provider for KubernetesProvider {
    fn name(&self) -> &'static str {
        match self.mode {
            Mode::Pods => "kubernetes",
            Mode::Agones => "agones",
        }
    }

    fn start<'a>(
        &'a self,
        req: StartRequest<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Started>> + Send + 'a>> {
        Box::pin(async move {
            let body = self.manifest(&req)?;
            let name = name_of(req.server_id);
            let res = self
                .req(reqwest::Method::POST, &self.path(None))
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::Unavailable(format!("kubernetes api: {e}")))?;
            let status = res.status();
            if !status.is_success() && status.as_u16() != 409 {
                let text = res.text().await.unwrap_or_default();
                return Err(Error::Internal(format!(
                    "kubernetes create {}: {status} {}",
                    self.name(),
                    text.lines().next().unwrap_or("")
                )));
            }
            let deadline = tokio::time::Instant::now() + self.ready_timeout;
            loop {
                tokio::time::sleep(self.poll).await;
                let obj: Value = match self
                    .req(reqwest::Method::GET, &self.path(Some(&name)))
                    .send()
                    .await
                {
                    Ok(r) if r.status().is_success() => r.json().await.unwrap_or(Value::Null),
                    Ok(r) => {
                        self.delete(&name).await;
                        return Err(Error::Internal(format!(
                            "kubernetes get {name}: {}",
                            r.status()
                        )));
                    }
                    Err(e) => {
                        tracing::debug!(error = %e, "kubernetes poll failed");
                        Value::Null
                    }
                };
                match self.endpoint(&obj, req.port) {
                    Ok(Some((address, port))) => {
                        tracing::info!(server = %req.server_id, %name, %address, port, provider = self.name(), "game server scheduled");
                        return Ok(Started {
                            address,
                            port,
                            pid: None,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.delete(&name).await;
                        return Err(e);
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    self.delete(&name).await;
                    return Err(Error::Unavailable(format!(
                        "{name}: not scheduled within {:?}",
                        self.ready_timeout
                    )));
                }
            }
        })
    }

    fn stop<'a>(
        &'a self,
        server_id: Uuid,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move { self.delete(&name_of(server_id)).await })
    }

    /// Liveness is learned from health pings; the API is not polled per tick.
    fn alive(&self, _server_id: Uuid) -> Option<bool> {
        None
    }

    fn logs<'a>(
        &'a self,
        server_id: Uuid,
        tail: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<String>>> + Send + 'a>>
    {
        Box::pin(async move {
            if !matches!(self.mode, Mode::Pods) {
                return Ok(None);
            }
            let name = name_of(server_id);
            let path = format!(
                "{}/log?tailLines={}&timestamps=true",
                self.path(Some(&name)),
                tail.clamp(1, 10_000)
            );
            let resp = self
                .req(reqwest::Method::GET, &path)
                .send()
                .await
                .map_err(Error::internal)?;
            if !resp.status().is_success() {
                return Err(Error::Internal(format!("pod log: {}", resp.status())));
            }
            Ok(Some(resp.text().await.map_err(Error::internal)?))
        })
    }
}
